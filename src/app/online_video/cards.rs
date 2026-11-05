//! Responsive discovery cards and a small, lazy artwork cache.
use super::*;
use egui::{FontId, StrokeKind};
use std::sync::OnceLock;

const GAP: f32 = 16.0;
const CACHE_LIMIT: usize = 48;
const FETCH_LIMIT: usize = 4;

enum Preview {
    Loading(Job<crate::media::Artwork>),
    Ready(egui::TextureHandle),
    Failed,
}
struct Entry {
    preview: Preview,
    used: u64,
}
#[derive(Default)]
pub(super) struct Previews {
    entries: HashMap<String, Entry>,
    tick: u64,
}
impl Previews {
    pub(super) fn cancel_pending(&mut self) {
        self.entries
            .retain(|_, entry| matches!(entry.preview, Preview::Ready(_)));
    }
    pub(super) fn poll(&mut self, ctx: &egui::Context) {
        for (key, entry) in &mut self.entries {
            if let Preview::Loading(job) = &entry.preview
                && let Some(result) = job.poll()
            {
                entry.preview = match result {
                    Ok(image) => Preview::Ready(ctx.load_texture(
                        format!("bilibili-art-{}", crate::comics::digest(key.as_bytes())),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [image.width, image.height],
                            &image.rgba,
                        ),
                        egui::TextureOptions::LINEAR,
                    )),
                    Err(_) => Preview::Failed,
                };
            }
        }
    }
    fn texture(
        &mut self,
        ui: &Ui,
        rect: Rect,
        value: Option<&str>,
        avatar: bool,
    ) -> Option<egui::TextureHandle> {
        if !ui.is_rect_visible(rect) {
            return None;
        }
        let url = bilibili::image_url(value?)?;
        let key = url.to_string();
        self.tick = self.tick.saturating_add(1);
        if !self.entries.contains_key(&key) {
            if self
                .entries
                .values()
                .filter(|e| matches!(e.preview, Preview::Loading(_)))
                .count()
                >= FETCH_LIMIT
            {
                return None;
            }
            if self.entries.len() >= CACHE_LIMIT {
                let oldest = self
                    .entries
                    .iter()
                    .filter(|(_, e)| !matches!(e.preview, Preview::Loading(_)))
                    .min_by_key(|(_, e)| e.used)
                    .map(|(key, _)| key.clone());
                self.entries.remove(&oldest?);
            }
            let ctx = ui.ctx().clone();
            self.entries.insert(
                key.clone(),
                Entry {
                    used: self.tick,
                    preview: Preview::Loading(Job::spawn(move |_| async move {
                        let result = fetch(url, avatar).await;
                        ctx.request_repaint();
                        result
                    })),
                },
            );
        }
        let entry = self.entries.get_mut(&key)?;
        entry.used = self.tick;
        match &entry.preview {
            Preview::Ready(texture) => Some(texture.clone()),
            _ => None,
        }
    }
    #[cfg(test)]
    pub(super) fn insert_fixture(&mut self, url: &str, texture: egui::TextureHandle) {
        self.entries.insert(
            url.into(),
            Entry {
                used: 0,
                preview: Preview::Ready(texture),
            },
        );
    }
}
async fn fetch(url: reqwest::Url, avatar: bool) -> anyhow::Result<crate::media::Artwork> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT
        .get_or_init(|| Bilibili::media_client().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|_| anyhow::anyhow!("Artwork client unavailable"))?;
    let mut response = client
        .get(url)
        .header("Referer", bilibili::REFERER)
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    anyhow::ensure!(response.status().is_success(), "Artwork unavailable");
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "Artwork too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || {
        let (width, height) = if avatar { (128, 128) } else { (512, 288) };
        let image = crate::pixels::thumbnail(&bytes, width, height)?;
        Ok(crate::media::Artwork {
            width: image.width() as usize,
            height: image.height() as usize,
            rgba: image.bytes().to_vec(),
        })
    })
    .await?
}

