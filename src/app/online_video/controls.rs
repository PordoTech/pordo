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
    pub(in crate::app) fn bilibili_parts_available(&self) -> bool {
        self.bilibili_controls_available()
            && self
                .online_video
                .active
                .as_ref()
                .is_some_and(|active| active.detail.parts.len() > 1)
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

    pub(in crate::app) fn bilibili_parts_button(&mut self, ui: &mut Ui, alpha: f32) {
        if !self.bilibili_parts_available() {
            return;
        }
        let Some(active) = self.online_video.active.clone() else {
            return;
        };
        let l = self.settings.language;
        let response = design::icon_button_sized(
            ui,
            28.0,
            alpha,
            false,
            l.text("分P", "Parts"),
            |p, rect, color| icons::paint(p, rect, color, icons::LIST_NUMBERS),
        );
        let mut selected = None;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(240.0);
            ui.set_max_width(340.0);
            ui.label(l.text("分P", "Parts"));
            ui.separator();
            design::scroll().max_height(280.0).show(ui, |ui| {
                for (index, part) in active.detail.parts.iter().enumerate() {
                    let label = format!("P{} · {}", part.page, part.part);
                    if design::menu_item(ui, index == active.part, &label)
                        .on_hover_text(&label)
                        .clicked()
                    {
                        selected = Some(index);
                        ui.close();
                    }
                }
            });
        });
        if let Some(part) = selected {
            self.change_bilibili(bilibili::DEFAULT_QUALITY, part);
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
                app.video_transport(ui, ui.max_rect(), 1.0, &mut act);
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
    fn quality_and_parts_live_in_bottom_right_icon_menus_and_switch_from_the_active_session() {
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        app.settings.language = Language::English;
        app.online_video.active = Some(active());
        app.source = Some(active().media_path);
        app.mode = UiMode::Video;
        app.online_video.detail = None; // Browsing/search state must not own playback controls.
        let mut output = frame(&ctx, &mut app, vec![]);
        for _ in 0..2 {
            output = frame(&ctx, &mut app, vec![]);
        }
        let quality = text_point(&output, icons::HIGH_DEFINITION).expect("quality icon");
        let parts = text_point(&output, icons::LIST_NUMBERS).expect("parts icon");
        assert!(quality.x > 440.0 && parts.x > 440.0 && quality.y > 450.0 && parts.y > 450.0);
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
        let parts = text_point(&output, icons::LIST_NUMBERS).unwrap();
        output = click(&ctx, &mut app, parts);
        let choice = text_point(&output, "P2 · Second part").expect("part menu entry");
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
            assert!(!app.bilibili_parts_available());
            let output = frame(&ctx, &mut app, vec![]);
            assert!(text_point(&output, icons::HIGH_DEFINITION).is_none());
            assert!(text_point(&output, icons::LIST_NUMBERS).is_none());
        }
        app.source = Some(active().media_path);
        app.mode = UiMode::Music;
        assert!(!app.bilibili_controls_available());
        assert!(!app.bilibili_parts_available());
        app.mode = UiMode::Video;
        assert!(app.bilibili_controls_available());
        app.online_video
            .active
            .as_mut()
            .unwrap()
            .detail
            .parts
            .truncate(1);
        assert!(!app.bilibili_parts_available());
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
}
