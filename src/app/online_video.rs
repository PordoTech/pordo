use super::*;
use crate::jobs::Job;
use crate::{
    bilibili::{
        self, Account, Bilibili, Detail, LoginPoll, Playback, QrCode, SearchKind, SearchPage,
        Uploader,
    },
    credentials::{SystemVault, Vault},
};
mod cards;
mod controls;

#[derive(Clone)]
struct Active {
    media_path: PathBuf,
    detail: Detail,
    part: usize,
    playback: Playback,
}
struct Prepared {
    source: bilibili::Source,
    path: String,
    active: Active,
    resume: Option<f64>,
    keep_position: bool,
}

struct Playing {
    path: String,
    _source: bilibili::Source,
}

#[derive(Default)]
pub(super) struct State {
    api: Option<Arc<Bilibili>>,
    account: Option<Account>,
    init: Option<Job<(Arc<Bilibili>, Option<Account>)>>,
    save: Option<Job<()>>,
    qr_job: Option<Job<QrCode>>,
    qr: Option<(QrCode, qrcodegen::QrCode, Instant)>,
    poll: Option<Job<LoginPoll>>,
    poll_at: Option<Instant>,
    keyword: String,
    searched: String,
    kind: SearchKind,
    searched_kind: SearchKind,
    uploaders: Vec<Uploader>,
    creator: Option<Uploader>,
    pending_creator: Option<Uploader>,
    uploads: Vec<bilibili::Video>,
    uploads_job: Option<Job<SearchPage>>,
    uploads_page: usize,
    uploads_more: bool,
    search: Option<Job<SearchPage>>,
    videos: Vec<bilibili::Video>,
    page: usize,
    has_more: bool,
    detail_job: Option<Job<Detail>>,
    detail: Option<Detail>,
    part: usize,
    resolve: Option<Job<Playback>>,
    ready: Option<Playback>,
    prepare: Option<Job<Prepared>>,
    playing: Option<Playing>,
    active: Option<Active>,
    status: String,
    auth_status: String,
    auto_play: bool,
    requested_page: usize,
    resume: Option<f64>,
    keep_position: bool,
    pub(in crate::app) previews: cards::Previews,
}
impl State {
    pub(super) fn stop(&mut self) {
        self.cancel_discovery();
        self.playing = None;
        self.active = None;
    }
    pub(super) fn record_path(&self) -> Option<String> {
        self.playing.as_ref().map(|p| p.path.clone())
    }
    pub(super) fn cancel_discovery(&mut self) {
        self.pending_creator = None;
        self.previews.cancel_pending();
        self.search = None;
        self.uploads_job = None;
        self.detail_job = None;
        self.resolve = None;
        self.prepare = None;
        self.auto_play = false;
        self.keep_position = false;
    }
    fn begin_video(&mut self, job: Job<Detail>, page: usize, resume: Option<f64>) {
        self.cancel_discovery();
        self.detail = None;
        self.ready = None;
        self.status.clear();
        self.requested_page = page;
        self.resume = resume;
        self.auto_play = true;
        self.detail_job = Some(job);
    }
    fn close_login(&mut self) {
        self.qr = None;
        self.qr_job = None;
        self.poll = None;
        self.poll_at = None;
    }
    fn busy(&self) -> bool {
        self.init.is_some()
            || self.search.is_some()
            || self.uploads_job.is_some()
            || self.detail_job.is_some()
            || self.resolve.is_some()
            || self.prepare.is_some()
    }
}
impl App {
    pub(in crate::app) fn bilibili_danmaku_source(&self) -> Option<(Arc<Bilibili>, u64)> {
        if !self.bilibili_controls_available() {
            return None;
        }
        let active = self.online_video.active.as_ref()?;
        Some((
            self.online_video.api.clone()?,
            active.detail.parts.get(active.part)?.cid,
        ))
    }
    pub(in crate::app) fn bilibili_cover_url(&self) -> Option<String> {
        self.bilibili_controls_available()
            .then(|| self.online_video.active.as_ref()?.detail.cover.clone())
            .flatten()
    }
    fn favorite_creator_ids(&self) -> std::collections::HashSet<u64> {
        self.library
            .creators
            .iter()
            .filter(|r| r.favorite)
            .map(|r| r.uploader.mid)
            .collect()
    }
    fn toggle_creator_favorite(&mut self, creator: &Uploader) {
        if let Err(error) = self.library.toggle_creator(creator) {
            self.error = Some(error.to_string());
        }
    }
    fn open_bilibili_creator(&mut self, creator: Uploader) {
        self.show_online_video();
        self.online_video.cancel_discovery();
        self.online_video.auto_play = false;
        self.online_video.detail = None;
        self.online_video.ready = None;
        self.online_video.uploads.clear();
        self.online_video.creator = Some(creator.clone());
        self.online_video.status.clear();
        if self.online_video.api.is_some() {
            self.creator_uploads(creator, 1);
        } else {
            self.online_video.pending_creator = Some(creator);
        }
    }
    pub(super) fn video_creator_favorites(&mut self, ui: &mut Ui, query: &str) -> bool {
        let creators = self.library.favorite_creators(query);
        if creators.is_empty() {
            return false;
        }
        let l = self.settings.language;
        ui.label(
            RichText::new(l.text("收藏UP主", "Favorite creators"))
                .size(20.0)
                .strong(),
        );
        ui.add_space(10.0);
        let favorites = self.favorite_creator_ids();
        match cards::creators(
            ui,
            &mut self.online_video.previews,
            &creators,
            l,
            &favorites,
        ) {
            Some(cards::Action::Creator(creator)) => self.open_bilibili_creator(creator),
            Some(cards::Action::FavoriteCreator(creator)) => self.toggle_creator_favorite(&creator),
            _ => {}
        }
        ui.add_space(16.0);
        true
    }
    pub(super) fn show_online_video(&mut self) {
        if self.workspace.page != workspace::Page::Video {
            self.navigate(workspace::Page::Video);
        }
        self.workspace.video_tab = 1;
        self.ensure_video_api();
    }
    fn ensure_video_api(&mut self) {
        let state = &mut self.online_video;
        if state.api.is_none() && state.init.is_none() {
            state.init = Some(Job::spawn(|_| async move {
                let saved =
                    tokio::task::spawn_blocking(|| SystemVault.get(&Settings::bilibili_target()))
                        .await??;
                let saved = saved.map(|s| serde_json::from_str::<bilibili::Session>(&s)).transpose()
                    .map_err(|_| anyhow::anyhow!("已保存的B站会话无效，请退出后重试 / Invalid saved Bilibili session; sign out and retry"))?;
                let api = Arc::new(Bilibili::new(saved.as_ref())?);
                let account = if saved.is_some() {
                    api.account().await.ok()
                } else {
                    None
                };
                Ok((api, account))
            }));
        }
    }
    pub(super) fn open_bilibili_record(
        &mut self,
        bvid: String,
        record: &library::Record,
        resume: bool,
    ) {
        self.show_online_video();
        self.online_video.requested_page = reqwest::Url::parse(&record.path)
            .ok()
            .and_then(|u| {
                u.query_pairs()
                    .find(|(k, _)| k == "p")
                    .and_then(|(_, v)| v.parse().ok())
            })
            .unwrap_or(1);
        self.online_video.resume = resume.then(|| record.resume_at()).flatten();
        self.online_video.auto_play = true;
        // The initializer is asynchronous; retain a stable BV ID until it finishes.
        self.online_video.keyword = bvid;
        if self.online_video.api.is_some() {
            self.video_search(1);
        }
    }
    fn video_search(&mut self, page: usize) {
        let Some(api) = self.online_video.api.clone() else {
            return;
        };
        let state = &mut self.online_video;
        let keyword = if page == 1 {
            state.keyword.trim().to_owned()
        } else {
            state.searched.clone()
        };
        if keyword.is_empty() {
            return;
        }
        state.status.clear();
        state.search = None;
        state.detail = None;
        state.detail_job = None;
        state.resolve = None;
        state.ready = None;
        state.prepare = None;
        let kind = if page == 1 {
            state.kind
        } else {
            state.searched_kind
        };
        state.creator = None;
        state.uploads_job = None;
        if let Some(id) =
            bilibili::video_id(&keyword).filter(|_| kind != SearchKind::Uploader || state.auto_play)
        {
            if let Ok(url) = reqwest::Url::parse(&keyword)
                && let Some(page) = url
                    .query_pairs()
                    .find(|(k, _)| k == "p")
                    .and_then(|(_, v)| v.parse().ok())
            {
                state.requested_page = page;
            }
            let job = Job::spawn(move |_| async move { api.detail(&id).await });
            state.begin_video(job, state.requested_page, state.resume);
            return;
        }
        state.search = None;
        if page == 1 {
            state.previews.cancel_pending();
            state.videos.clear();
            state.uploaders.clear();
            state.searched_kind = kind;
            state.searched = keyword.clone();
        }
        state.page = page;
        state.search = Some(Job::spawn(move |_| async move {
            api.search(&keyword, page, kind).await
        }));
    }
    fn creator_uploads(&mut self, creator: Uploader, page: usize) {
        let Some(api) = self.online_video.api.clone() else {
            return;
        };
        let state = &mut self.online_video;
        state.search = None;
        state.detail_job = None;
        state.resolve = None;
        state.prepare = None;
        state.ready = None;
        state.detail = None;
        state.status.clear();
        if page == 1 {
            state.uploads.clear();
            state.creator = Some(creator.clone());
        }
        state.uploads_page = page;
        state.uploads_job = Some(Job::spawn(move |_| async move {
            api.uploads(creator.mid, page).await
        }));
    }
    fn video_resolve(&mut self, quality: u32) {
        let state = &mut self.online_video;
        let Some((api, detail)) = state.api.clone().zip(state.detail.as_ref()) else {
            return;
        };
        let Some(part) = detail.parts.get(state.part) else {
            return;
        };
        let (bvid, cid) = (detail.bvid.clone(), part.cid);
        state.status.clear();
        state.ready = None;
        state.prepare = None;
        state.resolve = Some(Job::spawn(move |_| async move {
            api.resolve(&bvid, cid, quality).await
        }));
    }
    fn play_bilibili(&mut self) {
        let state = &mut self.online_video;
        let Some((api, playback)) = state.api.clone().zip(state.ready.clone()) else {
            return;
        };
        let Some(detail) = &state.detail else { return };
        let path = format!(
            "https://www.bilibili.com/video/{}?p={}",
            detail.bvid, detail.parts[state.part].page
        );
        state.status.clear();
        let detail = detail.clone();
        let part = state.part;
        let resume = state.resume;
        let keep_position = state.keep_position;
        state.prepare = Some(Job::spawn(move |_| async move {
            let source = bilibili::start(api, playback.clone()).await?;
            let active = Active {
                media_path: PathBuf::from(&source.video),
                detail,
                part,
                playback,
            };
            Ok(Prepared {
                source,
                path,
                active,
                resume,
                keep_position,
            })
        }));
    }
    fn video_sign_out(&mut self) {
        // Drop playback before its loopback transport, including in-flight reads.
        if self.online_video.playing.is_some() {
            self.update_library();
            self.player = None;
            self.source = None;
            self.video.clear();
            self.media_info = None;
            self.loading = false;
        }
        let state = &mut self.online_video;
        state.stop();
        state.close_login();
        state.init = None;
        state.account = None;
        state.ready = None;
        state.detail = None;
        state.api = Bilibili::new(None).ok().map(Arc::new);
        state.save = Some(Job::spawn(|_| async {
            tokio::task::spawn_blocking(|| SystemVault.set(&Settings::bilibili_target(), None))
                .await??;
            Ok(())
        }));
        state.auth_status = self
            .settings
            .language
            .text("已退出登录", "Signed out")
            .into();
    }
    pub(super) fn poll_online_video(&mut self, ctx: &egui::Context) {
        self.online_video.previews.poll(ctx);
        let auth_visible = self.settings_open && self.preferences_tab == PrefTab::VideoSources;
        let mut resolve = false;
        let mut play = false;
        let mut pending_record = false;
        let mut pending_creator = None;
        let state = &mut self.online_video;
        if !auth_visible {
            state.close_login();
        }
        if let Some(result) = state.init.as_ref().and_then(Job::poll) {
            state.init = None;
            match result {
                Ok((api, account)) => {
                    state.api = Some(api);
                    state.account = account;
                    pending_record = state.auto_play;
                    pending_creator = state.pending_creator.take();
                }
                Err(e) => state.auth_status = e.to_string(),
            }
        }
        if let Some(result) = state.save.as_ref().and_then(Job::poll) {
            state.save = None;
            if let Err(e) = result {
                state.auth_status = format!("无法保存账号状态 / Cannot save account state: {e}");
            }
        }
        if let Some(result) = state.qr_job.as_ref().and_then(Job::poll) {
            state.qr_job = None;
            match result {
                Ok(qr) => {
                    match qrcodegen::QrCode::encode_text(&qr.url, qrcodegen::QrCodeEcc::Medium) {
                        Ok(image) => {
                            state.qr = Some((qr, image, Instant::now()));
                            state.poll_at = Some(Instant::now());
                        }
                        Err(_) => {
                            state.auth_status = "无法生成二维码 / Cannot generate QR code".into()
                        }
                    }
                }
                Err(e) => state.auth_status = e.to_string(),
            }
        }
        if state
            .qr
            .as_ref()
            .is_some_and(|(_, _, start)| start.elapsed() >= Duration::from_secs(180))
        {
            state.qr = None;
            state.poll = None;
            state.poll_at = None;
            state.auth_status =
                "二维码已过期，请重新获取 / QR code expired; request a new one".into();
        }
        if let Some(result) = state.poll.as_ref().and_then(Job::poll) {
            state.poll = None;
            state.poll_at = Some(Instant::now() + Duration::from_secs(2));
            match result {
                Ok(LoginPoll::SignedIn(session, account)) => {
                    self.danmaku = Default::default();
                    state.account = Some(account);
                    state.qr = None;
                    state.poll_at = None;
                    state.auth_status = "登录成功 / Signed in".into();
                    state.ready = None;
                    state.resolve = None;
                    resolve = state.detail.is_some();
                    state.save = Some(Job::spawn(move |_| async move {
                        let value = serde_json::to_string(&session)?;
                        tokio::task::spawn_blocking(move || {
                            SystemVault.set(&Settings::bilibili_target(), Some(&value))
                        })
                        .await??;
                        Ok(())
                    }));
                }
                Ok(LoginPoll::Expired) => {
                    state.qr = None;
                    state.poll_at = None;
                    state.auth_status = "二维码已过期 / QR code expired".into();
                }
                Ok(LoginPoll::Waiting) => state.auth_status.clear(),
                Ok(LoginPoll::Confirm) => {
                    state.auth_status = self
                        .settings
                        .language
                        .text("请在手机上确认登录", "Confirm sign-in on your phone")
                        .into()
                }
                Err(e) => state.auth_status = e.to_string(),
            }
        }
        if auth_visible
            && state.poll.is_none()
            && state.poll_at.is_some_and(|at| Instant::now() >= at)
            && let Some(((qr, _, _), api)) = state.qr.as_ref().zip(state.api.clone())
        {
            let key = qr.qrcode_key.clone();
            state.poll = Some(Job::spawn(
                move |_| async move { api.poll_login(&key).await },
            ));
        }
        if let Some(result) = state.search.as_ref().and_then(Job::poll) {
            state.search = None;
            match result {
                Ok(page) => {
                    state.has_more = page.has_more;
                    state.videos.extend(page.videos);
                    state.uploaders.extend(page.uploaders);
                    if state.videos.is_empty() && state.uploaders.is_empty() {
                        state.status = "没有搜索结果 / No results".into();
                    }
                }
                Err(e) => {
                    state.status = e.to_string();
                    state.has_more = false;
                }
            }
        }
        if let Some(result) = state.uploads_job.as_ref().and_then(Job::poll) {
            state.uploads_job = None;
            match result {
                Ok(page) => {
                    state.uploads_more = page.has_more;
                    state.uploads.extend(page.videos);
                    if state.uploads.is_empty() {
                        state.status = "暂无投稿视频 / No uploaded videos".into();
                    }
                }
                Err(error) => {
                    state.status = error.to_string();
                    state.uploads_more = false;
                }
            }
        }
        if let Some(result) = state.detail_job.as_ref().and_then(Job::poll) {
            state.detail_job = None;
            match result {
                Ok(detail) => {
                    state.part = detail
                        .parts
                        .iter()
                        .position(|p| p.page as usize == state.requested_page)
                        .unwrap_or(0);
                    state.detail = Some(detail);
                    resolve = true;
                }
                Err(e) => {
                    state.status = e.to_string();
                    state.auto_play = false;
                    if self.workspace.page == workspace::Page::Playing {
                        self.error = Some(e.to_string());
                    }
                }
            }
        }
        if let Some(result) = state.resolve.as_ref().and_then(Job::poll) {
            state.resolve = None;
            match result {
                Ok(playback) => {
                    state.ready = Some(playback);
                    play = state.auto_play;
                    state.auto_play = false;
                }
                Err(e) => {
                    state.status = e.to_string();
                    state.auto_play = false;
                    if self.workspace.page == workspace::Page::Playing {
                        self.error = Some(e.to_string());
                    }
                }
            }
        }
        let prepared = state.prepare.as_ref().and_then(Job::poll);
        if prepared.is_some() {
            state.prepare = None;
        }
        if let Some(result) = prepared {
            match result {
                Ok(prepared) => {
                    let Prepared {
                        source,
                        path,
                        active,
                        resume,
                        keep_position,
                    } = prepared;
                    let title = if active.detail.parts.len() > 1 {
                        format!(
                            "{} · {}",
                            active.detail.title, active.detail.parts[active.part].part
                        )
                    } else {
                        active.detail.title.clone()
                    };
                    let (resume, playing) = if keep_position {
                        self.player
                            .as_ref()
                            .map(|p| {
                                (
                                    Some(
                                        self.pending_seek
                                            .map_or_else(|| p.position(), |(_, pos)| pos),
                                    ),
                                    p.is_playing(),
                                )
                            })
                            .unwrap_or((resume, true))
                    } else {
                        (resume, true)
                    };
                    let subtitles = keep_position.then(|| {
                        (
                            std::mem::take(&mut self.subtitles),
                            std::mem::take(&mut self.subtitle_skipped),
                            std::mem::take(&mut self.subtitle_metrics),
                        )
                    });
                    self.set_active_queue(false);
                    self.queue = Default::default();
                    self.workspace.resume = None;
                    self.torrent_job = None;
                    self.load_media_inputs(
                        source.video.clone(),
                        false,
                        None,
                        Some((source.audio.clone(), source.duration)),
                    );
                    self.title = title;
                    self.online_video.playing = Some(Playing {
                        path: path.clone(),
                        _source: source,
                    });
                    self.online_video.active = Some(active);
                    self.online_video.resume = None;
                    self.workspace.resume = resume.map(|position| (path, position));
                    self.online_video.cancel_discovery();
                    if let Some(player) = &self.player {
                        player.set_playing(playing);
                    }
                    if let Some((subtitles, skipped, metrics)) = subtitles {
                        self.subtitles = subtitles;
                        self.subtitle_skipped = skipped;
                        self.subtitle_metrics = metrics;
                    }
                }
                Err(e) => {
                    self.online_video.status = e.to_string();
                    if self.workspace.page == workspace::Page::Playing {
                        self.error = Some(e.to_string());
                    }
                }
            }
        }
        if resolve {
            self.video_resolve(bilibili::DEFAULT_QUALITY);
        }
        if play {
            self.play_bilibili();
        }
        if pending_record {
            self.video_search(1);
        }
        if let Some(creator) = pending_creator {
            self.creator_uploads(creator, 1);
        }
        if auth_visible
            || (self.workspace.page == workspace::Page::Video && self.workspace.video_tab == 1)
            || self.online_video.prepare.is_some()
            || self.online_video.resolve.is_some()
            || self.online_video.save.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    pub(super) fn preferences_video_sources(&mut self, ui: &mut Ui) {
        self.ensure_video_api();
        let l = self.settings.language;
        ui.heading(l.text("视频源", "Video sources"));
        preferences::form_row(ui, l.text("视频源", "Video source"), l, |ui| {
            egui::ComboBox::from_id_salt("settings-video-source")
                .selected_text(l.text("哔哩哔哩", "Bilibili"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.preferences_draft.video.source,
                        "bilibili".into(),
                        l.text("哔哩哔哩", "Bilibili"),
                    );
                });
        });
        ui.separator();
        let mut signout = false;
        let mut login = false;
        ui.horizontal(|ui| {
            if let Some(account) = &self.online_video.account {
                ui.label(format!("{} · UID {}", account.uname, account.mid));
            } else {
                ui.label(l.text("未登录", "Not signed in"));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.online_video.account.is_some() {
                    signout = ui
                        .add_enabled(
                            self.online_video.save.is_none(),
                            egui::Button::new(l.text("退出登录", "Sign out")),
                        )
                        .clicked();
                } else {
                    login = ui
                        .add_enabled(
                            self.online_video.api.is_some()
                                && self.online_video.save.is_none()
                                && self.online_video.qr_job.is_none(),
                            egui::Button::new(l.text("扫码登录", "Sign in with QR")),
                        )
                        .clicked();
                }
            });
        });
        if signout {
            self.video_sign_out();
        }
        if login && let Some(api) = self.online_video.api.clone() {
            self.online_video.auth_status.clear();
            self.online_video.qr = None;
            self.online_video.poll = None;
            self.online_video.poll_at = None;
            self.online_video.qr_job =
                Some(Job::spawn(move |_| async move { api.qr_code().await }));
        }
        if let Some((_, code, _)) = &self.online_video.qr {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(196.0, 196.0), Sense::hover());
                ui.painter().rect_filled(rect, 0, Color32::WHITE);
                let unit = rect.width() / (code.size() + 8) as f32;
                for y in 0..code.size() {
                    for x in 0..code.size() {
                        if code.get_module(x, y) {
                            ui.painter().rect_filled(
                                Rect::from_min_size(
                                    rect.min + vec2((x + 4) as f32 * unit, (y + 4) as f32 * unit),
                                    vec2(unit, unit),
                                ),
                                0,
                                Color32::BLACK,
                            );
                        }
                    }
                }
                ui.label(if self.online_video.auth_status.is_empty() {
                    l.text(
                        "用官方哔哩哔哩 App 扫码，\n并在手机上确认。",
                        "Scan with the official Bilibili app\nand confirm on your phone.",
                    )
                } else {
                    &self.online_video.auth_status
                });
            });
        }
        ui.separator();

        if self.online_video.init.is_some()
            || self.online_video.qr_job.is_some()
            || self.online_video.save.is_some()
        {
            ui.spinner();
        }
        if self.online_video.qr.is_none() && !self.online_video.auth_status.is_empty() {
            ui.label(&self.online_video.auth_status);
        }
    }

    pub(super) fn online_video_contents(&mut self, ui: &mut Ui) {
        self.ensure_video_api();
        let l = self.settings.language;
        let mut search = false;
        let previous_kind = self.online_video.kind;
        let previous_source = self.settings.video.source.clone();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let dropdown_width = ((ui.available_width() - 120.0 - design::CONTROL_HEIGHT - 24.0)
                / 2.0)
                .clamp(80.0, 136.0);
            design::dropdown_ui(
                ui,
                "video_provider",
                dropdown_width,
                l.text("哔哩哔哩", "Bilibili"),
                |ui| {
                    design::menu_value(
                        ui,
                        &mut self.settings.video.source,
                        "bilibili".into(),
                        l.text("哔哩哔哩", "Bilibili"),
                    );
                },
            );
            let kind_label = match self.online_video.kind {
                SearchKind::All => l.text("全部", "All"),
                SearchKind::Video => l.text("视频", "Videos"),
                SearchKind::Uploader => l.text("UP 主", "Creators"),
            };
            design::dropdown_ui(ui, "video_search_kind", dropdown_width, kind_label, |ui| {
                for (kind, label) in [
                    (SearchKind::All, l.text("全部", "All")),
                    (SearchKind::Video, l.text("视频", "Videos")),
                    (SearchKind::Uploader, l.text("UP 主", "Creators")),
                ] {
                    design::menu_value(ui, &mut self.online_video.kind, kind, label);
                }
            });
            let input_width = (ui.available_width() - design::CONTROL_HEIGHT - 8.0).max(120.0);
            let response = design::search_field(
                ui,
                &mut self.online_video.keyword,
                input_width,
                if self.online_video.kind == SearchKind::Uploader {
                    l.text("UP主名称", "Creator name")
                } else {
                    l.text("关键词、BV号或视频链接", "Keywords, BV ID or video URL")
                },
            );
            search = ui
                .add_enabled_ui(
                    self.online_video.api.is_some() && self.online_video.init.is_none(),
                    |ui| {
                        design::primary_icon_button(
                            ui,
                            icons::MAGNIFYING_GLASS,
                            l.text("搜索", "Search"),
                        )
                    },
                )
                .inner
                .clicked()
                || response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        });
        if previous_source != self.settings.video.source {
            self.preferences_draft.video.source = self.settings.video.source.clone();
            if let Err(error) = self.settings.save() {
                self.error = Some(error.to_string());
            }
        }
        if previous_kind != self.online_video.kind {
            self.online_video.cancel_discovery();
            self.online_video.videos.clear();
            self.online_video.uploaders.clear();
            self.online_video.creator = None;
            self.online_video.detail = None;
            self.online_video.ready = None;
            self.online_video.has_more = false;
            self.online_video.status.clear();
        }
        if search
            || (previous_kind != self.online_video.kind
                && !self.online_video.keyword.trim().is_empty())
        {
            self.online_video.auto_play = false;
            self.online_video.resume = None;
            self.online_video.requested_page = 1;
            self.video_search(1);
        }
        if self.online_video.busy() || self.online_video.qr_job.is_some() {
            ui.spinner();
        }
        if !self.online_video.status.is_empty() {
            ui.label(&self.online_video.status);
        }
        ui.add_space(16.0);
        if let Some(creator) = self.online_video.creator.clone() {
            if design::back_button(ui, l.text("返回搜索结果", "Back to results")).clicked() {
                self.online_video.creator = None;
                self.online_video.uploads_job = None;
                self.online_video.status.clear();
                return;
            }
            ui.horizontal(|ui| {
                ui.heading(&creator.uname);
                let favorite = self.library.creator_favorite(creator.mid);
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::splat(design::CONTROL_HEIGHT), Sense::hover());
                if design::favorite_at(
                    ui,
                    ui.id().with(("creator-favorite", creator.mid)),
                    rect,
                    favorite,
                )
                .on_hover_text(if favorite {
                    l.text("取消收藏UP主", "Remove creator from favorites")
                } else {
                    l.text("收藏UP主", "Favorite creator")
                })
                .clicked()
                {
                    self.toggle_creator_favorite(&creator);
                }
            });
        }
        let show_more = if self.online_video.creator.is_some() {
            self.online_video.uploads_more || !self.online_video.status.is_empty()
        } else {
            self.online_video.has_more
        };
        let favorites = self.favorite_creator_ids();
        let action = design::scroll()
            .max_height((ui.available_height() - if show_more { 48.0 } else { 0.0 }).max(120.0))
            .show(ui, |ui| {
                cards::results(ui, &mut self.online_video, l, &favorites)
            })
            .inner;
        match action {
            Some(cards::Action::Creator(creator)) => self.open_bilibili_creator(creator),
            Some(cards::Action::FavoriteCreator(creator)) => self.toggle_creator_favorite(&creator),
            Some(cards::Action::MoreCreators) => {
                self.online_video.kind = SearchKind::Uploader;
                self.video_search(1);
            }
            Some(cards::Action::Video(id)) => {
                if let Some(api) = self.online_video.api.clone() {
                    let job = Job::spawn(move |_| async move { api.detail(&id).await });
                    self.online_video.begin_video(job, 1, None);
                }
            }
            None => {}
        }
        if let Some(creator) = self.online_video.creator.clone() {
            if self.online_video.uploads_job.is_none()
                && (self.online_video.uploads_more || !self.online_video.status.is_empty())
                && ui
                    .button(if self.online_video.uploads_more {
                        l.text("加载更多投稿", "More uploads")
                    } else {
                        l.text("重试获取投稿", "Retry loading uploads")
                    })
                    .clicked()
            {
                let page = if self.online_video.uploads_more {
                    self.online_video.uploads_page + 1
                } else {
                    self.online_video.uploads_page.max(1)
                };
                self.creator_uploads(creator, page);
            }
        } else if self.online_video.has_more
            && self.online_video.search.is_none()
            && ui.button(l.text("加载更多", "Load more")).clicked()
        {
            self.video_search(self.online_video.page + 1);
        }
    }
}

#[cfg(test)]
mod tests;
