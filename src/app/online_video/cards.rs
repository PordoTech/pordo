//! Responsive discovery cards and a small, lazy artwork cache.
use super::*;
use egui::FontId;
use std::sync::OnceLock;

const GAP: f32 = 12.0;
const MIN_CARD_WIDTH: f32 = 165.0;
const MAX_COLUMNS: usize = 8;
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
pub(in crate::app) struct Previews {
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
    pub(in crate::app) fn texture(
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
    FavoriteCreator(Uploader),
    MoreCreators,
}
pub(super) fn results(
    ui: &mut Ui,
    state: &mut State,
    language: Language,
    favorites: &std::collections::HashSet<u64>,
) -> Option<Action> {
    let mut action = None;
    let creator_columns = result_columns(ui.available_width());
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
        if let Some(selected) = creators(
            ui,
            &mut state.previews,
            &state.uploaders[..count],
            language,
            favorites,
        ) {
            action = Some(selected);
        }
        ui.add_space(12.0);
    }
    let videos = if state.creator.is_some() {
        &state.uploads
    } else {
        &state.videos
    };
    if !videos.is_empty() {
        let now = chrono::Local::now();
        let show_uploader = state.creator.is_none();
        heading(ui, language.text("视频", "Videos"), videos.len(), None);
        let columns = result_columns(ui.available_width());
        let width = card_width(ui.available_width(), columns);
        for row in videos.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for video in row {
                    if video_card(
                        ui,
                        &mut state.previews,
                        video,
                        width,
                        language,
                        show_uploader,
                        &now,
                    )
                    .clicked()
                    {
                        action = Some(Action::Video(video.bvid.clone()));
                    }
                }
            });
            ui.add_space(GAP - 8.0);
        }
    }
    action
}
pub(super) fn creators(
    ui: &mut Ui,
    previews: &mut Previews,
    creators: &[Uploader],
    language: Language,
    favorites: &std::collections::HashSet<u64>,
) -> Option<Action> {
    let columns = result_columns(ui.available_width());
    let width = card_width(ui.available_width(), columns);
    let mut action = None;
    for row in creators.chunks(columns) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for creator in row {
                if let Some(selected) = creator_card(
                    ui,
                    previews,
                    creator,
                    width,
                    language,
                    favorites.contains(&creator.mid),
                ) {
                    action = Some(selected);
                }
            }
        });
        ui.add_space(GAP - 8.0);
    }
    action
}
fn columns(width: f32, minimum: f32, maximum: usize) -> usize {
    ((width + GAP) / (minimum + GAP))
        .floor()
        .clamp(1.0, maximum as f32) as usize
}
fn result_columns(width: f32) -> usize {
    columns(width, MIN_CARD_WIDTH, MAX_COLUMNS)
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
/// Shared artwork, duration badge and hover affordance for videos and parts.
fn video_cover(
    ui: &Ui,
    previews: &mut Previews,
    cover: Rect,
    cover_url: Option<&str>,
    duration: &str,
    views: Option<&str>,
    response: &Response,
) {
    if let Some(texture) = previews.texture(ui, cover, cover_url, false) {
        design::cover(ui, &texture, cover, 8);
    } else {
        ui.painter()
            .rect_filled(cover, 8, Color32::from_rgb(34, 48, 52));
        icons::paint(
            ui.painter(),
            Rect::from_center_size(cover.center(), vec2(24.0, 24.0)),
            Color32::from_rgb(83, 113, 113),
            icons::FILM_SLATE,
        );
    }
    if !duration.is_empty() || views.is_some() {
        let text = ui.painter().layout_no_wrap(
            duration.into(),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
        let band = Rect::from_min_max(
            pos2(cover.left(), cover.bottom() - 24.0),
            cover.right_bottom(),
        );
        ui.painter().rect_filled(
            band,
            CornerRadius {
                nw: 0,
                ne: 0,
                sw: 8,
                se: 8,
            },
            Color32::from_black_alpha(165),
        );
        let duration_position = pos2(
            cover.right() - text.size().x - 6.0,
            band.center().y - text.size().y * 0.5,
        );
        let count_right = if duration.is_empty() {
            cover.right() - 6.0
        } else {
            duration_position.x - 10.0
        };
        ui.painter().galley(duration_position, text, Color32::WHITE);
        if let Some(views) = views {
            let icon = Rect::from_center_size(
                pos2(cover.left() + 12.0, band.center().y),
                vec2(12.0, 12.0),
            );
            icons::paint(ui.painter(), icon, Color32::WHITE, icons::PLAY);
            lines(
                ui,
                views,
                pos2(icon.right() + 4.0, band.center().y - 6.5),
                (count_right - icon.right() - 4.0).max(1.0),
                11.0,
                Color32::WHITE,
                1,
            );
        }
    }
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(cover, 8, Color32::from_black_alpha(38));
        let button = Rect::from_center_size(cover.center(), vec2(32.0, 32.0));
        ui.painter()
            .circle_filled(button.center(), 16.0, Color32::from_black_alpha(170));
        icons::paint(
            ui.painter(),
            button.shrink(7.0).translate(vec2(1.0, 0.0)),
            Color32::WHITE,
            icons::PLAY,
        );
    }
}

pub(super) fn part_card(
    ui: &mut Ui,
    previews: &mut Previews,
    part: &bilibili::Part,
    cover_url: Option<&str>,
    current: bool,
) -> Response {
    let width = ui.available_width();
    let cover_height = (width - 16.0) * 9.0 / 16.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, cover_height + 60.0), Sense::click());
    let label = format!("P{} \u{00b7} {}", part.page, part.part);
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(&label);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &label)
    });
    if current {
        ui.painter().rect_filled(rect, 12, design::SELECTED);
    } else {
        surface(ui, rect, &response);
    }
    if ui.is_rect_visible(rect) {
        let cover =
            Rect::from_min_size(rect.min + vec2(8.0, 8.0), vec2(width - 16.0, cover_height));
        video_cover(
            ui,
            previews,
            cover,
            cover_url,
            &fmt_time(part.duration as f64),
            None,
            &response,
        );
        lines(
            ui,
            &label,
            pos2(cover.left(), cover.bottom() + 8.0),
            cover.width(),
            13.0,
            if current { ACCENT } else { design::TEXT },
            2,
        );
    }
    response
}

