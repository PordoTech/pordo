use super::*;
use serde_json::json;

fn response() -> Value {
    json!({"code":0,"data":{"quality":80,"support_formats":[{"quality":80,"new_description":"1080P"},{"quality":64,"new_description":"720P"}],
        "dash":{"video":[
            {"id":80,"codecid":12,"codecs":"hev1.1","baseUrl":"https://a.bilivideo.com/hevc"},
            {"id":80,"codecid":7,"codecs":"avc1.640028","baseUrl":"https://a.bilivideo.com/video","backupUrl":["https://b.bilivideo.com/video"]},
            {"id":64,"codecid":7,"codecs":"avc1.64001f","base_url":"https://a.bilivideo.com/720"}],
            "audio":[{"codecs":"mp4a.40.2","bandwidth":192000,"baseUrl":"https://a.bilivideo.com/audio"}]}}})
}
#[test]
fn wbi_matches_public_protocol_vector() {
    let key = wbi_key(&json!({"img_url":"https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png","sub_url":"https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"})).unwrap();
    assert_eq!(key, "ea1db124af3c7062474693fa704f4ff8");
    let signed = sign(
        params(&[("foo", "114"), ("bar", "514"), ("zab", "1919810")]),
        &key,
        1702204169,
    );
    assert_eq!(signed["w_rid"], "8f6f2b5b3d485fe1886cec6a0be8c5d4");
}
#[test]
fn signing_preserves_search_text_and_encodes_utf8_and_spaces() {
    assert_eq!(encode("五一四 one"), "%E4%BA%94%E4%B8%80%E5%9B%9B%20one");
    let signed = sign(params(&[("keyword", "Rust! (中文)")]), "key", 1);
    assert_eq!(signed["keyword"], "Rust! (中文)");
}
#[test]
fn titles_are_plain_text_and_pagination_is_preserved() {
    let value = json!({"code":0,"data":{"page":1,"numPages":2,"result":[{"bvid":"BV1xx411c7mD","title":"<em class=\"keyword\">Rust</em> &amp; video","author":"UP","duration":"02:12"}]}});
    let page = parse_search(&value, SearchKind::Video).unwrap();
    assert_eq!(page.videos[0].title, "Rust & video");
    assert_eq!(page.videos[0].uploader, "UP");
    assert!(page.has_more);
}

#[test]
fn overlapping_search_fields_work_in_mixed_video_and_creator_results_and_uploads() {
    let row = json!({"bvid":"BV1xx411c7mD","title":"<em>KAYGEZ</em> video",
        "pic":"//i0.hdslb.com/bfs/archive/actual.jpg","cover":"",
        "author":"KAYGEZ","uploader":"fallback","length":120,"duration":"2:00",
        "play":"1234","views":12,"pubdate":1730000000,"published":1700000000});
    let creator = json!({"mid":"123","uname":"KAYGEZ","upic":"//i0.hdslb.com/bfs/face/avatar.jpg","avatar":""});
    let mixed = parse_search(
        &json!({"code":0,"data":{"result":[
            {"result_type":"video","data":[row.clone()]},
            {"result_type":"bili_user","data":[creator.clone()]}
        ]}}),
        SearchKind::All,
    )
    .unwrap();
    let video = parse_search(
        &json!({"code":0,"data":{"result":[row.clone()]}}),
        SearchKind::Video,
    )
    .unwrap();
    let uploads = parse_uploads(
        &json!({"code":0,"data":{"list":{"vlist":[row]},"page":{"count":1,"ps":20}}}),
        1,
    )
    .unwrap();
    for result in [mixed.videos, video.videos, uploads.videos] {
        assert_eq!(result.len(), 1);
        let video = &result[0];
        assert_eq!(video.title, "KAYGEZ video");
        assert_eq!(
            video.cover.as_deref(),
            Some("//i0.hdslb.com/bfs/archive/actual.jpg")
        );
        assert_eq!(video.uploader, "KAYGEZ");
        assert_eq!(video.duration, "120");
        assert_eq!(video.views, Some(1234));
        assert_eq!(video.published, Some(1730000000));
    }
    let creators = parse_search(
        &json!({"code":0,"data":{"result":[creator]}}),
        SearchKind::Uploader,
    )
    .unwrap();
    for results in [mixed.uploaders, creators.uploaders] {
        assert_eq!(
            results[0].avatar.as_deref(),
            Some("//i0.hdslb.com/bfs/face/avatar.jpg")
        );
    }
}

