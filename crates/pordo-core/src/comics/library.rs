use super::*;
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::{collections::HashSet, path::Path};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub chapter: String,
    pub page: usize,
    pub fraction: f32,
    pub page_count: usize,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub book: Book,
    pub chapters: Vec<Chapter>,
    pub favorite: bool,
    pub favorited_at: u64,
    pub progress: Option<Progress>,
    pub last_read: u64,
    pub checked: u64,
    pub check_error: Option<String>,
    pub known: HashSet<String>,
    pub read: HashSet<String>,
    pub new_unread: HashSet<String>,
    pub update_summary: Option<UpdateSummary>,
    pub summary_unread: bool,
    pub mode: Option<ReadingMode>,
    pub zoom: f32,
}
impl Entry {
    pub fn has_updates(&self) -> bool {
        self.summary_unread || !self.new_unread.is_empty()
    }
    /// Summary checks never replace the directory or change reading progress.
    pub fn refresh_summary(&mut self, summary: UpdateSummary) {
        if self
            .update_summary
            .as_ref()
            .is_some_and(|old| old.updated_at > summary.updated_at)
        {
            return;
        }
        let changed = match &self.update_summary {
            Some(old) => old != &summary,
            None => [
                (ChapterKind::Chapter, &summary.last_chapter),
                (ChapterKind::Volume, &summary.last_volume),
            ]
            .into_iter()
            .filter(|(_, serial)| !serial.is_empty())
            .any(|(kind, serial)| {
                !self.chapters.iter().any(|c| {
                    let local = c.name.split_whitespace().nth(1).unwrap_or("");
                    c.kind == kind
                        && (local == serial
                            || serial
                                .parse::<f64>()
                                .ok()
                                .zip(local.parse::<f64>().ok())
                                .is_some_and(|(a, b)| a == b))
                })
            }),
        };
        self.summary_unread |= self.favorite && changed;
        self.update_summary = Some(summary);
        self.checked = now();
        self.check_error = None;
    }
    pub fn refresh(&mut self, detail: Detail) {
        if let Some(summary) = &detail.summary
            && self
                .update_summary
                .as_ref()
                .is_none_or(|old| old.updated_at <= summary.updated_at)
        {
            self.update_summary = Some(summary.clone());
            self.summary_unread = false;
        }
        // Unique title matching preserves read state when a source changes URLs.
        for chapter in &detail.chapters {
            if self.known.contains(chapter.key()) {
                continue;
            }
            let old: Vec<_> = self
                .chapters
                .iter()
                .filter(|old| old.name == chapter.name)
                .collect();
            if old.len() == 1
                && detail
                    .chapters
                    .iter()
                    .filter(|c| c.name == chapter.name)
                    .count()
                    == 1
            {
                let old = old[0].key();
                let key = chapter.key().to_owned();
                if self.known.contains(old) {
                    self.known.insert(key.clone());
                }
                if self.read.contains(old) {
                    self.read.insert(key.clone());
                }
                if self.new_unread.remove(old) {
                    self.new_unread.insert(key.clone());
                }
                if let Some(p) = &mut self.progress
                    && p.chapter == old
                {
                    p.chapter = key;
                }
            }
        }
        if self.favorite && !self.known.is_empty() {
            self.new_unread.extend(
                detail
                    .chapters
                    .iter()
                    .filter(|c| !self.known.contains(c.key()) && !self.read.contains(c.key()))
                    .map(|c| c.key().to_owned()),
            );
        }
        self.known
            .extend(detail.chapters.iter().map(|c| c.key().to_owned()));
        let current: HashSet<_> = detail.chapters.iter().map(|c| c.key()).collect();
        self.new_unread.retain(|id| current.contains(id.as_str()));
        self.book = detail.book;
        self.chapters = detail.chapters;
        self.checked = now();
        self.check_error = None;
    }
    pub fn set_favorite(&mut self, favorite: bool) {
        if favorite && !self.favorite {
            self.favorited_at = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64)
                .max(self.favorited_at.saturating_add(1));
        }
        self.favorite = favorite;
        if !favorite {
            self.new_unread.clear();
            self.summary_unread = false;
        }
        self.known
            .extend(self.chapters.iter().map(|c| c.key().to_owned()));
    }
    pub fn mark_read(&mut self, chapter: &str) {
        self.read.insert(chapter.into());
        self.new_unread.remove(chapter);
    }
}
pub struct Library {
    connection: Connection,
    pub entries: Vec<Entry>,
}
impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let connection =
            Connection::open(path).context("无法打开漫画书架 / Cannot open comic library")?;
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA user_version=1; CREATE TABLE IF NOT EXISTS entries (id TEXT PRIMARY KEY, payload TEXT NOT NULL);")?;
        let entries = {
            let mut statement = connection.prepare("SELECT payload FROM entries")?;
            let payloads = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            payloads
                .into_iter()
                .map(|s| serde_json::from_str(&s))
                .collect::<serde_json::Result<Vec<Entry>>>()
                .context("漫画书架数据损坏 / Invalid comic library")?
        };
        Ok(Self {
            connection,
            entries,
        })
    }
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.book.key() == key)
    }
    pub fn save(&mut self, entry: Entry) -> Result<()> {
        let key = entry.book.key();
        self.connection.execute("INSERT INTO entries(id,payload) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload",params![key,serde_json::to_string(&entry)?])?;
        if let Some(existing) = self.entries.iter_mut().find(|e| e.book.key() == key) {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
        Ok(())
    }
    pub fn refresh(&mut self, detail: Detail) -> Result<()> {
        let mut entry = self.get(&detail.book.key()).cloned().unwrap_or_default();
        entry.refresh(detail);
        self.save(entry)
    }
}

