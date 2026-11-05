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
    let value = json!({"code":0,"data":{"list":{"vlist":[{"bvid":"BV1xx411c7mD","title":"<em>upload</em>","length":"3:00","author":"UP"}]},"page":{"count":21,"ps":20}}});
    let first = parse_uploads(&value, 1).unwrap();
    assert_eq!(first.videos[0].title, "upload");
    assert_eq!(first.videos[0].duration, "3:00");
    assert!(first.has_more);
    assert!(!parse_uploads(&value, 2).unwrap().has_more);
    assert!(parse_uploads(&json!({"code":0,"data":{"v_voucher":"verify"}}), 1).is_err());
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
    let parsed = parse_playback(&value, "BV1xx411c7mD", 12).unwrap();
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
    let parsed = parse_playback(&response(), "BV1xx411c7mD", 12).unwrap();
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
fn unsupported_codecs_and_missing_urls_fail_explicitly() {
    let mut value = response();
    value["data"]["dash"]["video"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["codecid"] == 12);
    assert!(parse_playback(&value, "BV1xx411c7mD", 12).is_err());
    let mut value = response();
    value["data"]["dash"]["audio"][0]["codecs"] = json!("ec-3");
    assert!(parse_playback(&value, "BV1xx411c7mD", 12).is_err());
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
