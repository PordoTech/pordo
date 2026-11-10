//! Bilibili Web search, QR sessions and permission-aware UGC playback.
//! Sessions contain credentials: hosts must keep them in platform vaults.
use anyhow::{Result, anyhow, bail, ensure};
use reqwest::{
    Client, Url,
    cookie::{CookieStore, Jar},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
pub mod danmaku;
pub mod segments;

#[derive(Default)]
struct PrivateJar(Jar);
impl CookieStore for PrivateJar {
    fn set_cookies(
        &self,
        headers: &mut dyn Iterator<Item = &reqwest::header::HeaderValue>,
        url: &Url,
    ) {
        self.0.set_cookies(headers, url);
    }
    fn cookies(&self, url: &Url) -> Option<reqwest::header::HeaderValue> {
        self.0.cookies(url).map(|mut header| {
            header.set_sensitive(true);
            header
        })
    }
}

const API: &str = "https://api.bilibili.com";
const PASSPORT: &str = "https://passport.bilibili.com";
pub const REFERER: &str = "https://www.bilibili.com/";
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
pub const DEFAULT_QUALITY: u32 = 80;
const LIMIT: usize = 4 * 1024 * 1024;

// Deliberately no Debug: neither cookies nor refresh tokens belong in logs.
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub cookie: String,
    pub refresh_token: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Account {
    pub mid: u64,
    pub uname: String,
}
#[derive(Clone, Deserialize)]
pub struct QrCode {
    pub url: String,
    pub qrcode_key: String,
}
pub enum LoginPoll {
    Waiting,
    Confirm,
    Expired,
    SignedIn(Session, Account),
}
#[derive(Debug, PartialEq, Eq)]
enum LoginStage {
    Waiting,
    Confirm,
    Expired,
    Authenticated,
}
fn login_stage(value: &Value) -> Result<LoginStage> {
    let data = checked(value)?;
    Ok(match data["code"].as_i64() {
        Some(86101) => LoginStage::Waiting,
        Some(86090) => LoginStage::Confirm,
        Some(86038) => LoginStage::Expired,
        Some(0) => LoginStage::Authenticated,
        _ => bail!("登录失败，请刷新二维码 / Login failed; refresh the QR code"),
    })
}
#[derive(Clone, Debug, Deserialize)]
pub struct Video {
    pub bvid: String,
    pub title: String,
    #[serde(default)]
    pub uploader: String,
    #[serde(default, deserialize_with = "string_field")]
    pub duration: String,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default, deserialize_with = "optional_number")]
    pub views: Option<u64>,
    #[serde(default, deserialize_with = "optional_number")]
    pub published: Option<u64>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchKind {
    #[default]
    All,
    Video,
    Uploader,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uploader {
    #[serde(deserialize_with = "number_field")]
    pub mid: u64,
    pub uname: String,
    #[serde(default)]
    pub usign: String,
    #[serde(default, deserialize_with = "number_field")]
    pub fans: u64,
    #[serde(default, deserialize_with = "number_field")]
    pub videos: u64,
    #[serde(default)]
    pub avatar: Option<String>,
}
fn optional_number<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok())))
}
/// Artwork uses a separate cookie-free client and only the service's image CDNs.
pub fn image_url(value: &str) -> Option<Url> {
    let value = if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.to_owned()
    };
    let mut url = Url::parse(&value).ok()?;
    if url.scheme() == "http" {
        url.set_scheme("https").ok()?;
    }
    let host = url.host_str()?;
    (url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && ["hdslb.com", "biliimg.com"]
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}"))))
    .then_some(url)
}
fn number_field<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u64, D::Error> {
    let value = Value::deserialize(deserializer)?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| serde::de::Error::custom("Invalid numeric field"))
}
fn string_field<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<String, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(serde::de::Error::custom("Invalid text field")),
    }
}
pub struct SearchPage {
    pub videos: Vec<Video>,
    pub uploaders: Vec<Uploader>,
    pub has_more: bool,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Part {
    pub cid: u64,
    pub page: u32,
    pub part: String,
    pub duration: u64,
}
#[derive(Clone, Debug)]
pub struct Detail {
    pub bvid: String,
    pub title: String,
    pub cover: Option<String>,
    pub parts: Vec<Part>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quality {
    pub id: u32,
    pub label: String,
}
#[derive(Clone)]
pub struct Stream {
    pub urls: Vec<Url>,
    pub codec: String,
}
#[derive(Clone)]
pub struct Playback {
    pub bvid: String,
    pub cid: u64,
    pub quality: u32,
    pub duration: f64,
    pub qualities: Vec<Quality>,
    pub video: Stream,
    pub audio: Option<Stream>,
}

pub struct Bilibili {
    client: Client,
    jar: Arc<PrivateJar>,
    wbi: Mutex<Option<String>>,
}
impl Bilibili {
    pub fn new(session: Option<&Session>) -> Result<Self> {
        let jar = Arc::new(PrivateJar::default());
        if let Some(session) = session {
            ensure!(
                session.cookie.len() < 16384 && !session.cookie.contains(['\r', '\n']),
                "无效的登录会话 / Invalid login session"
            );
            let url = Url::parse(REFERER)?;
            for cookie in session
                .cookie
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                jar.0.add_cookie_str(
                    &format!("{cookie}; Domain=.bilibili.com; Path=/; Secure"),
                    &url,
                );
            }
        }
        let client = Client::builder()
            .cookie_provider(jar.clone())
            .user_agent(USER_AGENT)
            .referer(false)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(25))
            .build()?;
        Ok(Self {
            client,
            jar,
            wbi: Mutex::new(None),
        })
    }
    async fn get(
        &self,
        origin: &str,
        path: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<Value> {
        let mut request = self
            .client
            .get(format!("{origin}{path}"))
            .header("Referer", REFERER)
            .query(params);
        if path.starts_with("/x/web-interface/wbi/search/") {
            request = request
                .header("Origin", "https://search.bilibili.com")
                .header("Referer", "https://search.bilibili.com/");
        } else if path == "/x/space/wbi/arc/search" {
            request = request.header(
                "Referer",
                format!(
                    "https://space.bilibili.com/{}",
                    params.get("mid").map(String::as_str).unwrap_or_default()
                ),
            );
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow!("B站连接失败或超时 / Bilibili connection failed or timed out"))?;
        ensure!(
            response.status().is_success(),
            "B站请求失败 / Bilibili HTTP {}",
            response.status().as_u16()
        );
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("B站响应中断 / Bilibili response interrupted"))?
        {
            ensure!(
                body.len() + chunk.len() <= LIMIT,
                "B站响应过大 / Bilibili response too large"
            );
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map_err(|_| anyhow!("B站响应格式无效 / Invalid Bilibili response"))
    }
    async fn signed(&self, path: &str, params: BTreeMap<String, String>) -> Result<Value> {
        for attempt in 0..2 {
            let key = {
                let mut cache = self.wbi.lock().await;
                if cache.is_none() {
                    let nav = self
                        .get(API, "/x/web-interface/nav", &BTreeMap::new())
                        .await?;
                    *cache = Some(wbi_key(&nav["data"]["wbi_img"])?);
                }
                cache.as_ref().unwrap().clone()
            };
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let value = self
                .get(API, path, &sign(params.clone(), &key, now))
                .await?;
            if value["code"].as_i64() == Some(-403) && attempt == 0 {
                *self.wbi.lock().await = None;
                continue;
            }
            return Ok(value);
        }
        unreachable!()
    }
    pub async fn account(&self) -> Result<Account> {
        let value = self
            .get(API, "/x/web-interface/nav", &BTreeMap::new())
            .await?;
        let data = checked(&value)?;
        ensure!(
            data["isLogin"] == true,
            "请重新登录哔哩哔哩 / Sign in to Bilibili again"
        );
        Ok(serde_json::from_value(data.clone())?)
    }
    pub async fn qr_code(&self) -> Result<QrCode> {
        let value = self
            .get(
                PASSPORT,
                "/x/passport-login/web/qrcode/generate",
                &BTreeMap::new(),
            )
            .await?;
        let qr: QrCode = serde_json::from_value(checked(&value)?.clone())?;
        let url =
            Url::parse(&qr.url).map_err(|_| anyhow!("无效的登录二维码 / Invalid login QR code"))?;
        ensure!(
            url.scheme() == "https"
                && matches!(
                    url.host_str(),
                    Some("passport.bilibili.com" | "account.bilibili.com")
                )
                && url.username().is_empty()
                && url.password().is_none()
                && url.port_or_known_default() == Some(443)
                && !qr.qrcode_key.is_empty(),
            "无效的登录二维码 / Invalid login QR code"
        );
        Ok(qr)
    }
    pub async fn poll_login(&self, key: &str) -> Result<LoginPoll> {
        let value = self
            .get(
                PASSPORT,
                "/x/passport-login/web/qrcode/poll",
                &params(&[("qrcode_key", key)]),
            )
            .await?;
        let data = checked(&value)?;
        Ok(match login_stage(&value)? {
            LoginStage::Waiting => LoginPoll::Waiting,
            LoginStage::Confirm => LoginPoll::Confirm,
            LoginStage::Expired => LoginPoll::Expired,
            LoginStage::Authenticated => {
                let cookie = self
                    .jar
                    .cookies(&Url::parse(API)?)
                    .and_then(|c| c.to_str().ok().map(str::to_owned))
                    .unwrap_or_default();
                ensure!(
                    cookie.split(';').any(|c| c.trim().starts_with("SESSDATA=")),
                    "登录未返回会话 / Login did not return a session"
                );
                let account = self.account().await?;
                *self.wbi.lock().await = None;
                LoginPoll::SignedIn(
                    Session {
                        cookie,
                        refresh_token: data["refresh_token"].as_str().unwrap_or_default().into(),
                    },
                    account,
                )
            }
        })
    }
    pub async fn search(&self, keyword: &str, page: usize, kind: SearchKind) -> Result<SearchPage> {
        ensure!(
            !keyword.trim().is_empty() && keyword.len() <= 512 && page > 0,
            "请输入搜索内容 / Enter a search query"
        );
        // Obtain anonymous device cookies from the service, never manufacture them.
        if self.jar.cookies(&Url::parse(REFERER)?).is_none() {
            let _ = self.client.get(REFERER).send().await;
        }
        let mut query = params(&[
            ("keyword", keyword.trim()),
            ("page", &page.to_string()),
            ("page_size", "20"),
            ("platform", "pc"),
        ]);
        let endpoint = match kind {
            SearchKind::All => "/x/web-interface/wbi/search/all/v2",
            SearchKind::Video | SearchKind::Uploader => {
                query.insert(
                    "search_type".into(),
                    match kind {
                        SearchKind::Uploader => "bili_user",
                        _ => "video",
                    }
                    .into(),
                );
                "/x/web-interface/wbi/search/type"
            }
        };
        let value = self.signed(endpoint, query).await?;
        parse_search(&value, kind)
    }
    pub async fn uploads(&self, mid: u64, page: usize) -> Result<SearchPage> {
        ensure!(mid > 0 && page > 0, "无效的UP主 / Invalid creator");
        let value = self
            .signed(
                "/x/space/wbi/arc/search",
                params(&[
                    ("mid", &mid.to_string()),
                    ("pn", &page.to_string()),
                    ("ps", "20"),
                    ("order", "pubdate"),
                    ("platform", "web"),
                    ("web_location", "1550101"),
                ]),
            )
            .await?;
        parse_uploads(&value, page)
    }
    pub async fn detail(&self, bvid: &str) -> Result<Detail> {
        ensure!(valid_bvid(bvid), "无效的 BV 号 / Invalid BV ID");
        let value = self
            .get(API, "/x/web-interface/view", &params(&[("bvid", bvid)]))
            .await?;
        let data = checked(&value)?;
        let parts: Vec<Part> = serde_json::from_value(data["pages"].clone())?;
        ensure!(!parts.is_empty(), "视频没有可播放分集 / No playable parts");
        Ok(Detail {
            bvid: bvid.into(),
            title: text(data, "title"),
            cover: data["pic"]
                .as_str()
                .and_then(image_url)
                .map(|u| u.to_string()),
            parts,
        })
    }
    pub async fn resolve(&self, bvid: &str, cid: u64, quality: u32) -> Result<Playback> {
        ensure!(valid_bvid(bvid) && cid > 0, "无效的视频 / Invalid video");
        let value = self
            .signed(
                "/x/player/wbi/playurl",
                params(&[
                    ("bvid", bvid),
                    ("cid", &cid.to_string()),
                    ("qn", &quality.to_string()),
                    ("fnval", "4048"),
                    ("fourk", "1"),
                ]),
            )
            .await?;
        parse_playback(&value, bvid, cid, quality)
    }
    /// Dedicated media client: account cookies must never go to CDN hosts.
    pub fn media_client() -> Result<Client> {
        Ok(Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(20))
            .build()?)
    }
}
fn params(items: &[(&str, &str)]) -> BTreeMap<String, String> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().into()
}
fn valid_bvid(value: &str) -> bool {
    value.len() == 12 && value.starts_with("BV") && value.bytes().all(|b| b.is_ascii_alphanumeric())
}
pub fn video_id(input: &str) -> Option<String> {
    let value = input.trim();
    if valid_bvid(value) {
        return Some(value.into());
    }
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("www.bilibili.com" | "bilibili.com" | "m.bilibili.com")
        )
    {
        return None;
    }
    let segments: Vec<_> = url.path_segments()?.collect();
    (segments.first() == Some(&"video"))
        .then(|| segments.get(1).copied())
        .flatten()
        .filter(|v| valid_bvid(v))
        .map(str::to_owned)
}
fn checked(value: &Value) -> Result<&Value> {
    if value["data"]
        .get("v_voucher")
        .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        || matches!(value["code"].as_i64(), Some(-352 | -412))
    {
        bail!(
            "B站要求安全验证，请在官网验证或登录后重试 / Bilibili requires verification; verify on the website or sign in and retry"
        );
    }
    match value["code"].as_i64() {
        Some(0) => Ok(&value["data"]),
        Some(-101) => bail!("登录已过期，请重新扫码 / Session expired; scan to sign in again"),
        Some(code) => bail!("B站请求失败 / Bilibili API error {code}"),
        _ => bail!("B站响应格式无效 / Invalid Bilibili response"),
    }
}
fn clean_title(value: &str) -> String {
    static TAGS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let plain = TAGS
        .get_or_init(|| regex::Regex::new("<[^>]*>").unwrap())
        .replace_all(value, "");
    quick_xml::escape::unescape(&plain)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| plain.into_owned())
}

