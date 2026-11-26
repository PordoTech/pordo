use super::*;
use crate::sync::{
    self,
    model::Snapshot,
    webdav::{BILIBILI_ACCOUNT, KOMIIC_ACCOUNT, KOMIIC_SESSION},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct State {
    video_ready: bool,
    configured: Option<sync::Settings>,
    journal: Option<sync::Journal>,
    job: Option<crate::jobs::Job<sync::webdav::Report>>,
    probe: Option<crate::jobs::Job<()>>,
    requested_secrets: BTreeMap<String, String>,
    requested_accounts: BTreeMap<String, String>,
    lyrics: cloud_lyrics::State,
    path: PathBuf,
    capture_after: Instant,
    startup: bool,
    urgent: bool,
    close_ready: bool,
    probe_settings: Option<sync::Settings>,
    changed: Option<Instant>,
    session: Option<String>,
    pub status: String,
    pub failed: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            video_ready: false,
            configured: None,
            journal: None,
            job: None,
            probe: None,
            requested_secrets: BTreeMap::new(),
            requested_accounts: BTreeMap::new(),
            lyrics: Default::default(),
            path: PathBuf::new(),
            capture_after: Instant::now(),
            startup: false,
            urgent: false,
            close_ready: false,
            probe_settings: None,
            changed: None,
            session: None,
            status: String::new(),
            failed: false,
        }
    }
}
fn settings_snapshot(settings: &Settings) -> Snapshot {
    let mut data = Snapshot::new();
    for p in &settings.ai.providers {
        data.insert(format!("ai/{}/name", p.id), json!(p.name));
        data.insert(format!("ai/{}/url", p.id), json!(p.base_url));
        for model in &p.models {
            data.insert(
                format!(
                    "ai/{}/model/{}",
                    p.id,
                    crate::comics::digest(model.as_bytes())
                ),
                json!(model),
            );
        }
    }
    if let Value::Object(options) = serde_json::to_value(&settings.llm).unwrap_or_default() {
        for (key, value) in options {
            data.insert(format!("subtitle/{key}"), value);
        }
    }
    data.insert(
        "subtitle/concurrency".into(),
        json!(settings.subtitle_concurrency),
    );
    data.insert(
        "image/selected".into(),
        json!(settings.comics.image_selected),
    );
    data.insert("image/quality".into(), json!(settings.comics.image_quality));
    data.insert("image/target".into(), json!(settings.comics.target));
    data.insert("music/source".into(), json!(settings.music.source));
    data.insert("music/quality".into(), json!(settings.music.quality));
    data
}
fn secrets(settings: &Settings) -> BTreeMap<String, String> {
    settings
        .ai
        .providers
        .iter()
        .map(|p| (format!("ai/{}", p.id), p.api_key.clone()))
        .chain(std::iter::once((
            "music/chksz".into(),
            settings.music.api_key.clone(),
        )))
        .collect()
}