#[test]
fn empty_native_search_fields_keep_fallbacks_and_zero_counts_remain_valid() {
    for empty in [Value::Null, json!(""), json!("  ")] {
        let row = json!({"bvid":"BV1xx411c7mD","title":"Video","pic":empty,"cover":"//i0.hdslb.com/fallback.jpg",
            "author":empty,"uploader":"UP","length":empty,"duration":"2:00",
            "play":empty,"views":4,"pubdate":empty,"published":17});
        let video = parse_video(&row).unwrap().unwrap();
        assert_eq!(video.cover.as_deref(), Some("//i0.hdslb.com/fallback.jpg"));
        assert_eq!(video.uploader, "UP");
        assert_eq!(video.duration, "2:00");
        assert_eq!(video.views, Some(4));
        assert_eq!(video.published, Some(17));
    }
    let video = parse_video(&json!({"bvid":"BV1xx411c7mD","title":"Video","play":0,"views":4}))
        .unwrap()
        .unwrap();
    assert_eq!(video.views, Some(0));
}

#[tokio::test]
#[ignore = "Queries public Bilibili search for KAYGEZ without using saved credentials"]
async fn live_bilibili_search_kaygez() {
    let api = Bilibili::new(None).unwrap();
    for kind in [SearchKind::All, SearchKind::Video, SearchKind::Uploader] {
        let result = api.search("KAYGEZ", 1, kind).await.unwrap();
        println!(
            "{kind:?}: {} videos, {} creators",
            result.videos.len(),
            result.uploaders.len()
        );
    }
}
#[test]
fn search_kinds_parse_uploaders_and_mixed_groups_without_losing_pagination() {
    let creator = json!({"mid":"123","uname":"<em>UP</em> &amp; name","usign":"hello","fans":"234","videos":5});
    let user = parse_search(
        &json!({"code":0,"data":{"page":1,"numPages":3,"result":[creator.clone()]}}),
        SearchKind::Uploader,
    )
    .unwrap();
    assert!(user.videos.is_empty());
    assert!(user.has_more);
    assert_eq!(user.uploaders[0].mid, 123);
    assert_eq!(user.uploaders[0].uname, "UP & name");
    let mixed = json!({"code":0,"data":{"page":2,"numPages":2,"result":[
        {"result_type":"bili_user","data":[creator]},
        {"result_type":"video","data":[{"bvid":"BV1xx411c7mD","title":"video","duration":120}]},
        {"result_type":"media_bangumi","data":[{"title":"unsupported group"}]}
    ]}});
    let all = parse_search(&mixed, SearchKind::All).unwrap();
    assert_eq!(all.videos.len(), 1);
    assert_eq!(all.uploaders.len(), 1);
    assert!(!all.has_more);
    assert_eq!(all.videos[0].duration, "120");
    let empty = parse_search(
        &json!({"code":0,"data":{"result":null,"numResults":0}}),
        SearchKind::Uploader,
    )
    .unwrap();
    assert!(empty.uploaders.is_empty());
}
#[test]
fn creator_uploads_use_vlist_count_and_length_fields() {
    let value = json!({"code":0,"data":{"list":{"vlist":[{"bvid":"BV1xx411c7mD","title":"<em>upload</em>","length":"3:00","author":"UP","created":1730000000}]},"page":{"count":21,"ps":20}}});
    let first = parse_uploads(&value, 1).unwrap();
    assert_eq!(first.videos[0].title, "upload");
    assert_eq!(first.videos[0].duration, "3:00");
    assert_eq!(first.videos[0].published, Some(1730000000));
    assert!(first.has_more);
    assert!(!parse_uploads(&value, 2).unwrap().has_more);
    assert!(parse_uploads(&json!({"code":0,"data":{"v_voucher":"verify"}}), 1).is_err());
}
#[test]
fn upload_creation_dates_support_numeric_strings_and_pubdate_fallbacks() {
    for created in [json!(1730000000), json!("1730000000")] {
        for pubdate in [Value::Null, json!(""), json!("  ")] {
            let row = json!({"bvid":"BV1xx411c7mD","title":"upload",
                "created":created,"pubdate":pubdate});
            assert_eq!(
                parse_video(&row).unwrap().unwrap().published,
                Some(1730000000)
            );
        }
    }
    let row = json!({"bvid":"BV1xx411c7mD","title":"video",
        "created":1700000000,"pubdate":1730000000,"published":1600000000});
    assert_eq!(
        parse_video(&row).unwrap().unwrap().published,
        Some(1730000000)
    );
}
#[test]
fn verification_is_not_an_empty_successful_search() {
    let value = json!({"code":0,"data":{"v_voucher":"challenge","result":[]}});
    assert!(
        parse_search(&value, SearchKind::Video)
            .err()
            .unwrap()
            .to_string()
            .contains("verification")
    );
    assert!(
        checked(&json!({"code":-101}))
            .err()
            .unwrap()
            .to_string()
            .contains("Session expired")
    );
    assert!(checked(&json!({"data":{}})).is_err());
}
#[test]
fn qr_status_requires_both_response_and_authorization_success() {
    for (code, stage) in [
        (86101, LoginStage::Waiting),
        (86090, LoginStage::Confirm),
        (86038, LoginStage::Expired),
        (0, LoginStage::Authenticated),
    ] {
        assert_eq!(
            login_stage(&json!({"code":0,"data":{"code":code}})).unwrap(),
            stage
        );
    }
    for value in [
        json!({"code":0,"data":{}}),
        json!({"code":-400,"data":{"code":0}}),
        json!({"code":0,"data":{"code":12345}}),
    ] {
        assert!(login_stage(&value).is_err());
    }
}
#[test]
fn unsupported_pcdn_primary_uses_the_service_https_backup() {
    let mut value = response();
    value["data"]["timelength"] = json!(212393);
    value["data"]["dash"]["video"][1]["baseUrl"] = json!("https://pcdn.bilivideo.cn:8082/video");
    let parsed = parse_playback(&value, "BV1xx411c7mD", 12, DEFAULT_QUALITY).unwrap();
    assert_eq!(parsed.video.urls[0].host_str(), Some("b.bilivideo.com"));
    assert!((parsed.duration - 212.393).abs() < 0.001);
}
#[tokio::test]
#[ignore = "contacts Bilibili to generate a fresh QR code; does not sign in"]
async fn live_bilibili_qr_stays_pending_without_phone_confirmation() {
    let api = Bilibili::new(None).unwrap();
    let qr = api.qr_code().await.unwrap();
    assert!(matches!(
        api.poll_login(&qr.qrcode_key).await.unwrap(),
        LoginPoll::Waiting
    ));
}
#[test]
fn playback_selects_avc_aac_and_only_actual_qualities() {
    let parsed = parse_playback(&response(), "BV1xx411c7mD", 12, DEFAULT_QUALITY).unwrap();
    assert!(parsed.video.codec.starts_with("avc"));
    assert_eq!(parsed.video.urls.len(), 2);
    assert!(parsed.audio.unwrap().codec.starts_with("mp4a"));
    assert_eq!(parsed.quality, 80);
    assert_eq!(
        parsed.qualities,
        vec![
            Quality {
                id: 80,
                label: "1080P".into()
            },
            Quality {
                id: 64,
                label: "720P".into()
            }
        ]
    );
}

