use super::*;

impl App {
    pub(super) fn video_header(&mut self, root: &mut Ui, screen: Rect, alpha: f32, act: &mut Act) {
        let bar = Rect::from_min_size(screen.min, vec2(screen.width(), BAR_TOP));
        root.painter()
            .rect_filled(bar, 0, Color32::from_black_alpha((alpha * 200.0) as u8));
        let center = bar.center().y;
        let end = bar.right() - window::playback_controls_width(root.ctx()) - 12.0;
        let back_rect = Rect::from_center_size(pos2(bar.left() + 32.0, center), Vec2::splat(32.0));
        let mut back = workspace::child(root, "video-back", back_rect);
        if design::icon_button_sized(
            &mut back,
            32.0,
            alpha,
            false,
            self.settings
                .language
                .text("返回视频库", "Back to video library"),
            |p, rect, color| icons::paint(p, rect, color, icons::ARROW_LEFT),
        )
        .clicked()
        {
            self.navigate(workspace::Page::Video);
        }
        let open_rect = Rect::from_center_size(pos2(end - 16.0, center), Vec2::splat(32.0));
        let danmaku = self.bilibili_controls_available();
        let title_rect = Rect::from_min_max(
            pos2(back_rect.right() + 12.0, bar.top()),
            pos2(
                open_rect.left() - 12.0 - if danmaku { 40.0 } else { 0.0 },
                bar.bottom(),
            ),
        );
        let mut title = workspace::child(root, "video-title", title_rect);
        title.multiply_opacity(alpha);
        title.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(&self.title).size(18.0).strong()).truncate());
        });
        if danmaku {
            let rect = Rect::from_center_size(
                pos2(open_rect.center().x - 40.0, center),
                Vec2::splat(28.0),
            );
            let mut ui = workspace::child(root, "video-header-danmaku", rect);
            self.danmaku_button(&mut ui, alpha);
        }
        let mut open = workspace::child(root, "video-open", open_rect);
        if design::icon_button_sized(
            &mut open,
            32.0,
            alpha,
            false,
            self.settings.language.text("打开文件", "Open files"),
            |p, rect, color| icons::paint(p, rect, color, icons::FOLDER_OPEN),
        )
        .clicked()
        {
            *act = Act::Open;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires GPU; captures the real playback header without media or network"]
    fn playback_header_visuals() {
        let directory = std::path::Path::new("target/ui-review/bilibili");
        std::fs::create_dir_all(directory).unwrap();
        let mut capture = comic::visual_tests::Capture::new();
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        app.title = "40岁老宅男的枯燥生活".into();
        for width in [760, 1280] {
            capture.save(
                &ctx,
                [width, 100],
                &directory.join(format!("playback-header-{width}.png")),
                |ui| {
                    let screen = ui.max_rect();
                    ui.painter().rect_filled(screen, 0, Color32::BLACK);
                    app.video_header(ui, screen, 1.0, &mut Act::None);
                    app.window_frame(ui, true);
                },
            );
        }
    }
    #[test]
    fn playback_header_icons_and_text_share_the_window_caption_center() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for width in [760.0, 1280.0] {
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                let mut app = comic::visual_tests::settings_fixture(&ctx);
                app.title = "A long video title that must not overlap window controls".into();
                let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 540.0));
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ui| {
                        app.video_header(ui, screen, 1.0, &mut Act::None);
                        app.window_frame(ui, true);
                    },
                );
                let centers: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|s| match &s.shape {
                        Shape::Text(t)
                            if [icons::ARROW_LEFT, icons::FOLDER_OPEN, app.title.as_str()]
                                .contains(&t.galley.text()) =>
                        {
                            Some((
                                t.galley.text().to_owned(),
                                t.pos.y + t.galley.size().y / 2.0,
                            ))
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(centers.len(), 3);
                for (label, center) in centers {
                    assert!(
                        (center - window::CAPTION_HEIGHT / 2.0).abs() <= 1.0,
                        "{label}: {center}"
                    );
                }
                output.textures_delta.clear();
            }
        }
    }
}
