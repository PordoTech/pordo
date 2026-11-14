use super::*;
fn creator(mid: u64) -> library::CreatorRecord {
    library::CreatorRecord {
        uploader: Uploader {
            mid,
            uname: format!("UP 主 {mid}"),
            usign: String::new(),
            fans: 0,
            videos: 0,
            avatar: None,
        },
        favorite: true,
        favorited_at: 42,
    }
}
fn video(id: u64, at: u64) -> bilibili::Video {
    bilibili::Video {
        bvid: format!("BV{id:010}"),
        title: format!("新投稿 {id} · 视频更新"),
        uploader: "UP".into(),
        duration: "08:20".into(),
        cover: None,
        views: None,
        published: Some(at),
    }
}
fn page(videos: Vec<bilibili::Video>, has_more: bool) -> SearchPage {
    SearchPage {
        videos,
        has_more,
        uploaders: vec![],
    }
}
fn baseline(state: &mut State) {
    state.enqueue(&[creator(1)], true, None, true);
    state.queue.clear();
    state.page("1/42", page(vec![video(1, 100), video(2, 90)], true));
}
fn snapshot(state: &State) -> Snapshot {
    let mut data = Snapshot::new();
    state.snapshot(&mut data);
    data
}

#[test]
fn video_updates_baseline_deduplicates_and_keeps_historical_uploads_seen() {
    let mut state = State::default();
    baseline(&mut state);
    assert_eq!(state.counts(&[creator(1)]), (0, 0));
    state.enqueue(&[creator(1)], true, None, true);
    state.queue.clear();
    state.page(
        "1/42",
        page(
            vec![video(3, 200), video(1, 100), video(2, 90), video(4, 80)],
            false,
        ),
    );
    assert_eq!(state.counts(&[creator(1)]), (1, 1));
    state.page("1/42", page(vec![video(3, 200), video(3, 200)], false));
    assert_eq!(state.content["1/42"].unread()[0].bvid, video(3, 200).bvid);
    let mut other = State::default();
    other.import(&snapshot(&state)).unwrap();
    assert_eq!(other.counts(&[creator(1)]), (1, 1));
    assert!(other.local.is_empty(), "transport state is not synced");
}
#[test]
fn video_updates_scan_whole_pages_continue_after_budget_and_ignore_cancelled_epochs() {
    let mut state = State::default();
    baseline(&mut state);
    state.enqueue(&[creator(1)], true, None, true);
    state.queue.clear();
    for index in 0..5 {
        // A known pinned item cannot hide a newer unseen item in this page.
        state.page(
            "1/42",
            page(vec![video(1, 100), video(10 + index, 200 + index)], true),
        );
        state.queue.clear();
    }
    assert_eq!(
        state.content["1/42"].boundary, 100,
        "partial checks do not advance the boundary"
    );
    assert!(state.local["1/42"].paused);
    assert_eq!(state.local["1/42"].next_page, 6);
    state.enqueue(&[creator(1)], true, Some("1/42"), true);
    state.queue.clear();
    state.page("1/42", page(vec![video(2, 90)], true));
    assert!(!state.local["1/42"].paused);
    assert_eq!(state.local["1/42"].next_page, 0);
    assert_eq!(state.content["1/42"].boundary, 204);
    let mut replacement = creator(1);
    replacement.favorited_at = 99;
    state.reconcile(&[replacement]);
    state.page("1/42", page(vec![video(99, 300)], false));
    assert!(
        !state.content["1/42"]
            .posts
            .contains_key(&video(99, 300).bvid)
    );
}
#[test]
fn video_updates_manual_checks_bypass_backoff_and_wait_for_initial_cloud_pull() {
    let mut state = State::default();
    state.enqueue(&[creator(1)], false, None, false);
    assert!(!state.running());
    baseline(&mut state);
    state.local.get_mut("1/42").unwrap().error = Some("offline".into());
    state.enqueue(&[creator(1)], false, None, true);
    assert!(!state.running());
    state.enqueue(&[creator(1)], true, None, true);
    assert_eq!(state.queue.len(), 1);
    state.enqueue(&[creator(1)], true, None, true);
    assert_eq!(state.queue.len(), 1);
    state.reconcile(&[]);
    assert!(!state.running());
}
#[test]
fn video_updates_seen_undo_preserves_subsequent_uploads_and_actual_playback() {
    let mut state = State::default();
    baseline(&mut state);
    state.page("1/42", page(vec![video(3, 200), video(4, 201)], false));
    state.mark_read("1/42", vec![video(3, 200).bvid, video(4, 201).bvid.clone()]);
    state.mark_played(&video(4, 201).bvid);
    state.page("1/42", page(vec![video(5, 202)], false));
    state.undo();
    let ids: Vec<_> = state.content["1/42"]
        .unread()
        .iter()
        .map(|p| p.bvid.clone())
        .collect();
    assert_eq!(ids, [video(5, 202).bvid, video(3, 200).bvid]);
    // Do not export unrelated video history as creator-update data.
    assert!(!state.mark_played(&video(6, 203).bvid));
    assert!(!state.watched.contains(&video(6, 203).bvid));
}
#[test]
fn video_updates_two_devices_merge_new_uploads_seen_markers_and_undo() {
    let mut first = State::default();
    baseline(&mut first);
    first.page("1/42", page(vec![video(3, 200)], false));
    let mut a = crate::sync::Journal::default();
    a.document.version = 1;
    a.document.device = "A".into();
    a.capture(snapshot(&first), &Snapshot::new());
    let mut b = crate::sync::Journal::default();
    b.document.version = 1;
    b.document.device = "B".into();
    b.merge(&a.document).unwrap();
    b.observed = b.values();
    let mut second = State::default();
    second.import(&b.values()).unwrap();
    second.reconcile(&[creator(1)]);
    first.page("1/42", page(vec![video(4, 201)], false));
    second.mark_read("1/42", vec![video(3, 200).bvid]);
    a.capture(snapshot(&first), &Snapshot::new());
    b.capture(snapshot(&second), &Snapshot::new());
    a.merge(&b.document).unwrap();
    b.merge(&a.document).unwrap();
    first.import(&a.values()).unwrap();
    second.import(&b.values()).unwrap();
    assert_eq!(first.counts(&[creator(1)]), (1, 1));
    assert_eq!(snapshot(&first), snapshot(&second));
    // Concurrent playback wins over undoing a manual read marker.
    second.undo();
    first.mark_played(&video(3, 200).bvid);
    a.observed = a.values();
    b.observed = b.values();
    a.capture(snapshot(&first), &Snapshot::new());
    b.capture(snapshot(&second), &Snapshot::new());
    a.merge(&b.document).unwrap();
    second.import(&a.values()).unwrap();
    assert_eq!(second.content["1/42"].unread()[0].bvid, video(4, 201).bvid);
    assert_eq!(second.content["1/42"].unread().len(), 1);
}
#[test]
fn video_updates_invalid_cloud_data_does_not_replace_existing_content() {
    let mut state = State::default();
    baseline(&mut state);
    let before = snapshot(&state);
    let mut invalid = before.clone();
    invalid.insert(
        "video-updates/bilibili/1/42/post/BV0000000008".into(),
        json!({"bvid":"BV0000000009","title":"Bad","duration":"1","cover":null,"published":100}),
    );
    assert!(state.import(&invalid).is_err());
    assert_eq!(snapshot(&state), before);
}