fn video_card<Tz: chrono::TimeZone>(
    ui: &mut Ui,
    previews: &mut Previews,
    video: &bilibili::Video,
    width: f32,
    language: Language,
    show_uploader: bool,
    now: &chrono::DateTime<Tz>,
) -> Response {
    let cover_height = (width - 16.0) * 9.0 / 16.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, cover_height + 76.0), Sense::click());
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
    let views = video.views.map(|count| compact(count, language));
    video_cover(
        ui,
        previews,
        cover,
        video.cover.as_deref(),
        &video.duration,
        views.as_deref(),
        &response,
    );
    let x = rect.min.x + 8.0;
    let y = cover.max.y + 8.0;
    lines(
        ui,
        &video.title,
        pos2(x, y),
        width - 16.0,
        13.0,
        design::TEXT,
        2,
    );
    let date = video
        .published
        .and_then(|timestamp| age(timestamp, now, language));
    let footer_y = y + 36.0;
    if show_uploader && !video.uploader.is_empty() {
        let badge = Rect::from_min_size(pos2(x, footer_y + 1.0), vec2(15.0, 11.0));
        ui.painter()
            .rect_stroke(badge, 3, Stroke::new(0.8, MUTED), egui::StrokeKind::Inside);
        ui.painter().text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            "UP",
            FontId::proportional(7.0),
            MUTED,
        );
        let date_text = date.map(|date| {
            ui.painter()
                .layout_no_wrap(format!(" · {date}"), FontId::proportional(11.0), MUTED)
        });
        let author_x = badge.right() + 4.0;
        let author_width =
            (cover.right() - author_x - date_text.as_ref().map_or(0.0, |text| text.size().x))
                .max(1.0);
        let mut job = egui::text::LayoutJob::simple(
            video.uploader.clone(),
            FontId::proportional(11.0),
            MUTED,
            author_width,
        );
        job.wrap.max_rows = 1;
        job.wrap.overflow_character = Some('…');
        let author = ui.painter().layout_job(job);
        let date_x = author_x + author.size().x;
        ui.painter().galley(pos2(author_x, footer_y), author, MUTED);
        if let Some(date) = date_text {
            ui.painter().galley(pos2(date_x, footer_y), date, MUTED);
        }
    } else if let Some(date) = date {
        lines(ui, &date, pos2(x, footer_y), cover.width(), 11.0, MUTED, 1);
    }
    response
}
fn creator_card(
    ui: &mut Ui,
    previews: &mut Previews,
    creator: &Uploader,
    width: f32,
    language: Language,
    favorite: bool,
) -> Option<Action> {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 96.0), Sense::click());
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("{} · UID {}", creator.uname, creator.mid));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &creator.uname)
    });
    surface(ui, rect, &response);
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let avatar = Rect::from_min_size(rect.min + vec2(10.0, 12.0), vec2(40.0, 40.0));
    if let Some(texture) = previews.texture(ui, avatar, creator.avatar.as_deref(), true) {
        design::cover(ui, &texture, avatar, 20);
    } else {
        ui.painter()
            .circle_filled(avatar.center(), 20.0, Color32::from_rgb(45, 68, 63));
        ui.painter().text(
            avatar.center(),
            egui::Align2::CENTER_CENTER,
            creator.uname.chars().next().unwrap_or('U'),
            FontId::proportional(16.0),
            ACCENT,
        );
    }
    let x = avatar.max.x + 8.0;
    let text_width = (rect.max.x - x - 34.0).max(20.0);
    lines(
        ui,
        &creator.uname,
        pos2(x, rect.min.y + 12.0),
        text_width,
        13.0,
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
        pos2(x, rect.min.y + 34.0),
        text_width,
        11.0,
        MUTED,
        1,
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
        rect.min + vec2(10.0, 71.0),
        width - 20.0,
        10.0,
        Color32::from_rgb(183, 200, 193),
        1,
    );
    let heart = Rect::from_min_size(pos2(rect.max.x - 30.0, rect.min.y + 8.0), vec2(24.0, 24.0));
    if design::favorite_at(ui, response.id.with("favorite"), heart, favorite)
        .on_hover_text(if favorite {
            language.text("取消收藏UP主", "Remove creator from favorites")
        } else {
            language.text("收藏UP主", "Favorite creator")
        })
        .clicked()
    {
        return Some(Action::FavoriteCreator(creator.clone()));
    }
    response.clicked().then(|| Action::Creator(creator.clone()))
}
fn compact(value: u64, language: Language) -> String {
    let (unit, suffix) = match language {
        Language::Chinese if value >= 1_000_000_000_000 => (1_000_000_000_000u128, "万亿"),
        Language::Chinese if value >= 100_000_000 => (100_000_000u128, "亿"),
        Language::Chinese if value >= 10_000 => (10_000u128, "万"),
        Language::English if value >= 1_000_000_000 => (1_000_000_000u128, "B"),
        Language::English if value >= 1_000_000 => (1_000_000u128, "M"),
        Language::English if value >= 1_000 => (1_000u128, "K"),
        _ => return value.to_string(),
    };
    let tenths = u128::from(value) * 10 / unit;
    if tenths % 10 == 0 {
        format!("{}{suffix}", tenths / 10)
    } else {
        format!("{}.{}{suffix}", tenths / 10, tenths % 10)
    }
}
fn age<Tz: chrono::TimeZone>(
    timestamp: u64,
    now: &chrono::DateTime<Tz>,
    language: Language,
) -> Option<String> {
    use chrono::Datelike;
    if timestamp == 0 {
        return None;
    }
    let date = chrono::DateTime::from_timestamp(i64::try_from(timestamp).ok()?, 0)?
        .with_timezone(&now.timezone());
    let elapsed = now.timestamp().checked_sub(date.timestamp())?;
    if elapsed < 0 {
        return None;
    }
    Some(if elapsed <= 60 {
        language.text("刚刚", "Just now").into()
    } else if elapsed < 3600 {
        format!("{}{}", elapsed / 60, language.text("分钟前", "m ago"))
    } else if elapsed < 86400 {
        format!("{}{}", elapsed / 3600, language.text("小时前", "h ago"))
    } else if now.date_naive().pred_opt() == Some(date.date_naive()) {
        language.text("昨天", "Yesterday").into()
    } else if date.year() == now.year() {
        date.date_naive().format("%m-%d").to_string()
    } else {
        date.date_naive().format("%Y-%m-%d").to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_dates_follow_bilibili_relative_and_calendar_buckets() {
        use chrono::{Duration, FixedOffset, TimeZone};
        let zone = FixedOffset::east_opt(8 * 3600).unwrap();
        let now = zone.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        for (ago, label) in [
            (30, "刚刚"),
            (60, "刚刚"),
            (61, "1分钟前"),
            (3599, "59分钟前"),
            (3600, "1小时前"),
            (86399, "23小时前"),
            (86400, "昨天"),
            (2 * 86400, "11-08"),
        ] {
            assert_eq!(
                age(
                    (now - Duration::seconds(ago)).timestamp() as u64,
                    &now,
                    Language::Chinese
                )
                .as_deref(),
                Some(label)
            );
        }
        let old = zone.with_ymd_and_hms(2025, 12, 31, 12, 0, 0).unwrap();
        assert_eq!(
            age(old.timestamp() as u64, &now, Language::Chinese).as_deref(),
            Some("2025-12-31")
        );
        assert!(age(0, &now, Language::Chinese).is_none());
        assert!(
            age(
                (now + Duration::seconds(1)).timestamp() as u64,
                &now,
                Language::Chinese
            )
            .is_none()
        );
        assert!(age(u64::MAX, &now, Language::Chinese).is_none());
        for (count, label) in [
            (9999, "9999"),
            (10000, "1万"),
            (12999, "1.2万"),
            (99_999_999, "9999.9万"),
            (100_000_000, "1亿"),
            (123_456_789, "1.2亿"),
        ] {
            assert_eq!(compact(count, Language::Chinese), label);
        }
    }

    #[test]
    fn video_cards_place_views_on_cover_and_hide_repeated_creator_names() {
        use chrono::TimeZone;
        let ctx = egui::Context::default();
        design::apply(&ctx);
        setup_fonts(&ctx);
        let now = chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 10, 12, 0, 0)
            .unwrap();
        let video = bilibili::Video {
            bvid: "BV1GJ411x7h7".into(),
            title: "Video title".into(),
            uploader: "Creator".into(),
            duration: "02:27".into(),
            cover: None,
            views: Some(26_123),
            published: Some((now - chrono::Duration::hours(7)).timestamp() as u64),
        };
        let mut previews = Previews::default();
        for show in [true, false] {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                video_card(
                    ui,
                    &mut previews,
                    &video,
                    240.0,
                    Language::Chinese,
                    show,
                    &now,
                );
            });
            output.textures_delta.clear();
            let text = |label: &str| {
                output.shapes.iter().find_map(|s| match &s.shape {
                    Shape::Text(t) if t.galley.text() == label => Some(t.pos),
                    _ => None,
                })
            };
            let count = text("2.6万").expect("compact play count");
            let title = text("Video title").unwrap();
            assert!(count.y < title.y);
            assert_eq!(text("Creator").is_some(), show);
            assert_eq!(text("UP").is_some(), show);
            let date = text(if show { " · 7小时前" } else { "7小时前" }).expect("publication time");
            assert!(date.y > title.y);
            if show {
                assert!(date.x > text("Creator").unwrap().x);
            }
        }
    }
    #[test]
    fn creator_favorite_click_does_not_open_uploads_and_card_click_still_does() {
        let ctx = egui::Context::default();
        let creator: Uploader =
            serde_json::from_value(serde_json::json!({"mid":123,"uname":"KAYGEZ"})).unwrap();
        let mut previews = Previews::default();
        let mut render = |events| {
            let mut action = None;
            let mut origin = egui::Pos2::ZERO;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 240.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    origin = ui.available_rect_before_wrap().min;
                    action =
                        creator_card(ui, &mut previews, &creator, 180.0, Language::English, false);
                },
            );
            output.textures_delta.clear();
            (action, origin)
        };
        let (_, origin) = render(vec![]);
        for (offset, favorite) in [(vec2(162.0, 20.0), true), (vec2(70.0, 25.0), false)] {
            let point = origin + offset;
            render(vec![egui::Event::PointerMoved(point)]);
            render(vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }]);
            let (action, _) = render(vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            if favorite {
                assert!(matches!(action, Some(Action::FavoriteCreator(c)) if c.mid == 123));
            } else {
                assert!(matches!(action, Some(Action::Creator(c)) if c.mid == 123));
            }
        }
    }
    #[test]
    fn cards_fit_responsive_columns_and_compact_counts() {
        for width in [280.0, 628.0, 1030.0, 1500.0] {
            let count = result_columns(width);
            assert!(card_width(width, count) >= MIN_CARD_WIDTH);
            assert!(
                card_width(width, count) * count as f32 + GAP * count.saturating_sub(1) as f32
                    <= width + 0.01
            );
        }
        assert_eq!(compact(12500, Language::Chinese), "1.2万");
        assert_eq!(compact(12500, Language::English), "12.5K");
    }
}
