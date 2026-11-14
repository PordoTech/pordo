use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(super) struct Record {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub music: bool,
    pub position: f64,
    pub duration: f64,
    pub played: u64,
    pub favorite: bool,
    pub favorited_at: u64,
    pub online: Option<crate::music_sources::OnlineSong>,
    pub video_cover: Option<String>,
}
impl Record {
    fn needs_media_identity_repair(&self) -> bool {
        crate::bilibili::video_id(&self.path).is_some() && (self.music || self.online.is_some())
    }
    fn normalize_media_identity(&mut self) -> bool {
        if self.needs_media_identity_repair() {
            self.music = false;
            self.online = None;
            return true;
        }
        false
    }
    pub fn resume_at(&self) -> Option<f64> {
        (self.position.is_finite()
            && self.duration.is_finite()
            && self.position > 1.0
            && self.position < self.duration - 2.0)
            .then_some(self.position)
    }
}

pub(super) fn favorite_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(super) fn sort_favorites(records: &mut [Record]) {
    records.sort_by_cached_key(|r| {
        (
            std::cmp::Reverse(r.favorited_at),
            r.online
                .as_ref()
                .map(|s| s.key())
                .unwrap_or_else(|| r.path.clone()),
        )
    });
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct CreatorRecord {
    pub uploader: crate::bilibili::Uploader,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub favorited_at: u64,
}

pub(super) struct Library {
    pub records: Vec<Record>,
    pub creators: Vec<CreatorRecord>,
    dirty: bool,
    saved: Instant,
    path: PathBuf,
    suppressed: Option<String>,
}

pub(super) struct RemovedHistory {
    path: String,
    played: u64,
    position: f64,
}
impl Library {
    fn path() -> PathBuf {
        Settings::queue_path().with_file_name("media-library.json")
    }
    pub fn load() -> Self {
        Self::open(Self::path())
    }
    pub(super) fn open(path: PathBuf) -> Self {
        let mut library = Self {
            creators: std::fs::read(path.with_extension("creators.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default(),
            records: std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default(),
            dirty: false,
            saved: Instant::now(),
            path,
            suppressed: None,
        };
        if library
            .records
            .iter()
            .any(Record::needs_media_identity_repair)
        {
            let backup = library
                .path
                .with_extension("json.before-video-identity-fix");
            if !backup.exists()
                && let Err(error) = std::fs::copy(&library.path, backup)
            {
                eprintln!("Cannot back up media library before identity repair: {error}");
                return library;
            }
            for record in &mut library.records {
                library.dirty |= record.normalize_media_identity();
            }
            library.flush();
        }
        library
    }
    pub fn creator_favorite(&self, mid: u64) -> bool {
        self.creators
            .iter()
            .any(|r| r.uploader.mid == mid && r.favorite)
    }
    pub fn favorite_creators(&self, query: &str) -> Vec<crate::bilibili::Uploader> {
        let query = query.trim().to_lowercase();
        let mut creators: Vec<_> = self
            .creators
            .iter()
            .filter(|r| {
                r.favorite
                    && (query.is_empty()
                        || format!("{} {}", r.uploader.uname, r.uploader.usign)
                            .to_lowercase()
                            .contains(&query))
            })
            .collect();
        creators.sort_by_key(|r| (std::cmp::Reverse(r.favorited_at), r.uploader.mid));
        creators.into_iter().map(|r| r.uploader.clone()).collect()
    }
    pub fn toggle_creator(&mut self, creator: &crate::bilibili::Uploader) -> anyhow::Result<()> {
        anyhow::ensure!(
            creator.mid > 0 && !creator.uname.trim().is_empty(),
            "无效的UP主 / Invalid creator"
        );
        let favorited_at = favorite_time().max(
            self.creators
                .iter()
                .map(|r| r.favorited_at)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        if let Some(record) = self
            .creators
            .iter_mut()
            .find(|r| r.uploader.mid == creator.mid)
        {
            record.favorite = !record.favorite;
            record.uploader = creator.clone();
            if record.favorite {
                record.favorited_at = favorited_at;
            }
        } else {
            self.creators.push(CreatorRecord {
                uploader: creator.clone(),
                favorite: true,
                favorited_at,
            });
        }
        self.save_records()
    }
    pub fn remember(&mut self, mut record: Record) {
        record.normalize_media_identity();
        if self.suppressed.as_deref() == Some(&record.path) {
            return;
        }
        if let Some(old) = self.records.iter_mut().find(|r| {
            r.path == record.path
                || crate::bilibili::video_id(&r.path).is_none()
                    && r.online
                        .as_ref()
                        .zip(record.online.as_ref())
                        .is_some_and(|(a, b)| a.key() == b.key())
        }) {
            let favorite = old.favorite;
            let favorited_at = old.favorited_at;
            let online = old.online.clone();
            let video_cover = old.video_cover.clone();
            *old = record;
            old.favorite = favorite;
            old.favorited_at = favorited_at;
            if old.online.is_none() {
                old.online = online;
            }
            if old.video_cover.is_none() {
                old.video_cover = video_cover;
            }
            old.normalize_media_identity();
        } else {
            self.records.push(record);
        }
        self.dirty = true;
        if self.saved.elapsed() >= Duration::from_secs(5) {
            self.flush();
        }
    }
    pub fn toggle_favorite(&mut self, path: &str, title: &str, music: bool) {
        if let Some(record) = self.records.iter_mut().find(|r| r.path == path) {
            record.normalize_media_identity();
            record.favorite = !record.favorite;
            if record.favorite {
                record.favorited_at = favorite_time().max(record.favorited_at.saturating_add(1));
            }
        } else {
            let mut record = Record {
                path: path.into(),
                title: title.into(),
                music,
                favorite: true,
                favorited_at: favorite_time(),
                ..Default::default()
            };
            record.normalize_media_identity();
            self.records.push(record);
        }
        self.dirty = true;
        self.flush();
    }
    pub fn begin_playback(&mut self) {
        self.suppressed = None;
    }
    pub fn bind_online(&mut self, path: String, song: crate::music_sources::OnlineSong) {
        let path = if crate::bilibili::video_id(&path).is_some() {
            format!("online://{}", song.key())
        } else {
            path
        };
        let key = song.key();
        if let Some(old) = self.records.iter_mut().find(|r| {
            crate::bilibili::video_id(&r.path).is_none()
                && r.online.as_ref().is_some_and(|s| s.key() == key)
        }) {
            if old.path == path && old.online.as_ref() == Some(&song) && old.music {
                return;
            }
            old.path = path;
            old.title = song.title.clone();
            old.artist = song.artist.clone();
            old.album = song.album.clone();
            old.online = Some(song);
            old.music = true;
        } else if let Some(old) = self.records.iter_mut().find(|r| r.path == path) {
            old.online = Some(song);
            old.music = true;
        } else {
            self.records.push(Record {
                path,
                title: song.title.clone(),
                artist: song.artist.clone(),
                album: song.album.clone(),
                music: true,
                online: Some(song),
                ..Default::default()
            });
        }
        self.dirty = true;
        self.flush();
    }
    pub fn save_records(&mut self) -> anyhow::Result<()> {
        self.dirty = true;
        self.flush();
        anyhow::ensure!(
            !self.dirty,
            "无法保存媒体资料库 / Cannot save media library"
        );
        Ok(())
    }
    pub fn remove_history(&mut self, path: &str, active: bool) -> Option<RemovedHistory> {
        let record = self
            .records
            .iter_mut()
            .find(|r| r.path == path && r.played > 0)?;
        let removed = RemovedHistory {
            path: path.into(),
            played: record.played,
            position: record.position,
        };
        record.played = 0;
        record.position = 0.0;
        if active {
            self.suppressed = Some(path.into());
        }
        self.dirty = true;
        self.flush();
        Some(removed)
    }
    pub fn restore_history(&mut self, removed: RemovedHistory) {
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|r| r.path == removed.path && r.played == 0)
        {
            record.played = removed.played;
            record.position = removed.position;
            self.dirty = true;
        }
        if self.suppressed.as_deref() == Some(&removed.path) {
            self.suppressed = None;
        }
        self.flush();
    }
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let path = &self.path;
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, serde_json::to_vec(&self.records)?)?;
            std::fs::rename(temporary, path)?;
            let creators = path.with_extension("creators.json");
            if !self.creators.is_empty() || creators.exists() {
                let temporary = creators.with_extension("json.tmp");
                std::fs::write(&temporary, serde_json::to_vec(&self.creators)?)?;
                std::fs::rename(temporary, creators)?;
            }
            Ok(())
        })();
        if result.is_ok() {
            self.dirty = false;
            self.saved = Instant::now();
        }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        self.flush();
    }
}