#[test]
#[ignore = "Requires a GPU; renders grouped update UI without network"]
fn video_updates_visuals() {
    let directory = std::path::Path::new("target/ui-review/video-updates");
    std::fs::create_dir_all(directory).unwrap();
    let mut capture = comic::visual_tests::Capture::new();
    for (language, name) in [(Language::Chinese, "zh"), (Language::English, "en")] {
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        app.settings.language = language;
        app.settings.video.check_updates = false;
        app.library = library::Library::open(directory.join("fixture-library.json"));
        app.library.creators = vec![creator(1), creator(2), creator(3)];
        app.online_video.updates = State::default();
        baseline(&mut app.online_video.updates);
        app.online_video.updates.reconcile(&app.library.creators);
        app.online_video.updates.page(
            "1/42",
            page(
                (3..11)
                    .map(|id| video(id, crate::comics::now() - id * 3600))
                    .collect(),
                false,
            ),
        );
        app.online_video.updates.content.insert(
            "2/42".into(),
            Content {
                baseline: Some(100),
                ..Default::default()
            },
        );
        app.online_video.updates.local.insert(
            "2/42".into(),
            Local {
                error: Some("检查失败，请重试 / Check failed; retry".into()),
                ..Default::default()
            },
        );
        app.workspace.page = workspace::Page::Video;
        app.workspace.video_tab = 2;
        for width in [1100, 780] {
            capture.save(
                &ctx,
                [width, 760],
                &directory.join(format!("workspace-{name}-{width}.png")),
                |ui| app.workspace_ui(ui),
            );
        }
        capture.save(
            &ctx,
            [354, 760],
            &directory.join(format!("narrow-{name}.png")),
            |ui| app.video_updates_page(ui),
        );
        app.queue_open = true;
        app.queue_kind_music = false;
        app.queue.items = vec![crate::queue::Item::file("preview.mp4".into())];
        capture.save(
            &ctx,
            [1100, 760],
            &directory.join(format!("queue-{name}.png")),
            |ui| app.workspace_ui(ui),
        );
    }
}

