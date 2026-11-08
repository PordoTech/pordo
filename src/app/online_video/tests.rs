use super::*;

#[test]
fn choosing_video_enters_playback_before_metadata_and_back_cancels_loading() {
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.workspace.page = workspace::Page::Video;
    app.ui_kind_music = Some(true);
    app.online_video.search = Some(Job::spawn(|_| std::future::pending()));
    app.begin_bilibili_playback(
        Job::spawn(|_| std::future::pending()),
        2,
        None,
        "Selected card".into(),
    );
    assert_eq!(app.workspace.page as u8, workspace::Page::Playing as u8);
    assert!(app.loading && app.online_video.detail_job.is_some());
    assert!(app.online_video.search.is_none());
    assert_eq!(app.title, "Selected card");
    assert!(
        !app.is_music(),
        "switch directly from music to video loading"
    );
    app.navigate(workspace::Page::Video);
    assert!(!app.loading && app.online_video.detail_job.is_none());
}

#[test]
fn playback_device_warnings_do_not_become_global_comic_errors() {
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.language = Language::English;
    app.playback_warning = Some("audio device unavailable; continuing with video".into());
    app.loading = true;
    app.settings_open = false;
    for page in [
        workspace::Page::Comic,
        workspace::Page::Home,
        workspace::Page::Settings,
    ] {
        app.workspace.page = page;
        assert!(app.visible_playback_warning().is_none());
    }
    app.workspace.page = workspace::Page::Comic;
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 760.0))),
            ..Default::default()
        },
        |ui| app.workspace_ui(ui),
    );
    assert!(!output.shapes.iter().any(
        |s| matches!(&s.shape,Shape::Text(t) if t.galley.text().contains("continuing with video"))
    ));
    output.textures_delta.clear();
    assert!(app.error.is_none());
    app.workspace.page = workspace::Page::Playing;
    assert!(app.visible_playback_warning().is_some());
}

#[test]
fn search_never_shows_the_old_playback_setup_page_or_permission_hint() {
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.language = Language::English;
    app.online_video.api = Some(Arc::new(Bilibili::new(None).unwrap()));
    app.online_video.detail = Some(Detail {
        bvid: "BV1GJ411x7h7".into(),
        title: "Old setup heading".into(),
        cover: None,
        parts: vec![bilibili::Part {
            cid: 1,
            page: 1,
            part: "Old setup part".into(),
            duration: 300,
        }],
    });
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 760.0))),
            ..Default::default()
        },
        |ui| app.online_video_contents(ui),
    );
    output.textures_delta.clear();
    let text: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            Shape::Text(t) => Some(t.galley.text()),
            _ => None,
        })
        .collect();
    assert!(!text.contains(&"Old setup heading"));
    assert!(!text.contains(&"Old setup part"));
    assert!(!text.contains(&"Play"));
    assert!(!text.contains(&"Available qualities depend on your account and supported codecs."));
}

