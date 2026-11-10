//! Desktop-compatible comic metadata and the subset editable by mobile hosts.
use super::{Journal, model::Snapshot};
use crate::comics::{self, Book, library::Entry};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn progress_key(key: &str) -> bool {
    key.ends_with("/progress")
        || key.starts_with("comic/")
            && matches!(
                key.split('/').nth(2),
                Some("read" | "unread" | "updates_seen")
            )
}

/// Keep local reading/playback edits out of metadata-only synchronization,
/// including newly created progress and removed chapter flags.
pub fn defer_progress(snapshot: &mut Snapshot, baseline: &Snapshot) {
    snapshot.retain(|key, _| !progress_key(key));
    snapshot.extend(
        baseline
            .iter()
            .filter(|(key, _)| progress_key(key))
            .map(|(key, value)| (key.clone(), value.clone())),
    );
}

/// A response must not rewind progress edited locally while it was in flight.
pub fn preserve_local_progress(values: &mut Snapshot, local: &Snapshot, baseline: &Snapshot) {
    for key in local
        .keys()
        .chain(baseline.keys())
        .filter(|key| progress_key(key))
    {
        if local.get(key) != baseline.get(key) {
            if let Some(value) = local.get(key) {
                values.insert(key.clone(), value.clone());
            } else {
                values.remove(key);
            }
        }
    }
}

pub fn mobile_defaults() -> Snapshot {
    let defaults = crate::music_sources::Settings::default();
    Snapshot::from([
        ("music/source".into(), json!(defaults.source)),
        ("music/quality".into(), json!(defaults.quality)),
    ])
}

pub fn mobile_snapshot(
    mut mobile: Snapshot,
    journal: &Journal,
    entries: &[Entry],
) -> Result<Snapshot> {
    validate_mobile(&mobile)?;
    capture_comics(&mut mobile, entries, Some(journal));
    Ok(mobile)
}

pub struct Applied {
    pub journal: Journal,
    pub entries: Vec<Entry>,
    pub mobile: Snapshot,
    pub secrets: BTreeMap<String, String>,
}