impl App {
    pub(super) fn active_record_path(&self) -> Option<String> {
        if let Some(path) = self.online_video.record_path(self.source.as_ref()) {
            return Some(path);
        }
        if let Some(song) = &self.online_origin {
            return Some(
                self.library
                    .records
                    .iter()
                    .find(|r| r.online.as_ref().is_some_and(|s| s.key() == song.key()))
                    .map(|r| r.path.clone())
                    .unwrap_or_else(|| format!("online://{}", song.key())),
            );
        }
        self.source
            .as_ref()
            .filter(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned())
    }

    pub(super) fn remove_history(&mut self, path: &str) {
        let active = self.active_record_path().as_deref() == Some(path);
        if let Some(removed) = self.library.remove_history(path, active) {
            self.history_undo = Some((removed, Instant::now()));
            if self
                .workspace
                .resume
                .as_ref()
                .is_some_and(|(p, _)| p == path)
            {
                self.workspace.resume = None;
            }
        }
    }

    pub(super) fn history_notice(&mut self, ctx: &egui::Context) {
        let Some((_, created)) = &self.history_undo else {
            return;
        };
        let remaining = Duration::from_secs(5).saturating_sub(created.elapsed());
        if remaining.is_zero() {
            self.history_undo = None;
            return;
        }
        ctx.request_repaint_after(remaining);
        let l = self.settings.language;
        let (undo, _) = design::floating_toast(
            ctx,
            Id::new("history-undo"),
            l.text("已删除播放记录", "Playback history removed"),
            Some(l.text("撤销", "Undo")),
            None,
        );
        if undo && let Some((removed, _)) = self.history_undo.take() {
            self.library.restore_history(removed);
        }
    }

