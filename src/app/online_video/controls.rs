use super::*;

impl App {
    pub(in crate::app) fn bilibili_controls_available(&self) -> bool {
        !self.is_music()
            && self
                .online_video
                .active
                .as_ref()
                .is_some_and(|active| self.source.as_ref() == Some(&active.media_path))
    }
    pub(in crate::app) fn bilibili_queue_len(&self) -> Option<usize> {
        self.online_video
            .active
            .as_ref()
            .filter(|_| self.bilibili_controls_available())
            .map(|active| active.detail.parts.len())
    }

    pub(in crate::app) fn bilibili_quality_button(&mut self, ui: &mut Ui, alpha: f32) {
        if !self.bilibili_controls_available() {
            return;
        }
        let Some(active) = self.online_video.active.clone() else {
            return;
        };
        let l = self.settings.language;
        let label = active
            .playback
            .qualities
            .iter()
            .find(|q| q.id == active.playback.quality)
            .map(|q| q.label.clone())
            .unwrap_or_else(|| active.playback.quality.to_string());
        let caption = format!("{} · {label}", l.text("画质", "Quality"));
        let response =
            design::icon_button_sized(ui, 28.0, alpha, false, &caption, |p, rect, color| {
                icons::paint(p, rect, color, icons::HIGH_DEFINITION)
            });
        let mut selected = None;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(180.0);
            ui.label(l.text("画质", "Quality"));
            ui.separator();
            for quality in &active.playback.qualities {
                if design::menu_item(ui, quality.id == active.playback.quality, &quality.label)
                    .clicked()
                {
                    selected = Some(quality.id);
                    ui.close();
                }
            }
        });
        if let Some(quality) = selected {
            self.change_bilibili(quality, active.part);
        }
    }

    pub(in crate::app) fn bilibili_queue_contents(&mut self, ui: &mut Ui) {
        let Some(active) = self
            .online_video
            .active
            .as_ref()
            .filter(|_| self.bilibili_controls_available())
        else {
            return;
        };
        let quality = active.playback.quality;
        let mut selected = None;
        design::scroll()
            .id_salt("bilibili-play-queue")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (index, part) in active.detail.parts.iter().enumerate() {
                    let label = format!("P{} \u{00b7} {}", part.page, part.part);
                    let current = index == active.part;
                    let response = ui.add_sized(
                        [ui.available_width(), 44.0],
                        egui::Button::new(RichText::new(&label).color(if current {
                            ACCENT
                        } else {
                            design::TEXT
                        }))
                        .selected(current)
                        .wrap()
                        .frame(true),
                    );
                    if response.on_hover_text(&label).clicked() {
                        selected = Some(index);
                    }
                }
            });
        if let Some(part) = selected {
            self.change_bilibili(quality, part);
        }
    }

    fn change_bilibili(&mut self, quality: u32, part: usize) {
        if self.online_video.plan_change(quality, part) {
            self.error = None;
            self.video_resolve(quality);
        }
    }
}