#[test]
fn default_quality_prefers_1080_then_720_and_respects_server_permissions_and_stream_order() {
    let mut value = response();
    value["data"]["dash"]["video"]
        .as_array_mut()
        .unwrap()
        .insert(
            0,
            json!({"id":120,"codecid":7,"codecs":"avc1","baseUrl":"https://a.bilivideo.com/4k"}),
        );
    assert_eq!(
        parse_playback(&value, "BV1xx411c7mD", 12, DEFAULT_QUALITY)
            .unwrap()
            .quality,
        80
    );
    assert_eq!(
        parse_playback(&value, "BV1xx411c7mD", 12, 64)
            .unwrap()
            .quality,
        64
    );
    value["data"]["quality"] = json!(64);
    assert_eq!(
        parse_playback(&value, "BV1xx411c7mD", 12, DEFAULT_QUALITY)
            .unwrap()
            .quality,
        64
    );
    value["data"]["quality"] = json!(80);
    value["data"]["dash"]["video"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["id"] != 80 || v["codecid"] != 7);
    assert_eq!(
        parse_playback(&value, "BV1xx411c7mD", 12, DEFAULT_QUALITY)
            .unwrap()
            .quality,
        64
    );
    value["data"]["quality"] = json!(32);
    value["data"]["dash"]["video"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":32,"codecid":7,"codecs":"avc1","baseUrl":"https://a.bilivideo.com/480"}));
    assert_eq!(
        parse_playback(&value, "BV1xx411c7mD", 12, DEFAULT_QUALITY)
            .unwrap()
            .quality,
        32
    );
}

#[test]
fn unavailable_1080_cdn_falls_back_to_720_without_exposing_invalid_urls() {
    let mut value = response();
    value["data"]["dash"]["video"][1]["baseUrl"] = json!("https://127.0.0.1/private");
    value["data"]["dash"]["video"][1]["backupUrl"] = json!([]);
    let parsed = parse_playback(&value, "BV1xx411c7mD", 12, DEFAULT_QUALITY).unwrap();
    assert_eq!(parsed.quality, 64);
    assert!(parsed.qualities.iter().all(|q| q.id != 80));
    assert_eq!(parsed.video.urls[0].path(), "/720");
}

#[tokio::test]
#[ignore = "Fetches public video metadata and authorized playback URLs, without downloading media"]
async fn live_bilibili_default_quality() {
    let api = Bilibili::new(None).unwrap();
    let detail = api.detail("BV1GJ411x7h7").await.unwrap();
    let playback = api
        .resolve(&detail.bvid, detail.parts[0].cid, DEFAULT_QUALITY)
        .await
        .unwrap();
    assert!(playback.quality <= DEFAULT_QUALITY);
    println!(
        "Default playback quality: {}; {} authorized choices",
        playback.quality,
        playback.qualities.len()
    );
}
#[test]
fn unsupported_codecs_and_missing_urls_fail_explicitly() {
    let mut value = response();
    value["data"]["dash"]["video"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["codecid"] == 12);
    assert!(parse_playback(&value, "BV1xx411c7mD", 12, DEFAULT_QUALITY).is_err());
    let mut value = response();
    value["data"]["dash"]["audio"][0]["codecs"] = json!("ec-3");
    assert!(parse_playback(&value, "BV1xx411c7mD", 12, DEFAULT_QUALITY).is_err());
}
#[test]
fn urls_cannot_redirect_credentials_or_proxy_local_networks() {
    for url in [
        "http://a.bilivideo.com/v",
        "https://127.0.0.1/v",
        "https://bilivideo.com.evil.test/v",
        "https://a.bilivideo.com:8443/v",
        "https://secret@a.bilivideo.com/v",
    ] {
        assert!(cdn_url(url).is_err());
    }
    assert!(cdn_url("https://a.bilivideo.com/v?deadline=1").is_ok());
    assert_eq!(
        video_id("https://www.bilibili.com/video/BV1xx411c7mD?p=2").as_deref(),
        Some("BV1xx411c7mD")
    );
    assert!(video_id("https://evil.test/video/BV1xx411c7mD").is_none());
}
#[test]
fn artwork_metadata_and_image_urls_are_safe_and_optional() {
    let value = json!({"code":0,"data":{"page":1,"numPages":1,"result":[{"bvid":"BV1xx411c7mD","title":"Video","pic":"//i0.hdslb.com/bfs/archive/cover.jpg","play":"12500","pubdate":1730000000}]}});
    let result = parse_search(&value, SearchKind::Video).unwrap();
    let video = &result.videos[0];
    assert_eq!(video.views, Some(12500));
    assert_eq!(video.published, Some(1730000000));
    assert_eq!(
        image_url(video.cover.as_deref().unwrap()).unwrap().scheme(),
        "https"
    );
    for value in [
        "file:///private/image.jpg",
        "https://127.0.0.1/a",
        "https://i0.hdslb.com.evil.test/a",
        "https://secret@i0.hdslb.com/a",
        "https://i0.hdslb.com:8443/a",
    ] {
        assert!(image_url(value).is_none());
    }
}
#[test]
fn restoring_a_session_keeps_cookies_scoped_to_bilibili() {
    let session = Session {
        cookie: "SESSDATA=private; bili_jct=csrf; DedeUserID=12".into(),
        refresh_token: "private-refresh".into(),
    };
    let api = Bilibili::new(Some(&session)).unwrap();
    assert!(api.jar.cookies(&Url::parse(API).unwrap()).is_some());
    assert!(
        api.jar
            .cookies(&Url::parse("https://a.bilivideo.com/").unwrap())
            .is_none()
    );
    assert!(
        Bilibili::new(Some(&Session {
            cookie: "a=1\r\nX: secret".into(),
            refresh_token: String::new()
        }))
        .is_err()
    );
}