#[test]
fn video_favorites_show_creators_without_empty_video_notice_and_cancel_pending_navigation() {
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.language = Language::English;
    app.workspace.page = workspace::Page::Video;
    app.workspace.video_tab = 0;
    app.library.records.clear();
    let creator: Uploader =
        serde_json::from_value(serde_json::json!({"mid":123,"uname":"Favorite UP"})).unwrap();
    app.library.creators = vec![library::CreatorRecord {
        uploader: creator.clone(),
        favorite: true,
        favorited_at: 1,
    }];
    let mut output = egui::FullOutput::default();
    for _ in 0..3 {
        output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 760.0))),
                ..Default::default()
            },
            |ui| app.workspace_ui(ui),
        );
        output.textures_delta.clear();
    }
    let text: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            Shape::Text(t) => Some(t.galley.text()),
            _ => None,
        })
        .collect();
    assert!(text.contains(&"Favorite creators"));
    assert!(text.contains(&"Favorite UP"));
    assert!(!text.contains(&"No favorite videos or creators"));
    app.online_video.api = None;
    app.online_video.init = Some(Job::spawn(|_| std::future::pending()));
    app.open_bilibili_creator(creator);
    assert_eq!(app.workspace.video_tab, 1);
    assert!(app.online_video.pending_creator.is_some());
    app.navigate(workspace::Page::Music);
    assert!(app.online_video.pending_creator.is_none());
    assert!(app.online_video.uploads_job.is_none());
}
#[test]
fn video_search_uses_page_tabs_and_a_type_selector_left_of_the_query() {
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.language = Language::English;
    app.workspace.page = workspace::Page::Video;
    app.workspace.video_tab = 1;
    app.settings_open = false;
    app.online_video.api = Some(Arc::new(Bilibili::new(None).unwrap()));
    let mut output = egui::FullOutput::default();
    for _ in 0..3 {
        output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 760.0))),
                ..Default::default()
            },
            |ui| app.workspace_ui(ui),
        );
        output.textures_delta.clear();
    }
    let text_pos = |label: &str| {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            Shape::Text(text) if text.galley.text() == label => Some(text.pos),
            _ => None,
        })
    };
    assert!(text_pos("Favorites").is_some());
    assert!(text_pos("Search").is_some());
    let kind = text_pos("All").expect("search type selector");
    let query = text_pos("Keywords, BV ID or video URL").expect("query field");
    let source = text_pos("Bilibili").expect("video source selector");
    assert!(
        source.x < kind.x
            && kind.x < query.x
            && (source.y - kind.y).abs() < 16.0
            && (kind.y - query.y).abs() < 16.0
    );
    assert!(
        ctx.memory(|m| m.area_rect(Id::new("online-video")))
            .is_none()
    );
    let point = kind + vec2(8.0, 8.0);
    for pressed in [true, false] {
        output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 760.0))),
                events: vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                ..Default::default()
            },
            |ui| app.workspace_ui(ui),
        );
        output.textures_delta.clear();
    }
    for _ in 0..20 {
        output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 760.0))),
                ..Default::default()
            },
            |ui| app.workspace_ui(ui),
        );
        output.textures_delta.clear();
    }
    for label in ["All", "Videos", "Creators"] {
        assert!(
            output
                .shapes
                .iter()
                .any(|shape| matches!(&shape.shape,Shape::Text(text) if text.galley.text()==label)),
            "Missing search kind: {label}"
        );
    }
}
#[test]
fn account_controls_live_in_video_source_settings_not_search() {
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.language = Language::English;
    app.preferences_tab = PrefTab::Video;
    app.online_video.api = Some(Arc::new(Bilibili::new(None).unwrap()));
    let render = |app: &mut App, settings: bool| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 760.0))),
                ..Default::default()
            },
            |ui| {
                if settings {
                    app.preferences_page(ui);
                } else {
                    app.online_video_contents(ui);
                }
            },
        );
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    app.online_video.init = Some(Job::spawn(|_| std::future::pending()));
    let pending = render(&mut app, true);
    assert!(pending.iter().any(|s| s == "Checking sign-in status"));
    assert!(!pending.iter().any(|s| s == "Not signed in"));
    app.online_video.init = None;
    render(&mut app, true);
    let settings = render(&mut app, true);
    assert!(settings.iter().any(|s| s == "Not signed in"));
    assert!(!settings.iter().any(|s| s == "Checking sign-in status"));
    assert!(settings.iter().any(|s| s == "Sign in with QR"));
    assert!(settings.iter().any(|s| s == "Bilibili"));
    assert!(
        settings.iter().position(|s| s == "Video sources").unwrap()
            < settings.iter().position(|s| s == "AI subtitles").unwrap()
    );
    assert!(settings.iter().any(|s| s == "Video"));
    for label in ["Auto-skip intro", "Auto-skip outro"] {
        let position = settings.iter().position(|s| s == label).expect(label);
        assert!(position > settings.iter().position(|s| s == "Bilibili").unwrap());
        assert!(position < settings.iter().position(|s| s == "AI subtitles").unwrap());
    }
    assert!(
        app.workspace.page != workspace::Page::Video,
        "Viewing account settings must not navigate to video search"
    );
    let search = render(&mut app, false);
    assert!(search.iter().any(|s| s == "Bilibili"));
    assert!(search.iter().any(|s| s == "Keywords, BV ID or video URL"));
    assert!(
        !search
            .iter()
            .any(|s| s == "Video source settings" || s == "Bilibili website")
    );
    assert!(
        !search
            .iter()
            .any(|s| s == "Sign in with QR" || s == "Sign out" || s == "Not signed in")
    );
}
#[test]
fn leaving_discovery_cancels_prepared_playback_and_qr_polling() {
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.online_video.auto_play = true;
    app.online_video.prepare = Some(Job::spawn(|_| std::future::pending()));
    app.online_video.poll = Some(Job::spawn(|_| std::future::pending()));
    app.online_video.cancel_discovery();
    app.poll_online_video(&ctx);
    assert!(app.player.is_none());
    assert!(app.online_video.prepare.is_none());
    assert!(app.online_video.poll.is_none());
    assert!(!app.online_video.auto_play);
}
#[test]
#[ignore = "Requires a GPU; renders Bilibili UI without account credentials or network"]
fn bilibili_visuals() {
    let directory = std::path::Path::new("target/ui-review/bilibili");
    std::fs::create_dir_all(directory).unwrap();
    let mut capture = comic::visual_tests::Capture::new();
    for (language, name) in [(Language::Chinese, "zh"), (Language::English, "en")] {
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        app.settings.language = language;
        app.online_video.api = Some(Arc::new(Bilibili::new(None).unwrap()));
        app.settings_open = true;
        app.preferences_tab = PrefTab::Video;
        let qr=QrCode {url:"https://account.bilibili.com/h5/account-h5/auth/scan-web?qrcode_key=offline-layout-fixture".into(),qrcode_key:"offline-layout-fixture".into()};
        let image = qrcodegen::QrCode::encode_text(&qr.url, qrcodegen::QrCodeEcc::Medium).unwrap();
        app.online_video.qr = Some((qr, image, Instant::now()));
        capture.save(
            &ctx,
            [1100, 760],
            &directory.join(format!("qr-{name}.png")),
            |ui| app.preferences_page(ui),
        );
        app.settings_open = false;
        app.workspace.page = workspace::Page::Video;
        app.workspace.video_tab = 1;
        app.online_video.qr = None;
        let creator_names = ["音乐现场", "硬核技术社", "影像收藏家"];
        for (index, title) in creator_names.iter().enumerate() {
            let url = format!("https://i0.hdslb.com/bfs/face/preview-avatar-{index}.png");
            app.online_video
                .previews
                .insert_fixture(&url, artwork_fixture(&ctx, index, true));
            app.online_video.uploaders.push(Uploader {
                mid: 123 + index as u64,
                uname: (*title).into(),
                usign: [
                    "把喜欢的音乐，分享给每个认真生活的人。",
                    "用有趣的方式，把复杂技术讲清楚。",
                    "记录值得重看的片段。",
                ][index]
                    .into(),
                fans: [128600, 432000, 85200][index],
                videos: [85, 126, 248][index],
                avatar: Some(url),
            });
        }
        let titles = [
            "Never Gonna Give You Up · 经典音乐现场",
            "从零开始学 Rust：所有权、借用与错误处理",
            "【初音未来】把夏天写成一首歌",
            "经典舞台回顾：那些让我们反复重播的瞬间",
            "Rust 实战｜从一个小项目开始理解异步编程",
            "音乐与影像：给平凡的一天加一点灵感",
        ];
        for (index, title) in titles.iter().enumerate() {
            let url = format!("https://i0.hdslb.com/bfs/archive/preview-cover-{index}.jpg");
            app.online_video
                .previews
                .insert_fixture(&url, artwork_fixture(&ctx, index, false));
            app.online_video.videos.push(bilibili::Video {
                bvid: "BV1GJ411x7h7".into(),
                title: (*title).into(),
                uploader: creator_names[index % 3].into(),
                duration: ["03:32", "28:16", "04:08"][index % 3].into(),
                cover: Some(url),
                views: Some([128600, 34500, 862000][index % 3]),
                published: None,
            });
        }
        for width in [900, 1280] {
            capture.save(
                &ctx,
                [width, 900],
                &directory.join(format!("search-{name}-{width}.png")),
                |ui| app.workspace_ui(ui),
            );
        }
        capture.save(
            &ctx,
            [900, 740],
            &directory.join(format!("search-{name}.png")),
            |ui| app.workspace_ui(ui),
        );
        app.online_video.search = Some(Job::spawn(|_| std::future::pending()));
        capture.save(
            &ctx,
            [900, 740],
            &directory.join(format!("search-loading-{name}.png")),
            |ui| app.workspace_ui(ui),
        );
        app.online_video.search = None;
        app.online_video.searched_kind = SearchKind::Uploader;
        app.online_video.kind = SearchKind::Uploader;
        app.online_video.videos.clear();
        capture.save(
            &ctx,
            [900, 740],
            &directory.join(format!("creators-{name}.png")),
            |ui| app.workspace_ui(ui),
        );
        app.online_video.detail = Some(Detail {
            bvid: "BV1GJ411x7h7".into(),
            title: "Bilibili video · 视频详情".into(),
            cover: None,
            parts: vec![bilibili::Part {
                cid: 1,
                page: 1,
                part: "Part 1 · 第一集".into(),
                duration: 212,
            }],
        });
        app.online_video.ready = Some(Playback {
            bvid: "BV1GJ411x7h7".into(),
            cid: 1,
            quality: 80,
            duration: 212.0,
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
        });
        capture.save(
            &ctx,
            [900, 740],
            &directory.join(format!("detail-{name}.png")),
            |ui| app.workspace_ui(ui),
        );
        assert!(
            ctx.memory(|m| m.area_rect(Id::new("online-video")))
                .is_none(),
            "Video search must be inline, without a separate window"
        );
    }
}

fn artwork_fixture(ctx: &egui::Context, index: usize, avatar: bool) -> egui::TextureHandle {
    let name = if avatar {
        "avatar.jpg".to_owned()
    } else {
        format!("cover-{}.jpg", index % 3 + 1)
    };
    let image =
        std::fs::read(std::path::Path::new("target/ui-review/bilibili/fixtures").join(name))
            .ok()
            .and_then(|bytes| crate::pixels::thumbnail(&bytes, 512, 288).ok())
            .unwrap_or_else(|| {
                crate::pixels::generated(320, 180, |x, y| {
                    [35 + (x / 4) as u8, 55 + (y / 3) as u8, 90, 255]
                })
            });
    ctx.load_texture(
        format!("fixture-cover-{index}-{avatar}"),
        egui::ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, image.height() as usize],
            image.bytes(),
        ),
        egui::TextureOptions::LINEAR,
    )
}