/// Merge a network reply with edits made while it was in flight. Unknown
/// desktop fields stay in the journal without becoming mobile deletions.
pub fn finish_mobile(
    mut journal: Journal,
    report: super::webdav::Report,
    mobile: Snapshot,
    mut secrets: BTreeMap<String, String>,
    requested_secrets: &BTreeMap<String, String>,
    existing: &[Entry],
) -> Result<Applied> {
    let local = mobile_snapshot(mobile, &journal, existing)?;
    let baseline = journal.observed.clone();
    let mut captured = local.clone();
    defer_progress(&mut captured, &baseline);
    journal.capture(captured, &mobile_defaults());
    journal.merge(&report.journal.document)?;
    let values = journal.values();
    let mut applied_values = values.clone();
    preserve_local_progress(&mut applied_values, &local, &baseline);
    let mobile: Snapshot = applied_values
        .iter()
        .filter(|(k, _)| mobile_key(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    validate_mobile(&mobile)?;
    let entries = apply_comics(&applied_values, existing)?;
    for (id, key) in report.secrets {
        if secrets.get(&id) == requested_secrets.get(&id) {
            secrets.insert(id, key);
        }
    }
    journal.uploaded = report.journal.uploaded;
    journal.last_success = report.journal.last_success;
    journal.secret_hashes = report.journal.secret_hashes;
    journal.remote_etags = report.journal.remote_etags;
    let mut observed = mobile_snapshot(mobile.clone(), &journal, &entries)?;
    for (key, value) in &values {
        if key.starts_with("comic/") && !progress_key(key) && observed.get(key) != Some(value) {
            journal.write(key.clone(), observed.get(key).cloned());
        }
    }
    defer_progress(&mut observed, &values);
    journal.observed = observed;
    Ok(Applied {
        journal,
        entries,
        mobile,
        secrets,
    })
}

pub fn capture_comics(data: &mut Snapshot, entries: &[Entry], journal: Option<&Journal>) {
    for entry in entries {
        let prefix = format!("comic/{}", entry.book.key());
        if !entry.favorite
            && !journal.is_some_and(|j| {
                j.document
                    .entries
                    .contains_key(&format!("{prefix}/favorite"))
            })
        {
            continue;
        }
        data.insert(
            format!("{prefix}/book"),
            json!({"book":entry.book,"chapters":entry.chapters}),
        );
        data.insert(format!("{prefix}/favorite"), json!(entry.favorite));
        data.insert(format!("{prefix}/favorited_at"), json!(entry.favorited_at));
        if let Some(seen) = &entry.updates_seen {
            data.insert(format!("{prefix}/updates_seen"), json!(seen));
        }
        if let Some(progress) = &entry.progress {
            data.insert(
                format!("{prefix}/progress"),
                json!({"position":progress,"read_at":entry.last_read}),
            );
        }
        for (set, chapters) in [("read", &entry.read), ("unread", &entry.new_unread)] {
            for chapter in chapters {
                data.insert(
                    format!("{prefix}/{set}/{}", comics::digest(chapter.as_bytes())),
                    json!(chapter),
                );
            }
        }
    }
}

/// Validate the entire import before a host starts persisting individual rows.
pub fn apply_comics(values: &Snapshot, existing: &[Entry]) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for (key, value) in values
        .iter()
        .filter(|(k, _)| k.starts_with("comic/") && k.ends_with("/book"))
    {
        let prefix = key.trim_end_matches("/book");
        let book: Book = serde_json::from_value(value["book"].clone())?;
        ensure!(
            prefix == format!("comic/{}", book.key()),
            "Invalid synced comic identity"
        );
        let mut entry = existing
            .iter()
            .find(|e| e.book.key() == book.key())
            .cloned()
            .unwrap_or_default();
        entry.book = book;
        entry.chapters = serde_json::from_value(value["chapters"].clone())?;
        entry.favorite = values
            .get(&format!("{prefix}/favorite"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        entry.favorited_at = values
            .get(&format!("{prefix}/favorited_at"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if let Some(progress) = values.get(&format!("{prefix}/progress")) {
            entry.progress = Some(serde_json::from_value(progress["position"].clone())?);
            entry.last_read = progress["read_at"].as_u64().unwrap_or(0);
        }
        entry.read = values
            .iter()
            .filter(|(k, _)| k.starts_with(&format!("{prefix}/read/")))
            .filter_map(|(_, v)| v.as_str().map(str::to_owned))
            .collect();
        entry.new_unread = values
            .iter()
            .filter(|(k, _)| k.starts_with(&format!("{prefix}/unread/")))
            .filter_map(|(_, v)| v.as_str().map(str::to_owned))
            .filter(|id| !entry.read.contains(id))
            .collect();
        if let Some(seen) = values.get(&format!("{prefix}/updates_seen")) {
            entry.updates_seen = Some(serde_json::from_value(seen.clone())?);
        }
        entry
            .known
            .extend(entry.chapters.iter().map(|c| c.key().to_owned()));
        entry.reconcile_updates();
        entries.push(entry);
    }
    Ok(entries)
}

pub fn mobile_key(key: &str) -> bool {
    matches!(key, "music/source" | "music/quality") || key.starts_with("song/")
}

pub fn validate_mobile(values: &Snapshot) -> Result<()> {
    ensure!(
        values.keys().all(|key| mobile_key(key)),
        "Unsupported mobile sync field"
    );
    for (key, value) in values
        .iter()
        .filter(|(k, _)| k.starts_with("song/") && k.ends_with("/song"))
    {
        let song: crate::music_sources::OnlineSong = serde_json::from_value(value.clone())?;
        ensure!(
            key == &format!("song/{}/song", song.key()),
            "Invalid synced song identity"
        );
    }
    Ok(())
}