pub fn sort_favorites(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|e| (std::cmp::Reverse(e.favorited_at), e.book.key()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(chapter: &str, date: &str) -> UpdateSummary {
        UpdateSummary {
            updated_at: date.into(),
            last_chapter: chapter.into(),
            last_volume: "".into(),
        }
    }
    fn chapter(id: &str, serial: &str) -> Chapter {
        Chapter {
            id: id.into(),
            name: format!("第 {serial} 话"),
            ..Default::default()
        }
    }

    #[test]
    fn summaries_preserve_directory_progress_and_unread_state_across_restart() {
        let mut entry = Entry {
            favorite: true,
            chapters: vec![chapter("one", "01")],
            progress: Some(Progress {
                chapter: "one".into(),
                page: 7,
                ..Default::default()
            }),
            ..Default::default()
        };
        entry.known.insert("one".into());
        let first = summary("1", "2026-01-01T00:00:00Z");
        entry.refresh_summary(first.clone());
        assert!(
            !entry.has_updates(),
            "first matching summary establishes a baseline"
        );
        let latest = summary("2.5", "2026-01-02T00:00:00Z");
        entry.refresh_summary(latest.clone());
        assert!(entry.has_updates());
        assert!(
            entry.new_unread.is_empty(),
            "unknown chapter IDs must not be invented"
        );
        assert_eq!(entry.chapters.len(), 1);
        assert_eq!(entry.progress.as_ref().unwrap().page, 7);
        let mut restored: Entry =
            serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        restored.refresh_summary(latest.clone());
        assert!(
            restored.has_updates(),
            "an unchanged check must not acknowledge updates"
        );
        restored.refresh_summary(first);
        assert_eq!(restored.update_summary.as_ref(), Some(&latest));
        restored.refresh(Detail {
            book: restored.book.clone(),
            chapters: vec![chapter("one", "01"), chapter("two", "2.5")],
            summary: Some(latest),
        });
        assert!(!restored.summary_unread);
        assert!(restored.new_unread.contains("two"));
        restored.mark_read("two");
        assert!(!restored.has_updates());
        assert_eq!(restored.progress.as_ref().unwrap().page, 7);
    }

    #[test]
    fn first_check_detects_outdated_directories_and_older_details_cannot_clear_the_dot() {
        let mut entry: Entry =
            serde_json::from_value(serde_json::json!({"favorite":true})).unwrap();
        entry.chapters = vec![chapter("one", "1")];
        let current = summary("3", "2026-01-03T00:00:00Z");
        entry.refresh_summary(current.clone());
        assert!(entry.summary_unread);
        entry.refresh(Detail {
            book: entry.book.clone(),
            chapters: vec![chapter("one", "1")],
            summary: Some(summary("2", "2026-01-02T00:00:00Z")),
        });
        assert!(entry.summary_unread);
        assert_eq!(entry.update_summary, Some(current));
        entry.set_favorite(false);
        assert!(!entry.has_updates());
    }
}