impl State {
    fn plan_change(&mut self, quality: u32, part: usize) -> bool {
        let Some(active) = self.active.clone() else {
            return false;
        };
        if part >= active.detail.parts.len() {
            return false;
        }
        let changing = self.resolve.is_some() || self.prepare.is_some();
        self.cancel_discovery();
        if part == active.part && quality == active.playback.quality {
            if changing {
                self.status.clear();
            }
            return false;
        }
        self.detail = Some(active.detail);
        self.part = part;
        self.resume = None;
        self.keep_position = part == active.part;
        self.auto_play = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active() -> Active {
        Active {
            media_path: PathBuf::from("http://127.0.0.1:12345/session/video.mp4"),
            detail: Detail {
                bvid: "BV1GJ411x7h7".into(),
                title: "Current video".into(),
                cover: None,
                parts: vec![
                    bilibili::Part {
                        cid: 1,
                        page: 1,
                        part: "First part".into(),
                        duration: 300,
                    },
                    bilibili::Part {
                        cid: 2,
                        page: 2,
                        part: "Second part".into(),
                        duration: 300,
                    },
                ],
            },
            part: 0,
            playback: Playback {
                bvid: "BV1GJ411x7h7".into(),
                cid: 1,
                quality: 80,
                duration: 300.0,
                qualities: vec![
                    bilibili::Quality {
                        id: 80,
                        label: "1080P".into(),
                    },
                    bilibili::Quality {
                        id: 64,
                        label: "720P".into(),
                    },
                ],
                video: bilibili::Stream {
                    urls: vec![],
                    codec: "avc1".into(),
                },
                audio: None,
            },
        }
    }
    fn frame(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(760.0, 540.0))),
                events,
                ..Default::default()
            },
            |ui| {
                let mut act = Act::None;
                let screen = ui.max_rect();
                app.video_header(ui, screen, 1.0, &mut act);
                app.video_transport(ui, screen, 1.0, &mut act);
                if app.queue_open {
                    app.queue_sidebar(
                        ui,
                        Rect::from_min_max(
                            pos2(screen.right() - 280.0, screen.top() + BAR_TOP),
                            pos2(screen.right(), screen.bottom() - BAR_BOTTOM),
                        ),
                        &mut act,
                    );
                }
            },
        );
        output.textures_delta.clear();
        output
    }
    fn text_point(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
    }
    fn click(ctx: &egui::Context, app: &mut App, point: egui::Pos2) -> egui::FullOutput {
        frame(ctx, app, vec![egui::Event::PointerMoved(point)]);
        for pressed in [true, false] {
            frame(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        frame(ctx, app, vec![])
    }

    #[test]
    fn quality_menu_and_queue_sidebar_switch_from_the_active_session_with_an_empty_local_queue() {
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        app.settings.language = Language::English;
        app.online_video.active = Some(active());
        app.source = Some(active().media_path);
        app.mode = UiMode::Video;
        app.queue = Default::default();
        app.online_video.detail = None; // Browsing/search state must not own playback controls.
        let mut output = frame(&ctx, &mut app, vec![]);
        for _ in 0..2 {
            output = frame(&ctx, &mut app, vec![]);
        }
        let quality = text_point(&output, icons::HIGH_DEFINITION).expect("quality icon");
        assert!(text_point(&output, icons::LIST_NUMBERS).is_none());
        let speed = text_point(&output, "1.0\u{00d7}").unwrap();
        let subtitles = text_point(&output, icons::SUBTITLES).unwrap();
        let queue = text_point(&output, icons::QUEUE).unwrap();
        assert!(quality.x < speed.x && speed.x < subtitles.x && subtitles.x < queue.x);
        assert!(app.queue.items.is_empty());
        assert!(
            text_point(&output, "1080P").is_none(),
            "choices are not laid out inline"
        );
        output = click(&ctx, &mut app, quality);
        let choice = text_point(&output, "720P").expect("720P menu entry");
        click(&ctx, &mut app, choice);
        assert!(app.online_video.keep_position);
        assert!(app.online_video.auto_play);
        assert_eq!(
            app.online_video.active.as_ref().unwrap().playback.quality,
            80,
            "old session remains active until replacement is ready"
        );
        output = frame(&ctx, &mut app, vec![]);
        let queue = text_point(&output, icons::QUEUE).unwrap();
        output = click(&ctx, &mut app, queue);
        assert!(app.queue_open);
        assert_eq!(app.bilibili_queue_len(), Some(2));
        let choice = text_point(&output, "P2 · Second part").expect("part queue entry");
        click(&ctx, &mut app, choice);
        assert_eq!(app.online_video.part, 1);
        assert!(!app.online_video.keep_position);
        assert!(app.online_video.auto_play);
        app.online_video.stop();
        output = frame(&ctx, &mut app, vec![]);
        assert!(text_point(&output, icons::HIGH_DEFINITION).is_none());
        assert!(text_point(&output, icons::LIST_NUMBERS).is_none());
    }

    #[test]
    fn bilibili_buttons_are_hidden_for_local_magnet_music_and_stale_metadata() {
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        app.mode = UiMode::Video;
        app.online_video.active = Some(active());
        for path in [
            None,
            Some("F:/videos/movie.mp4"),
            Some("magnet:?xt=urn:btih:example"),
            Some("http://127.0.0.1:12345/torrent/video.mp4"),
            Some("F:/music/song.flac"),
            Some("https://example.com/video.mp4"),
        ] {
            app.source = path.map(PathBuf::from);
            assert!(!app.bilibili_controls_available());
            assert!(app.bilibili_queue_len().is_none());
            let output = frame(&ctx, &mut app, vec![]);
            assert!(text_point(&output, icons::HIGH_DEFINITION).is_none());
            assert!(text_point(&output, icons::LIST_NUMBERS).is_none());
        }
        app.source = Some(active().media_path);
        app.mode = UiMode::Music;
        assert!(!app.bilibili_controls_available());
        assert!(app.bilibili_queue_len().is_none());
        app.mode = UiMode::Video;
        assert!(app.bilibili_controls_available());
        app.online_video
            .active
            .as_mut()
            .unwrap()
            .detail
            .parts
            .truncate(1);
        assert_eq!(app.bilibili_queue_len(), Some(1));
    }

    #[test]
    fn new_video_starts_automatically_and_stale_jobs_are_cancelled_when_leaving_or_opening_local_media()
     {
        let mut state = State {
            active: Some(active()),
            ..Default::default()
        };
        state.resolve = Some(Job::spawn(|_| std::future::pending()));
        state.begin_video(Job::spawn(|_| std::future::pending()), 2, Some(25.0));
        assert!(state.auto_play);
        assert_eq!(state.requested_page, 2);
        assert_eq!(state.resume, Some(25.0));
        assert!(state.resolve.is_none());
        state.stop();
        assert!(state.active.is_none());
        assert!(state.detail_job.is_none());
        assert!(state.resolve.is_none());
        assert!(state.prepare.is_none());
        assert!(!state.auto_play);
    }

    #[test]
    fn speed_stays_in_transport_and_danmaku_moves_to_the_header() {
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        app.settings.language = Language::English;
        app.online_video.active = Some(active());
        app.source = Some(active().media_path);
        app.mode = UiMode::Video;
        let mut output = frame(&ctx, &mut app, vec![]);
        let speed = text_point(&output, "1.0×").expect("speed entry");
        assert!(text_point(&output, icons::CHAT_CENTERED_TEXT).unwrap().y < BAR_TOP);
        assert!(text_point(&output, icons::GEAR_SIX).is_none());
        output = click(&ctx, &mut app, speed);
        for _ in 0..12 {
            output = frame(&ctx, &mut app, vec![]);
        }
        for label in ["1.25×", "1.5×", "2.0×", "3.0×"] {
            assert!(text_point(&output, label).is_some(), "{label}");
        }
        egui::Popup::close_all(&ctx);
        output = frame(&ctx, &mut app, vec![]);
        let danmaku = text_point(&output, icons::CHAT_CENTERED_TEXT).unwrap();
        output = click(&ctx, &mut app, danmaku);
        for _ in 0..12 {
            output = frame(&ctx, &mut app, vec![]);
        }
        for label in ["Display area", "Opacity", "Font size", "Speed"] {
            assert!(text_point(&output, label).is_some(), "{label}");
        }
        let area_label = text_point(&output, "Display area").unwrap();
        let speed_label = text_point(&output, "Speed").unwrap();
        // Move the area's slider to its upper end, then move speed to its lower end.
        let area_end = pos2(area_label.x + 176.0, area_label.y);
        click(&ctx, &mut app, area_end);
        assert!(app.settings.video.danmaku.area >= 90);
        assert_eq!(app.settings.video.danmaku.speed, 100);
        let speed_start = pos2(speed_label.x + 63.0, speed_label.y);
        let output = click(&ctx, &mut app, speed_start);
        assert!(app.settings.video.danmaku.speed < 80);
        assert!(app.settings.video.danmaku.area >= 90);
        assert!(
            text_point(&output, "Opacity").is_some(),
            "configuration stays open"
        );
        assert!(!output.shapes.iter().any(
            |s| matches!(&s.shape, Shape::Text(t) if t.galley.text().contains("use of widget ID"))
        ));
        assert!(!app.settings_open);
    }

    #[test]
    #[ignore = "requires GPU; captures actual player controls and danmaku using sample comments"]
    fn playback_danmaku_visuals() {
        fn draw(app: &mut App, ui: &mut Ui, width: u32) {
            let screen = ui.max_rect();
            let painter = ui.painter();
            painter.rect_filled(screen, 0, Color32::from_rgb(50, 68, 72));
            painter.rect_filled(
                Rect::from_min_max(pos2(screen.left(), screen.center().y), screen.max),
                0,
                Color32::from_rgb(35, 57, 56),
            );
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(0.0, 450.0),
                    pos2(width as f32 * 0.30, 220.0),
                    pos2(width as f32 * 0.70, 450.0),
                ],
                Color32::from_rgb(41, 54, 52),
                Stroke::NONE,
            ));
            app.paint_danmaku(&ui.painter().clone(), screen, screen, 30.0);
            app.video_header(ui, screen, 1.0, &mut Act::None);
            app.video_transport(ui, screen, 1.0, &mut Act::None);
            if app.queue_open {
                app.queue_sidebar(
                    ui,
                    Rect::from_min_max(
                        pos2(screen.right() - 280.0, BAR_TOP),
                        pos2(screen.right(), screen.bottom() - BAR_BOTTOM),
                    ),
                    &mut Act::None,
                );
            }
            app.window_frame(ui, true);
        }
        let directory = std::path::Path::new("target/ui-review/playback");
        std::fs::create_dir_all(directory).unwrap();
        let mut capture = comic::visual_tests::Capture::new();
        for (language, suffix) in [(Language::Chinese, "zh"), (Language::English, "en")] {
            for width in [760, 1280] {
                let ctx = egui::Context::default();
                let mut app = comic::visual_tests::settings_fixture(&ctx);
                app.settings.language = language;
                app.workspace.page = workspace::Page::Playing;
                app.mode = UiMode::Video;
                app.online_video.active = Some(active());
                app.source = Some(active().media_path);
                app.title = language
                    .text("沿着山路，去看远方的海", "A road to the distant sea")
                    .into();
                app.danmaku.seed_fixture();
                let mut point = None;
                for _ in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                egui::Pos2::ZERO,
                                vec2(width as f32, 700.0),
                            )),
                            ..Default::default()
                        },
                        |ui| draw(&mut app, ui, width),
                    );
                    point = text_point(&output, icons::CHAT_CENTERED_TEXT);
                    capture.apply_textures(&mut output);
                }
                let point = point.unwrap();
                for pressed in [true, false] {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                egui::Pos2::ZERO,
                                vec2(width as f32, 700.0),
                            )),
                            events: vec![
                                egui::Event::PointerMoved(point),
                                egui::Event::PointerButton {
                                    pos: point,
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ],
                            ..Default::default()
                        },
                        |ui| draw(&mut app, ui, width),
                    );
                    capture.apply_textures(&mut output);
                }
                capture.save(
                    &ctx,
                    [width, 700],
                    &directory.join(format!("danmaku-{suffix}-{width}.png")),
                    |ui| draw(&mut app, ui, width),
                );
                egui::Popup::close_all(&ctx);
                app.queue_open = true;
                capture.save(
                    &ctx,
                    [width, 700],
                    &directory.join(format!("queue-{suffix}-{width}.png")),
                    |ui| draw(&mut app, ui, width),
                );
            }
        }
    }
}
