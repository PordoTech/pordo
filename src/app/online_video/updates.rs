//! Creator update content lives in the existing cloud journal, never a separate local library.
use super::*;
use crate::sync::model::Snapshot;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const PREFIX: &str = "video-updates/bilibili/";
const INTERVAL: u64 = 15 * 60;
const ERROR_INTERVAL: u64 = 60 * 60;
const PAGE_BUDGET: usize = 5;

#[derive(Clone, Default)]
pub(super) struct Content {
    pub baseline: Option<u64>,
    pub boundary: u64,
    pub posts: BTreeMap<String, Post>,
    pub read: BTreeSet<String>,
    pub played: BTreeSet<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Post {
    pub bvid: String,
    pub title: String,
    pub duration: String,
    pub cover: Option<String>,
    pub published: Option<u64>,
}
impl Post {
    pub fn video(&self) -> bilibili::Video {
        bilibili::Video {
            bvid: self.bvid.clone(),
            title: self.title.clone(),
            uploader: String::new(),
            duration: self.duration.clone(),
            cover: self.cover.clone(),
            published: self.published,
            views: None,
        }
    }
}
impl Content {
    pub fn unread(&self) -> Vec<Post> {
        let mut posts: Vec<_> = self
            .posts
            .values()
            .filter(|p| !self.read.contains(&p.bvid) && !self.played.contains(&p.bvid))
            .cloned()
            .collect();
        posts.sort_by_key(|p| (std::cmp::Reverse(p.published.unwrap_or(0)), p.bvid.clone()));
        posts
    }
}
pub(super) fn key(record: &library::CreatorRecord) -> String {
    format!("{}/{}", record.uploader.mid, record.favorited_at)
}

#[derive(Default)]
pub(super) struct Local {
    pub checked: u64,
    pub success: u64,
    pub error: Option<String>,
    pub next_page: usize,
    boundary: u64,
    maximum: u64,
    pages: usize,
    pub paused: bool,
}
struct Request {
    key: String,
    page: usize,
    job: Job<SearchPage>,
}
struct Undo {
    key: String,
    ids: Vec<String>,
    at: Instant,
}
#[derive(Default)]
pub(in crate::app) struct State {
    pub(super) content: BTreeMap<String, Content>,
    watched: BTreeSet<String>,
    pub(super) local: BTreeMap<String, Local>,
    queue: VecDeque<String>,
    jobs: Vec<Request>,
    active: BTreeSet<String>,
    after: Option<Instant>,
    pub(super) total: usize,
    pub(super) done: usize,
    pub(super) collapsed: BTreeSet<String>,
    pub(super) expanded: BTreeSet<String>,
    undo: Option<Undo>,
    pub(super) status: String,
    pub(super) return_to_updates: bool,
    trigger: Option<workspace::Page>,
    startup: bool,
    pub(super) stopped: bool,
    marked_playback: Option<String>,
    history_dirty: bool,
}
impl State {
    pub fn snapshot(&self, data: &mut Snapshot) {
        for id in &self.watched {
            data.insert(format!("{PREFIX}viewed/{id}"), json!(true));
        }
        for (key, content) in &self.content {
            let prefix = format!("{PREFIX}{key}");
            if let Some(baseline) = content.baseline {
                data.insert(format!("{prefix}/baseline"), json!(baseline));
            }
            data.insert(format!("{prefix}/boundary"), json!(content.boundary));
            for (id, post) in &content.posts {
                data.insert(format!("{prefix}/post/{id}"), json!(post));
            }
            for id in &content.read {
                data.insert(format!("{prefix}/read/{id}"), json!(true));
            }
            for id in &content.played {
                data.insert(format!("{prefix}/played/{id}"), json!(true));
            }
        }
    }
    pub fn import(&mut self, values: &Snapshot) -> Result<()> {
        let mut next: BTreeMap<String, Content> = BTreeMap::new();
        let mut watched = BTreeSet::new();
        for (field, value) in values
            .iter()
            .filter_map(|(k, v)| k.strip_prefix(PREFIX).map(|k| (k, v)))
        {
            if let Some(id) = field.strip_prefix("viewed/") {
                ensure!(
                    bilibili::video_id(id).as_deref() == Some(id) && value.is_boolean(),
                    "Invalid watched video identity"
                );
                if value == &json!(true) {
                    watched.insert(id.to_owned());
                }
                continue;
            }
            let parts: Vec<_> = field.split('/').collect();
            ensure!(
                parts.len() >= 3
                    && parts[0].parse::<u64>().is_ok_and(|id| id > 0)
                    && parts[1].parse::<u64>().is_ok(),
                "Invalid creator update identity"
            );
            let content = next
                .entry(format!("{}/{}", parts[0], parts[1]))
                .or_default();
            match (parts[2], parts.get(3)) {
                ("baseline", None) => {
                    content.baseline = Some(
                        value
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("Invalid update baseline"))?,
                    )
                }
                ("boundary", None) => {
                    content.boundary = value
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("Invalid update boundary"))?
                }
                ("post", Some(id)) => {
                    let post: Post = serde_json::from_value(value.clone())?;
                    ensure!(
                        parts.len() == 4
                            && bilibili::video_id(&post.bvid).as_deref() == Some(*id)
                            && post.bvid == *id,
                        "Invalid synced video identity"
                    );
                    content.posts.insert(post.bvid.clone(), post);
                }
                ("read" | "played", Some(id)) => {
                    ensure!(
                        parts.len() == 4
                            && bilibili::video_id(id).as_deref() == Some(*id)
                            && value.is_boolean(),
                        "Invalid video read state"
                    );
                    if value == &json!(true) {
                        if parts[2] == "read" {
                            content.read.insert(id.to_string());
                        } else {
                            content.played.insert(id.to_string());
                        }
                    }
                }
                _ => anyhow::bail!("Unsupported video update field"),
            }
        }
        for content in next.values_mut() {
            for id in &watched {
                if content.posts.contains_key(id) {
                    content.played.insert(id.clone());
                }
            }
        }
        self.history_dirty = true;
        self.watched = watched;
        self.content = next;
        Ok(())
    }
    pub fn counts(&self, creators: &[library::CreatorRecord]) -> (usize, usize) {
        let counts: Vec<_> = creators
            .iter()
            .filter(|r| r.favorite)
            .map(|r| self.content.get(&key(r)).map_or(0, |c| c.unread().len()))
            .filter(|n| *n > 0)
            .collect();
        (counts.len(), counts.iter().sum())
    }
    pub fn running(&self) -> bool {
        !self.queue.is_empty() || !self.jobs.is_empty()
    }
    fn reconcile(&mut self, creators: &[library::CreatorRecord]) {
        let active: BTreeSet<_> = creators.iter().filter(|r| r.favorite).map(key).collect();
        self.queue.retain(|k| active.contains(k));
        self.jobs.retain(|r| active.contains(&r.key));
        self.local.retain(|k, _| active.contains(k));
        self.active = active;
    }
    pub fn enqueue(
        &mut self,
        creators: &[library::CreatorRecord],
        manual: bool,
        only: Option<&str>,
        can_initialize: bool,
    ) {
        self.reconcile(creators);
        if self.running() && only.is_none() {
            return;
        }
        if !self.running() {
            self.total = 0;
            self.done = 0;
        }
        if manual {
            self.stopped = false;
            self.status.clear();
        }
        if self.stopped {
            return;
        }
        let now = crate::comics::now();
        for creator in creators.iter().filter(|r| r.favorite) {
            let key = key(creator);
            if only.is_some_and(|id| id != key)
                || self.queue.contains(&key)
                || self.jobs.iter().any(|r| r.key == key)
            {
                continue;
            }
            let initialized = self.content.get(&key).is_some_and(|c| c.baseline.is_some());
            if !initialized && !can_initialize {
                continue;
            }
            let local = self.local.entry(key.clone()).or_default();
            if !manual
                && (local.paused
                    || now.saturating_sub(local.checked)
                        < if local.error.is_some() {
                            ERROR_INTERVAL
                        } else {
                            INTERVAL
                        })
            {
                continue;
            }
            local.pages = 0;
            local.paused = false;
            local.error = None;
            if local.next_page == 0 {
                local.next_page = 1;
                local.boundary = self.content.get(&key).map_or(0, |c| c.boundary);
                local.maximum = local.boundary;
            }
            self.queue.push_back(key);
            self.total += 1;
        }
    }
    /// Merge one page completely before deciding whether an old boundary was reached.
    fn page(&mut self, key: &str, page: SearchPage) {
        if !self.active.contains(key) {
            return;
        }
        self.history_dirty = true;
        let now = crate::comics::now();
        let content = self.content.entry(key.into()).or_default();
        let local = self.local.entry(key.into()).or_default();
        let first = content.baseline.is_none();
        let maximum = page
            .videos
            .iter()
            .filter_map(|v| v.published)
            .max()
            .unwrap_or(now);
        let reached = !page.videos.is_empty()
            && page
                .videos
                .iter()
                .all(|v| v.published.is_some_and(|at| at < local.boundary))
            && page
                .videos
                .iter()
                .any(|v| content.posts.contains_key(&v.bvid));
        if first {
            content.baseline = Some(maximum);
        }
        for video in page.videos {
            if bilibili::video_id(&video.bvid).as_deref() != Some(&video.bvid) {
                continue;
            }
            let historical = first
                || video
                    .published
                    .is_some_and(|at| at < content.baseline.unwrap_or(0));
            // Never turn a remotely unread item into a baseline/read item.
            if historical && !content.posts.contains_key(&video.bvid) {
                content.read.insert(video.bvid.clone());
            }
            if self.watched.contains(&video.bvid) {
                content.played.insert(video.bvid.clone());
            }
            content.posts.insert(
                video.bvid.clone(),
                Post {
                    bvid: video.bvid,
                    title: video.title,
                    cover: video.cover,
                    duration: video.duration,
                    published: video.published,
                },
            );
        }
        local.maximum = local.maximum.max(maximum);
        local.pages += 1;
        local.checked = now;
        if first || !page.has_more || reached {
            content.boundary = content.boundary.max(local.maximum);
            local.success = now;
            local.error = None;
            local.next_page = 0;
            local.paused = false;
            self.done += 1;
        } else {
            local.next_page += 1;
            if local.pages >= PAGE_BUDGET {
                local.paused = true;
                self.done += 1;
            } else {
                self.queue.push_back(key.into());
            }
        }
    }
    pub(super) fn mark_read(&mut self, key: &str, ids: Vec<String>) {
        if let Some(content) = self.content.get_mut(key) {
            let ids: Vec<_> = ids
                .into_iter()
                .filter(|id| {
                    content.posts.contains_key(id)
                        && !content.read.contains(id)
                        && !content.played.contains(id)
                })
                .collect();
            content.read.extend(ids.iter().cloned());
            self.undo = Some(Undo {
                key: key.into(),
                ids,
                at: Instant::now(),
            });
        }
    }
    fn mark_played(&mut self, id: &str) -> bool {
        if !self
            .content
            .iter()
            .any(|(key, content)| self.active.contains(key) && content.posts.contains_key(id))
        {
            return false;
        }
        self.watched.insert(id.into());
        for (key, content) in &mut self.content {
            if self.active.contains(key) && content.posts.contains_key(id) {
                content.played.insert(id.into());
            }
        }
        true
    }
    pub(super) fn undo_available(&self) -> bool {
        self.undo
            .as_ref()
            .is_some_and(|u| u.at.elapsed() < Duration::from_secs(8))
    }
    pub(super) fn undo(&mut self) {
        if let Some(undo) = self
            .undo
            .take()
            .filter(|u| u.at.elapsed() < Duration::from_secs(8))
            && self.active.contains(&undo.key)
            && let Some(content) = self.content.get_mut(&undo.key)
        {
            for id in undo.ids {
                if !content.played.contains(&id) {
                    content.read.remove(&id);
                }
            }
        }
    }
}
impl App {
    pub(in crate::app) fn video_updates_snapshot(&self, data: &mut Snapshot) {
        self.online_video.updates.snapshot(data);
    }
    pub(in crate::app) fn import_video_updates(&mut self, values: &Snapshot) -> Result<()> {
        self.online_video.updates.import(values)
    }
    pub(in crate::app) fn reset_video_updates(&mut self) {
        self.online_video.updates = State::default();
    }
    pub(in crate::app) fn queue_video_updates(&mut self, manual: bool, only: Option<&str>) {
        let can_initialize = self.video_updates_cloud_ready();
        self.online_video
            .updates
            .enqueue(&self.library.creators, manual, only, can_initialize);
        if self.online_video.updates.running() {
            self.ensure_video_api();
        }
    }
    pub(in crate::app) fn poll_video_updates(&mut self, ctx: &egui::Context) {
        let page = self.workspace.page;
        let entered = self.online_video.updates.trigger != Some(page);
        self.online_video.updates.trigger = Some(page);
        self.online_video.updates.reconcile(&self.library.creators);
        let new_subscription = self
            .library
            .creators
            .iter()
            .filter(|r| r.favorite)
            .any(|r| !self.online_video.updates.local.contains_key(&key(r)));
        let startup = !self.online_video.updates.startup;
        if self.settings.video.check_updates
            && (startup
                || new_subscription
                || (entered && matches!(page, workspace::Page::Home | workspace::Page::Video)))
        {
            self.online_video.updates.startup = true;
            self.queue_video_updates(false, None);
        }
        // Read markers follow real playback progress, not card clicks or metadata loads.
        let playing = self
            .online_video
            .active
            .as_ref()
            .filter(|_| self.bilibili_controls_available())
            .and_then(|a| {
                self.player
                    .as_ref()
                    .filter(|p| p.is_playing() && p.position() >= 1.0)
                    .map(|_| a.detail.bvid.clone())
            });
        if let Some(id) = playing {
            if self.online_video.updates.marked_playback.as_ref() != Some(&id)
                && self.online_video.updates.mark_played(&id)
            {
                self.online_video.updates.marked_playback = Some(id);
            }
        } else {
            self.online_video.updates.marked_playback = None;
        }
        let state = &mut self.online_video.updates;
        let mut completed = vec![];
        state.jobs.retain(|r| {
            if let Some(result) = r.job.poll() {
                completed.push((r.key.clone(), r.page, result));
                false
            } else {
                true
            }
        });
        for (key, _page, result) in completed {
            if !state.active.contains(&key) {
                continue;
            }
            match result {
                Ok(page) => state.page(&key, page),
                Err(error) => {
                    let message = error.to_string();
                    let restricted = [
                        "429",
                        "-352",
                        "-412",
                        "验证",
                        "verification",
                        "限流",
                        "rate limit",
                    ]
                    .iter()
                    .any(|s| message.to_lowercase().contains(s));
                    let local = state.local.entry(key).or_default();
                    local.error = Some(message.clone());
                    local.checked = crate::comics::now();
                    state.done += 1;
                    if restricted {
                        state.queue.clear();
                        state.jobs.clear();
                        state.stopped = true;
                        state.status = message;
                    }
                }
            }
        }
        if state.history_dirty {
            // Only update watched markers for videos in a subscribed creator's list.
            // Unrelated playback history is never exported as update data.
            for record in self
                .library
                .records
                .iter()
                .filter(|r| !r.music && r.played > 0 && r.position >= 1.0)
            {
                if let Some(id) = bilibili::video_id(&record.path) {
                    state.mark_played(&id);
                }
            }
            state.history_dirty = false;
        }
        if state.running() {
            if let Some(api) = self.online_video.api.clone() {
                let now = Instant::now();
                if state.jobs.len() < 2
                    && state.after.is_none_or(|at| now >= at)
                    && let Some(key) = state.queue.pop_front()
                {
                    let mid = key
                        .split('/')
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    let page = state.local.get(&key).map_or(1, |l| l.next_page.max(1));
                    state.after = Some(now + Duration::from_secs(2));
                    state.jobs.push(Request {
                        key,
                        page,
                        job: Job::spawn(move |_| async move {
                            tokio::time::timeout(Duration::from_secs(20), api.uploads(mid, page))
                                .await
                                .map_err(|_| {
                                    anyhow::anyhow!("检查更新超时 / Update check timed out")
                                })?
                        }),
                    });
                }
            } else if self.online_video.init.is_none() {
                state.status = self.online_video.auth_status.clone();
                state.queue.clear();
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests;