fn ui_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 760.0))),
            events,
            ..Default::default()
        },
        |ui| app.video_updates_page(ui),
    );
    output.textures_delta.clear();
    output
}
fn text_position(output: &egui::FullOutput, text: &str) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            Shape::Text(t) if t.galley.text() == text => Some(t.pos + t.galley.size() * 0.5),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing UI text: {text}"))
}
fn click(app: &mut App, ctx: &egui::Context, position: egui::Pos2) {
    ui_frame(
        app,
        ctx,
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    ui_frame(
        app,
        ctx,
        vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    );
}
#[test]
fn video_updates_group_mark_seen_and_undo_are_clickable_and_keep_all_posts() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&directory).unwrap();
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.language = Language::English;
    app.library = library::Library::open(directory.join("library.json"));
    app.library.creators = vec![creator(1)];
    app.online_video.updates = State::default();
    baseline(&mut app.online_video.updates);
    app.online_video.updates.page(
        "1/42",
        page((3..11).map(|id| video(id, 200 + id)).collect(), false),
    );
    ui_frame(&mut app, &ctx, vec![]);
    let output = ui_frame(&mut app, &ctx, vec![]);
    let button = text_position(&output, "Mark all as seen");
    click(&mut app, &ctx, button);
    assert_eq!(
        app.online_video.updates.counts(&app.library.creators),
        (0, 0)
    );
    ui_frame(&mut app, &ctx, vec![]);
    let output = ui_frame(&mut app, &ctx, vec![]);
    click(&mut app, &ctx, text_position(&output, "Undo mark as seen"));
    assert_eq!(
        app.online_video.updates.counts(&app.library.creators),
        (1, 8)
    );
    drop(app);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn video_updates_discovered_posts_use_existing_history_without_exporting_unrelated_views() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&directory).unwrap();
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.video.check_updates = false;
    app.library = library::Library::open(directory.join("library.json"));
    app.library.creators = vec![creator(1)];
    app.library.records = vec![
        library::Record {
            path: "https://www.bilibili.com/video/BV0000000003?p=1".into(),
            position: 10.0,
            played: 10,
            ..Default::default()
        },
        library::Record {
            path: "https://www.bilibili.com/video/BV0000000099?p=1".into(),
            position: 10.0,
            played: 10,
            ..Default::default()
        },
    ];
    app.online_video.updates = State::default();
    baseline(&mut app.online_video.updates);
    app.online_video
        .updates
        .page("1/42", page(vec![video(3, 200)], false));
    app.poll_video_updates(&ctx);
    assert_eq!(
        app.online_video.updates.counts(&app.library.creators),
        (0, 0)
    );
    assert!(app.online_video.updates.watched.contains("BV0000000003"));
    assert!(!app.online_video.updates.watched.contains("BV0000000099"));
    // Browsing cancellations do not cancel background update jobs.
    app.online_video.updates.queue.push_back("1/42".into());
    app.online_video.cancel_discovery();
    assert!(app.online_video.updates.running());
    app.online_video.updates.reconcile(&[]);
    assert!(!app.online_video.updates.running());
    drop(app);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn video_updates_rate_limit_stops_the_batch_without_clearing_unseen_content() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&directory).unwrap();
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.video.check_updates = false;
    app.library = library::Library::open(directory.join("library.json"));
    app.library.creators = vec![creator(1), creator(2)];
    app.online_video.updates = State::default();
    baseline(&mut app.online_video.updates);
    app.online_video
        .updates
        .page("1/42", page(vec![video(3, 200)], false));
    app.online_video.updates.queue.push_back("2/42".into());
    app.online_video.updates.jobs.push(Request {
        key: "1/42".into(),
        page: 1,
        job: Job::spawn(|_| async { anyhow::bail!("HTTP 429 rate limit") }),
    });
    for _ in 0..100 {
        app.poll_video_updates(&ctx);
        if app.online_video.updates.stopped {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.online_video.updates.stopped);
    assert!(!app.online_video.updates.running());
    assert!(
        app.online_video.updates.local["1/42"]
            .error
            .as_ref()
            .unwrap()
            .contains("429")
    );
    assert_eq!(
        app.online_video.updates.counts(&app.library.creators),
        (1, 1)
    );
    drop(app);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn video_updates_tab_uses_red_dot_without_count_in_label() {
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.language = Language::English;
    app.library.creators = vec![creator(1)];
    app.online_video.updates = State::default();
    baseline(&mut app.online_video.updates);
    app.online_video
        .updates
        .page("1/42", page(vec![video(3, 200)], false));
    assert_eq!(
        app.online_video.updates.counts(&app.library.creators),
        (1, 1)
    );

    let render_tabs = |app: &mut App| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 200.0))),
                ..Default::default()
            },
            |ui| app.video_library_tabs(ui),
        );
        output.textures_delta.clear();
        output
    };

    let output = render_tabs(&mut app);
    assert!(output.shapes.iter().any(|s| match &s.shape {
        Shape::Text(t) => t.galley.text() == "Updates",
        _ => false,
    }));
    assert!(!output.shapes.iter().any(|s| match &s.shape {
        Shape::Text(t) => t.galley.text().contains("Updates ·"),
        _ => false,
    }));
    assert!(output.shapes.iter().any(|s| match &s.shape {
        Shape::Circle(c) => c.fill == design::BADGE,
        _ => false,
    }));

    app.online_video
        .updates
        .mark_read("1/42", vec!["BV0000000003".into()]);
    let output = render_tabs(&mut app);
    assert!(!output.shapes.iter().any(|s| match &s.shape {
        Shape::Circle(c) => c.fill == design::BADGE,
        _ => false,
    }));
}