pub(super) enum Action {
    Video(String),
    Creator(Uploader),
    MoreCreators,
}
pub(super) fn results(ui: &mut Ui, state: &mut State, language: Language) -> Option<Action> {
    let mut action = None;
    let creator_columns = columns(ui.available_width(), 300.0, 3);
    if state.creator.is_none() && !state.uploaders.is_empty() {
        let preview = state.searched_kind == SearchKind::All;
        let count = if preview {
            creator_columns.min(state.uploaders.len())
        } else {
            state.uploaders.len()
        };
        if heading(
            ui,
            language.text("UP 主", "Creators"),
            state.uploaders.len(),
            preview.then_some(language.text("查看全部UP主", "All creators")),
        ) {
            action = Some(Action::MoreCreators);
        }
        let width = card_width(ui.available_width(), creator_columns);
        for row in state.uploaders[..count].chunks(creator_columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for creator in row {
                    if creator_card(ui, &mut state.previews, creator, width, language).clicked() {
                        action = Some(Action::Creator(creator.clone()));
                    }
                }
            });
            ui.add_space(GAP - 8.0);
        }
        ui.add_space(12.0);
    }
    let videos = if state.creator.is_some() {
        &state.uploads
    } else {
        &state.videos
    };
    if !videos.is_empty() {
        heading(ui, language.text("视频", "Videos"), videos.len(), None);
        let columns = columns(ui.available_width(), 264.0, 4);
        let width = card_width(ui.available_width(), columns);
        for row in videos.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for video in row {
                    if video_card(ui, &mut state.previews, video, width, language).clicked() {
                        action = Some(Action::Video(video.bvid.clone()));
                    }
                }
            });
            ui.add_space(GAP - 8.0);
        }
    }
    action
}
fn columns(width: f32, minimum: f32, maximum: usize) -> usize {
    ((width + GAP) / (minimum + GAP))
        .floor()
        .clamp(1.0, maximum as f32) as usize
}
fn card_width(width: f32, columns: usize) -> f32 {
    (width - GAP * columns.saturating_sub(1) as f32) / columns as f32
}
fn heading(ui: &mut Ui, title: &str, count: usize, link: Option<&str>) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let title = ui
        .painter()
        .layout_no_wrap(title.into(), FontId::proportional(17.0), design::TEXT);
    let title_width = title.size().x;
    ui.painter().galley(
        pos2(rect.min.x, rect.center().y - title.size().y / 2.0),
        title,
        design::TEXT,
    );
    ui.painter().text(
        pos2(rect.min.x + title_width + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        count,
        FontId::proportional(12.0),
        MUTED,
    );
    let mut clicked = false;
    if let Some(label) = link {
        let galley = ui
            .painter()
            .layout_no_wrap(label.into(), FontId::proportional(12.0), ACCENT);
        let link_rect = Rect::from_min_size(
            pos2(rect.max.x - galley.size().x - 4.0, rect.min.y),
            vec2(galley.size().x + 4.0, rect.height()),
        );
        let response = ui
            .interact(
                link_rect,
                ui.id().with(("result-section", label)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
        });
        ui.painter().galley(
            pos2(link_rect.min.x, rect.center().y - galley.size().y / 2.0),
            galley,
            ACCENT,
        );
        clicked = response.clicked();
    }
    ui.add_space(8.0);
    clicked
}
fn surface(ui: &Ui, rect: Rect, response: &Response) {
    let hover = response.hovered() || response.has_focus();
    ui.painter().rect_filled(
        rect,
        12,
        if hover {
            Color32::from_rgb(32, 39, 40)
        } else {
            SURFACE
        },
    );
    ui.painter().rect_stroke(
        rect,
        12,
        Stroke::new(
            1.0,
            if hover {
                Color32::from_rgb(83, 113, 104)
            } else {
                design::LINE
            },
        ),
        StrokeKind::Inside,
    );
}
fn lines(
    ui: &Ui,
    text: &str,
    position: egui::Pos2,
    width: f32,
    size: f32,
    color: Color32,
    rows: usize,
) {
    let mut job = egui::text::LayoutJob::simple(
        text.into(),
        FontId::proportional(size),
        color,
        width.max(1.0),
    );
    job.wrap.max_rows = rows;
    job.wrap.overflow_character = Some('…');
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(position, galley, color);
}
fn video_card(
    ui: &mut Ui,
    previews: &mut Previews,
    video: &bilibili::Video,
    width: f32,
    language: Language,
) -> Response {
    let cover_height = (width - 16.0) * 9.0 / 16.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, cover_height + 108.0), Sense::click());
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(&video.title);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &video.title)
    });
    surface(ui, rect, &response);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let cover = Rect::from_min_size(rect.min + vec2(8.0, 8.0), vec2(width - 16.0, cover_height));
    if let Some(texture) = previews.texture(ui, cover, video.cover.as_deref(), false) {
        design::cover(ui, &texture, cover, 8);
    } else {
        ui.painter()
            .rect_filled(cover, 8, Color32::from_rgb(34, 48, 52));
        icons::paint(
            ui.painter(),
            Rect::from_center_size(cover.center(), vec2(34.0, 34.0)),
            Color32::from_rgb(83, 113, 113),
            icons::FILM_SLATE,
        );
    }
    if !video.duration.is_empty() {
        let text = ui.painter().layout_no_wrap(
            video.duration.clone(),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
        let badge = Rect::from_min_size(
            cover.right_bottom() - text.size() - vec2(18.0, 12.0),
            text.size() + vec2(12.0, 6.0),
        );
        ui.painter()
            .rect_filled(badge, 4, Color32::from_black_alpha(195));
        ui.painter()
            .galley(badge.min + vec2(6.0, 3.0), text, Color32::WHITE);
    }
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(cover, 8, Color32::from_black_alpha(38));
        let button = Rect::from_center_size(cover.center(), vec2(40.0, 40.0));
        ui.painter()
            .circle_filled(button.center(), 20.0, Color32::from_black_alpha(170));
        icons::paint(
            ui.painter(),
            button.shrink(9.0).translate(vec2(1.0, 0.0)),
            Color32::WHITE,
            icons::PLAY,
        );
    }
    let x = rect.min.x + 12.0;
    let y = cover.max.y + 10.0;
    lines(
        ui,
        &video.title,
        pos2(x, y),
        width - 24.0,
        15.0,
        design::TEXT,
        2,
    );
    lines(
        ui,
        &video.uploader,
        pos2(x, y + 43.0),
        width - 24.0,
        12.0,
        MUTED,
        1,
    );
    let mut details = Vec::new();
    if let Some(views) = video.views {
        details.push(format!(
            "{} {}",
            compact(views, language),
            language.text("次播放", "views")
        ));
    }
    if let Some(date) = video.published
        && let Some(age) = age(date, language)
    {
        details.push(age);
    }
    lines(
        ui,
        &details.join(" · "),
        pos2(x, y + 63.0),
        width - 24.0,
        11.0,
        MUTED,
        1,
    );
    response
}
fn creator_card(
    ui: &mut Ui,
    previews: &mut Previews,
    creator: &Uploader,
    width: f32,
    language: Language,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 108.0), Sense::click());
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("{} · UID {}", creator.uname, creator.mid));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &creator.uname)
    });
    surface(ui, rect, &response);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let avatar = Rect::from_min_size(rect.min + vec2(16.0, 18.0), vec2(52.0, 52.0));
    if let Some(texture) = previews.texture(ui, avatar, creator.avatar.as_deref(), true) {
        design::cover(ui, &texture, avatar, 26);
    } else {
        ui.painter()
            .circle_filled(avatar.center(), 26.0, Color32::from_rgb(45, 68, 63));
        ui.painter().text(
            avatar.center(),
            egui::Align2::CENTER_CENTER,
            creator.uname.chars().next().unwrap_or('U'),
            FontId::proportional(21.0),
            ACCENT,
        );
    }
    let x = avatar.max.x + 12.0;
    let text_width = (rect.max.x - x - 22.0).max(20.0);
    lines(
        ui,
        &creator.uname,
        pos2(x, rect.min.y + 18.0),
        text_width,
        15.0,
        design::TEXT,
        1,
    );
    lines(
        ui,
        if creator.usign.is_empty() {
            language.text("这个UP主还没有填写简介", "No bio yet")
        } else {
            &creator.usign
        },
        pos2(x, rect.min.y + 43.0),
        text_width,
        12.0,
        MUTED,
        2,
    );
    let stats = format!(
        "{} {}   ·   {} {}",
        compact(creator.fans, language),
        language.text("粉丝", "followers"),
        compact(creator.videos, language),
        language.text("投稿", "videos")
    );
    lines(
        ui,
        &stats,
        rect.min + vec2(16.0, 82.0),
        width - 44.0,
        12.0,
        Color32::from_rgb(183, 200, 193),
        1,
    );
    let arrow =
        Rect::from_center_size(pos2(rect.max.x - 16.0, rect.min.y + 29.0), vec2(12.0, 12.0));
    icons::paint(
        ui.painter(),
        arrow,
        if response.hovered() { ACCENT } else { MUTED },
        icons::CARET_RIGHT,
    );
    response
}
fn compact(value: u64, language: Language) -> String {
    let (unit, suffix) = match language {
        Language::Chinese if value >= 100_000_000 => (100_000_000.0, "亿"),
        Language::Chinese if value >= 10_000 => (10_000.0, "万"),
        Language::English if value >= 1_000_000 => (1_000_000.0, "M"),
        Language::English if value >= 1_000 => (1_000.0, "K"),
        _ => return value.to_string(),
    };
    format!(
        "{}{suffix}",
        format!("{:.1}", value as f64 / unit).trim_end_matches(".0")
    )
}
fn age(timestamp: u64, language: Language) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let days = now.saturating_sub(timestamp) / 86400;
    Some(if days == 0 {
        language.text("今天", "Today").into()
    } else if days < 365 {
        format!("{days}{}", language.text("天前", "d ago"))
    } else {
        format!("{}{}", days / 365, language.text("年前", "y ago"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cards_fit_responsive_columns_and_compact_counts() {
        for width in [280.0, 628.0, 1030.0, 1500.0] {
            let count = columns(width, 232.0, 4);
            assert!(
                card_width(width, count) * count as f32 + GAP * count.saturating_sub(1) as f32
                    <= width + 0.01
            );
        }
        assert_eq!(compact(12500, Language::Chinese), "1.2万");
        assert_eq!(compact(12500, Language::English), "12.5K");
    }
}