fn unset_secrets(
    keys: &BTreeMap<String, String>,
    journal: &sync::Journal,
    vault: &crate::credentials::FileVault,
) -> anyhow::Result<BTreeSet<String>> {
    let candidates: Vec<_> = keys
        .iter()
        .filter(|(id, key)| key.is_empty() && journal.secret_hashes.contains_key(*id))
        .map(|(id, _)| id)
        .collect();
    if candidates.is_empty() {
        return Ok(BTreeSet::new());
    }
    let stored = vault.stored_targets()?;
    Ok(candidates
        .into_iter()
        .filter(|id| !stored.contains(&Settings::credential_target(id)))
        .cloned()
        .collect())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct KomiicCredentials {
    email: String,
    password: String,
    auto_login: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct KomiicSession {
    account: String,
    cookies: crate::comics::source::KomiicSessionCookies,
}

impl App {
    pub(in crate::app) fn video_updates_cloud_ready(&self) -> bool {
        !self.settings.sync.enabled || self.cloud.video_ready
    }
    fn cloud_session(&self) -> Option<String> {
        match self.workspace.page {
            workspace::Page::Comic => self.comic.reading_session(),
            workspace::Page::Listening | workspace::Page::Playing => Some(format!(
                "media:{}",
                self.active_record_path().unwrap_or_default()
            )),
            _ => None,
        }
    }

    fn checkpoint_cloud(&mut self) {
        self.update_library();
        self.library.flush();
        self.comic.flush();
        match self.capture_cloud(true) {
            Ok(changed) => {
                self.cloud.urgent |= changed
                    || self
                        .cloud
                        .journal
                        .as_ref()
                        .is_some_and(|j| j.digest() != j.uploaded);
            }
            Err(error) => {
                self.cloud.failed = true;
                self.cloud.status = error.to_string();
            }
        }
    }

    fn finish_close(&mut self) {
        if self.cloud.close_ready {
            return;
        }
        self.cloud.close_ready = true;
        self.cloud.job = None;
        self.cloud.probe = None;
        self.video_skip = Default::default();
        self.update_library();
        self.library.flush();
        self.comic.flush();
        if self.settings.sync.enabled
            && self.cloud.configured.as_ref() == Some(&self.settings.sync)
            && self.cloud.journal.is_some()
            && let Err(error) = self.capture_cloud_snapshot(true)
        {
            eprintln!("Unable to save pending cloud changes: {error}");
        }
    }

    pub(super) fn request_close(&mut self, ctx: &egui::Context) {
        self.finish_close();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub(super) fn should_close(&mut self, ctx: &egui::Context) -> bool {
        if ctx.input(|i| i.viewport().close_requested()) {
            self.finish_close();
        }
        self.cloud.close_ready
    }

    fn cloud_accounts(&self) -> BTreeMap<String, String> {
        let mut data = BTreeMap::new();
        if !self.settings.sync.enabled || !self.settings.sync.sync_keys {
            return data;
        }
        if let Ok(Some(session)) = self.cloud_video_session() {
            data.insert(BILIBILI_ACCOUNT.into(), session);
        }
        let account = &self.settings.comics.komiic;
        if !account.email.trim().is_empty() && !account.password.is_empty() {
            let credentials = KomiicCredentials {
                email: account.email.trim().into(),
                password: account.password.clone(),
                auto_login: account.auto_login,
            };
            data.insert(
                KOMIIC_ACCOUNT.into(),
                serde_json::to_string(&credentials).unwrap(),
            );
            if let Some(cookies) = self.comic.cloud_komiic_session(account) {
                let session = KomiicSession {
                    account: account.credential_id(),
                    cookies,
                };
                data.insert(
                    KOMIIC_SESSION.into(),
                    serde_json::to_string(&session).unwrap(),
                );
            }
        } else if self.settings.comics.komiic_signed_out {
            data.insert(KOMIIC_ACCOUNT.into(), String::new());
            data.insert(KOMIIC_SESSION.into(), String::new());
        }
        data
    }

    fn poll_cloud_lyrics(&mut self, ctx: &egui::Context) {
        let Some(journal) = &self.cloud.journal else {
            return;
        };
        let active = if self.is_music() {
            self.source.as_deref()
        } else {
            None
        };
        let may_have_synced_lyrics = journal
            .document
            .entries
            .range("lyric/file/".to_owned()..)
            .next()
            .is_some_and(|(key, _)| key.starts_with("lyric/file/"));
        self.cloud
            .lyrics
            .poll(ctx, &self.library.records, active, may_have_synced_lyrics);
        if self.is_music()
            && (active.is_some() || self.online_origin.is_some())
            && self.lyrics.lines.is_empty()
            && let Some((_, text)) = self.cloud_lyric_versions().first()
        {
            self.lyrics = crate::lyrics::Lyrics::parse(text);
            self.reset_lyrics_follow();
        }
    }
    pub(super) fn cloud_lyric_versions(&self) -> Vec<(u64, &str)> {
        let Some(journal) = &self.cloud.journal else {
            return Vec::new();
        };
        self.cloud.lyrics.versions(
            &journal.document,
            self.online_origin.as_ref(),
            self.source.as_deref(),
        )
    }

    fn cloud_snapshot(&self) -> Snapshot {
        let mut data = settings_snapshot(&self.settings);
        let tracked = |key: &str| {
            self.cloud
                .journal
                .as_ref()
                .is_some_and(|j| j.document.entries.contains_key(key))
        };
        sync::portable::capture_comics(
            &mut data,
            self.comic.entries(),
            self.cloud.journal.as_ref(),
        );
        for record in &self.library.creators {
            let prefix = format!("creator/bilibili/{}", record.uploader.mid);
            if !record.favorite && !tracked(&format!("{prefix}/favorite")) {
                continue;
            }
            data.insert(format!("{prefix}/profile"), json!(record.uploader));
            data.insert(format!("{prefix}/favorite"), json!(record.favorite));
            data.insert(format!("{prefix}/favorited_at"), json!(record.favorited_at));
        }
        for record in &self.library.records {
            if !record.music || crate::bilibili::video_id(&record.path).is_some() {
                continue;
            }
            let Some(song) = &record.online else { continue };
            let prefix = format!("song/{}", song.key());
            let known = |field: &str| {
                self.cloud.journal.as_ref().is_some_and(|j| {
                    j.document
                        .entries
                        .get(&format!("{prefix}/{field}"))
                        .is_some_and(|e| e.value.is_some())
                })
            };
            if !record.favorite && !known("favorite") {
                continue;
            }
            data.insert(format!("{prefix}/favorite"), json!(record.favorite));
            data.insert(format!("{prefix}/favorited_at"), json!(record.favorited_at));
            if record.favorite || known("song") {
                data.insert(format!("{prefix}/song"), json!(song));
                data.insert(format!("{prefix}/progress"), json!({"position":record.position,"duration":record.duration,"played":record.played}));
            }
        }
        self.video_updates_snapshot(&mut data);
        self.playlists.capture_cloud(&mut data);
        self.cloud.lyrics.capture(
            &self.settings.sync.device_id,
            &self.library.records,
            &mut data,
        );
        if let Some(journal) = &self.cloud.journal {
            // A missing local library/file is not an explicit unfavorite or
            // playlist deletion. Those actions have false registers of their own.
            for (key, value) in journal.values() {
                if key.starts_with("song/")
                    || key.starts_with("playlist/")
                    || key.starts_with("lyric/")
                {
                    data.entry(key).or_insert(value);
                }
            }
        }
        data
    }

    fn capture_cloud_snapshot(&mut self, progress: bool) -> anyhow::Result<bool> {
        self.playlists.cloud_ready()?;
        let mut snapshot = self.cloud_snapshot();
        let Some(journal) = &mut self.cloud.journal else {
            return Ok(false);
        };
        if !progress {
            sync::portable::defer_progress(&mut snapshot, &journal.observed);
        }
        let changed = journal.capture(snapshot, &settings_snapshot(&Settings::default()));
        if changed {
            journal.save(&self.cloud.path)?;
        }
        Ok(changed)
    }

    fn capture_cloud(&mut self, progress: bool) -> anyhow::Result<bool> {
        if progress {
            self.update_library();
            self.library.flush();
            self.comic.flush();
        }
        let changed = self.capture_cloud_snapshot(progress)?;
        let Some(journal) = &self.cloud.journal else {
            return Ok(changed);
        };
        let accounts = self.cloud_accounts();
        let keys_changed = if self.settings.sync.sync_keys {
            let keys = secrets(&self.settings);
            let unset = unset_secrets(&keys, journal, &crate::credentials::SystemVault::local())?;
            keys.iter().chain(&accounts).any(|(id, key)| {
                !unset.contains(id)
                    && (id.starts_with("account/")
                        || !key.is_empty()
                        || journal.secret_hashes.contains_key(id))
                    && journal.secret_hashes.get(id) != Some(&crate::comics::digest(key.as_bytes()))
            })
        } else {
            false
        };
        Ok(changed || keys_changed)
    }

    pub(super) fn poll_cloud(&mut self, ctx: &egui::Context) {
        if self.cloud.close_ready {
            return;
        }
        let now = Instant::now();
        if self.cloud.configured.as_ref() != Some(&self.settings.sync) {
            self.cloud.video_ready = false;
            self.reset_video_updates();
            self.cloud.job = None;
            self.cloud.probe = None;
            self.cloud.journal = None;
            self.cloud.configured = Some(self.settings.sync.clone());
            self.cloud.status.clear();
            self.cloud.failed = false;
            self.cloud.changed = None;
            self.cloud.startup = self.settings.sync.enabled;
            self.cloud.urgent = false;
            if self.settings.sync.enabled {
                self.cloud.path = Settings::queue_path()
                    .with_file_name("sync")
                    .join(self.settings.sync.account())
                    .join(format!("{}.json", self.settings.sync.device_id));
                match sync::Journal::load(&self.cloud.path, &self.settings.sync.device_id) {
                    Ok(journal) => {
                        if let Err(error) = self.import_video_updates(&journal.values()) {
                            self.cloud.failed = true;
                            self.cloud.status = error.to_string();
                            return;
                        }
                        self.cloud.journal = Some(journal);
                        self.cloud.failed = false;
                        self.cloud.status.clear();
                    }
                    Err(error) => {
                        self.cloud.failed = true;
                        self.cloud.status = error.to_string();
                    }
                }
            }
        }
        if self
            .cloud
            .probe_settings
            .as_ref()
            .is_some_and(|s| *s != self.preferences_draft.sync)
        {
            self.cloud.probe = None;
            self.cloud.probe_settings = None;
            self.cloud.status.clear();
        }
        if let Some(result) = self.cloud.probe.as_ref().and_then(crate::jobs::Job::poll) {
            self.cloud.probe = None;
            self.cloud.probe_settings = None;
            self.cloud.failed = result.is_err();
            self.cloud.status = result.err().map_or_else(
                || {
                    self.settings
                        .language
                        .text("连接正常", "Connection verified")
                        .into()
                },
                |e| e.to_string(),
            );
        }
        if !self.settings.sync.enabled || self.cloud.journal.is_none() {
            return;
        }
        self.poll_cloud_lyrics(ctx);
        let session = self.cloud_session();
        let leaving = self.cloud.session.is_some() && self.cloud.session != session;
        self.cloud.session = session;
        if leaving {
            self.checkpoint_cloud();
        }
        if now >= self.cloud.capture_after || self.cloud.startup {
            let progress = self.cloud.startup;
            match self.capture_cloud(progress) {
                Ok(true) => {
                    self.cloud.changed.get_or_insert(now);
                }
                Ok(false) => {}
                Err(error) => {
                    self.cloud.failed = true;
                    self.cloud.status = error.to_string();
                }
            }
            self.cloud.capture_after = now + Duration::from_secs(1);
        }
        if let Some(result) = self.cloud.job.as_ref().and_then(crate::jobs::Job::poll) {
            self.cloud.job = None;
            match result.and_then(|report| self.finish_cloud(report)) {
                Ok(()) => {
                    self.cloud.failed = false;
                    self.cloud.status = self
                        .settings
                        .language
                        .text("同步完成", "Sync complete")
                        .into();
                }
                Err(error) => {
                    self.cloud.failed = true;
                    self.cloud.status = error.to_string();
                    self.cloud.changed = None;
                    self.cloud.urgent = false;
                }
            }
        }
        let dirty = self
            .cloud
            .changed
            .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_secs(5));
        if self.cloud.job.is_none()
            && self.cloud.probe.is_none()
            && (dirty || self.cloud.startup || self.cloud.urgent)
        {
            self.begin_cloud(ctx);
        }
        if self.cloud.changed.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    fn begin_cloud(&mut self, ctx: &egui::Context) {
        if self.cloud.job.is_some() || self.cloud.journal.is_none() {
            return;
        }
        self.cloud.startup = false;
        self.cloud.urgent = false;
        let keys = secrets(&self.settings);
        let prepared = self.capture_cloud(false).and_then(|_| {
            anyhow::ensure!(
                self.settings.credential_error.is_none(),
                "无法读取本机密钥，已暂停同步 / Credential storage is unavailable; sync paused"
            );
            self.settings.sync.validate()?;
            let mut journal = self.cloud.journal.clone().unwrap();
            if self.settings.sync.sync_keys {
                // An empty field with no local record can still be filled by
                // cloud sync. Do not publish it as a cleared key.
                for id in unset_secrets(&keys, &journal, &crate::credentials::SystemVault::local())?
                {
                    journal.secret_hashes.remove(&id);
                }
            }
            Ok(journal)
        });
        let journal = match prepared {
            Ok(journal) => journal,
            Err(error) => {
                self.cloud.status = error.to_string();
                self.cloud.failed = true;
                self.cloud.changed = None;
                return;
            }
        };
        let settings = self.settings.sync.clone();
        let accounts = self.cloud_accounts();
        self.cloud.requested_accounts = accounts.clone();
        self.cloud.requested_secrets = keys.clone();
        self.cloud.changed = None;
        self.cloud.status = self.settings.language.text("正在同步…", "Syncing…").into();
        let ctx = ctx.clone();
        self.cloud.job = Some(crate::jobs::Job::spawn(move |_| async move {
            let result = sync::bounded(
                sync::webdav::run_desktop(settings, journal, keys, accounts),
                Duration::from_secs(120),
            )
            .await;
            ctx.request_repaint();
            result
        }));
    }
    fn finish_cloud(&mut self, report: sync::webdav::Report) -> anyhow::Result<()> {
        self.update_library();
        self.comic.flush();
        let local = self.cloud_snapshot();
        let baseline = self.cloud.journal.as_ref().unwrap().observed.clone();
        self.capture_cloud(false)?;
        let mut journal = self.cloud.journal.clone().unwrap();
        journal.merge(&report.journal.document)?;
        if !journal.music_repaired {
            // Earlier clients could delete whole song groups when a local
            // library was missing. Recover known favorites once on upgrade;
            // explicit unfavorites (false) remain authoritative.
            for record in
                self.library.records.iter().filter(|r| {
                    r.music && r.favorite && crate::bilibili::video_id(&r.path).is_none()
                })
            {
                let Some(song) = &record.online else { continue };
                let prefix = format!("song/{}", song.key());
                if ["song", "favorite"].iter().all(|field| {
                    journal
                        .document
                        .entries
                        .get(&format!("{prefix}/{field}"))
                        .is_some_and(|e| e.value.is_none())
                }) {
                    journal.write(format!("{prefix}/song"), Some(json!(song)));
                    let key = format!("{prefix}/favorite");
                    journal.write(key.clone(), Some(json!(true)));
                    journal.document.entries.get_mut(&key).unwrap().favorited_at =
                        Some(record.favorited_at);
                }
            }
            journal.music_repaired = true;
        }
        let old_settings = self.settings.clone();
        let old_draft = self.preferences_draft.clone();
        let current_keys = secrets(&old_settings);
        let accounts = self.cloud_accounts();
        let mut bilibili_session = None;
        let mut komiic_session = None;
        let values = journal.values();
        let mut next = old_settings.clone();
        apply_settings(&mut next, &values)?;
        for (id, key) in report.secrets {
            if id.starts_with("account/") {
                if !self.settings.sync.sync_keys
                    || accounts.get(&id) != self.cloud.requested_accounts.get(&id)
                {
                    continue;
                }
                match id.as_str() {
                    BILIBILI_ACCOUNT
                        if accounts.get(&id) != Some(&key) && !self.cloud_video_account_busy() =>
                    {
                        bilibili_session = Some(key);
                    }
                    KOMIIC_ACCOUNT if accounts.get(&id) != Some(&key) => {
                        if key.is_empty() {
                            next.comics.komiic.password.clear();
                            next.comics.komiic.auto_login = false;
                            next.comics.komiic_signed_out = true;
                        } else {
                            let value: KomiicCredentials =
                                serde_json::from_str(&key).map_err(|_| {
                                    anyhow::anyhow!("Invalid synced Komiic credentials")
                                })?;
                            anyhow::ensure!(
                                value.email.contains('@') && !value.password.is_empty(),
                                "Invalid synced Komiic account"
                            );
                            next.comics.komiic = crate::comics::source::KomiicAccount {
                                email: value.email,
                                password: value.password,
                                auto_login: value.auto_login,
                            };
                            next.comics.komiic.validate()?;
                            next.comics.komiic_signed_out = false;
                        }
                    }
                    KOMIIC_SESSION if !key.is_empty() && accounts.get(&id) != Some(&key) => {
                        komiic_session = Some(
                            serde_json::from_str::<KomiicSession>(&key)
                                .map_err(|_| anyhow::anyhow!("Invalid synced Komiic session"))?,
                        );
                    }
                    _ => {}
                }
                continue;
            }
            if current_keys.get(&id) != self.cloud.requested_secrets.get(&id) {
                continue;
            }
            if let Some(provider) = id
                .strip_prefix("ai/")
                .and_then(|id| next.ai.providers.iter_mut().find(|p| p.id == id))
            {
                provider.api_key = key;
            } else if id == "music/chksz" {
                next.music.api_key = key;
            }
        }
        next.ai.validate()?;
        if next != old_settings {
            next.save()?;
        }
        self.settings = next;
        if let Some(bilibili) = bilibili_session {
            self.apply_cloud_video_session(&bilibili)?;
        }
        if let Some(session) = komiic_session
            && !self.comic.cloud_account_busy()
            && !self.settings.comics.komiic.password.is_empty()
            && session.account == self.settings.comics.komiic.credential_id()
        {
            self.comic.restore_cloud_komiic_session(
                self.settings.comics.komiic.clone(),
                session.cookies,
            )?;
        }
        if old_draft == old_settings {
            self.preferences_draft = self.settings.clone();
        } else {
            // Rebase unsaved edits so saving a draft cannot undo an unrelated pull.
            let before = settings_snapshot(&old_settings);
            let draft = settings_snapshot(&old_draft);
            let mut rebased = values.clone();
            for key in before.keys().chain(draft.keys()) {
                if before.get(key) != draft.get(key) {
                    if let Some(v) = draft.get(key) {
                        rebased.insert(key.clone(), v.clone());
                    } else {
                        rebased.remove(key);
                    }
                }
            }
            self.preferences_draft = old_draft;
            apply_settings(&mut self.preferences_draft, &rebased)?;
            for p in &mut self.preferences_draft.ai.providers {
                let before = old_settings
                    .ai
                    .providers
                    .iter()
                    .find(|v| v.id == p.id)
                    .map(|v| v.api_key.as_str())
                    .unwrap_or_default();
                if p.api_key == before
                    && let Some(pulled) = self.settings.ai.providers.iter().find(|v| v.id == p.id)
                {
                    p.api_key = pulled.api_key.clone();
                }
            }
            if self.preferences_draft.comics.komiic == old_settings.comics.komiic {
                self.preferences_draft.comics.komiic = self.settings.comics.komiic.clone();
            }
            self.preferences_draft.comics.komiic_signed_out =
                self.settings.comics.komiic_signed_out;
            if self.preferences_draft.music.api_key == old_settings.music.api_key {
                self.preferences_draft.music.api_key = self.settings.music.api_key.clone();
            }
        }
        let mut applied = values.clone();
        sync::portable::preserve_local_progress(&mut applied, &local, &baseline);
        self.playlists.apply_cloud(&applied)?;
        self.apply_cloud_song_deletions(&journal.document)?;
        self.apply_cloud_library(&applied)?;
        self.cloud.video_ready = true;
        journal.uploaded = report.journal.uploaded;
        journal.last_success = report.journal.last_success;
        journal.secret_hashes = report.journal.secret_hashes;
        journal.remote_etags = report.journal.remote_etags;
        self.cloud.journal = Some(journal);
        let mut observed = self.cloud_snapshot();
        let journal = self.cloud.journal.as_mut().unwrap();
        // Persist normalization (e.g. a read chapter clears its unread flag),
        // otherwise stale remote fields could return on a later synchronization.
        for (key, value) in &values {
            if key.starts_with("secret/")
                || key.starts_with("account/")
                || sync::portable::progress_key(key)
            {
                continue;
            }
            if observed.get(key) != Some(value) {
                journal.write(key.clone(), observed.get(key).cloned());
            }
        }
        sync::portable::defer_progress(&mut observed, &values);
        journal.observed = observed;
        self.cloud
            .journal
            .as_ref()
            .unwrap()
            .save(&self.cloud.path)?;
        if self.cloud.journal.as_ref().unwrap().digest()
            != self.cloud.journal.as_ref().unwrap().uploaded
        {
            self.cloud.changed = Some(Instant::now());
        }
        Ok(())
    }

    fn apply_cloud_library(&mut self, values: &Snapshot) -> anyhow::Result<()> {
        self.import_video_updates(values)?;
        let previous_records = self.library.records.clone();
        let previous_creators = self.library.creators.clone();
        let creators = values
            .iter()
            .filter(|(key, _)| key.starts_with("creator/bilibili/") && key.ends_with("/profile"))
            .map(|(key, value)| -> anyhow::Result<library::CreatorRecord> {
                let uploader: crate::bilibili::Uploader = serde_json::from_value(value.clone())?;
                let prefix = key.trim_end_matches("/profile");
                anyhow::ensure!(
                    uploader.mid > 0
                        && !uploader.uname.trim().is_empty()
                        && prefix == format!("creator/bilibili/{}", uploader.mid),
                    "Invalid synced creator identity"
                );
                Ok(library::CreatorRecord {
                    uploader,
                    favorite: values
                        .get(&format!("{prefix}/favorite"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    favorited_at: values
                        .get(&format!("{prefix}/favorited_at"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        for creator in creators {
            if let Some(old) = self
                .library
                .creators
                .iter_mut()
                .find(|r| r.uploader.mid == creator.uploader.mid)
            {
                *old = creator;
            } else {
                self.library.creators.push(creator);
            }
        }
        for entry in sync::portable::apply_comics(values, self.comic.entries())? {
            let unchanged = self
                .comic
                .entries()
                .iter()
                .find(|old| old.book.key() == entry.book.key())
                .is_some_and(|old| {
                    serde_json::to_value(old).ok() == serde_json::to_value(&entry).ok()
                });
            if !unchanged {
                self.comic.import_synced(entry)?;
            }
        }
        for (key, value) in values
            .iter()
            .filter(|(k, _)| k.starts_with("song/") && k.ends_with("/song"))
        {
            let song: crate::music_sources::OnlineSong = serde_json::from_value(value.clone())?;
            let prefix = key.trim_end_matches("/song");
            anyhow::ensure!(
                prefix == format!("song/{}", song.key()),
                "Invalid synced song identity"
            );
            let path = self
                .library
                .records
                .iter()
                .find(|r| {
                    r.music
                        && crate::bilibili::video_id(&r.path).is_none()
                        && r.online.as_ref().is_some_and(|s| s.key() == song.key())
                })
                .map(|r| r.path.clone())
                .unwrap_or_else(|| format!("online://{}", song.key()));
            self.library.bind_online(path.clone(), song);
            let record = self
                .library
                .records
                .iter_mut()
                .find(|r| r.path == path)
                .unwrap();
            record.favorite = values
                .get(&format!("{prefix}/favorite"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            record.favorited_at = values
                .get(&format!("{prefix}/favorited_at"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if let Some(p) = values.get(&format!("{prefix}/progress")) {
                record.position = p["position"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .unwrap_or(0.0);
                record.duration = p["duration"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .unwrap_or(0.0);
                record.played = p["played"].as_u64().unwrap_or(0);
            }
        }
        if self.library.records != previous_records || self.library.creators != previous_creators {
            self.library.save_records()?;
        }
        Ok(())
    }

    fn apply_cloud_song_deletions(&mut self, document: &sync::Document) -> anyhow::Result<()> {
        let mut changed = false;
        for record in &mut self.library.records {
            if !record.music || crate::bilibili::video_id(&record.path).is_some() {
                continue;
            }
            let Some(song) = &record.online else { continue };
            if let Some(entry) = document
                .entries
                .get(&format!("song/{}/favorite", song.key()))
                && (entry.value.is_none() || entry.value == Some(json!(false)))
                && record.favorite
            {
                record.favorite = false;
                record.favorited_at = entry.favorited_at.unwrap_or(0);
                changed = true;
            }
        }
        if changed {
            self.library.save_records()?;
        }
        Ok(())
    }

    pub(super) fn preferences_cloud(&mut self, ui: &mut Ui) {
        let l = self.settings.language;
        let settings = &mut self.preferences_draft.sync;
        preferences::form_row(ui, l.text("启用同步", "Enable sync"), l, |ui| {
            design::toggle(ui, &mut settings.enabled, l.text("启用同步", "Enable sync"));
        });
        preferences::form_row(ui, l.text("提供商", "Provider"), l, |ui| {
            let old = settings.provider;
            let selected = if old == sync::Provider::Custom {
                l.text("自定义", "Custom")
            } else {
                old.label()
            };
            design::dropdown_ui(ui, "cloud-provider", ui.available_width(), selected, |ui| {
                for provider in sync::Provider::ALL {
                    let name = if provider == sync::Provider::Custom {
                        l.text("自定义", "Custom")
                    } else {
                        provider.label()
                    };
                    design::menu_value(ui, &mut settings.provider, provider, name);
                }
            });
            if settings.provider != old {
                settings.base_url = settings.provider.url().into();
            }
        });
        if settings.provider == sync::Provider::Custom {
            preferences::form_row(ui, l.text("接口地址", "Base URL"), l, |ui| {
                design::input_field(
                    ui,
                    &mut settings.base_url,
                    ui.available_width(),
                    "https://dav.example.com",
                );
            });
        }
        preferences::form_row(ui, l.text("用户名", "Username"), l, |ui| {
            design::input_field(ui, &mut settings.username, ui.available_width(), "");
        });
        preferences::form_row(ui, l.text("密码", "Password"), l, |ui| {
            design::secret_field(
                ui,
                "cloud-password",
                &mut settings.password,
                ui.available_width(),
                "",
            );
        });
        preferences::form_row(ui, l.text("设备名称", "Device name"), l, |ui| {
            design::input_field(ui, &mut settings.device_name, ui.available_width(), "");
        });
        preferences::form_row(ui, l.text("同步密钥", "Sync credentials"), l, |ui| {
            design::toggle(
                ui,
                &mut settings.sync_keys,
                l.text("同步密钥", "Sync credentials"),
            )
            .on_hover_text(l.text(
                "加密同步 API Key、B 站登录会话及 Komiic 账号和会话。",
                "Encrypt and sync API keys, Bilibili sign-in, and the Komiic account and session.",
            ));
        });
        if settings.sync_keys {
            preferences::form_row(ui, l.text("同步口令", "Sync passphrase"), l, |ui| {
                design::secret_field(
                    ui,
                    "cloud-passphrase",
                    &mut settings.passphrase,
                    ui.available_width(),
                    l.text("至少 12 个字符", "At least 12 characters"),
                );
            });
        }
        ui.add_space(6.0);
        let busy = self.cloud.job.is_some() || self.cloud.probe.is_some();
        let mut test = false;
        let mut sync_now = false;
        preferences::form_row(ui, "", l, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                test = design::control(
                    ui,
                    l.text("测试连接", "Test connection"),
                    None,
                    design::Control::Outline,
                    0.0,
                )
                .clicked();
            });
            ui.add_enabled_ui(
                !busy
                    && self.settings.sync.enabled
                    && self.preferences_draft.sync == self.settings.sync,
                |ui| {
                    sync_now = workspace::primary(ui, l.text("立即同步", "Sync now")).clicked();
                },
            );
        });
        if test {
            let mut settings = self.preferences_draft.sync.clone();
            self.cloud.probe_settings = Some(settings.clone());
            settings.sync_keys = false;
            let ctx = ui.ctx().clone();
            self.cloud.status = l.text("正在测试连接…", "Testing connection…").into();
            self.cloud.failed = false;
            self.cloud.probe = Some(crate::jobs::Job::spawn(move |_| async move {
                let result = sync::webdav::test(settings).await;
                ctx.request_repaint();
                result
            }));
        }
        if sync_now {
            self.checkpoint_cloud();
            self.begin_cloud(ui.ctx());
        }
        preferences::form_row(ui, "", l, |ui| {
            if busy {
                ui.spinner();
            }
            if !self.cloud.status.is_empty() {
                ui.colored_label(
                    if self.cloud.failed {
                        Color32::LIGHT_RED
                    } else {
                        MUTED
                    },
                    &self.cloud.status,
                );
            }
        });
        if let Some(journal) = &self.cloud.journal
            && journal.last_success > 0
        {
            let minutes = crate::comics::now().saturating_sub(journal.last_success) / 60;
            preferences::form_row(ui, l.text("最近同步", "Last sync"), l, |ui| {
                ui.label(if minutes == 0 {
                    l.text("刚刚", "Just now").into()
                } else {
                    format!("{minutes} {}", l.text("分钟前", "minutes ago"))
                });
            });
        }
    }
}

fn apply_settings(settings: &mut Settings, values: &Snapshot) -> anyhow::Result<()> {
    let ids: BTreeSet<_> = values
        .keys()
        .filter_map(|k| {
            k.strip_prefix("ai/")?
                .split_once('/')
                .map(|(id, _)| id.to_owned())
        })
        .collect();
    settings.ai.providers = ids
        .into_iter()
        .filter_map(|id| {
            let name = values.get(&format!("ai/{id}/name"))?.as_str()?.to_owned();
            let api_key = settings
                .ai
                .providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.api_key.clone())
                .unwrap_or_default();
            let prefix = format!("ai/{id}/model/");
            Some(crate::ai::Provider {
                name,
                base_url: values
                    .get(&format!("ai/{id}/url"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                models: values
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .filter_map(|(_, v)| v.as_str().map(str::to_owned))
                    .collect(),
                api_key,
                id,
            })
        })
        .collect();
    let mut llm = serde_json::to_value(&settings.llm)?;
    for (key, value) in values
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("subtitle/").map(|s| (s, v)))
    {
        if matches!(
            key,
            "selected"
                | "audio_encoding"
                | "audio_format"
                | "bilingual"
                | "reasoning_effort"
                | "chunk_seconds"
        ) {
            llm[key] = value.clone();
        }
    }
    settings.llm = serde_json::from_value(llm)?;
    if let Some(value) = values.get("subtitle/concurrency") {
        let concurrency = value
            .as_u64()
            .filter(|n| (1..=6).contains(n))
            .ok_or_else(|| anyhow::anyhow!("Invalid synced subtitle concurrency"))?;
        settings.subtitle_concurrency = concurrency as usize;
    }
    if let Some(value) = values.get("image/selected") {
        settings.comics.image_selected = serde_json::from_value(value.clone())?;
    }
    for (key, field) in [
        ("image/quality", &mut settings.comics.image_quality),
        ("image/target", &mut settings.comics.target),
        ("music/source", &mut settings.music.source),
        ("music/quality", &mut settings.music.quality),
    ] {
        if let Some(value) = values.get(key) {
            *field = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Invalid sync setting"))?
                .into();
        }
    }
    for selected in [
        &mut settings.llm.selected,
        &mut settings.comics.image_selected,
    ] {
        if !settings
            .ai
            .providers
            .iter()
            .any(|p| p.id == selected.provider && p.models.contains(&selected.model))
        {
            *selected = Default::default();
        }
    }
    crate::subtitles::LlmConfig::validate_options(&settings.llm)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_local_keys_are_distinct_from_explicitly_cleared_keys() {
        use crate::credentials::Vault;
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string());
        let vault = crate::credentials::FileVault::isolated(directory.join("credentials.json"));
        let mut journal = sync::Journal::default();
        journal
            .secret_hashes
            .insert("music/chksz".into(), "old".into());
        journal
            .secret_hashes
            .insert("ai/known".into(), "old".into());
        let keys = BTreeMap::from([
            ("music/chksz".into(), String::new()),
            ("ai/known".into(), String::new()),
        ]);
        vault
            .set(&Settings::credential_target("ai/known"), None)
            .unwrap();
        assert_eq!(
            unset_secrets(&keys, &journal, &vault).unwrap(),
            BTreeSet::from(["music/chksz".into()])
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
    fn app(ctx: &egui::Context, directory: &std::path::Path) -> App {
        let mut app = comic::visual_tests::settings_fixture(ctx);
        app.library = library::Library::open(directory.join("library.json"));
        app.playlists = playlists::State::open(directory.join("playlists.json"));
        app
    }
    fn directory() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string())
    }
    #[test]
    fn song_sync_never_exports_video_records_or_overwrites_their_favorites() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = app(&ctx, &directory);
        let song = crate::music_sources::OnlineSong {
            source: "chksz-163".into(),
            id: "song-a".into(),
            title: "Song A".into(),
            ..Default::default()
        };
        let prefix = format!("song/{}", song.key());
        let video = library::Record {
            path: "https://www.bilibili.com/video/BV1GJ411x7h7?p=1".into(),
            title: "Video".into(),
            music: true,
            online: Some(song.clone()),
            favorite: true,
            favorited_at: 42,
            ..Default::default()
        };
        app.library.records.push(video.clone());
        assert!(!app.cloud_snapshot().contains_key(&format!("{prefix}/song")));
        let snapshot = Snapshot::from([
            (format!("{prefix}/song"), json!(song)),
            (format!("{prefix}/favorite"), json!(false)),
        ]);
        app.apply_cloud_library(&snapshot).unwrap();
        assert!(app.library.records[0] == video);
        assert_eq!(app.library.records.len(), 2);
        let music = &app.library.records[1];
        assert!(music.music && music.path.starts_with("online://") && !music.favorite);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn creator_favorites_sync_profiles_cancellations_and_identity_without_sessions() {
        let ctx = egui::Context::default();
        let da = directory();
        let db = directory();
        let mut first = app(&ctx, &da);
        let mut second = app(&ctx, &db);
        let creator = serde_json::from_value(
            serde_json::json!({"mid":123,"uname":"Favorite UP","upic":null}),
        )
        .unwrap();
        first.library.toggle_creator(&creator).unwrap();
        let mut snapshot = first.cloud_snapshot();
        let prefix = "creator/bilibili/123";
        assert_eq!(snapshot[&format!("{prefix}/favorite")], json!(true));
        second.apply_cloud_library(&snapshot).unwrap();
        assert!(second.library.creator_favorite(123));
        assert!(
            second.library.records.is_empty(),
            "creators are not playable media records"
        );
        assert_eq!(
            second.library.creators[0].favorited_at,
            first.library.creators[0].favorited_at
        );
        snapshot.insert(format!("{prefix}/favorite"), json!(false));
        second.apply_cloud_library(&snapshot).unwrap();
        assert!(!second.library.creator_favorite(123));
        snapshot.get_mut(&format!("{prefix}/profile")).unwrap()["mid"] = json!(456);
        assert!(second.apply_cloud_library(&snapshot).is_err());
        assert!(!second.library.creator_favorite(456));
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("SESSDATA")
        );
        drop((first, second));
        std::fs::remove_dir_all(da).unwrap();
        std::fs::remove_dir_all(db).unwrap();
    }
    fn song() -> crate::music_sources::OnlineSong {
        crate::music_sources::OnlineSong {
            source: "chksz-163".into(),
            id: "123456".into(),
            title: "Online song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            cover: None,
        }
    }
    #[test]
    fn portable_snapshot_excludes_local_media_paths_credentials_and_machine_preferences() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = app(&ctx, &directory);
        app.settings.music.directory = "F:/private/downloads".into();
        app.settings.subtitle_concurrency = 4;
        app.settings.sync.password = "private-webdav-password".into();
        app.settings.sync.passphrase = "private-sync-passphrase".into();
        app.settings.music.api_key = "private-music-key".into();
        app.settings.comics.komiic = crate::comics::source::KomiicAccount {
            email: "private-reader@example.test".into(),
            password: "private-komiic-password".into(),
            auto_login: true,
        };
        let keys = serde_json::to_string(&secrets(&app.settings)).unwrap();
        assert!(!keys.contains("komiic"));
        assert!(!keys.contains("private-reader"));
        app.library.records.push(library::Record {
            path: "F:/private/local.flac".into(),
            title: "LocalOnlyUnique".into(),
            music: true,
            favorite: true,
            ..Default::default()
        });
        app.library.records.push(library::Record {
            path: "F:/private/local.mp4".into(),
            title: "VideoOnlyUnique".into(),
            favorite: true,
            ..Default::default()
        });
        app.library
            .bind_online("F:/private/download.flac".into(), song());
        app.library
            .toggle_favorite("F:/private/download.flac", "Online song", true);
        let data = app.cloud_snapshot();
        let text = serde_json::to_string(&data).unwrap();
        for excluded in [
            "F:/private",
            "LocalOnlyUnique",
            "VideoOnlyUnique",
            "private-webdav-password",
            "private-sync-passphrase",
            "private-music-key",
            "private-reader@example.test",
            "private-komiic-password",
            "not-a-real-key",
        ] {
            assert!(!text.contains(excluded), "{excluded} leaked into sync data");
        }
        assert!(text.contains("Online song"));
        let mut target = Settings {
            language: Language::English,
            ..Default::default()
        };
        target.music.directory = "F:/another-device".into();
        apply_settings(&mut target, &data).unwrap();
        assert!(target.language == Language::English);
        assert_eq!(target.music.directory, "F:/another-device");
        assert_eq!(target.subtitle_concurrency, 4);
        assert!(target.ai.providers.iter().all(|p| p.api_key.is_empty()));
        assert_eq!(target.llm.selected.model, app.settings.llm.selected.model);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn cloud_song_import_keeps_local_bindings_and_metadata_refresh_does_not_erase_identity() {
        let ctx = egui::Context::default();
        let da = directory();
        let db = directory();
        let mut first = app(&ctx, &da);
        let mut second = app(&ctx, &db);
        let song = song();
        first
            .library
            .bind_online("F:/first/song.flac".into(), song.clone());
        first
            .library
            .toggle_favorite("F:/first/song.flac", "Online song", true);
        first.library.records[0].duration = 240.0;
        first.library.records[0].position = 45.0;
        second.apply_cloud_library(&first.cloud_snapshot()).unwrap();
        assert_eq!(second.library.records.len(), 1);
        assert!(second.library.records[0].path.starts_with("online://"));
        assert_eq!(second.library.records[0].resume_at(), Some(45.0));
        assert!(second.library.records[0].favorite);
        assert_eq!(
            second.library.records[0].favorited_at,
            first.library.records[0].favorited_at
        );
        second
            .library
            .bind_online("F:/second/song.flac".into(), song.clone());
        second.apply_cloud_library(&first.cloud_snapshot()).unwrap();
        assert_eq!(second.library.records.len(), 1);
        assert_eq!(second.library.records[0].path, "F:/second/song.flac");
        second.library.remember(library::Record {
            path: "F:/second/song.flac".into(),
            title: "Refreshed".into(),
            music: true,
            ..Default::default()
        });
        assert_eq!(
            second.library.records[0].online.as_ref().unwrap().key(),
            song.key()
        );
        assert!(second.library.records[0].favorite);
        assert_eq!(
            second.library.records[0].favorited_at,
            first.library.records[0].favorited_at
        );
        drop((first, second));
        std::fs::remove_dir_all(da).unwrap();
        std::fs::remove_dir_all(db).unwrap();
    }
    #[test]
    fn changing_sync_account_discards_old_tasks_and_unsaved_probe_results() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = app(&ctx, &directory);
        app.cloud.configured = Some(app.settings.sync.clone());
        app.cloud.job = Some(crate::jobs::Job::spawn(|_| async {
            std::future::pending().await
        }));
        app.settings.sync.base_url = "https://new.example/dav".into();
        app.poll_cloud(&ctx);
        assert!(app.cloud.job.is_none());
        assert!(app.cloud.journal.is_none());
        app.cloud.probe_settings = Some(app.preferences_draft.sync.clone());
        app.cloud.probe = Some(crate::jobs::Job::spawn(|_| async {
            std::future::pending().await
        }));
        app.preferences_draft.sync.username = "new-user".into();
        app.poll_cloud(&ctx);
        assert!(app.cloud.probe.is_none());
        drop(app);
        let _ = std::fs::remove_dir_all(directory);
    }

    fn configured(ctx: &egui::Context, directory: &std::path::Path) -> App {
        let mut app = app(ctx, directory);
        app.settings.sync.enabled = true;
        app.settings.sync.base_url = "http://127.0.0.1:9/dav".into();
        app.settings.sync.username = "test".into();
        app.settings.sync.password = "test-password".into();
        app.cloud.configured = Some(app.settings.sync.clone());
        app.cloud.path = directory.join("journal.json");
        app.cloud.journal =
            Some(sync::Journal::load(&app.cloud.path, &app.settings.sync.device_id).unwrap());
        app.library.bind_online("test.flac".into(), song());
        app.library
            .toggle_favorite("test.flac", "Online song", true);
        app.library.records[0].position = 10.0;
        app.capture_cloud(true).unwrap();
        let journal = app.cloud.journal.as_mut().unwrap();
        journal.uploaded = journal.digest();
        app
    }

    #[test]
    fn missing_music_library_does_not_publish_implicit_deletions() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = configured(&ctx, &directory);
        let before = app.cloud.journal.as_ref().unwrap().document.entries.clone();
        app.library.records.clear();
        app.capture_cloud(true).unwrap();
        assert!(app.cloud.journal.as_ref().unwrap().document.entries == before);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn playlist_pipeline_preserves_inflight_edits_and_restores_a_missing_local_file() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = configured(&ctx, &directory);
        let id = uuid::Uuid::new_v4().to_string();
        let key = format!("playlist/{id}");
        let mut playlist =
            json!({"id":id,"name":"Home","created":1,"modified":1,"tracks":[song()]});
        app.playlists
            .apply_cloud(&Snapshot::from([(key.clone(), playlist.clone())]))
            .unwrap();
        app.capture_cloud(false).unwrap();
        let mut remote = app.cloud.journal.clone().unwrap();
        remote.document.device = "other".into();
        playlist["name"] = json!("Remote");
        remote.write(key.clone(), Some(playlist.clone()));
        remote.document.entries.get_mut(&key).unwrap().time = 1;
        playlist["name"] = json!("Local edit while syncing");
        app.playlists
            .apply_cloud(&Snapshot::from([(key.clone(), playlist)]))
            .unwrap();
        app.finish_cloud(sync::webdav::Report {
            journal: remote,
            secrets: BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(
            app.cloud_snapshot()[&key]["name"],
            json!("Local edit while syncing")
        );
        assert!(app.cloud.changed.is_some());
        // A deleted/missing cache file must be pulled again, never uploaded as a deletion.
        std::fs::remove_file(directory.join("playlists.json")).unwrap();
        app.playlists = playlists::State::open(directory.join("playlists.json"));
        app.capture_cloud(false).unwrap();
        let remote = app.cloud.journal.clone().unwrap();
        app.finish_cloud(sync::webdav::Report {
            journal: remote,
            secrets: BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(
            app.cloud_snapshot()[&key]["name"],
            json!("Local edit while syncing")
        );
        assert!(directory.join("playlists.json").exists());
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn upgrade_recovers_known_favorites_from_legacy_deletions_once_and_honors_later_removals() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = configured(&ctx, &directory);
        let prefix = format!("song/{}", song().key());
        let mut remote = app.cloud.journal.clone().unwrap();
        remote.document.device = "other".into();
        remote.write(format!("{prefix}/song"), None);
        remote.write(format!("{prefix}/favorite"), None);
        app.finish_cloud(sync::webdav::Report {
            journal: remote,
            secrets: BTreeMap::new(),
        })
        .unwrap();
        assert!(app.library.records[0].favorite);
        assert_eq!(
            app.cloud.journal.as_ref().unwrap().values()[&format!("{prefix}/favorite")],
            json!(true)
        );
        assert!(app.cloud.journal.as_ref().unwrap().music_repaired);
        let mut remote = app.cloud.journal.clone().unwrap();
        remote.document.device = "other".into();
        remote.write(format!("{prefix}/song"), None);
        remote.write(format!("{prefix}/favorite"), None);
        app.finish_cloud(sync::webdav::Report {
            journal: remote,
            secrets: BTreeMap::new(),
        })
        .unwrap();
        assert!(!app.library.records[0].favorite);
        assert_eq!(app.library.records[0].path, "test.flac");
        app.library
            .toggle_favorite("test.flac", "Online song", true);
        app.capture_cloud(false).unwrap();
        let values = app.cloud.journal.as_ref().unwrap().values();
        assert_eq!(values[&format!("{prefix}/favorite")], json!(true));
        assert!(values.contains_key(&format!("{prefix}/song")));
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn upgrade_does_not_restore_an_explicit_unfavorite_without_song_metadata() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = configured(&ctx, &directory);
        let prefix = format!("song/{}", song().key());
        let mut remote = app.cloud.journal.clone().unwrap();
        remote.document.device = "other".into();
        remote.write(format!("{prefix}/song"), None);
        remote.write(format!("{prefix}/favorite"), Some(json!(false)));
        app.finish_cloud(sync::webdav::Report {
            journal: remote,
            secrets: BTreeMap::new(),
        })
        .unwrap();
        assert!(!app.library.records[0].favorite);
        assert_eq!(app.library.records[0].path, "test.flac");
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn playback_progress_never_starts_periodic_sync_but_page_exit_checkpoints_it() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = configured(&ctx, &directory);
        app.workspace.page = workspace::Page::Listening;
        app.cloud.session = app.cloud_session();
        let key = format!("song/{}/progress", song().key());
        app.library.records[0].position = 25.0;
        for _ in 0..5 {
            app.cloud.capture_after = Instant::now() - Duration::from_secs(3600);
            app.poll_cloud(&ctx);
            assert!(app.cloud.job.is_none());
            assert!(app.cloud.changed.is_none());
            assert_eq!(
                app.cloud.journal.as_ref().unwrap().values()[&key]["position"],
                10.0
            );
        }
        // Block network I/O while inspecting the queued page-exit checkpoint.
        app.cloud.probe = Some(crate::jobs::Job::spawn(|_| async {
            std::future::pending().await
        }));
        app.workspace.page = workspace::Page::Music;
        app.poll_cloud(&ctx);
        assert!(app.cloud.urgent);
        assert_eq!(
            app.cloud.journal.as_ref().unwrap().values()[&key]["position"],
            25.0
        );
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn close_saves_latest_progress_and_cancels_sync_without_starting_another() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = configured(&ctx, &directory);
        app.cloud.job = Some(crate::jobs::Job::spawn(|_| async {
            std::future::pending().await
        }));
        app.library.records[0].position = 45.0;
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let mut output = ctx.run_ui(input, |ui| {
            assert!(app.should_close(ui.ctx()));
            app.cloud.urgent = true;
            app.poll_cloud(ui.ctx());
            assert!(app.cloud.job.is_none());
        });
        assert!(
            !output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        output.textures_delta.clear();
        assert!(app.cloud.close_ready);
        let restored = sync::Journal::load(&app.cloud.path, &app.settings.sync.device_id).unwrap();
        let key = format!("song/{}/progress", song().key());
        assert_eq!(restored.values()[&key]["position"], 45.0);
        assert_ne!(restored.digest(), restored.uploaded);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn metadata_reply_preserves_unsubmitted_playback_progress_without_publishing_it() {
        let ctx = egui::Context::default();
        let directory = directory();
        let mut app = configured(&ctx, &directory);
        let key = format!("song/{}/progress", song().key());
        let report = sync::webdav::Report {
            journal: app.cloud.journal.clone().unwrap(),
            secrets: BTreeMap::new(),
        };
        app.library.records[0].position = 35.0;
        app.finish_cloud(report).unwrap();
        assert_eq!(app.library.records[0].position, 35.0);
        assert_eq!(
            app.cloud.journal.as_ref().unwrap().values()[&key]["position"],
            10.0
        );
        assert!(app.cloud.changed.is_none());
        app.checkpoint_cloud();
        assert_eq!(
            app.cloud.journal.as_ref().unwrap().values()[&key]["position"],
            35.0
        );
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod video_update_tests {
    use super::*;
    fn snapshot() -> Snapshot {
        Snapshot::from([
            (
                "creator/bilibili/123/profile".into(),
                json!({"mid":123,"uname":"Creator","usign":"","fans":0,"videos":1,"avatar":null}),
            ),
            ("creator/bilibili/123/favorite".into(), json!(true)),
            ("creator/bilibili/123/favorited_at".into(), json!(42)),
            ("video-updates/bilibili/123/42/baseline".into(), json!(100)),
            ("video-updates/bilibili/123/42/boundary".into(), json!(200)),
            (
                "video-updates/bilibili/123/42/post/BV0000000003".into(),
                json!({"bvid":"BV0000000003","title":"New upload","duration":"1:20","cover":null,"published":200}),
            ),
        ])
    }
    #[test]
    fn video_updates_cloud_pipeline_persists_and_merges_seen_state_with_inflight_new_uploads() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let ctx = egui::Context::default();
        let mut app = comic::visual_tests::settings_fixture(&ctx);
        app.library = library::Library::open(directory.join("library.json"));
        app.cloud.path = directory.join("journal.json");
        app.cloud.journal = Some(sync::Journal::load(&app.cloud.path, "A").unwrap());
        app.apply_cloud_library(&snapshot()).unwrap();
        assert_eq!(
            app.online_video.updates.counts(&app.library.creators),
            (1, 1)
        );
        app.capture_cloud(false).unwrap();
        let persisted = sync::Journal::load(&app.cloud.path, "A").unwrap();
        assert!(
            persisted
                .values()
                .contains_key("video-updates/bilibili/123/42/post/BV0000000003")
        );
        let mut remote = sync::Journal::load(&directory.join("remote.json"), "B").unwrap();
        remote.merge(&persisted.document).unwrap();
        remote.observed = remote.values();
        let mut marked = remote.values();
        marked.insert(
            "video-updates/bilibili/123/42/read/BV0000000003".into(),
            json!(true),
        );
        remote.capture(marked, &Snapshot::new());
        // A new post arrives locally while the remote request is in flight.
        let mut local = app.cloud_snapshot();
        local.insert("video-updates/bilibili/123/42/post/BV0000000004".into(), json!({"bvid":"BV0000000004","title":"Later upload","duration":"1:00","cover":null,"published":201}));
        app.import_video_updates(&local).unwrap();
        app.finish_cloud(sync::webdav::Report {
            journal: remote,
            secrets: Default::default(),
        })
        .unwrap();
        assert!(app.video_updates_cloud_ready());
        assert_eq!(
            app.online_video.updates.counts(&app.library.creators),
            (1, 1)
        );
        let merged = app.cloud_snapshot();
        assert_eq!(
            merged["video-updates/bilibili/123/42/read/BV0000000003"],
            json!(true)
        );
        assert!(merged.contains_key("video-updates/bilibili/123/42/post/BV0000000004"));
        // Restore from the existing sync journal without an update-library file.
        let restored = sync::Journal::load(&app.cloud.path, "A").unwrap();
        app.reset_video_updates();
        app.import_video_updates(&restored.values()).unwrap();
        assert_eq!(
            app.online_video.updates.counts(&app.library.creators),
            (1, 1)
        );
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
