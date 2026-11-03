use super::model::Snapshot;
use super::*;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::{Arc, Mutex},
};

fn journal(device: &str) -> Journal {
    Journal {
        document: Document {
            version: 1,
            device: device.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn progress_is_deferred_until_checkpoint_and_inflight_edits_survive_a_remote_reply() {
    let baseline = values(&[
        ("comic/a/progress", json!(1)),
        ("comic/a/read/old", json!("old")),
        ("song/x/progress", json!(10)),
    ]);
    let local = values(&[
        ("comic/a/progress", json!(2)),
        ("comic/a/read/new", json!("new")),
        ("song/x/progress", json!(20)),
        ("song/new/progress", json!(5)),
        ("song/x/favorite", json!(true)),
    ]);
    let mut captured = local.clone();
    portable::defer_progress(&mut captured, &baseline);
    assert_eq!(captured["comic/a/progress"], json!(1));
    assert_eq!(captured["song/x/progress"], json!(10));
    assert!(captured.contains_key("comic/a/read/old"));
    assert!(!captured.contains_key("song/new/progress"));
    assert!(!captured.contains_key("comic/a/read/new"));
    assert_eq!(captured["song/x/favorite"], json!(true));
    let mut remote = baseline.clone();
    remote.insert("song/other/progress".into(), json!(99));
    portable::preserve_local_progress(&mut remote, &local, &baseline);
    assert_eq!(remote["comic/a/progress"], json!(2));
    assert_eq!(remote["song/new/progress"], json!(5));
    assert_eq!(remote["song/other/progress"], json!(99));
    assert!(!remote.contains_key("comic/a/read/old"));
    let mut j = journal("test");
    j.capture(captured, &Snapshot::new());
    j.capture(local, &Snapshot::new());
    assert_eq!(j.values()["song/x/progress"], json!(20));
}

#[tokio::test]
async fn desktop_and_mobile_exchange_comic_progress_without_erasing_unsupported_settings() {
    use crate::comics::{
        Book, Chapter,
        library::{Entry, Progress},
    };
    let server = Server::new().await;
    let desktop = server.settings();
    let mobile = server.settings();
    let mut book = Entry {
        book: Book {
            provider: "komiic".into(),
            site: "komiic".into(),
            url: "https://komiic.com/comic/1".into(),
            ..Default::default()
        },
        chapters: vec![Chapter {
            id: "one".into(),
            ..Default::default()
        }],
        favorite: true,
        favorited_at: 42,
        ..Default::default()
    };
    book.new_unread.insert("one".into());
    let mut desktop_journal = journal(&desktop.device_id);
    let mut data = values(&[
        ("ai/desktop/name", json!("Desktop provider")),
        ("subtitle/bilingual", json!(true)),
    ]);
    portable::capture_comics(&mut data, &[book.clone()], None);
    desktop_journal.capture(data, &Snapshot::new());
    desktop_journal = webdav::run(desktop.clone(), desktop_journal, BTreeMap::new())
        .await
        .unwrap()
        .journal;
    let mobile_journal = journal(&mobile.device_id);
    let report = webdav::run(mobile.clone(), mobile_journal.clone(), BTreeMap::new())
        .await
        .unwrap();
    let applied = portable::finish_mobile(
        mobile_journal,
        report,
        portable::mobile_defaults(),
        BTreeMap::new(),
        &BTreeMap::new(),
        &[],
    )
    .unwrap();
    assert!(applied.entries[0].new_unread.contains("one"));
    let mut entries = applied.entries;
    entries[0].mark_read("one");
    entries[0].progress = Some(Progress {
        chapter: "one".into(),
        page: 0,
        fraction: 0.75,
        page_count: 1,
    });
    entries[0].last_read = 123;
    let mut mobile_journal = applied.journal;
    let data = portable::mobile_snapshot(applied.mobile, &mobile_journal, &entries).unwrap();
    mobile_journal.capture(data, &portable::mobile_defaults());
    webdav::run(mobile, mobile_journal, BTreeMap::new())
        .await
        .unwrap();
    desktop_journal = webdav::run(desktop, desktop_journal, BTreeMap::new())
        .await
        .unwrap()
        .journal;
    let restored = portable::apply_comics(&desktop_journal.values(), &[book]).unwrap();
    assert!(restored[0].read.contains("one"));
    assert!(restored[0].new_unread.is_empty());
    assert_eq!(restored[0].progress.as_ref().unwrap().fraction, 0.75);
    assert_eq!(
        desktop_journal.values()["ai/desktop/name"],
        json!("Desktop provider")
    );
    assert_eq!(desktop_journal.values()["subtitle/bilingual"], json!(true));
}

#[test]
fn mobile_finish_preserves_inflight_edits_and_opaque_desktop_keys() {
    let song = crate::music_sources::OnlineSong {
        source: "chksz-163".into(),
        id: "7".into(),
        ..Default::default()
    };
    let prefix = format!("song/{}", song.key());
    let mut data = portable::mobile_defaults();
    data.insert(format!("{prefix}/song"), json!(song));
    data.insert(format!("{prefix}/favorite"), json!(true));
    let mut local = journal("mobile");
    local.capture(data.clone(), &portable::mobile_defaults());
    let mut remote = local.clone();
    remote.write("ai/desktop/name".into(), Some(json!("Keep me")));
    let report = webdav::Report {
        journal: remote,
        secrets: BTreeMap::from([
            ("music/chksz".into(), "old-key".into()),
            ("ai/desktop".into(), "opaque-key".into()),
        ]),
    };
    data.insert(format!("{prefix}/favorite"), json!(false));
    let applied = portable::finish_mobile(
        local,
        report,
        data,
        BTreeMap::from([("music/chksz".into(), "edited-while-syncing".into())]),
        &BTreeMap::from([("music/chksz".into(), "old-key".into())]),
        &[],
    )
    .unwrap();
    assert_eq!(applied.mobile[&format!("{prefix}/favorite")], json!(false));
    assert_eq!(applied.secrets["music/chksz"], "edited-while-syncing");
    assert_eq!(applied.secrets["ai/desktop"], "opaque-key");
    assert!(!applied.journal.observed.contains_key("ai/desktop/name"));
    assert_eq!(
        applied.journal.values()["ai/desktop/name"],
        json!("Keep me")
    );
    let encoded = serde_json::to_string(&applied.journal).unwrap();
    assert!(!encoded.contains("opaque-key") && !encoded.contains("edited-while-syncing"));
}
fn values(items: &[(&str, serde_json::Value)]) -> Snapshot {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn merge_preserves_independent_offline_changes_and_deletions_survive_stale_devices() {
    let initial = values(&[
        ("comic/x/favorite", json!(true)),
        ("ai/p/name", json!("Provider")),
    ]);
    let mut first = journal("first");
    first.capture(initial.clone(), &Snapshot::new());
    let mut second = journal("second");
    second.merge(&first.document).unwrap();
    second.observed = second.values();
    let mut left = initial.clone();
    left.insert("comic/x/favorite".into(), json!(false));
    first.capture(left, &Snapshot::new());
    let mut right = initial;
    right.insert("ai/p/name".into(), json!("Renamed"));
    second.capture(right, &Snapshot::new());
    first.merge(&second.document).unwrap();
    second.merge(&first.document).unwrap();
    assert_eq!(first.values(), second.values());
    assert_eq!(first.values()["comic/x/favorite"], json!(false));
    assert_eq!(first.values()["ai/p/name"], json!("Renamed"));
    let stale = journal("empty");
    first.merge(&stale.document).unwrap();
    assert_eq!(first.values()["comic/x/favorite"], json!(false));
    second.observed = second.values();
    let mut favorite_again = second.values();
    favorite_again.insert("comic/x/favorite".into(), json!(true));
    second.capture(favorite_again, &Snapshot::new());
    first.merge(&second.document).unwrap();
    assert_eq!(first.values()["comic/x/favorite"], json!(true));
    let mut deleted = first.values();
    deleted.remove("ai/p/name");
    first.observed = first.values();
    first.capture(deleted, &Snapshot::new());
    second.merge(&first.document).unwrap();
    assert!(!second.values().contains_key("ai/p/name"));
}

#[test]
fn new_defaults_do_not_overwrite_cloud_and_progress_can_move_backwards() {
    let defaults = values(&[("subtitle/bilingual", json!(false))]);
    let mut fresh = journal("fresh");
    assert!(!fresh.capture(defaults.clone(), &defaults));
    let mut old = journal("old");
    old.capture(
        values(&[
            ("subtitle/bilingual", json!(true)),
            ("comic/x/progress", json!(99)),
        ]),
        &Snapshot::new(),
    );
    fresh.merge(&old.document).unwrap();
    assert_eq!(fresh.values()["subtitle/bilingual"], json!(true));
    fresh.observed = fresh.values();
    let mut reread = fresh.values();
    reread.insert("comic/x/progress".into(), json!(5));
    fresh.capture(reread, &defaults);
    old.merge(&fresh.document).unwrap();
    assert_eq!(old.values()["comic/x/progress"], json!(5));
}

#[test]
fn concurrent_cancel_wins_but_a_later_explicit_refavorite_is_allowed() {
    let mut a = journal("a");
    a.capture(values(&[("favorite", json!(true))]), &Snapshot::new());
    let mut b = journal("b");
    b.merge(&a.document).unwrap();
    b.observed = b.values();
    a.capture(values(&[("favorite", json!(false))]), &Snapshot::new());
    b.capture(values(&[("favorite", json!(false))]), &Snapshot::new());
    b.capture(values(&[("favorite", json!(true))]), &Snapshot::new());
    let mut reverse = b.clone();
    reverse.merge(&a.document).unwrap();
    a.merge(&b.document).unwrap();
    assert_eq!(a.values(), reverse.values());
    assert_eq!(a.values()["favorite"], json!(false));
    b.merge(&a.document).unwrap();
    b.observed = b.values();
    b.capture(values(&[("favorite", json!(true))]), &Snapshot::new());
    a.merge(&b.document).unwrap();
    assert_eq!(a.values()["favorite"], json!(true));
}

#[test]
fn pending_changes_survive_restart_and_corrupt_journals_are_not_silently_replaced() {
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-data/sync")
        .join(uuid::Uuid::new_v4().to_string());
    let path = directory.join("sync.json");
    let mut state = Journal::load(&path, "device").unwrap();
    state.capture(
        values(&[("song/x/favorite", json!(false))]),
        &Snapshot::new(),
    );
    state.save(&path).unwrap();
    let restored = Journal::load(&path, "device").unwrap();
    assert_eq!(restored.values()["song/x/favorite"], json!(false));
    std::fs::write(&path, b"broken").unwrap();
    assert!(Journal::load(&path, "device").is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn encrypted_keys_are_authenticated_randomized_and_never_serialized_as_plaintext() {
    let phrase = "independent-sync-passphrase";
    let a = crypto::seal(phrase, "secret/ai/p", b"private-api-key").unwrap();
    let b = crypto::seal(phrase, "secret/ai/p", b"private-api-key").unwrap();
    assert_ne!(a.ciphertext, b.ciphertext);
    let json = serde_json::to_string(&a).unwrap();
    assert!(!json.contains("private-api-key") && !json.contains(phrase));
    assert_eq!(
        crypto::open(phrase, "secret/ai/p", &a).unwrap(),
        b"private-api-key"
    );
    assert!(crypto::open("incorrect-passphrase", "secret/ai/p", &a).is_err());
    assert!(crypto::open(phrase, "secret/ai/other", &a).is_err());
    let mut tampered = a;
    tampered.ciphertext.replace_range(..1, "!");
    assert!(crypto::open(phrase, "secret/ai/p", &tampered).is_err());
}

#[test]
fn webdav_listing_rejects_foreign_origins_and_ignores_traversal_or_non_device_files() {
    let directory = reqwest::Url::parse("https://example.com/dav/replayer/v1/devices/").unwrap();
    let id = uuid::Uuid::new_v4();
    let xml = format!(
        "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/dav/replayer/v1/devices/{id}.json</d:href></d:response><d:response><d:href>../../settings.json</d:href></d:response></d:multistatus>"
    );
    assert_eq!(
        webdav::list_paths(&xml, &directory).unwrap(),
        vec![format!("replayer/v1/devices/{id}.json")]
    );
    assert!(
        webdav::list_paths(
            "<x><href>https://evil.invalid/file.json</href></x>",
            &directory
        )
        .is_err()
    );
    assert!(webdav::list_paths("<invalid", &directory).is_err());
}

#[derive(Default)]
struct Objects {
    files: Mutex<BTreeMap<String, (Vec<u8>, usize)>>,
    writes: AtomicUsize,
    fail: AtomicBool,
    traffic_exhausted: AtomicBool,
    read_forbidden: AtomicBool,
    omit_listing_etags: AtomicBool,
    downloads: AtomicUsize,
    downloaded_bytes: AtomicUsize,
}
struct Server {
    url: String,
    objects: Arc<Objects>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new() -> Self {
        use axum::{
            Router,
            body::{Body, to_bytes},
            extract::State,
            http::{Request, Response, StatusCode},
        };
        async fn handle(
            State(objects): State<Arc<Objects>>,
            request: Request<Body>,
        ) -> Response<Body> {
            let method = request.method().as_str().to_owned();
            let path = request.uri().path().to_owned();
            let if_match = request
                .headers()
                .get("if-match")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let create = request.headers().get("if-none-match").is_some();
            let body = to_bytes(request.into_body(), 8 * 1024 * 1024)
                .await
                .unwrap();
            let mut files = objects.files.lock().unwrap();
            let response = |code: u16, body: Vec<u8>, etag: Option<String>| {
                let mut builder = Response::builder().status(StatusCode::from_u16(code).unwrap());
                if let Some(etag) = etag {
                    builder = builder.header("etag", etag);
                }
                builder.body(Body::from(body)).unwrap()
            };
            match method.as_str() {
                "MKCOL" => response(201, vec![], None),
                "PROPFIND" => {
                    let mut xml = String::from("<d:multistatus xmlns:d=\"DAV:\">");
                    for (key, (_, n)) in files.iter().filter(|(p, _)| p.starts_with(&path)) {
                        let status = if objects.omit_listing_etags.load(Ordering::Relaxed) { 404 } else { 200 };
                        xml.push_str(&format!("<d:response><d:href>{key}</d:href><d:propstat><d:prop><d:getetag>&quot;{n}&quot;</d:getetag></d:prop><d:status>HTTP/1.1 {status} Status</d:status></d:propstat></d:response>"));
                    }
                    xml.push_str("</d:multistatus>");
                    response(207, xml.into_bytes(), None)
                }
                "GET" => match files.get(&path) {
                    Some((body, _))
                        if body.len() > 2
                            && objects.traffic_exhausted.load(Ordering::Relaxed) =>
                    {
                        response(403, br#"<?xml version="1.0"?><d:error xmlns:d="DAV:" xmlns:s="http://ns.jianguoyun.com"><s:exception>TrafficRateExhausted</s:exception><s:message>The downstream traffic rate is exhausted. The free rate is 167 while you want to consume 246501. private-account-detail</s:message></d:error>"#.to_vec(), None)
                    }
                    Some(_) if objects.read_forbidden.load(Ordering::Relaxed) => {
                        response(403, b"<html>private-account-detail: permission denied</html>".to_vec(), None)
                    }
                    Some((body, n)) => {
                        objects.downloads.fetch_add(1, Ordering::Relaxed);
                        objects.downloaded_bytes.fetch_add(body.len(), Ordering::Relaxed);
                        response(200, body.clone(), Some(format!("\"{n}\"")))
                    }
                    None => response(404, vec![], None),
                },
                "PUT" => {
                    if objects.fail.load(Ordering::Relaxed) {
                        return response(503, vec![], None);
                    }
                    if create && files.contains_key(&path)
                        || if_match.is_some_and(|tag| {
                            files
                                .get(&path)
                                .is_none_or(|(_, n)| tag != format!("\"{n}\""))
                        })
                    {
                        return response(412, vec![], None);
                    }
                    let n = objects.writes.fetch_add(1, Ordering::Relaxed) + 1;
                    files.insert(path, (body.to_vec(), n));
                    response(201, vec![], Some(format!("\"{n}\"")))
                }
                "DELETE" => {
                    files.remove(&path);
                    response(204, vec![], None)
                }
                _ => response(405, vec![], None),
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/dav", listener.local_addr().unwrap());
        let objects = Arc::new(Objects::default());
        let router = Router::new().fallback(handle).with_state(objects.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { url, objects, task }
    }
    fn settings(&self) -> Settings {
        Settings {
            enabled: true,
            base_url: self.url.clone(),
            username: "user".into(),
            password: "webdav-password".into(),
            ..Default::default()
        }
    }
}

#[tokio::test]
async fn two_devices_exchange_metadata_skip_unchanged_uploads_and_keep_offline_deletions() {
    let server = Server::new().await;
    let a = server.settings();
    let b = server.settings();
    let mut ja = journal(&a.device_id);
    ja.capture(
        values(&[
            ("comic/x/favorite", json!(true)),
            ("song/y/favorite", json!(true)),
        ]),
        &Snapshot::new(),
    );
    let report = webdav::run(a.clone(), ja.clone(), BTreeMap::new())
        .await
        .unwrap();
    ja = report.journal;
    let report = webdav::run(b.clone(), journal(&b.device_id), BTreeMap::new())
        .await
        .unwrap();
    let mut jb = report.journal;
    assert_eq!(jb.values()["song/y/favorite"], json!(true));
    let writes = server.objects.writes.load(Ordering::Relaxed);
    ja = webdav::run(a.clone(), ja, BTreeMap::new())
        .await
        .unwrap()
        .journal;
    assert_eq!(writes, server.objects.writes.load(Ordering::Relaxed));
    ja.observed = ja.values();
    let mut removed = ja.values();
    removed.insert("song/y/favorite".into(), json!(false));
    ja.capture(removed, &Snapshot::new());
    server.objects.fail.store(true, Ordering::Relaxed);
    assert!(
        webdav::run(a.clone(), ja.clone(), BTreeMap::new())
            .await
            .is_err()
    );
    assert_eq!(ja.values()["song/y/favorite"], json!(false));
    jb.observed = jb.values();
    let mut other = jb.values();
    other.insert("ai/p/name".into(), json!("Offline provider"));
    jb.capture(other, &Snapshot::new());
    server.objects.fail.store(false, Ordering::Relaxed);
    webdav::run(a.clone(), ja, BTreeMap::new()).await.unwrap();
    jb = webdav::run(b, jb, BTreeMap::new()).await.unwrap().journal;
    assert_eq!(jb.values()["song/y/favorite"], json!(false));
    let ja = webdav::run(a, journal("reconnected"), BTreeMap::new())
        .await
        .unwrap()
        .journal;
    assert_eq!(ja.values()["ai/p/name"], json!("Offline provider"));
}

#[tokio::test]
async fn encrypted_partition_survives_devices_without_keys_and_wrong_phrase_does_not_write() {
    let server = Server::new().await;
    let mut a = server.settings();
    a.sync_keys = true;
    a.passphrase = "independent-sync-password".into();
    let keys = BTreeMap::from([("ai/p".into(), "private-api-secret".into())]);
    let report = webdav::run(a.clone(), journal(&a.device_id), keys)
        .await
        .unwrap();
    assert_eq!(report.secrets["ai/p"], "private-api-secret");
    let b = server.settings();
    let mut jb = webdav::run(b.clone(), journal(&b.device_id), BTreeMap::new())
        .await
        .unwrap()
        .journal;
    assert!(jb.values().contains_key("secret/ai/p"));
    {
        let files = server.objects.files.lock().unwrap();
        for (bytes, _) in files.values() {
            assert!(!String::from_utf8_lossy(bytes).contains("private-api-secret"));
        }
    }
    let writes = server.objects.writes.load(Ordering::Relaxed);
    let mut wrong = b.clone();
    wrong.sync_keys = true;
    wrong.passphrase = "incorrect-passphrase".into();
    assert!(
        webdav::run(wrong, jb.clone(), BTreeMap::new())
            .await
            .is_err()
    );
    assert_eq!(writes, server.objects.writes.load(Ordering::Relaxed));
    let mut unlocked = b;
    unlocked.sync_keys = true;
    unlocked.passphrase = a.passphrase;
    jb.observed = jb.values();
    let report = webdav::run(unlocked, jb, BTreeMap::new()).await.unwrap();
    assert_eq!(report.secrets["ai/p"], "private-api-secret");
}

#[tokio::test]
async fn etags_skip_unchanged_downloads_across_restart_and_refresh_only_changed_devices() {
    let server = Server::new().await;
    let a = server.settings();
    let b = server.settings();
    let mut ja = journal(&a.device_id);
    ja.write("comic/x/favorite".into(), Some(json!(true)));
    ja = webdav::run(a.clone(), ja, BTreeMap::new())
        .await
        .unwrap()
        .journal;
    let jb = webdav::run(b.clone(), journal(&b.device_id), BTreeMap::new())
        .await
        .unwrap()
        .journal;
    assert_eq!(jb.remote_etags.len(), 2);
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-data/sync")
        .join(uuid::Uuid::new_v4().to_string());
    let path = directory.join("journal.json");
    jb.save(&path).unwrap();
    let jb = Journal::load(&path, &b.device_id).unwrap();
    server.objects.downloads.store(0, Ordering::Relaxed);
    server.objects.downloaded_bytes.store(0, Ordering::Relaxed);
    let writes = server.objects.writes.load(Ordering::Relaxed);
    let mut jb = webdav::run(b.clone(), jb, BTreeMap::new())
        .await
        .unwrap()
        .journal;
    assert_eq!(server.objects.downloads.load(Ordering::Relaxed), 0);
    assert_eq!(server.objects.downloaded_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(server.objects.writes.load(Ordering::Relaxed), writes);
    ja.write("comic/x/favorite".into(), Some(json!(false)));
    webdav::run(a.clone(), ja, BTreeMap::new()).await.unwrap();
    server.objects.downloads.store(0, Ordering::Relaxed);
    jb = webdav::run(b.clone(), jb, BTreeMap::new())
        .await
        .unwrap()
        .journal;
    assert_eq!(server.objects.downloads.load(Ordering::Relaxed), 1);
    assert_eq!(jb.values()["comic/x/favorite"], json!(false));
    server.objects.downloads.store(0, Ordering::Relaxed);
    jb = webdav::run(b.clone(), jb, BTreeMap::new())
        .await
        .unwrap()
        .journal;
    assert_eq!(server.objects.downloads.load(Ordering::Relaxed), 0);
    // If PROPFIND cannot return a usable ETag, download rather than trusting a
    // stale cached version. A failed getetag propstat must not count as success.
    server
        .objects
        .omit_listing_etags
        .store(true, Ordering::Relaxed);
    webdav::run(b, jb, BTreeMap::new()).await.unwrap();
    assert_eq!(server.objects.downloads.load(Ordering::Relaxed), 2);
    for (bytes, _) in server.objects.files.lock().unwrap().values() {
        assert!(!String::from_utf8_lossy(bytes).contains("remote_etags"));
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn connection_probe_leaves_no_files_and_conditional_put_prevents_overwriting() {
    let server = Server::new().await;
    let settings = server.settings();
    webdav::test(settings.clone()).await.unwrap();
    assert!(server.objects.files.lock().unwrap().is_empty());
    let http = webdav::WebDav::new(&settings).unwrap();
    http.put("replayer/v1/check.json", b"first".to_vec(), None)
        .await
        .unwrap();
    assert!(
        http.put("replayer/v1/check.json", b"second".to_vec(), None)
            .await
            .is_err()
    );
    assert!(
        http.put(
            "replayer/v1/check.json",
            b"second".to_vec(),
            Some("\"invalid\"")
        )
        .await
        .is_err()
    );
    assert_eq!(
        server.objects.files.lock().unwrap()["/dav/replayer/v1/check.json"].0,
        b"first"
    );
}

#[tokio::test]
async fn connection_probe_detects_document_quota_failures_without_changing_cloud_data() {
    let server = Server::new().await;
    let settings = server.settings();
    server
        .objects
        .traffic_exhausted
        .store(true, Ordering::Relaxed);
    // Exactly the observed service behavior: an empty account's tiny write/read
    // probe works, but an existing device document exceeds its remaining quota.
    webdav::test(settings.clone()).await.unwrap();
    let path = format!("/dav/replayer/v1/devices/{}.json", settings.device_id);
    let mut document = serde_json::to_vec(&journal(&settings.device_id).document).unwrap();
    document.resize(246_501, b' ');
    server
        .objects
        .files
        .lock()
        .unwrap()
        .insert(path.clone(), (document.clone(), 7));
    let writes = server.objects.writes.load(Ordering::Relaxed);
    for error in [
        webdav::test(settings.clone()).await.err().unwrap(),
        webdav::run(
            settings.clone(),
            journal(&settings.device_id),
            BTreeMap::new(),
        )
        .await
        .err()
        .unwrap(),
    ] {
        let message = error.to_string();
        assert!(message.contains("traffic quota exhausted"), "{message}");
        assert!(!message.contains("private-account-detail"));
    }
    assert_eq!(writes, server.objects.writes.load(Ordering::Relaxed));
    {
        let files = server.objects.files.lock().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[&path], (document, 7));
    }
    server
        .objects
        .traffic_exhausted
        .store(false, Ordering::Relaxed);
    webdav::test(settings).await.unwrap();
    assert_eq!(server.objects.files.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn forbidden_document_permissions_are_not_misreported_as_exhausted_quota() {
    let server = Server::new().await;
    let settings = server.settings();
    let path = format!("/dav/replayer/v1/devices/{}.json", settings.device_id);
    server
        .objects
        .files
        .lock()
        .unwrap()
        .insert(path, (b"{}".to_vec(), 1));
    server.objects.read_forbidden.store(true, Ordering::Relaxed);
    let message = webdav::test(settings).await.err().unwrap().to_string();
    assert!(message.contains("access denied"));
    assert!(!message.contains("quota") && !message.contains("private-account-detail"));
    assert_eq!(server.objects.writes.load(Ordering::Relaxed), 0);
}

#[test]
fn favorite_time_follows_the_winning_operation_and_old_booleans_stay_compatible() {
    let snapshot = |favorite, at| {
        values(&[
            ("song/x/favorite", json!(favorite)),
            ("song/x/favorited_at", json!(at)),
        ])
    };
    let mut a = journal("a");
    a.capture(snapshot(true, 100), &Snapshot::new());
    assert!(!a.document.entries.contains_key("song/x/favorited_at"));
    assert_eq!(
        a.document.entries["song/x/favorite"].value,
        Some(json!(true))
    );
    let mut b = journal("b");
    b.merge(&a.document).unwrap();
    b.observed = b.values();
    a.capture(snapshot(false, 100), &Snapshot::new());
    b.capture(snapshot(false, 100), &Snapshot::new());
    b.capture(snapshot(true, 200), &Snapshot::new());
    let mut reverse = b.clone();
    reverse.merge(&a.document).unwrap();
    a.merge(&b.document).unwrap();
    assert_eq!(a.values(), reverse.values());
    assert_eq!(a.values()["song/x/favorite"], json!(false));
    assert_eq!(a.values()["song/x/favorited_at"], json!(100));
    b.merge(&a.document).unwrap();
    b.observed = b.values();
    b.capture(snapshot(true, 300), &Snapshot::new());
    a.merge(&b.document).unwrap();
    assert_eq!(a.values()["song/x/favorited_at"], json!(300));
    // A cancel/refavorite between sync ticks changes the timestamp even though
    // the final boolean is still true, and must produce a new operation.
    b.capture(snapshot(true, 400), &Snapshot::new());
    a.merge(&b.document).unwrap();
    assert_eq!(a.values()["song/x/favorited_at"], json!(400));
    let mut legacy = a.document.clone();
    legacy
        .entries
        .get_mut("song/x/favorite")
        .unwrap()
        .favorited_at = None;
    let mut fresh = journal("fresh");
    fresh.merge(&legacy).unwrap();
    fresh.merge(&a.document).unwrap();
    a.merge(&legacy).unwrap();
    assert_eq!(fresh.values(), a.values());
}
