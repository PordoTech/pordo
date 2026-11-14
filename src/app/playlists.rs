use super::workspace::child;
use super::*;
use anyhow::{Result, bail, ensure};
use library::Record;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Playlist {
    id: String,
    name: String,
    created: u64,
    modified: u64,
    tracks: Vec<Record>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Data {
    version: u32,
    playlists: Vec<Playlist>,
}
impl Default for Data {
    fn default() -> Self {
        Self {
            version: 1,
            playlists: vec![],
        }
    }
}
struct Store {
    data: Data,
    path: PathBuf,
    // Never overwrite a file we could not read, including newer formats.
    load_error: Option<String>,
}
fn identity(record: &Record) -> String {
    record
        .online
        .as_ref()
        .map(|s| format!("online://{}", s.key()))
        .unwrap_or_else(|| record.path.clone())
}
fn append_tracks(tracks: &mut Vec<Record>, records: &[Record]) -> usize {
    let mut keys: HashSet<_> = tracks.iter().map(identity).collect();
    let before = tracks.len();
    for record in records.iter().filter(|r| r.music) {
        if keys.insert(identity(record)) {
            tracks.push(Record {
                path: record.path.clone(),
                title: record.title.clone(),
                artist: record.artist.clone(),
                album: record.album.clone(),
                music: true,
                duration: record.duration,
                online: record.online.clone(),
                ..Default::default()
            });
        }
    }
    tracks.len() - before
}
impl Store {
    fn open(path: PathBuf) -> Self {
        let loaded = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Data>(&bytes)
                .map_err(anyhow::Error::from)
                .and_then(|data| {
                    ensure!(data.version == 1, "Unsupported playlist format");
                    Ok(data)
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Data::default()),
            Err(error) => Err(error.into()),
        };
        match loaded {
            Ok(data) => Self {
                data,
                path,
                load_error: None,
            },
            Err(error) => Self {
                data: Data::default(),
                path,
                load_error: Some(error.to_string()),
            },
        }
    }
    fn commit<T>(&mut self, change: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        if let Some(error) = &self.load_error {
            bail!("{error}");
        }
        let mut next = self.data.clone();
        let result = change(&mut next)?;
        crate::comics::cache::atomic_write(&self.path, &serde_json::to_vec_pretty(&next)?)?;
        self.data = next;
        Ok(result)
    }
    fn named(&mut self, id: Option<&str>, name: &str, pending: &[Record]) -> Result<String> {
        let name = name.trim();
        ensure!(
            !name.is_empty() && name.chars().count() <= 40,
            "名称需为 1–40 个字符 / Use 1–40 characters"
        );
        self.commit(|data| {
            ensure!(
                !data
                    .playlists
                    .iter()
                    .any(|p| Some(p.id.as_str()) != id
                        && p.name.to_lowercase() == name.to_lowercase()),
                "歌单名称已存在 / Playlist name already exists"
            );
            let now = library::favorite_time();
            if let Some(id) = id {
                let playlist = data
                    .playlists
                    .iter_mut()
                    .find(|p| p.id == id)
                    .ok_or_else(|| anyhow::anyhow!("Playlist not found"))?;
                playlist.name = name.into();
                playlist.modified = now;
                Ok(id.to_owned())
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                data.playlists.insert(
                    0,
                    Playlist {
                        id: id.clone(),
                        name: name.into(),
                        created: now,
                        modified: now,
                        tracks: {
                            let mut tracks = vec![];
                            append_tracks(&mut tracks, pending);
                            tracks
                        },
                    },
                );
                Ok(id)
            }
        })
    }
    fn edit<T>(&mut self, id: &str, change: impl FnOnce(&mut Playlist) -> Result<T>) -> Result<T> {
        self.commit(|data| {
            let playlist = data
                .playlists
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| anyhow::anyhow!("Playlist not found"))?;
            let result = change(playlist)?;
            playlist.modified = library::favorite_time();
            Ok(result)
        })
    }
    fn add(&mut self, id: &str, records: &[Record]) -> Result<usize> {
        self.edit(id, |playlist| {
            Ok(append_tracks(&mut playlist.tracks, records))
        })
    }
    fn relocate(&mut self, id: &str, from: usize, to: usize) -> Result<()> {
        self.edit(id, |playlist| {
            ensure!(
                from < playlist.tracks.len() && to <= playlist.tracks.len(),
                "Invalid song position"
            );
            let item = playlist.tracks.remove(from);
            playlist
                .tracks
                .insert(if to > from { to - 1 } else { to }, item);
            Ok(())
        })
    }
}
#[derive(Clone)]
enum Dialog {
    Name {
        id: Option<String>,
        name: String,
        pending: Vec<Record>,
    },
    Choose {
        record: Box<Record>,
    },
    Add {
        id: String,
        query: String,
        selected: HashSet<String>,
    },
    Delete {
        id: String,
        name: String,
    },
}
pub(super) struct State {
    store: Store,
    selected: Option<String>,
    dialog: Option<Dialog>,
    dialog_error: String,
    notice: Option<(String, Instant)>,
    undo: Option<(String, usize, Record)>,
    pub drag: queue_drag::QueueDrag,
}
impl State {
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }
    pub fn load() -> Self {
        Self::open(Settings::queue_path().with_file_name("music-playlists.json"))
    }
    fn open(path: PathBuf) -> Self {
        Self {
            store: Store::open(path),
            selected: None,
            dialog: None,
            dialog_error: String::new(),
            notice: None,
            undo: None,
            drag: Default::default(),
        }
    }
    fn show_dialog(&mut self, dialog: Dialog) {
        self.dialog = Some(dialog);
        self.dialog_error.clear();
    }
    fn result<T>(&mut self, result: Result<T>, message: String) -> Option<T> {
        match result {
            Ok(value) => {
                self.undo = None;
                self.notice = Some((message, Instant::now()));
                Some(value)
            }
            Err(error) => {
                self.notice = Some((
                    format!("保存失败 / Could not save: {error}"),
                    Instant::now(),
                ));
                None
            }
        }
    }
}
impl App {
    fn playlist_records(&self, id: &str) -> Vec<Record> {
        self.playlists
            .store
            .data
            .playlists
            .iter()
            .find(|p| p.id == id)
            .map(|p| {
                p.tracks
                    .iter()
                    .map(|saved| {
                        let mut record = saved.clone();
                        if let Some(current) = self
                            .library
                            .records
                            .iter()
                            .find(|r| identity(r) == identity(saved))
                        {
                            // Refresh tags and downloaded locations without losing membership.
                            record = current.clone();
                        } else {
                            record.favorite = false;
                        }
                        record
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(super) fn play_selected_playlist(&mut self, start: usize) {
        if let Some(id) = self.playlists.selected.clone() {
            self.play_playlist(&id, start);
        }
    }
    fn play_playlist(&mut self, id: &str, start: usize) {
        let records = self.playlist_records(id);
        // Skip missing files before queue creation; online tracks resolve at playback.
        let mut playable = Vec::new();
        let mut position = None;
        let mut skipped = 0;
        for (index, record) in records.iter().enumerate() {
            if record.online.is_some() || std::path::Path::new(&record.path).is_file() {
                if index >= start && position.is_none() {
                    position = Some(playable.len());
                }
                playable.push(record.clone());
            } else {
                skipped += 1;
            }
        }
        let l = self.settings.language;
        if playable.is_empty() {
            self.playlists.notice = Some((
                l.text(
                    "歌单没有可播放的歌曲，请检查文件或音乐源",
                    "No playable songs; check files or music sources",
                )
                .into(),
                Instant::now(),
            ));
            return;
        }
        self.play_records_from(&playable, position.unwrap_or(0));
        if skipped > 0 {
            self.playlists.notice = Some((
                format!(
                    "{} {skipped}",
                    l.text("已跳过缺失文件：", "Skipped missing files:")
                ),
                Instant::now(),
            ));
        }
    }
    pub(super) fn playlist_track_menu(
        &mut self,
        ui: &mut Ui,
        record: &Record,
        index: usize,
        in_playlist: bool,
    ) {
        let l = self.settings.language;
        if design::menu_item(ui, false, l.text("播放", "Play")).clicked() {
            if in_playlist {
                self.play_selected_playlist(index);
            } else {
                self.play_record(record.clone(), false);
            }
            ui.close();
        }
        if design::menu_item(ui, false, l.text("添加到歌单", "Add to playlist")).clicked() {
            self.playlists.show_dialog(Dialog::Choose {
                record: Box::new(record.clone()),
            });
            ui.close();
        }
        if in_playlist {
            let Some(id) = self.playlists.selected.clone() else {
                return;
            };
            let count = self
                .playlists
                .store
                .data
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map_or(0, |p| p.tracks.len());
            for (label, enabled, target) in [
                (
                    l.text("上移", "Move up"),
                    index > 0,
                    index.saturating_sub(1),
                ),
                (l.text("下移", "Move down"), index + 1 < count, index + 2),
            ] {
                ui.add_enabled_ui(enabled, |ui| {
                    if design::menu_item(ui, false, label).clicked() {
                        let result = self.playlists.store.relocate(&id, index, target);
                        self.playlists.result(
                            result,
                            l.text("已更新歌单顺序", "Playlist order saved").into(),
                        );
                        ui.close();
                    }
                });
            }
            if design::menu_item(
                ui,
                false,
                l.text("从此歌单移除", "Remove from this playlist"),
            )
            .clicked()
            {
                let result = self.playlists.store.edit(&id, |p| {
                    ensure!(index < p.tracks.len(), "Song not found");
                    Ok(p.tracks.remove(index))
                });
                if let Some(track) = self.playlists.result(
                    result,
                    l.text("已从歌单移除", "Removed from playlist").into(),
                ) {
                    self.playlists.undo = Some((id, index, track));
                }
                ui.close();
            }
        }
    }
}
fn song_count_text(language: Language, count: usize) -> String {
    if count == 1 {
        language.text("1 首歌曲", "1 song").into()
    } else {
        format!("{count} {}", language.text("首歌曲", "songs"))
    }
}
fn action_song_count(
    language: Language,
    zh_verb: &str,
    zh_unit: &str,
    en_verb: &str,
    count: usize,
) -> String {
    if matches!(language, Language::Chinese) {
        format!("{zh_verb} {count} {zh_unit}")
    } else {
        format!(
            "{en_verb} {count} {}",
            if count == 1 { "song" } else { "songs" }
        )
    }
}
impl App {
    fn playlist_cover(&mut self, ui: &Ui, rect: Rect, tracks: &[Record]) {
        let radius = if rect.width() < 48.0 { 6 } else { 10 };
        if tracks.is_empty() {
            ui.painter().rect_filled(rect, radius, SURFACE);
            ui.painter().rect_stroke(
                rect,
                radius,
                Stroke::new(1.0, design::LINE),
                egui::StrokeKind::Inside,
            );
            icons::paint(
                ui.painter(),
                Rect::from_center_size(rect.center(), Vec2::splat(rect.width() * 0.34)),
                MUTED,
                icons::MUSIC_NOTES,
            );
            return;
        }
        let rounding = egui::CornerRadius::same(radius);
        if tracks.len() == 1 {
            self.record_art_rounded(ui, rect, &tracks[0], rounding);
            return;
        }
        ui.painter().rect_filled(rect, radius, SURFACE);
        let seam = if rect.width() < 48.0 { 1.0 } else { 2.0 };
        let left_w = ((rect.width() - seam) * 0.5).floor();
        let right_w = rect.width() - left_w - seam;
        if tracks.len() == 2 {
            let left = Rect::from_min_size(rect.min, vec2(left_w, rect.height()));
            let right = Rect::from_min_size(
                rect.min + vec2(left_w + seam, 0.0),
                vec2(right_w, rect.height()),
            );
            self.record_art_rounded(
                ui,
                left,
                &tracks[0],
                egui::CornerRadius {
                    nw: radius,
                    sw: radius,
                    ne: 0,
                    se: 0,
                },
            );
            self.record_art_rounded(
                ui,
                right,
                &tracks[1],
                egui::CornerRadius {
                    nw: 0,
                    sw: 0,
                    ne: radius,
                    se: radius,
                },
            );
            return;
        }
        let top_h = ((rect.height() - seam) * 0.5).floor();
        let bottom_h = rect.height() - top_h - seam;
        if tracks.len() == 3 {
            let left = Rect::from_min_size(rect.min, vec2(left_w, rect.height()));
            let top_right =
                Rect::from_min_size(rect.min + vec2(left_w + seam, 0.0), vec2(right_w, top_h));
            let bottom_right = Rect::from_min_size(
                rect.min + vec2(left_w + seam, top_h + seam),
                vec2(right_w, bottom_h),
            );
            self.record_art_rounded(
                ui,
                left,
                &tracks[0],
                egui::CornerRadius {
                    nw: radius,
                    sw: radius,
                    ne: 0,
                    se: 0,
                },
            );
            self.record_art_rounded(
                ui,
                top_right,
                &tracks[1],
                egui::CornerRadius {
                    nw: 0,
                    ne: radius,
                    sw: 0,
                    se: 0,
                },
            );
            self.record_art_rounded(
                ui,
                bottom_right,
                &tracks[2],
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 0,
                    se: radius,
                },
            );
            return;
        }
        let tiles = [
            (
                Rect::from_min_size(rect.min, vec2(left_w, top_h)),
                egui::CornerRadius {
                    nw: radius,
                    ne: 0,
                    sw: 0,
                    se: 0,
                },
            ),
            (
                Rect::from_min_size(rect.min + vec2(left_w + seam, 0.0), vec2(right_w, top_h)),
                egui::CornerRadius {
                    nw: 0,
                    ne: radius,
                    sw: 0,
                    se: 0,
                },
            ),
            (
                Rect::from_min_size(rect.min + vec2(0.0, top_h + seam), vec2(left_w, bottom_h)),
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: radius,
                    se: 0,
                },
            ),
            (
                Rect::from_min_size(
                    rect.min + vec2(left_w + seam, top_h + seam),
                    vec2(right_w, bottom_h),
                ),
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 0,
                    se: radius,
                },
            ),
        ];
        for (i, (tile, tile_radius)) in tiles.into_iter().enumerate() {
            self.record_art_rounded(ui, tile, &tracks[i], tile_radius);
        }
    }
    pub(super) fn playlist_notice(&mut self, ui: &mut Ui) {
        let l = self.settings.language;
        if let Some(error) = self.playlists.store.load_error.clone() {
            let msg = format!(
                "{}: {error}",
                l.text(
                    "无法读取歌单，原文件已保留",
                    "Cannot read playlists; original file preserved"
                )
            );
            let (_, dismissed) =
                design::alert_banner(ui, true, &msg, None, Some(l.text("关闭", "Dismiss")));
            ui.add_space(8.0);
            if dismissed {
                self.playlists.store.load_error = None;
            }
        }
        if self
            .playlists
            .notice
            .as_ref()
            .is_some_and(|(_, when)| when.elapsed() > Duration::from_secs(8))
        {
            self.playlists.notice = None;
            self.playlists.undo = None;
        }
        if let Some((message, _)) = self.playlists.notice.clone() {
            let undo_label = self
                .playlists
                .undo
                .is_some()
                .then_some(l.text("撤销", "Undo"));
            let (undo_clicked, dismissed) = design::floating_toast(
                ui.ctx(),
                Id::new("playlist-toast"),
                &message,
                undo_label,
                Some(l.text("关闭", "Dismiss")),
            );
            if undo_clicked && let Some((id, index, record)) = self.playlists.undo.take() {
                let result = self.playlists.store.edit(&id, |p| {
                    if !p.tracks.iter().any(|r| identity(r) == identity(&record)) {
                        p.tracks.insert(index.min(p.tracks.len()), record);
                    }
                    Ok(())
                });
                self.playlists
                    .result(result, l.text("已恢复歌曲", "Song restored").into());
            } else if dismissed {
                self.playlists.notice = None;
                self.playlists.undo = None;
            }
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
    }
    pub(super) fn playlists_page(&mut self, ui: &mut Ui) {
        let l = self.settings.language;
        self.playlist_notice(ui);
        let selected = self
            .playlists
            .selected
            .as_ref()
            .and_then(|id| {
                self.playlists
                    .store
                    .data
                    .playlists
                    .iter()
                    .find(|p| p.id == *id)
            })
            .cloned();
        if let Some(playlist) = selected {
            ui.horizontal(|ui| {
                if design::back_button(ui, l.text("返回我的歌单", "Back to playlists")).clicked()
                {
                    self.playlists.selected = None;
                }
            });
            ui.add_space(6.0);
            let records = self.playlist_records(&playlist.id);
            let count_text = song_count_text(l, records.len());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                let size = (ui.available_width() * 0.22).clamp(80.0, 144.0);
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
                self.playlist_cover(ui, rect, &records);
                ui.vertical(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&playlist.name).size(24.0).strong())
                            .truncate(),
                    )
                    .on_hover_text(&playlist.name);
                    ui.add_space(4.0);
                    ui.label(RichText::new(&count_text).color(MUTED));
                    ui.add_space(12.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.add_enabled_ui(!records.is_empty(), |ui| {
                            if design::control(
                                ui,
                                l.text("播放全部", "Play all"),
                                Some(|p, r, c| icons::paint(p, r, c, icons::PLAY)),
                                design::Control::Primary,
                                0.0,
                            )
                            .clicked()
                            {
                                self.play_playlist(&playlist.id, 0);
                            }
                        });
                        if design::control(
                            ui,
                            l.text("添加歌曲", "Add songs"),
                            Some(draw_add),
                            design::Control::Outline,
                            0.0,
                        )
                        .clicked()
                        {
                            self.playlists.show_dialog(Dialog::Add {
                                id: playlist.id.clone(),
                                query: String::new(),
                                selected: HashSet::new(),
                            });
                        }
                        let more = design::icon_button(
                            ui,
                            icons::DOTS_THREE,
                            l.text("更多", "More"),
                            false,
                        );
                        egui::Popup::menu(&more).show(|ui| self.playlist_menu(ui, &playlist));
                    });
                });
            });
            ui.add_space(16.0);
            if records.is_empty() {
                design::empty_state(
                    ui,
                    icons::MUSIC_NOTES,
                    l.text(
                        "歌单里还没有歌曲，点击「添加歌曲」从收藏添加",
                        "No songs yet. Add songs from your favorites.",
                    ),
                );
                return;
            }
            self.track_table_header(ui);
            self.playlists.drag.begin(ui);
            let mut rects = vec![];
            let scroll = design::scroll()
                .id_salt(("playlist-tracks", &playlist.id))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (index, record) in records.iter().enumerate() {
                        let rect = ui
                            .push_id(identity(record), |ui| {
                                self.track_row(ui, record, index, true)
                            })
                            .inner;
                        rects.push(rect);
                    }
                });
            if let Some(drag) = self.playlists.drag.finish(ui) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                if scroll.inner_rect.contains(drag.position) {
                    let target = rects
                        .iter()
                        .filter(|r| drag.position.y > r.center().y)
                        .count();
                    if let Some(rect) = rects.get(target).or_else(|| rects.last()) {
                        let y = if target < rects.len() {
                            rect.top() - 2.0
                        } else {
                            rect.bottom() + 2.0
                        };
                        ui.painter().with_clip_rect(scroll.inner_rect).line_segment(
                            [pos2(rect.left(), y), pos2(rect.right(), y)],
                            Stroke::new(2.0, ACCENT),
                        );
                    }
                    if drag.released && target != drag.index && target != drag.index + 1 {
                        let result =
                            self.playlists
                                .store
                                .relocate(&playlist.id, drag.index, target);
                        self.playlists.result(
                            result,
                            l.text("已更新歌单顺序", "Playlist order saved").into(),
                        );
                    }
                }
            }
        } else {
            let lists = self.playlists.store.data.playlists.clone();
            let list_count = lists.len().to_string();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(l.text("我的歌单", "My playlists"))
                        .size(20.0)
                        .strong(),
                );
                ui.label(RichText::new(&list_count).color(MUTED));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if design::control(
                        ui,
                        l.text("新建歌单", "New playlist"),
                        Some(draw_add),
                        design::Control::Outline,
                        0.0,
                    )
                    .clicked()
                    {
                        self.playlists.show_dialog(Dialog::Name {
                            id: None,
                            name: String::new(),
                            pending: vec![],
                        });
                    }
                });
            });
            ui.add_space(12.0);
            if lists.is_empty() {
                design::empty_state(
                    ui,
                    icons::MUSIC_NOTES,
                    l.text(
                        "创建歌单，整理喜欢的歌曲",
                        "Create a playlist for your favorite songs",
                    ),
                );
                return;
            }
            design::scroll()
                .id_salt("playlist-cards")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let columns = (ui.available_width() / 180.0).floor().clamp(1.0, 5.0) as usize;
                    let width = (ui.available_width() - 12.0 * columns.saturating_sub(1) as f32)
                        / columns as f32;
                    let art_height = width - 2.0 * design::CARD_PADDING;
                    for row in lists.chunks(columns) {
                        ui.horizontal_top(|ui| {
                            for playlist in row {
                                ui.push_id(&playlist.id, |ui| {
                                    let (rect, response) = ui.allocate_exact_size(
                                        vec2(width, art_height + 62.0 + 2.0 * design::CARD_PADDING),
                                        Sense::click(),
                                    );
                                    design::card_hover(ui, &response, 10);
                                    let content = design::card_content(rect);
                                    let art =
                                        Rect::from_min_size(content.min, Vec2::splat(art_height));
                                    self.playlist_cover(ui, art, &playlist.tracks);
                                    let mut labels = child(
                                        ui,
                                        "playlist-label",
                                        Rect::from_min_max(
                                            pos2(content.left(), art.bottom() + 8.0),
                                            content.max,
                                        ),
                                    );
                                    labels.add(
                                        egui::Label::new(RichText::new(&playlist.name).strong())
                                            .truncate(),
                                    );
                                    labels.add(
                                        egui::Label::new(
                                            RichText::new(song_count_text(
                                                l,
                                                playlist.tracks.len(),
                                            ))
                                            .small()
                                            .color(MUTED),
                                        )
                                        .truncate(),
                                    );
                                    let menu_id =
                                        ui.make_persistent_id(("playlist-card-menu", &playlist.id));
                                    let menu_open = egui::Popup::is_id_open(ui.ctx(), menu_id);
                                    let menu_rect = Rect::from_min_size(
                                        pos2(art.right() - 40.0, art.top() + 8.0),
                                        Vec2::splat(32.0),
                                    );
                                    let hovered = response.hovered()
                                        || ui.rect_contains_pointer(art)
                                        || ui.rect_contains_pointer(menu_rect)
                                        || menu_open;
                                    let mut overlay_clicked = false;
                                    if hovered {
                                        let more = design::cover_overlay_button(
                                            ui,
                                            ("playlist-more", &playlist.id),
                                            menu_rect,
                                            icons::DOTS_THREE,
                                            l.text("更多", "More"),
                                            menu_open,
                                        );
                                        if more.clicked() {
                                            overlay_clicked = true;
                                        }
                                        egui::Popup::menu(&more).id(menu_id).show(|ui| {
                                            self.playlist_menu(ui, playlist);
                                        });
                                        if !playlist.tracks.is_empty() {
                                            let play_rect = Rect::from_min_size(
                                                art.right_bottom() - vec2(44.0, 44.0),
                                                Vec2::splat(36.0),
                                            );
                                            let play = ui.interact(
                                                play_rect,
                                                ui.id().with(("playlist-play", &playlist.id)),
                                                Sense::click(),
                                            );
                                            let fill = if play.hovered() {
                                                ACCENT
                                            } else {
                                                Color32::from_black_alpha(185)
                                            };
                                            let stroke_color = if play.hovered() {
                                                ACCENT
                                            } else {
                                                Color32::from_white_alpha(38)
                                            };
                                            ui.painter().circle_filled(
                                                play_rect.center(),
                                                18.0,
                                                fill,
                                            );
                                            ui.painter().circle_stroke(
                                                play_rect.center(),
                                                18.0,
                                                Stroke::new(1.0, stroke_color),
                                            );
                                            icons::paint(
                                                ui.painter(),
                                                Rect::from_center_size(
                                                    play_rect.center() + vec2(1.0, 0.0),
                                                    Vec2::splat(16.0),
                                                ),
                                                if play.hovered() {
                                                    BACKGROUND
                                                } else {
                                                    design::TEXT
                                                },
                                                icons::PLAY,
                                            );
                                            if play
                                                .on_hover_text(l.text("播放歌单", "Play playlist"))
                                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                                .clicked()
                                            {
                                                self.play_playlist(&playlist.id, 0);
                                                overlay_clicked = true;
                                            }
                                        }
                                    }
                                    response.context_menu(|ui| self.playlist_menu(ui, playlist));
                                    if response.on_hover_text(&playlist.name).clicked()
                                        && !overlay_clicked
                                    {
                                        self.playlists.selected = Some(playlist.id.clone());
                                    }
                                });
                            }
                        });
                        ui.add_space(12.0);
                    }
                });
        }
    }
    fn playlist_menu(&mut self, ui: &mut Ui, playlist: &Playlist) {
        let l = self.settings.language;
        ui.add_enabled_ui(!playlist.tracks.is_empty(), |ui| {
            if design::menu_item(ui, false, l.text("播放", "Play")).clicked() {
                self.play_playlist(&playlist.id, 0);
                ui.close();
            }
        });
        if design::menu_item(ui, false, l.text("重命名", "Rename")).clicked() {
            self.playlists.show_dialog(Dialog::Name {
                id: Some(playlist.id.clone()),
                name: playlist.name.clone(),
                pending: vec![],
            });
            ui.close();
        }
        if design::menu_item(ui, false, l.text("删除歌单", "Delete playlist")).clicked() {
            self.playlists.show_dialog(Dialog::Delete {
                id: playlist.id.clone(),
                name: playlist.name.clone(),
            });
            ui.close();
        }
    }
    pub(super) fn playlist_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.playlists.dialog.take() else {
            return;
        };
        let l = self.settings.language;
        let mut close = false;
        let response = egui::Modal::new(Id::new("playlist-dialog"))
            .frame(design::modal_frame())
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 440.0));
                match &mut dialog {
                    Dialog::Name { id, name, pending } => {
                        design::modal_header(
                            ui,
                            if id.is_some() {
                                l.text("重命名歌单", "Rename playlist")
                            } else {
                                l.text("新建歌单", "New playlist")
                            },
                            None,
                        );
                        let field = design::input_field(
                            ui,
                            name,
                            ui.available_width(),
                            l.text("例如：通勤、夜间", "For example: Commute, Night"),
                        );
                        if !ctx.memory(|m| m.has_focus(field.id))
                            && !ctx.egui_wants_keyboard_input()
                        {
                            field.request_focus();
                        }
                        let trimmed_len = name.trim().chars().count();
                        let valid = !name.trim().is_empty() && trimmed_len <= 40;
                        if trimmed_len > 40 {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(
                                    l.text("名称最多 40 个字符", "Use at most 40 characters"),
                                )
                                .small()
                                .color(Color32::LIGHT_RED),
                            );
                        }
                        if !self.playlists.dialog_error.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(&self.playlists.dialog_error)
                                    .small()
                                    .color(Color32::LIGHT_RED),
                            );
                        }
                        let enter = ui.input(|i| i.key_pressed(Key::Enter));
                        design::dialog_footer(ui, |ui| {
                            let submit = ui
                                .add_enabled_ui(valid, |ui| {
                                    design::control(
                                        ui,
                                        l.text("保存", "Save"),
                                        None,
                                        design::Control::Primary,
                                        0.0,
                                    )
                                })
                                .inner
                                .clicked();
                            if valid && (submit || enter) {
                                // Creating and adding is one durable transaction.
                                let result =
                                    self.playlists.store.named(id.as_deref(), name, pending);
                                match result {
                                    Ok(created) => {
                                        if pending.is_empty() && id.is_none() {
                                            self.playlists.selected = Some(created);
                                        }
                                        close = true;
                                        self.playlists.notice = Some((
                                            l.text("歌单已保存", "Playlist saved").into(),
                                            Instant::now(),
                                        ));
                                    }
                                    Err(error) => {
                                        self.playlists.dialog_error = error.to_string();
                                    }
                                }
                            }
                            if design::control(
                                ui,
                                l.text("取消", "Cancel"),
                                None,
                                design::Control::Quiet,
                                0.0,
                            )
                            .clicked()
                            {
                                close = true;
                            }
                        });
                    }
                    Dialog::Choose { record } => {
                        design::modal_header(
                            ui,
                            l.text("添加到歌单", "Add to playlist"),
                            Some(&record.title),
                        );
                        let playlists = self.playlists.store.data.playlists.clone();
                        if playlists.is_empty() {
                            ui.label(
                                RichText::new(l.text(
                                    "还没有歌单，点击左下角新建",
                                    "No playlists yet. Create one below.",
                                ))
                                .small()
                                .color(MUTED),
                            );
                        } else {
                            design::scroll()
                                .id_salt("playlist-choose-scroll")
                                .max_height(280.0)
                                .show(ui, |ui| {
                                    for playlist in playlists {
                                        let exists = playlist
                                            .tracks
                                            .iter()
                                            .any(|r| identity(r) == identity(record));
                                        let count_text = song_count_text(l, playlist.tracks.len());
                                        let badge = exists.then_some(l.text("已添加", "Added"));
                                        let mut clicked = false;
                                        ui.add_enabled_ui(!exists, |ui| {
                                            clicked = design::check_row(
                                                ui,
                                                exists,
                                                false,
                                                &playlist.name,
                                                Some(&count_text),
                                                badge,
                                                Some(|ui: &mut Ui, thumb: Rect| {
                                                    self.playlist_cover(
                                                        ui,
                                                        thumb,
                                                        &playlist.tracks,
                                                    );
                                                }),
                                            )
                                            .clicked();
                                        });
                                        if clicked {
                                            match self
                                                .playlists
                                                .store
                                                .add(&playlist.id, std::slice::from_ref(record))
                                            {
                                                Ok(_) => {
                                                    close = true;
                                                    self.playlists.notice = Some((
                                                        format!(
                                                            "{}「{}」",
                                                            l.text("已添加到", "Added to "),
                                                            playlist.name
                                                        ),
                                                        Instant::now(),
                                                    ));
                                                }
                                                Err(error) => {
                                                    self.playlists.dialog_error = error.to_string()
                                                }
                                            }
                                        }
                                    }
                                });
                        }
                        if !self.playlists.dialog_error.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(&self.playlists.dialog_error)
                                    .small()
                                    .color(Color32::LIGHT_RED),
                            );
                        }
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if design::control(
                                ui,
                                l.text("新建歌单", "New playlist"),
                                Some(draw_add),
                                design::Control::Outline,
                                0.0,
                            )
                            .clicked()
                            {
                                self.playlists.show_dialog(Dialog::Name {
                                    id: None,
                                    name: String::new(),
                                    pending: vec![record.as_ref().clone()],
                                });
                                close = true;
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if design::control(
                                    ui,
                                    l.text("取消", "Cancel"),
                                    None,
                                    design::Control::Quiet,
                                    0.0,
                                )
                                .clicked()
                                {
                                    close = true;
                                }
                            });
                        });
                    }
                    Dialog::Add {
                        id,
                        query,
                        selected,
                    } => {
                        design::modal_header(
                            ui,
                            l.text("从收藏添加歌曲", "Add favorite songs"),
                            None,
                        );
                        design::search_field(
                            ui,
                            query,
                            ui.available_width(),
                            l.text("搜索标题、艺术家或专辑", "Search title, artist or album"),
                        );
                        ui.add_space(10.0);
                        let mut favorites: Vec<_> = self
                            .records()
                            .into_iter()
                            .filter(|r| r.music && r.favorite)
                            .collect();
                        library::sort_favorites(&mut favorites);
                        let existing: HashSet<_> =
                            self.playlist_records(id).iter().map(identity).collect();
                        if favorites.is_empty() {
                            ui.label(
                                RichText::new(l.text(
                                    "先收藏喜欢的歌曲，再加入歌单",
                                    "Favorite songs first, then add them to a playlist",
                                ))
                                .small()
                                .color(MUTED),
                            );
                        } else {
                            let keyword = query.to_lowercase();
                            design::scroll()
                                .id_salt("playlist-add-scroll")
                                .max_height(300.0)
                                .show(ui, |ui| {
                                    let mut count = 0;
                                    for record in &favorites {
                                        if !format!(
                                            "{} {} {}",
                                            record.title, record.artist, record.album
                                        )
                                        .to_lowercase()
                                        .contains(&keyword)
                                        {
                                            continue;
                                        }
                                        count += 1;
                                        let key = identity(record);
                                        let exists = existing.contains(&key);
                                        let checked = exists || selected.contains(&key);
                                        let subtitle = if record.artist.is_empty()
                                            && record.album.is_empty()
                                        {
                                            l.text("本地音乐", "Local track").to_owned()
                                        } else if record.album.is_empty() {
                                            record.artist.clone()
                                        } else if record.artist.is_empty() {
                                            record.album.clone()
                                        } else {
                                            format!("{} · {}", record.artist, record.album)
                                        };
                                        let badge = exists.then_some(l.text("已添加", "Added"));
                                        let mut clicked = false;
                                        ui.add_enabled_ui(!exists, |ui| {
                                            clicked = design::check_row(
                                                ui,
                                                checked,
                                                true,
                                                &record.title,
                                                Some(&subtitle),
                                                badge,
                                                Some(|ui: &mut Ui, thumb: Rect| {
                                                    self.record_art_rounded(
                                                        ui,
                                                        thumb,
                                                        record,
                                                        egui::CornerRadius::same(6),
                                                    );
                                                }),
                                            )
                                            .clicked();
                                        });
                                        if clicked {
                                            if selected.contains(&key) {
                                                selected.remove(&key);
                                            } else {
                                                selected.insert(key);
                                            }
                                        }
                                    }
                                    if count == 0 {
                                        ui.label(
                                            RichText::new(l.text(
                                                "没有匹配的收藏歌曲",
                                                "No matching favorites",
                                            ))
                                            .small()
                                            .color(MUTED),
                                        );
                                    }
                                });
                        }
                        if !self.playlists.dialog_error.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(&self.playlists.dialog_error)
                                    .small()
                                    .color(Color32::LIGHT_RED),
                            );
                        }
                        let count = selected.len();
                        let submit_label = action_song_count(l, "添加", "首", "Add", count);
                        design::dialog_footer(ui, |ui| {
                            ui.add_enabled_ui(count > 0, |ui| {
                                if design::control(
                                    ui,
                                    &submit_label,
                                    None,
                                    design::Control::Primary,
                                    0.0,
                                )
                                .clicked()
                                {
                                    let records: Vec<_> = favorites
                                        .into_iter()
                                        .filter(|r| selected.contains(&identity(r)))
                                        .collect();
                                    match self.playlists.store.add(id, &records) {
                                        Ok(added) => {
                                            close = true;
                                            self.playlists.notice = Some((
                                                action_song_count(
                                                    l,
                                                    "已添加",
                                                    "首歌曲",
                                                    "Added",
                                                    added,
                                                ),
                                                Instant::now(),
                                            ));
                                        }
                                        Err(error) => {
                                            self.playlists.dialog_error = error.to_string()
                                        }
                                    }
                                }
                            });
                            if design::control(
                                ui,
                                l.text("取消", "Cancel"),
                                None,
                                design::Control::Quiet,
                                0.0,
                            )
                            .clicked()
                            {
                                close = true;
                            }
                        });
                    }
                    Dialog::Delete { id, name } => {
                        design::modal_header(ui, l.text("删除歌单", "Delete playlist"), None);
                        ui.label(format!("{}「{name}」？", l.text("删除", "Delete ")));
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(l.text(
                                "歌曲、收藏和文件会保留。",
                                "Songs, favorites and files will be kept.",
                            ))
                            .small()
                            .color(MUTED),
                        );
                        if !self.playlists.dialog_error.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(&self.playlists.dialog_error)
                                    .small()
                                    .color(Color32::LIGHT_RED),
                            );
                        }
                        design::dialog_footer(ui, |ui| {
                            if design::control(
                                ui,
                                l.text("删除歌单", "Delete playlist"),
                                None,
                                design::Control::Danger,
                                0.0,
                            )
                            .clicked()
                            {
                                let result = self.playlists.store.commit(|data| {
                                    data.playlists.retain(|p| p.id != *id);
                                    Ok(())
                                });
                                match result {
                                    Ok(()) => {
                                        if self.playlists.selected.as_ref() == Some(id) {
                                            self.playlists.selected = None;
                                        }
                                        close = true;
                                    }
                                    Err(error) => self.playlists.dialog_error = error.to_string(),
                                }
                            }
                            if design::control(
                                ui,
                                l.text("取消", "Cancel"),
                                None,
                                design::Control::Quiet,
                                0.0,
                            )
                            .clicked()
                            {
                                close = true;
                            }
                        });
                    }
                }
            });
        if !close && !response.should_close() {
            self.playlists.dialog = Some(dialog);
        }
    }
}

#[cfg(test)]
mod tests;
