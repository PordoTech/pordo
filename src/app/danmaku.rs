//! Media-clock driven comments, bounded segment cache and native egui overlay.
use super::*;
use crate::bilibili::{
    Bilibili,
    danmaku::{Comment, SEGMENT_SECONDS},
};
use crate::jobs::Job;
use pordo_core::video_sources::DanmakuSettings;
use std::collections::BTreeMap;

const MAX_ACTIVE: usize = 120;

#[derive(Default)]
pub(super) struct State {
    cid: Option<u64>,
    segments: BTreeMap<u32, Vec<Comment>>,
    jobs: BTreeMap<u32, Job<Vec<Comment>>>,
    errors: BTreeMap<u32, String>,
    active: Vec<Bullet>,
    last: Option<f64>,
    geometry: Option<(Rect, DanmakuSettings)>,
}
struct Bullet {
    start: f64,
    end: f64,
    row: usize,
    width: f32,
    screen_width: f32,
    mode: u8,
    color: Color32,
    text: Arc<egui::Galley>,
}
impl Bullet {
    fn x(&self, t: f64) -> f32 {
        let progress = ((t - self.start) / (self.end - self.start)) as f32;
        match self.mode {
            4 | 5 => (self.screen_width - self.width) * 0.5,
            6 => -self.width + (self.screen_width + self.width) * progress,
            _ => self.screen_width - (self.screen_width + self.width) * progress,
        }
    }
    fn conflicts(&self, other: &Self) -> bool {
        let from = self.start.max(other.start);
        let to = self.end.min(other.end);
        if to <= from || self.row != other.row {
            return false;
        }
        let left = |t| self.x(t) + self.width + 14.0 <= other.x(t);
        let right = |t| other.x(t) + other.width + 14.0 <= self.x(t);
        !(left(from) && left(to) || right(from) && right(to))
    }
}
impl State {
    #[cfg(test)]
    pub(in crate::app) fn seed_fixture(&mut self) {
        let texts = [
            "这个镜头太美了",
            "风景好舒服",
            "终于等到更新！",
            "路上的风景值得慢慢看",
            "好想去这里旅行",
            "这就是旅行的意义",
            "记录平凡的一天",
            "前方高能",
            "带上耳机，出发吧",
        ];
        self.segments.insert(
            1,
            (0..80)
                .map(|i| Comment {
                    id: i,
                    time: 22.0 + i as f64 * 0.27,
                    mode: if i % 13 == 0 { 5 } else { 1 },
                    size: 25,
                    color: if i % 7 == 0 { 0xa9e0cd } else { 0xffffff },
                    text: texts[i as usize % texts.len()].into(),
                })
                .collect(),
        );
    }
    fn poll(
        &mut self,
        ctx: &egui::Context,
        source: Option<(Arc<Bilibili>, u64)>,
        position: f64,
        duration: f64,
        visible: bool,
    ) {
        let cid = source.as_ref().map(|(_, cid)| *cid);
        if self.cid != cid {
            *self = Self {
                cid,
                ..Default::default()
            };
        }
        if !visible || source.is_none() {
            self.jobs.clear();
            self.active.clear();
            self.last = None;
            return;
        }
        let (api, cid) = source.unwrap();
        let index = (position.max(0.0) / SEGMENT_SECONDS) as u32 + 1;
        self.segments.retain(|i, _| i.abs_diff(index) <= 1);
        self.jobs.retain(|i, _| i.abs_diff(index) <= 1);
        self.errors.retain(|i, _| i.abs_diff(index) <= 1);
        let done: Vec<_> = self
            .jobs
            .iter()
            .filter_map(|(i, j)| j.poll().map(|v| (*i, v)))
            .collect();
        for (i, result) in done {
            self.jobs.remove(&i);
            match result {
                Ok(comments) => {
                    self.segments.insert(i, comments);
                    self.last = None;
                }
                Err(error) => {
                    self.errors.insert(i, error.to_string());
                }
            }
        }
        let previous = (index > 1 && position % SEGMENT_SECONDS < 12.0).then(|| index - 1);
        let next = (duration > index as f64 * SEGMENT_SECONDS).then_some(index.saturating_add(1));
        for i in [Some(index), previous, next].into_iter().flatten() {
            if self.jobs.len() >= 2 {
                break;
            }
            if self.segments.contains_key(&i)
                || self.jobs.contains_key(&i)
                || self.errors.contains_key(&i)
            {
                continue;
            }
            let api = api.clone();
            let ctx = ctx.clone();
            self.jobs.insert(
                i,
                Job::spawn(move |_| async move {
                    let result = api.danmaku_segment(cid, i).await;
                    ctx.request_repaint();
                    result
                }),
            );
        }
        if !self.jobs.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn paint(&mut self, painter: &Painter, rect: Rect, position: f64, settings: &DanmakuSettings) {
        if !settings.enabled || rect.height() < 20.0 || rect.width() < 40.0 {
            return;
        }
        let geometry = (rect, settings.clone());
        let rebuild = self.geometry.as_ref() != Some(&geometry)
            || self
                .last
                .is_none_or(|last| position < last || position - last > 1.0);
        let duration = 8.0 / (settings.speed.clamp(50, 200) as f64 / 100.0);
        let from = if rebuild {
            self.active.clear();
            position - duration
        } else {
            self.last.unwrap()
        };
        self.geometry = Some(geometry);
        let font =
            (rect.width() / 40.0).clamp(18.0, 30.0) * settings.font.clamp(75, 150) as f32 / 100.0;
        let row_height = font * 1.6;
        let rows = (rect.height() / row_height).floor().max(1.0) as usize;
        for comments in self.segments.values() {
            let start = comments.partition_point(|c| c.time <= from);
            let end = comments.partition_point(|c| c.time <= position);
            for comment in &comments[start..end] {
                self.active.retain(|b| b.end > comment.time);
                if self.active.len() >= MAX_ACTIVE {
                    continue;
                }
                let size = font * (comment.size as f32 / 25.0).clamp(0.75, 1.25);
                let text = painter.layout_no_wrap(
                    comment.text.clone(),
                    egui::FontId::proportional(size),
                    Color32::WHITE,
                );
                let mut bullet = Bullet {
                    start: comment.time,
                    end: comment.time
                        + if matches!(comment.mode, 4 | 5) {
                            duration.min(5.0)
                        } else {
                            duration
                        },
                    row: 0,
                    width: text.size().x,
                    screen_width: rect.width(),
                    mode: comment.mode,
                    color: Color32::from_rgb(
                        (comment.color >> 16) as u8,
                        (comment.color >> 8) as u8,
                        comment.color as u8,
                    ),
                    text,
                };
                let slot = (0..rows).find(|row| {
                    bullet.row = if comment.mode == 4 {
                        rows - 1 - row
                    } else {
                        *row
                    };
                    self.active.iter().all(|old| !bullet.conflicts(old))
                });
                if slot.is_some() {
                    self.active.push(bullet);
                }
            }
        }
        self.last = Some(position);
        self.active.retain(|b| b.end > position);
        let alpha = settings.opacity.clamp(10, 100) as f32 / 100.0;
        let mut painter = painter.with_clip_rect(rect);
        painter.multiply_opacity(alpha);
        for bullet in &self.active {
            let point = rect.min + vec2(bullet.x(position), bullet.row as f32 * row_height);
            // A small shadow improves readability without rasterizing each glyph repeatedly.
            painter.galley_with_override_text_color(
                point + vec2(1.0, 1.0),
                bullet.text.clone(),
                Color32::from_black_alpha(220),
            );
            painter.galley_with_override_text_color(point, bullet.text.clone(), bullet.color);
        }
    }
}

impl App {
    pub(super) fn poll_danmaku(&mut self, ctx: &egui::Context) {
        let source = self.bilibili_danmaku_source();
        let (position, duration) = self
            .player
            .as_ref()
            .map(|p| (p.position(), p.duration()))
            .unwrap_or_default();
        self.danmaku.poll(
            ctx,
            source,
            position,
            duration,
            self.settings.video.danmaku.enabled
                && self.settings.video.danmaku.area > 0
                && self.workspace.page == workspace::Page::Playing,
        );
    }
    pub(super) fn paint_danmaku(
        &mut self,
        painter: &Painter,
        video: Rect,
        screen: Rect,
        position: f64,
    ) {
        if !self.bilibili_controls_available() {
            return;
        }
        let settings = &self.settings.video.danmaku;
        let top = video.top().max(screen.top() + BAR_TOP + 8.0);
        let mut bottom = (video.top() + video.height() * settings.area.min(100) as f32 / 100.0)
            .min(screen.bottom() - BAR_BOTTOM - 12.0)
            .min(video.bottom() - 12.0);
        if self.subtitles_visible && subtitles::active(&self.subtitles, position).is_some() {
            bottom = bottom.min(video.bottom().min(screen.bottom() - BAR_BOTTOM) - 100.0);
        }
        let rect = Rect::from_min_max(
            pos2(video.left() + 12.0, top),
            pos2(video.right() - 12.0, bottom),
        );
        self.danmaku.paint(painter, rect, position, settings);
    }
    pub(in crate::app) fn video_speed_button(&mut self, ui: &mut Ui, alpha: f32) {
        let l = self.settings.language;
        let speed = self.player.as_ref().map_or(1.0, Player::speed);
        let label = speed_label(speed);
        let response = design::icon_button_with_background(
            ui,
            40.0,
            vec2(36.0, 28.0),
            alpha,
            speed != 1.0,
            l.text("播放速度", "Playback speed"),
            |p, r, color| {
                p.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    &label,
                    egui::FontId::proportional(13.0),
                    color,
                );
            },
        );
        let mut selected = None;
        egui::Popup::menu(&response).width(148.0).show(|ui| {
            ui.label(l.text("播放速度", "Playback speed"));
            ui.separator();
            for value in [1.0, 1.25, 1.5, 2.0, 3.0] {
                if design::menu_item(ui, value == speed, &speed_label(value)).clicked() {
                    selected = Some(value);
                    ui.close();
                }
            }
        });
        if let Some(value) = selected {
            if let Some(player) = &self.player
                && let Some(id) = player.set_speed(value)
            {
                let position = self
                    .pending_seek
                    .map_or_else(|| player.position(), |(_, p)| p);
                self.pending_seek = Some((id, position));
            }
            self.settings.video.speed = (value * 100.0) as u16;
            self.preferences_draft.video.speed = self.settings.video.speed;
            self.persist_video_controls();
        }
    }
    pub(in crate::app) fn danmaku_button(&mut self, ui: &mut Ui, alpha: f32) {
        let l = self.settings.language;
        let response = design::icon_button_sized(
            ui,
            28.0,
            alpha,
            self.settings.video.danmaku.enabled,
            l.text("弹幕设置", "Danmaku settings"),
            |p, r, color| {
                icons::paint(p, r, color, icons::CHAT_CENTERED_TEXT);
            },
        );
        let before = self.settings.video.danmaku.clone();
        egui::Popup::menu(&response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .width(300.0)
            .show(|ui| {
                self.danmaku_panel(ui);
            });
        if before != self.settings.video.danmaku {
            self.preferences_draft.video.danmaku = self.settings.video.danmaku.clone();
            // Persist once after a slider gesture, without vault/filesystem writes each frame.
            self.video_controls_dirty = true;
        }
    }
    pub(super) fn danmaku_panel(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 8.0;
        let l = self.settings.language;
        ui.horizontal(|ui| {
            ui.strong(l.text("弹幕", "Danmaku"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                design::toggle(
                    ui,
                    &mut self.settings.video.danmaku.enabled,
                    l.text("显示弹幕", "Show danmaku"),
                );
            });
        });
        ui.separator();
        ui.add_space(4.0);
        let settings = &mut self.settings.video.danmaku;
        ui.add_enabled_ui(settings.enabled, |ui| {
            let mut area = settings.area as u16;
            config_slider(
                ui,
                l.text("显示区域", "Display area"),
                &mut area,
                0..=100,
                &[0, 25, 50, 75, 100],
                |v| format!("{v}%"),
            );
            settings.area = area as u8;
            let mut opacity = settings.opacity as u16;
            config_slider(
                ui,
                l.text("不透明度", "Opacity"),
                &mut opacity,
                10..=100,
                &[],
                |v| format!("{v}%"),
            );
            settings.opacity = opacity as u8;
            config_slider(
                ui,
                l.text("字号", "Font size"),
                &mut settings.font,
                75..=150,
                &[],
                |v| format!("{v}%"),
            );
            config_slider(
                ui,
                l.text("弹幕速度", "Speed"),
                &mut settings.speed,
                50..=150,
                &[50, 75, 100, 125, 150],
                |v| {
                    match v {
                        0..=62 => l.text("慢", "Slow"),
                        63..=87 => l.text("较慢", "Slower"),
                        88..=112 => l.text("适中", "Normal"),
                        113..=137 => l.text("较快", "Faster"),
                        _ => l.text("快", "Fast"),
                    }
                    .into()
                },
            );
        });
        let index =
            (self.player.as_ref().map_or(0.0, Player::position) / SEGMENT_SECONDS) as u32 + 1;
        if let Some(error) = self.danmaku.errors.get(&index).cloned() {
            ui.separator();
            ui.label(RichText::new(l.error(&error)).color(MUTED).size(12.0));
            if design::control(
                ui,
                l.text("重试", "Retry"),
                Some(icons::refresh),
                design::Control::Outline,
                0.0,
            )
            .clicked()
            {
                self.danmaku.errors.clear();
            }
        } else if self.danmaku.jobs.contains_key(&index) {
            ui.separator();
            ui.small(l.text("正在加载弹幕…", "Loading danmaku…"));
        } else if self.danmaku.segments.get(&index).is_some_and(Vec::is_empty) {
            ui.separator();
            ui.small(l.text("当前时段暂无弹幕", "No danmaku in this segment"));
        }
    }
    fn persist_video_controls(&mut self) {
        if let Err(e) = self.settings.save() {
            self.error = Some(e.to_string());
        }
        self.video_controls_dirty = false;
    }
    pub(super) fn flush_video_controls(&mut self, ctx: &egui::Context) {
        if self.video_controls_dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.persist_video_controls();
        }
    }
}
fn config_slider(
    ui: &mut Ui,
    label: &str,
    value: &mut u16,
    range: std::ops::RangeInclusive<u16>,
    ticks: &[u16],
    format: impl Fn(u16) -> String,
) {
    let (min, max) = (*range.start(), *range.end());
    ui.push_id(label, |ui| {
        ui.horizontal(|ui| {
            ui.add_sized(vec2(76.0, 30.0), egui::Label::new(label));
            let width = (ui.available_width() - 52.0 - ui.spacing().item_spacing.x).max(50.0);
            let fraction = ((*value).clamp(min, max) - min) as f32 / (max - min) as f32;
            let (rect, response) = hslider(
                ui,
                width,
                30.0,
                fraction,
                4.0,
                false,
                if ui.is_enabled() { 1.0 } else { 0.4 },
            );
            for tick in ticks {
                let x = rect.left() + rect.width() * (*tick - min) as f32 / (max - min) as f32;
                if (x - (rect.left() + rect.width() * fraction)).abs() > 7.0 {
                    ui.painter()
                        .circle_filled(pos2(x, rect.center().y), 1.8, MUTED);
                }
            }
            if (response.clicked() || response.dragged())
                && let Some(point) = response.interact_pointer_pos()
            {
                *value = (min as f32
                    + ((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * (max - min) as f32)
                    .round() as u16;
                response.request_focus();
            }
            if ui.is_enabled() && response.has_focus() {
                ui.painter().rect_stroke(
                    rect.shrink2(vec2(0.0, 5.0)),
                    4,
                    Stroke::new(1.0, ACCENT),
                    egui::StrokeKind::Outside,
                );
                if ui.input(|i| i.key_pressed(Key::ArrowLeft)) {
                    *value = value.saturating_sub(1).max(min);
                }
                if ui.input(|i| i.key_pressed(Key::ArrowRight)) {
                    *value = value.saturating_add(1).min(max);
                }
            }
            response
                .widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), *value as f64, label));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(format(*value));
            });
        })
    });
}
fn speed_label(speed: f32) -> String {
    if speed == 1.25 {
        "1.25×".into()
    } else {
        format!("{speed:.1}×")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tracks_reject_catch_up_and_reverse_crossings() {
        let ctx = egui::Context::default();
        let mut text = None;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            text = Some(ui.painter().layout_no_wrap(
                "text".into(),
                egui::FontId::default(),
                Color32::WHITE,
            ));
        });
        output.textures_delta.clear();
        let text = text.unwrap();
        let bullet = |start, end, width, mode| Bullet {
            start,
            end,
            row: 0,
            width,
            screen_width: 800.0,
            mode,
            color: Color32::WHITE,
            text: text.clone(),
        };
        let first = bullet(0.0, 8.0, 100.0, 1);
        assert!(first.conflicts(&bullet(0.1, 8.1, 100.0, 1)));
        assert!(!first.conflicts(&bullet(3.0, 11.0, 100.0, 1)));
        assert!(first.conflicts(&bullet(2.0, 6.0, 100.0, 6)));
        assert!(!first.conflicts(&bullet(8.0, 16.0, 100.0, 1)));
    }

    #[test]
    fn seek_resize_pause_and_visibility_rebuild_only_current_comments() {
        let ctx = egui::Context::default();
        let mut state = State::default();
        state.seed_fixture();
        let mut settings = DanmakuSettings::default();
        for (time, width, font) in [
            (30.0, 900.0, 100),
            (30.0, 900.0, 100),
            (24.0, 900.0, 100),
            (40.0, 760.0, 150),
        ] {
            settings.font = font;
            let mut output = ctx.run_ui(Default::default(), |ui| {
                state.paint(
                    ui.painter(),
                    Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 250.0)),
                    time,
                    &settings,
                )
            });
            output.textures_delta.clear();
            assert!(state.active.len() <= MAX_ACTIVE);
            assert!(state.active.iter().all(|b| b.start <= time && b.end > time));
            for (i, a) in state.active.iter().enumerate() {
                assert!(state.active[i + 1..].iter().all(|b| !a.conflicts(b)));
            }
        }
        state.poll(&ctx, None, 40.0, 300.0, false);
        assert!(state.active.is_empty());
        assert!(state.jobs.is_empty());
    }

    #[test]
    fn returning_from_settings_clears_route_flag_and_retains_edits() {
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        for page in [
            workspace::Page::Playing,
            workspace::Page::Listening,
            workspace::Page::Comic,
        ] {
            app.navigate(page);
            app.navigate(workspace::Page::Settings);
            assert!(app.settings_open);
            assert!(app.workspace.settings_return == Some(page));
            app.preferences_draft.video.danmaku.opacity = 45;
            app.navigate(page);
            assert!(!app.settings_open);
            app.preferences(&ctx);
            assert!(app.workspace.page == page);
            app.navigate(workspace::Page::Settings);
            assert_eq!(app.preferences_draft.video.danmaku.opacity, 45);
        }
    }
}
