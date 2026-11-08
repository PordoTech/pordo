//! Optional community OP/ED markers. This client never receives Bilibili cookies.
use anyhow::{Result, ensure};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Intro,
    Outro,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub kind: Kind,
    pub start: f64,
    pub end: f64,
}

fn client() -> Result<&'static Client> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .user_agent(concat!("Pordo/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(8))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn fetch(bvid: &str, cid: u64, duration: f64) -> Result<Vec<Segment>> {
    fetch_from(client()?, "https://bsbsb.top", bvid, cid, duration).await
}
async fn fetch_from(
    client: &Client,
    origin: &str,
    bvid: &str,
    cid: u64,
    duration: f64,
) -> Result<Vec<Segment>> {
    ensure!(
        bvid.starts_with("BV")
            && bvid.len() == 12
            && bvid.bytes().all(|c| c.is_ascii_alphanumeric())
            && cid > 0
            && duration.is_finite()
            && duration > 0.0,
        "Invalid video for OP/ED lookup"
    );
    let hash = crate::crypto::sha256_hex(bvid.as_bytes());
    let mut response = client
        .get(format!("{origin}/api/skipSegments/{}", &hash[..4]))
        .header("Origin", "https://github.com/PordoTech/pordo")
        .header("x-ext-version", env!("CARGO_PKG_VERSION"))
        .query(&[("categories", "[\"intro\",\"outro\"]")])
        .send()
        .await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    ensure!(
        response.status().is_success(),
        "OP/ED lookup HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 1024 * 1024,
            "OP/ED response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    parse(&bytes, bvid, cid, duration)
}
fn parse(bytes: &[u8], bvid: &str, cid: u64, duration: f64) -> Result<Vec<Segment>> {
    let value: Value = serde_json::from_slice(bytes)?;
    let videos = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid OP/ED response"))?;
    let mut segments = Vec::new();
    for video in videos
        .iter()
        .filter(|v| v["videoID"].as_str() == Some(bvid))
    {
        for raw in video["segments"].as_array().into_iter().flatten() {
            let segment_cid = raw["cid"]
                .as_u64()
                .or_else(|| raw["cid"].as_str()?.parse().ok());
            if segment_cid != Some(cid)
                || raw["actionType"] != "skip"
                || raw["votes"].as_i64().is_some_and(|v| v < 0)
            {
                continue;
            }
            let kind = match raw["category"].as_str() {
                Some("intro") => Kind::Intro,
                Some("outro") => Kind::Outro,
                _ => continue,
            };
            let Some(recorded) = raw["videoDuration"].as_f64() else {
                continue;
            };
            if !recorded.is_finite()
                || recorded < 0.0
                || recorded > 0.0 && (recorded - duration).abs() > 2.0
            {
                continue;
            }
            let Some(bounds) = raw["segment"].as_array().filter(|v| v.len() == 2) else {
                continue;
            };
            let (Some(start), Some(end)) = (bounds[0].as_f64(), bounds[1].as_f64()) else {
                continue;
            };
            if !start.is_finite()
                || !end.is_finite()
                || start < 0.0
                || end <= start
                || start >= duration
                || end > duration + 0.5
            {
                continue;
            }
            segments.push(Segment {
                kind,
                start,
                end: end.min(duration),
            });
        }
    }
    segments.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
    segments.dedup();
    segments.truncate(128);
    Ok(segments)
}

/// Each marker is consumed once per playback pass. Manual seeks into a marker
/// consume it too, allowing users to watch a skipped OP/ED without a seek loop.
#[derive(Default)]
pub struct Playback {
    pub segments: Vec<Segment>,
    handled: Vec<bool>,
}
impl Playback {
    pub fn new(mut segments: Vec<Segment>) -> Self {
        segments.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
        Self {
            handled: vec![false; segments.len()],
            segments,
        }
    }
    pub fn reset_pass(&mut self) {
        self.handled.fill(false);
    }
    pub fn manual_seek(&mut self, position: f64) {
        for (segment, handled) in self.segments.iter().zip(&mut self.handled) {
            if position >= segment.start && position < segment.end {
                *handled = true;
            }
        }
    }
    pub fn target(&mut self, position: f64, settings: &super::super::SkipSettings) -> Option<f64> {
        let mut target = None;
        // Merge overlapping enabled intervals so duplicate/community submissions
        // cannot produce a chain of seeks through the same OP/ED.
        for (segment, handled) in self.segments.iter().zip(&mut self.handled) {
            let enabled = match segment.kind {
                Kind::Intro => settings.intro,
                Kind::Outro => settings.outro,
            };
            if enabled && !*handled && position >= segment.start && position < segment.end {
                *handled = true;
                target = Some(target.map_or(segment.end, |end: f64| end.max(segment.end)));
            } else if enabled
                && !*handled
                && target.is_some_and(|end| segment.start <= end && segment.end > position)
            {
                *handled = true;
                target = Some(target.unwrap().max(segment.end));
            }
        }
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video_sources::{Settings, SkipSettings};
    use serde_json::json;

    const BV: &str = "BV1GJ411x7h7";
    #[tokio::test]
    #[ignore = "contacts the public community marker API without any account cookies"]
    async fn live_community_intro_markers() {
        let markers = fetch("BV11Kc2zZEDK", 36656057057, 231.0).await.unwrap();
        assert!(
            markers
                .iter()
                .any(|s| s.kind == Kind::Intro && s.start == 0.0 && s.end > 20.0)
        );
    }
    fn raw(category: &str, start: f64, end: f64) -> Value {
        json!({"cid":"11","category":category,"actionType":"skip","segment":[start,end],"videoDuration":300,"votes":1})
    }
    #[test]
    fn markers_validate_video_part_duration_category_and_bounds() {
        let valid = raw("intro", 10.0, 40.0);
        let mut values = vec![
            valid.clone(),
            valid,
            raw("outro", 270.0, 300.0),
            raw("sponsor", 50.0, 60.0),
            raw("intro", -1.0, 5.0),
            raw("intro", 30.0, 20.0),
            raw("outro", 290.0, 310.0),
        ];
        for (key, value) in [
            ("cid", json!(12)),
            ("videoDuration", json!(303)),
            ("votes", json!(-1)),
            ("actionType", json!("mute")),
            ("segment", json!([1, 2, 3])),
        ] {
            let mut invalid = raw("intro", 10.0, 40.0);
            invalid[key] = value;
            values.push(invalid);
        }
        let data = json!([{"videoID":"BVwrong","segments":[raw("intro",0.0,9.0)]},{"videoID":BV,"segments":values}]);
        let markers = parse(&serde_json::to_vec(&data).unwrap(), BV, 11, 300.0).unwrap();
        assert_eq!(
            markers,
            vec![
                Segment {
                    kind: Kind::Intro,
                    start: 10.0,
                    end: 40.0
                },
                Segment {
                    kind: Kind::Outro,
                    start: 270.0,
                    end: 300.0
                }
            ]
        );
        assert!(parse(b"{}", BV, 11, 300.0).is_err());
    }
    #[test]
    fn independent_switches_manual_rewatch_overlap_and_eof_are_respected() {
        let mut playback = Playback::new(vec![
            Segment {
                kind: Kind::Intro,
                start: 10.0,
                end: 40.0,
            },
            Segment {
                kind: Kind::Intro,
                start: 30.0,
                end: 50.0,
            },
            Segment {
                kind: Kind::Outro,
                start: 270.0,
                end: 300.0,
            },
        ]);
        let intro = SkipSettings {
            intro: true,
            outro: false,
        };
        let both = SkipSettings {
            intro: true,
            outro: true,
        };
        assert_eq!(playback.target(15.0, &SkipSettings::default()), None);
        assert_eq!(playback.target(9.0, &both), None);
        assert_eq!(playback.target(10.0, &intro), Some(50.0));
        assert_eq!(playback.target(15.0, &intro), None, "one seek per pass");
        assert_eq!(playback.target(275.0, &intro), None);
        assert_eq!(
            playback.target(275.0, &both),
            Some(300.0),
            "ED may end at EOF"
        );
        playback.reset_pass();
        playback.manual_seek(35.0);
        assert_eq!(
            playback.target(36.0, &both),
            None,
            "manual seek keeps both overlapping OP markers watchable"
        );
        assert_eq!(playback.target(275.0, &both), Some(300.0));
        playback.reset_pass();
        assert_eq!(
            playback.target(10.0, &both),
            Some(50.0),
            "new playback pass skips again"
        );
    }
    #[test]
    fn old_settings_default_to_disabled_and_skip_choices_round_trip() {
        let mut settings: Settings = serde_json::from_str(r#"{"source":"bilibili"}"#).unwrap();
        assert_eq!(settings.skip, SkipSettings::default());
        settings.skip = SkipSettings {
            intro: true,
            outro: false,
        };
        assert_eq!(
            serde_json::from_str::<Settings>(&serde_json::to_string(&settings).unwrap()).unwrap(),
            settings
        );
    }
    #[tokio::test]
    async fn lookup_uses_hash_prefix_identity_headers_and_no_cookies_with_soft_404() {
        use axum::{
            Json, Router,
            extract::{Path, Query},
            http::HeaderMap,
            routing::get,
        };
        use std::collections::HashMap;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router=Router::new().route("/api/skipSegments/{hash}",get(|Path(hash):Path<String>,Query(query):Query<HashMap<String,String>>,headers:HeaderMap| async move {
            assert_eq!(hash,crate::crypto::sha256_hex(BV.as_bytes())[..4]);
            assert_eq!(query["categories"],"[\"intro\",\"outro\"]");
            assert_eq!(headers["origin"],"https://github.com/PordoTech/pordo");
            assert_eq!(headers["x-ext-version"],env!("CARGO_PKG_VERSION"));
            assert!(!headers.contains_key("cookie") && !headers.contains_key("authorization"));
            Json(json!([{"videoID":BV,"segments":[raw("intro",10.0,40.0),raw("outro",270.0,300.0)]}]))
        }));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let origin = format!("http://{address}");
        assert_eq!(
            fetch_from(client().unwrap(), &origin, BV, 11, 300.0)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(
            fetch_from(
                client().unwrap(),
                &format!("{origin}/missing"),
                BV,
                11,
                300.0
            )
            .await
            .unwrap()
            .is_empty()
        );
        server.abort();
    }
}