    pub(super) fn update_library(&mut self) {
        let Some(path) = self.active_record_path() else {
            return;
        };
        let Some(player) = &self.player else { return };
        if self.loading || !player.duration().is_finite() {
            return;
        }
        let position = self
            .pending_seek
            .map_or_else(|| player.position(), |(_, t)| t);
        let previous = self.library.records.iter().find(|r| r.path == path);
        let video_cover = self
            .bilibili_cover_url()
            .or_else(|| previous.and_then(|r| r.video_cover.clone()));
        let music = crate::bilibili::video_id(&path).is_none() && self.is_music();
        if previous.is_some_and(|r| {
            (r.position - position).abs() < 1.0
                && r.title == self.title
                && r.video_cover == video_cover
                && r.music == music
        }) {
            return;
        }
        let info = self.media_info.as_ref();
        self.library.remember(Record {
            path,
            title: self.title.clone(),
            artist: info.and_then(|i| i.artist.clone()).unwrap_or_default(),
            album: info.and_then(|i| i.album.clone()).unwrap_or_default(),
            music,
            position,
            duration: player.duration(),
            played: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            favorite: false,
            favorited_at: 0,
            online: self
                .online_origin
                .clone()
                .or_else(|| previous.and_then(|r| r.online.clone())),
            video_cover,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_bilibili_music_flags_are_repaired_without_losing_favorites_or_original_data() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("library.json");
        let song = crate::music_sources::OnlineSong {
            source: "chksz-163".into(),
            id: "song-a".into(),
            title: "Song A".into(),
            ..Default::default()
        };
        let original = vec![
            Record {
                path: "https://www.bilibili.com/video/BV1GJ411x7h7?p=2".into(),
                title: "Video favorite".into(),
                music: true,
                online: Some(song.clone()),
                favorite: true,
                favorited_at: 42,
                played: 123,
                position: 20.0,
                duration: 300.0,
                ..Default::default()
            },
            Record {
                path: "audio-only.mp4".into(),
                music: true,
                favorite: true,
                ..Default::default()
            },
        ];
        let bytes = serde_json::to_vec(&original).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut library = Library::open(path.clone());
        let repaired = &library.records[0];
        assert!(!repaired.music && repaired.online.is_none());
        assert!(
            repaired.favorite
                && repaired.favorited_at == 42
                && repaired.played == 123
                && repaired.position == 20.0
        );
        assert!(
            library.records[1].music,
            "an audio-only MP4 must not be reclassified by extension"
        );
        let backup = path.with_extension("json.before-video-identity-fix");
        assert_eq!(std::fs::read(&backup).unwrap(), bytes);
        library.bind_online(original[0].path.clone(), song);
        assert_eq!(
            library.records.len(),
            3,
            "music binding must create its own identity"
        );
        assert_eq!(library.records[0].path, original[0].path);
        assert!(!library.records[0].music && library.records[0].online.is_none());
        drop(library);
        let library = Library::open(path);
        assert_eq!(library.records.len(), 3);
        assert_eq!(std::fs::read(backup).unwrap(), bytes);
        drop(library);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn video_history_updates_cannot_merge_with_online_song_favorites() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string());
        let mut library = Library::open(folder.join("library.json"));
        let song = crate::music_sources::OnlineSong {
            source: "chksz-163".into(),
            id: "song-a".into(),
            ..Default::default()
        };
        library.bind_online("song.flac".into(), song.clone());
        library.toggle_favorite("song.flac", "Song", true);
        let at = library.records[0].favorited_at;
        let video = "https://www.bilibili.com/video/BV1GJ411x7h7?p=1";
        library.remember(Record {
            path: video.into(),
            title: "Video".into(),
            music: true,
            online: Some(song),
            ..Default::default()
        });
        library.toggle_favorite(video, "Video", true);
        assert_eq!(library.records.len(), 2);
        assert_eq!(library.records[0].path, "song.flac");
        assert!(
            library.records[0].favorite
                && library.records[0].music
                && library.records[0].favorited_at == at
        );
        assert!(
            library.records[1].favorite
                && !library.records[1].music
                && library.records[1].online.is_none()
        );
        drop(library);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn creator_favorites_use_uid_preserve_media_and_survive_restart_and_removal() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("library.json");
        std::fs::write(&path, br#"[{"path":"old.mp4","title":"Old video"}]"#).unwrap();
        let mut library = Library::open(path.clone());
        let mut creator: crate::bilibili::Uploader =
            serde_json::from_value(serde_json::json!({"mid":123,"uname":"Same name"})).unwrap();
        library.toggle_creator(&creator).unwrap();
        let mut second = creator.clone();
        second.mid = 456;
        library.toggle_creator(&second).unwrap();
        assert_eq!(library.favorite_creators("")[0].mid, 456);
        creator.uname = "Renamed creator".into();
        library.toggle_creator(&creator).unwrap();
        assert!(!library.creator_favorite(123));
        library.toggle_creator(&creator).unwrap();
        assert_eq!(library.creators.len(), 2);
        assert_eq!(library.favorite_creators("renamed")[0].mid, 123);
        assert_eq!(library.favorite_creators("")[0].mid, 123);
        library.toggle_creator(&second).unwrap();
        drop(library);
        let library = Library::open(path);
        assert!(library.creator_favorite(123));
        assert!(!library.creator_favorite(456));
        assert_eq!(library.records.len(), 1);
        assert_eq!(library.records[0].path, "old.mp4");
        drop(library);
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn favorite_timestamps_survive_metadata_binding_history_and_restart() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string());
        let path = folder.join("library.json");
        let mut library = Library::open(path.clone());
        library.toggle_favorite("old.flac", "Old", true);
        let at = library.records[0].favorited_at;
        assert!(at > 0);
        library.remember(Record {
            path: "old.flac".into(),
            title: "Refreshed".into(),
            played: u64::MAX,
            music: true,
            ..Default::default()
        });
        library.bind_online(
            "old.flac".into(),
            crate::music_sources::OnlineSong {
                source: "chksz-163".into(),
                id: "1".into(),
                ..Default::default()
            },
        );
        library.records.push(Record {
            path: "new.mp4".into(),
            favorited_at: at + 1,
            favorite: true,
            ..Default::default()
        });
        library.save_records().unwrap();
        drop(library);
        let mut library = Library::open(path);
        sort_favorites(&mut library.records);
        assert_eq!(library.records[0].path, "new.mp4");
        assert_eq!(library.records[1].favorited_at, at);
        library.toggle_favorite("old.flac", "Old", true);
        library.toggle_favorite("old.flac", "Old", true);
        assert!(library.records[1].favorited_at > at);
        let old: Record = serde_json::from_str(r#"{"favorite":true}"#).unwrap();
        assert_eq!(old.favorited_at, 0);
        drop(library);
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn deleting_history_persists_keeps_favorites_and_suppresses_only_current_session() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string());
        let path = folder.join("library.json");
        let mut library = Library::open(path.clone());
        let original = Record {
            path: "song.flac".into(),
            title: "Track".into(),
            artist: "Artist".into(),
            music: true,
            position: 60.0,
            duration: 240.0,
            played: 100,
            ..Default::default()
        };
        library.remember(original.clone());
        library.toggle_favorite(&original.path, &original.title, true);
        let removed = library.remove_history(&original.path, true).unwrap();
        library.remember(Record {
            position: 70.0,
            played: 101,
            ..original.clone()
        });
        assert_eq!(library.records[0].played, 0);
        assert!(library.records[0].resume_at().is_none());
        assert!(library.records[0].favorite);
        let persisted = Library::open(path.clone());
        assert_eq!(persisted.records[0].played, 0);
        assert_eq!(persisted.records[0].artist, "Artist");
        drop(persisted);
        // Undo restores history without rolling back independent favorite changes.
        library.toggle_favorite(&original.path, &original.title, true);
        library.restore_history(removed);
        assert_eq!(library.records[0].resume_at(), Some(60.0));
        assert!(!library.records[0].favorite);
        library.remove_history(&original.path, true).unwrap();
        library.begin_playback();
        library.remember(original);
        assert_eq!(library.records[0].played, 100);
        drop(library);
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn favorites_and_playback_history_survive_restart_and_metadata_refresh() {
        let folder = std::env::temp_dir().join(format!("pordo-library-{}", uuid::Uuid::new_v4()));
        let path = folder.join("library.json");
        {
            let mut library = Library::open(path.clone());
            library.toggle_favorite("song.flac", "filename", true);
            library.remember(Record {
                path: "song.flac".into(),
                title: "Track title".into(),
                artist: "Artist".into(),
                position: 74.0,
                duration: 251.0,
                played: 5,
                music: true,
                ..Default::default()
            });
        }
        let library = Library::open(path);
        assert_eq!(library.records.len(), 1);
        assert!(library.records[0].favorite);
        assert_eq!(library.records[0].title, "Track title");
        assert_eq!(library.records[0].resume_at(), Some(74.0));
        drop(library);
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn completed_and_invalid_playback_positions_do_not_resume() {
        for position in [0.0, -10.0, 249.0, 251.0, f64::NAN, f64::INFINITY] {
            assert!(
                Record {
                    position,
                    duration: 251.0,
                    ..Default::default()
                }
                .resume_at()
                .is_none()
            );
        }
    }
}
