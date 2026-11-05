//! Session-scoped loopback transport for authorized Bilibili media URLs.
//! Only API requests use account cookies. CDN URLs are never persisted or logged.
use anyhow::{Result, anyhow};
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
pub use pordo_core::video_sources::bilibili::*;
use std::{sync::Arc, time::Instant};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

pub struct Source {
    pub video: String,
    pub audio: Option<String>,
    pub duration: f64,
    server: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}
impl Drop for Source {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.server.abort();
    }
}
struct Proxy {
    api: Arc<Bilibili>,
    media: reqwest::Client,
    playback: RwLock<Playback>,
    refresh: Mutex<Instant>,
    token: String,
    cancel: CancellationToken,
}
pub async fn start(api: Arc<Bilibili>, playback: Playback) -> Result<Source> {
    let duration = playback.duration;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let base = format!("http://{address}/{token}");
    let video = format!("{base}/video.mp4");
    let audio = playback.audio.as_ref().map(|_| format!("{base}/audio.m4a"));
    let cancel = CancellationToken::new();
    let state = Arc::new(Proxy {
        api,
        media: Bilibili::media_client()?,
        playback: RwLock::new(playback),
        refresh: Mutex::new(Instant::now() - std::time::Duration::from_secs(60)),
        token,
        cancel: cancel.clone(),
    });
    let app = Router::new()
        .route("/{token}/{kind}", get(serve))
        .with_state(state);
    let shutdown = cancel.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await;
    });
    Ok(Source {
        video,
        audio,
        duration,
        server,
        cancel,
    })
}
async fn serve(
    State(state): State<Arc<Proxy>>,
    Path((token, kind)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if token != state.token || !matches!(kind.as_str(), "video.mp4" | "audio.m4a") {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Do not forward arbitrary caller headers to the service, especially cookies.
    let range = match headers.get("range") {
        Some(value) => match value.to_str() {
            Ok(value) if valid_range(value) => Some(value.to_owned()),
            _ => return StatusCode::RANGE_NOT_SATISFIABLE.into_response(),
        },
        None => None,
    };
    tokio::select! {
        _ = state.cancel.cancelled() => StatusCode::GONE.into_response(),
        result = fetch(&state, kind == "audio.m4a", range.as_deref()) => match result {
            Ok(response) => response,
            Err(_) => StatusCode::BAD_GATEWAY.into_response(),
        }
    }
}
fn valid_range(value: &str) -> bool {
    let Some((start, end)) = value.strip_prefix("bytes=").and_then(|v| v.split_once('-')) else {
        return false;
    };
    (!start.is_empty() || !end.is_empty())
        && (start.is_empty() || start.parse::<u64>().is_ok())
        && (end.is_empty() || end.parse::<u64>().is_ok())
        && (start.is_empty()
            || end.is_empty()
            || start.parse::<u64>().unwrap() <= end.parse::<u64>().unwrap())
}
async fn fetch(state: &Proxy, audio: bool, range: Option<&str>) -> Result<Response> {
    for attempt in 0..2 {
        let playback = state.playback.read().await.clone();
        let stream = if audio {
            playback
                .audio
                .as_ref()
                .ok_or_else(|| anyhow!("Missing audio"))?
        } else {
            &playback.video
        };
        for url in &stream.urls {
            let mut request = state
                .media
                .get(url.clone())
                .header("Referer", REFERER)
                .header("Accept-Encoding", "identity");
            if let Some(range) = range {
                request = request.header("Range", range);
            }
            let Ok(remote) = request.send().await else {
                continue;
            };
            let status = remote.status();
            if !status.is_success() {
                continue;
            }
            if range.is_some() && status != StatusCode::PARTIAL_CONTENT {
                continue;
            }
            let mut response = Response::builder().status(status);
            for name in [
                "content-type",
                "content-length",
                "content-range",
                "accept-ranges",
            ] {
                if let Some(value) = remote.headers().get(name) {
                    response = response.header(name, value);
                }
            }
            // No full-file buffering. Stop already-open bodies when the source is dropped.
            let cancel = state.cancel.clone();
            let body = futures_util::stream::unfold(
                (Some(remote), cancel),
                |(remote, cancel)| async move {
                    let mut remote = remote?;
                    tokio::select! {
                        _ = cancel.cancelled() => None,
                        chunk = remote.chunk() => match chunk {
                            Ok(Some(bytes)) => Some((Ok::<_, std::io::Error>(bytes), (Some(remote), cancel))),
                            Ok(None) => None,
                            Err(_) => Some((Err(std::io::Error::other("Video stream interrupted")), (None, cancel))),
                        }
                    }
                },
            );
            return Ok(response.body(Body::from_stream(body))?);
        }
        if attempt == 0 {
            // Serialize renewals; a concurrent audio/video request can reuse the new URLs.
            let mut refreshed = state.refresh.lock().await;
            if refreshed.elapsed() >= std::time::Duration::from_secs(5) {
                let next = state
                    .api
                    .resolve(&playback.bvid, playback.cid, playback.quality)
                    .await?;
                anyhow::ensure!(
                    next.quality == playback.quality
                        && next.video.codec == playback.video.codec
                        && next.audio.as_ref().map(|a| &a.codec)
                            == playback.audio.as_ref().map(|a| &a.codec),
                    "Playback representation changed"
                );
                *state.playback.write().await = next;
                *refreshed = Instant::now();
            }
        }
    }
    Err(anyhow!("Video source unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_input_rejects_multiple_and_invalid_ranges() {
        for value in ["bytes=0-511", "bytes=512-", "bytes=-512"] {
            assert!(valid_range(value));
        }
        for value in [
            "bytes=-",
            "bytes=3-1",
            "bytes=0-1,5-6",
            "bytes=x-9",
            "bytes=0-1\r\nCookie: secret",
        ] {
            assert!(!valid_range(value));
        }
    }
    #[tokio::test]
    async fn loopback_preserves_range_uses_backups_and_never_forwards_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/media",
                get(|headers: HeaderMap| async move {
                    assert!(headers.get("cookie").is_none());
                    assert_eq!(headers["referer"], REFERER);
                    assert_eq!(headers["range"], "bytes=2-3");
                    Response::builder()
                        .status(206)
                        .header("Content-Range", "bytes 2-3/8")
                        .header("Content-Length", "2")
                        .body(Body::from("cd"))
                        .unwrap()
                }),
            )
            .route(
                "/unavailable",
                get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let stream = Stream {
            urls: vec![
                format!("http://{address}/unavailable").parse().unwrap(),
                format!("http://{address}/media").parse().unwrap(),
            ],
            codec: "avc1".into(),
        };
        let source = start(
            Arc::new(Bilibili::new(None).unwrap()),
            Playback {
                bvid: "BV1xx411c7mD".into(),
                cid: 1,
                quality: 16,
                duration: 8.0,
                qualities: vec![],
                video: stream,
                audio: None,
            },
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let response = client
            .get(&source.video)
            .header("Range", "bytes=2-3")
            .header("Cookie", "SESSDATA=must-not-leak")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["content-range"], "bytes 2-3/8");
        assert_eq!(response.text().await.unwrap(), "cd");
        let response = client
            .get(&source.video)
            .header("Range", "bytes=0-1,3-4")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 416);
        let wrong = source
            .video
            .rsplit_once('/')
            .unwrap()
            .0
            .rsplit_once('/')
            .unwrap()
            .0
            .to_string()
            + "/wrong/video.mp4";
        assert_eq!(client.get(wrong).send().await.unwrap().status(), 404);
        drop(source);
        server.abort();
    }
}
