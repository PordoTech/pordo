use super::*;
fn directory() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn song(id: &str) -> Record {
    let online = crate::music_sources::OnlineSong {
        source: "unavailable-test-source".into(),
        id: id.into(),
        title: format!("Song {id}"),
        ..Default::default()
    };
    Record {
        path: format!("online://{}", online.key()),
        title: online.title.clone(),
        artist: "Artist".into(),
        online: Some(online),
        music: true,
        ..Default::default()
    }
}
#[test]
fn playlists_persist_identity_order_metadata_and_independent_membership() {
    let directory = directory();
    let path = directory.join("playlists.json");
    let mut store = Store::open(path.clone());
    let first = store.named(None, " Commute ", &[]).unwrap();
    let second = store.named(None, "Night", &[]).unwrap();
    assert!(store.named(None, "Commute", &[]).is_err());
    assert!(store.named(None, "   ", &[]).is_err());
    assert_eq!(
        store
            .add(&first, &[song("a"), song("b"), song("c")])
            .unwrap(),
        3
    );
    let mut download = song("a");
    download.path = "download.flac".into();
    assert_eq!(store.add(&first, &[download]).unwrap(), 0);
    store.add(&second, &[song("a")]).unwrap();
    store.relocate(&first, 0, 3).unwrap();
    store.named(Some(&first), "Daily", &[]).unwrap();
    let mut reopened = Store::open(path);
    let p = reopened
        .data
        .playlists
        .iter()
        .find(|p| p.id == first)
        .unwrap();
    assert_eq!(p.name, "Daily");
    assert_eq!(
        p.tracks
            .iter()
            .map(|r| r.title.as_str())
            .collect::<Vec<_>>(),
        ["Song b", "Song c", "Song a"]
    );
    assert!(p.tracks[0].online.is_some());
    reopened
        .edit(&first, |p| {
            p.tracks.clear();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        reopened
            .data
            .playlists
            .iter()
            .find(|p| p.id == second)
            .unwrap()
            .tracks
            .len(),
        1
    );
    reopened
        .commit(|d| {
            d.playlists.retain(|p| p.id != first);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        Store::open(directory.join("playlists.json"))
            .data
            .playlists
            .len(),
        1
    );
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn failed_writes_and_unreadable_formats_preserve_original_data() {
    let directory = directory();
    let mut store = Store::open(directory.join("data.json"));
    store.named(None, "Original", &[]).unwrap();
    // A directory cannot be atomically replaced with the JSON file.
    store.path = directory.clone();
    assert!(store.named(None, "Rejected", &[song("a")]).is_err());
    assert_eq!(store.data.playlists.len(), 1);
    let newer = directory.join("newer.json");
    let bytes = br#"{"version":99,"playlists":[]}"#;
    std::fs::write(&newer, bytes).unwrap();
    let mut store = Store::open(newer.clone());
    assert!(store.named(None, "Rejected", &[]).is_err());
    assert_eq!(std::fs::read(newer).unwrap(), bytes);
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn playlist_playback_starts_in_middle_and_editing_does_not_change_queue() {
    let directory = directory();
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.library = library::Library::open(directory.join("library.json"));
    app.playlists = State::open(directory.join("playlists.json"));
    let id = app
        .playlists
        .store
        .named(None, "Test", &[song("a"), song("b"), song("c")])
        .unwrap();
    app.queue = Default::default();
    app.queue_kind_music = false;
    app.queue
        .items
        .push(crate::queue::Item::file("video.mp4".into()));
    app.play_playlist(&id, 1);
    assert_eq!(app.queue.items.len(), 3);
    assert_eq!(app.queue.current, Some(1));
    assert_eq!(app.online_origin.as_ref().unwrap().id, "b");
    assert_eq!(app.queue_inactive.items[0].title, "video.mp4");
    app.playlists
        .store
        .edit(&id, |p| {
            p.tracks.remove(1);
            Ok(())
        })
        .unwrap();
    assert_eq!(app.queue.items.len(), 3);
    assert_eq!(app.queue.current, Some(1));
    // Metadata survives history/favorite deletion; refreshes a known download.
    app.library
        .bind_online("download.flac".into(), song("a").online.unwrap());
    assert_eq!(app.playlist_records(&id)[0].path, "download.flac");
    app.library.records.clear();
    assert!(app.playlist_records(&id)[0].online.is_some());
    app.online_music.cancel_playback();
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
#[ignore = "Requires a GPU; renders playlist layouts"]
fn playlists_visuals() {
    let directory = PathBuf::from("target/ui-review");
    std::fs::create_dir_all(&directory).unwrap();
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.playlists = State::open(directory.join("fixture-playlists.json"));
    app.playlists.store.data = Data::default();
    let id = app
        .playlists
        .store
        .named(None, "通勤 · Commute", &[song("a"), song("b"), song("c")])
        .unwrap();
    app.playlists.store.named(None, "夜间", &[]).unwrap();
    app.library = library::Library::open(directory.join("fixture-library.json"));
    app.library.records = vec![song("a"), song("b"), song("d")];
    for record in &mut app.library.records {
        record.favorite = true;
    }
    let mut capture = comic::visual_tests::Capture::new();
    app.workspace.page = workspace::Page::Music;
    app.workspace.music_tab = 1;
    for width in [900, 354] {
        app.playlists.selected = None;
        capture.save(
            &ctx,
            [width, 650],
            &directory.join(format!("playlists-{width}.png")),
            |ui| app.playlists_page(ui),
        );
        app.playlists.selected = Some(id.clone());
        capture.save(
            &ctx,
            [width, 650],
            &directory.join(format!("playlist-detail-{width}.png")),
            |ui| app.playlists_page(ui),
        );
        app.playlists.show_dialog(Dialog::Add {
            id: id.clone(),
            query: String::new(),
            selected: HashSet::new(),
        });
        capture.save(
            &ctx,
            [width, 650],
            &directory.join(format!("playlist-add-{width}.png")),
            |ui| app.playlist_dialog(ui.ctx()),
        );
        app.playlists.dialog = None;
    }
    app.playlists.selected = Some(id.clone());
    app.queue.items = vec![crate::queue::Item::online(song("a").online.unwrap())];
    app.queue.current = Some(0);
    app.queue_open = true;
    capture.save(
        &ctx,
        [1100, 740],
        &directory.join("playlist-workspace-queue.png"),
        |ui| app.workspace_ui(ui),
    );
}

fn dialog_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(700.0, 600.0))),
            events,
            ..Default::default()
        },
        |ui| app.playlist_dialog(ui.ctx()),
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
        .unwrap_or_else(|| panic!("Missing text: {text}"))
}
fn click_dialog(app: &mut App, ctx: &egui::Context, position: egui::Pos2) -> egui::FullOutput {
    dialog_frame(
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
    dialog_frame(
        app,
        ctx,
        vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    )
}
#[test]
fn dialogs_add_existing_create_with_song_and_batch_select_favorites() {
    let directory = directory();
    let ctx = egui::Context::default();
    let mut app = comic::visual_tests::settings_fixture(&ctx);
    app.settings.language = Language::English;
    app.library = library::Library::open(directory.join("library.json"));
    app.queue = Default::default();
    app.queue_inactive = Default::default();
    app.playlists = State::open(directory.join("playlists.json"));
    let id = app.playlists.store.named(None, "Test", &[]).unwrap();
    app.playlists.show_dialog(Dialog::Choose {
        record: Box::new(song("a")),
    });
    dialog_frame(&mut app, &ctx, vec![]);
    let output = dialog_frame(&mut app, &ctx, vec![]);
    click_dialog(&mut app, &ctx, text_position(&output, "Test"));
    assert!(!app.playlists.dialog_open());
    assert_eq!(app.playlist_records(&id).len(), 1);
    app.playlists.show_dialog(Dialog::Name {
        id: None,
        name: "New".into(),
        pending: vec![song("b")],
    });
    dialog_frame(&mut app, &ctx, vec![]);
    let output = dialog_frame(&mut app, &ctx, vec![]);
    click_dialog(&mut app, &ctx, text_position(&output, "Save"));
    assert!(!app.playlists.dialog_open());
    assert_eq!(
        Store::open(directory.join("playlists.json"))
            .data
            .playlists
            .iter()
            .find(|p| p.name == "New")
            .unwrap()
            .tracks
            .len(),
        1
    );
    app.library.records = vec![song("a"), song("b")];
    for record in &mut app.library.records {
        record.favorite = true;
    }
    app.playlists.show_dialog(Dialog::Add {
        id: id.clone(),
        query: String::new(),
        selected: HashSet::new(),
    });
    dialog_frame(&mut app, &ctx, vec![]);
    let output = dialog_frame(&mut app, &ctx, vec![]);
    click_dialog(&mut app, &ctx, text_position(&output, "Song b · Artist"));
    let output = dialog_frame(&mut app, &ctx, vec![]);
    click_dialog(&mut app, &ctx, text_position(&output, "Add 1 songs"));
    assert!(!app.playlists.dialog_open());
    assert_eq!(app.playlist_records(&id).len(), 2);
    std::fs::remove_dir_all(directory).unwrap();
}