/// The API can return both its native field and the normalized field together.
/// Prefer a nonempty native field; null/empty aliases retain the fallback value.
fn search_row<T: serde::de::DeserializeOwned>(value: &Value, fields: &[(&str, &str)]) -> Result<T> {
    let mut value = value.clone();
    if let Some(row) = value.as_object_mut() {
        for &(native, normalized) in fields {
            if let Some(field) = row.remove(native)
                && !field.is_null()
                && !field.as_str().is_some_and(|s| s.trim().is_empty())
            {
                row.insert(normalized.into(), field);
            }
        }
    }
    Ok(serde_json::from_value(value)?)
}
fn parse_search(value: &Value, kind: SearchKind) -> Result<SearchPage> {
    let data = checked(value)?;
    let rows: &[Value] = match data.get("result") {
        Some(Value::Array(rows)) => rows,
        None | Some(Value::Null) if data["numResults"].as_u64() == Some(0) => &[],
        _ => bail!("无效的搜索结果 / Invalid search results"),
    };
    let mut videos = Vec::new();
    let mut uploaders = Vec::new();
    let mut collect = |rows: &[Value], kind: SearchKind| -> Result<()> {
        for row in rows {
            if kind == SearchKind::Uploader {
                let mut uploader: Uploader = search_row(row, &[("upic", "avatar")])?;
                uploader.uname = clean_title(&uploader.uname);
                uploader.usign = clean_title(&uploader.usign);
                if uploader.mid > 0 {
                    uploaders.push(uploader);
                }
            } else if let Some(video) = parse_video(row)? {
                videos.push(video);
            }
        }
        Ok(())
    };
    if kind == SearchKind::All {
        for group in rows {
            let kind = match group["result_type"].as_str() {
                Some("video") => SearchKind::Video,
                Some("bili_user") => SearchKind::Uploader,
                _ => continue,
            };
            let items: &[Value] = match group.get("data") {
                Some(Value::Array(rows)) => rows,
                None | Some(Value::Null) => &[],
                _ => bail!("无效的搜索分组 / Invalid search group"),
            };
            collect(items, kind)?;
        }
    } else {
        collect(rows, kind)?;
    }
    let page = data["page"].as_u64().unwrap_or(1);
    let pages = data["numPages"].as_u64().unwrap_or(page);
    Ok(SearchPage {
        videos,
        uploaders,
        has_more: page < pages,
    })
}
fn parse_video(value: &Value) -> Result<Option<Video>> {
    let video: Video = search_row(
        value,
        &[
            ("author", "uploader"),
            ("length", "duration"),
            ("pic", "cover"),
            ("play", "views"),
            // Creator uploads use `created`; search results use `pubdate`.
            ("created", "published"),
            ("pubdate", "published"),
        ],
    )?;
    Ok(valid_bvid(&video.bvid).then(|| Video {
        title: clean_title(&video.title),
        ..video
    }))
}
fn parse_uploads(value: &Value, page: usize) -> Result<SearchPage> {
    let data = checked(value)?;
    let rows = data["list"]["vlist"]
        .as_array()
        .ok_or_else(|| anyhow!("无效的投稿列表 / Invalid uploads response"))?;
    let videos = rows
        .iter()
        .map(parse_video)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    let total = data["page"]["count"].as_u64().unwrap_or(0);
    let size = data["page"]["ps"].as_u64().filter(|s| *s > 0).unwrap_or(20);
    Ok(SearchPage {
        videos,
        uploaders: vec![],
        has_more: (page as u64).saturating_mul(size) < total,
    })
}
/// Restrict media URLs, including backups, before using them as network inputs.
pub fn cdn_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| anyhow!("无效的视频地址 / Invalid video URL"))?;
    let host = url.host_str().unwrap_or_default();
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && [
                "bilivideo.com",
                "bilivideo.cn",
                "bilivideo.net",
                "akamaized.net"
            ]
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}"))),
        "不支持的视频服务器 / Unsupported video server"
    );
    Ok(url)
}
fn stream(value: &Value) -> Result<Stream> {
    let main = value
        .get("baseUrl")
        .or_else(|| value.get("base_url"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("没有播放地址 / Missing playback URL"))?;
    let mut candidates = vec![main];
    if let Some(backups) = value
        .get("backupUrl")
        .or_else(|| value.get("backup_url"))
        .and_then(Value::as_array)
    {
        candidates.extend(backups.iter().filter_map(Value::as_str).take(4));
    }
    // PCDN primary URLs can use unsupported ports/hosts. Prefer the ordinary
    // HTTPS CDN backup supplied by the service rather than rejecting the video.
    let urls: Vec<_> = candidates
        .into_iter()
        .filter_map(|v| cdn_url(v).ok())
        .collect();
    ensure!(
        !urls.is_empty(),
        "没有兼容的视频服务器 / No compatible video server"
    );
    Ok(Stream {
        urls,
        codec: text(value, "codecs"),
    })
}
fn parse_playback(value: &Value, bvid: &str, cid: u64, requested: u32) -> Result<Playback> {
    let data = checked(value)?;
    let formats = data["support_formats"].as_array();
    let mut qualities = Vec::new();
    let dash = &data["dash"];
    let videos = dash["video"].as_array().ok_or_else(|| {
        anyhow!("此视频暂不提供兼容的音视频流 / No compatible video streams available")
    })?;
    // Only expose AVC streams the current Windows/macOS backend can decode.
    let avc: Vec<_> = videos
        .iter()
        .filter(|v| v["codecid"] == 7 || v["codecs"].as_str().is_some_and(|s| s.starts_with("avc")))
        .filter_map(|v| {
            let id = u32::try_from(v["id"].as_u64()?).ok().filter(|id| *id > 0)?;
            Some((id, stream(v).ok()?))
        })
        .collect();
    for (id, _) in &avc {
        if qualities.iter().any(|q: &Quality| q.id == *id) {
            continue;
        }
        let label = formats
            .and_then(|f| f.iter().find(|f| f["quality"] == *id))
            .map(|f| {
                ["new_description", "display_desc", "description"]
                    .iter()
                    .find_map(|k| f[k].as_str())
                    .unwrap_or("")
                    .to_owned()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("{id}"));
        qualities.push(Quality { id: *id, label });
    }
    qualities.sort_by_key(|q| std::cmp::Reverse(q.id));
    let actual = data["quality"].as_u64().unwrap_or(0) as u32;
    // Respect the server's account limit and the requested ceiling. In
    // particular, never take an arbitrary first DASH entry (e.g. 4K) when
    // 1080P is unavailable; choose 720P, then a lower available AVC stream.
    let ceiling = if actual > 0 {
        requested.min(actual)
    } else {
        requested
    };
    let (quality, video) = avc
        .iter()
        .filter(|(id, _)| *id <= ceiling)
        .max_by_key(|(id, _)| *id)
        .ok_or_else(|| anyhow!("该画质无 H.264 视频，请尝试其他画质 / No H.264 video available"))?;
    let audio = dash["audio"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .filter(|a| a["codecs"].as_str().is_some_and(|c| c.starts_with("mp4a")))
                .max_by_key(|a| a["bandwidth"].as_u64().unwrap_or(0))
        })
        .map(stream)
        .transpose()?;
    ensure!(
        dash["audio"].as_array().is_none_or(|a| a.is_empty()) || audio.is_some(),
        "不支持此音频编码 / Unsupported audio codec"
    );
    Ok(Playback {
        bvid: bvid.into(),
        cid,
        quality: *quality,
        duration: data["timelength"]
            .as_f64()
            .filter(|d| *d > 0.0)
            .map(|d| d / 1000.0)
            .or_else(|| dash["duration"].as_f64())
            .unwrap_or(0.0),
        qualities,
        video: video.clone(),
        audio,
    })
}
fn wbi_key(value: &Value) -> Result<String> {
    let key: String = ["img_url", "sub_url"]
        .iter()
        .filter_map(|k| value[k].as_str())
        .filter_map(|v| v.rsplit('/').next()?.split('.').next())
        .collect();
    ensure!(
        key.len() == 64 && key.is_ascii(),
        "无法获取请求签名 / Cannot obtain request signature"
    );
    let order = [
        46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19,
        29, 28, 14, 39, 12, 38, 41, 13,
    ];
    Ok(order.iter().map(|&i| key.as_bytes()[i] as char).collect())
}
fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    encoded
}
fn sign(
    mut params: BTreeMap<String, String>,
    key: &str,
    timestamp: u64,
) -> BTreeMap<String, String> {
    params.insert("wts".into(), timestamp.to_string());
    let query = params
        .iter()
        .map(|(k, v)| {
            let filtered: String = v.chars().filter(|c| !"!'()*".contains(*c)).collect();
            format!("{}={}", encode(k), encode(&filtered))
        })
        .collect::<Vec<_>>()
        .join("&");
    params.insert(
        "w_rid".into(),
        format!("{:x}", md5::compute(format!("{query}{key}"))),
    );
    params
}

#[cfg(test)]
mod tests;
