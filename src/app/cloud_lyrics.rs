use super::*;
use crate::sync::model::{Document, Snapshot};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

const PREFIX: &str = "lyric/";
const MAX_LRC_BYTES: usize = 512 * 1024;

#[derive(Clone)]
struct Fingerprint {
    size: u64,
    modified: std::time::SystemTime,
    id: String,
}

#[derive(Default)]
struct Scan {
    fingerprints: BTreeMap<String, Fingerprint>,
    lyrics: BTreeMap<String, (String, String)>,
}

#[derive(Default)]
pub(super) struct State {
    scan: Scan,
    job: Option<crate::jobs::Job<Scan>>,
    next: Option<Instant>,
    active: Option<String>,
}

fn fingerprint(path: &str, cache: &mut BTreeMap<String, Fingerprint>) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified = meta.modified().ok()?;
    if let Some(previous) = cache.get(path)
        && previous.modified == modified
        && previous.size == meta.len()
    {
        return Some(previous.id.clone());
    }
    let mut hash = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    struct HashWriter<'a>(&'a mut aws_lc_rs::digest::Context);
    impl Write for HashWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    if std::io::copy(&mut file, &mut HashWriter(&mut hash)).ok()? != meta.len()
        || file.metadata().ok()?.modified().ok()? != modified
    {
        return None;
    }
    let id = format!(
        "file/{}",
        hash.finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    cache.insert(
        path.into(),
        Fingerprint {
            size: meta.len(),
            modified,
            id: id.clone(),
        },
    );
    Some(id)
}

fn local_lrc(path: &str) -> Option<String> {
    let path = Path::new(path);
    [
        path.with_extension("lrc"),
        crate::media::companion_path(path, ".lrc"),
    ]
    .into_iter()
    .find_map(|path| {
        let file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        file.take((MAX_LRC_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_LRC_BYTES {
            return None;
        }
        let text = String::from_utf8_lossy(&bytes);
        valid_lrc(&text).then(|| text.into_owned())
    })
}

fn valid_lrc(text: &str) -> bool {
    text.len() <= MAX_LRC_BYTES && !crate::lyrics::Lyrics::parse(text).lines.is_empty()
}

fn online_id(song: &crate::music_sources::OnlineSong) -> String {
    format!("online/{}", song.key())
}

impl State {
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        records: &[library::Record],
        active: Option<&Path>,
        has_local_lyrics: bool,
    ) {
        if let Some(result) = self.job.as_ref().and_then(crate::jobs::Job::poll) {
            self.job = None;
            if let Ok(scan) = result {
                self.scan = scan;
            }
        }
        let active = active
            .filter(|_| has_local_lyrics)
            .map(|p| p.to_string_lossy().into_owned());
        if active != self.active {
            self.next = None;
        }
        if self.job.is_none() && self.next.is_none_or(|next| Instant::now() >= next) {
            self.active = active.clone();
            let records: Vec<_> = records
                .iter()
                .filter(|r| r.music && r.favorite)
                .map(|r| (r.path.clone(), r.online.as_ref().map(online_id)))
                .collect();
            let mut cache = self.scan.fingerprints.clone();
            let ctx = ctx.clone();
            self.next = Some(Instant::now() + Duration::from_secs(5));
            self.job = Some(crate::jobs::Job::spawn(move |_| async move {
                let scan = tokio::task::spawn_blocking(move || {
                    cache.retain(|path, _| {
                        active.as_ref() == Some(path) || records.iter().any(|(p, _)| p == path)
                    });
                    let mut lyrics = BTreeMap::new();
                    for (path, id) in records {
                        if let Some(text) = local_lrc(&path)
                            && let Some(id) = id.or_else(|| fingerprint(&path, &mut cache))
                        {
                            lyrics.insert(path, (id, text));
                        }
                    }
                    if let Some(path) = active {
                        fingerprint(&path, &mut cache);
                    }
                    Scan {
                        fingerprints: cache,
                        lyrics,
                    }
                })
                .await?;
                ctx.request_repaint();
                Ok(scan)
            }));
        }
        ctx.request_repaint_after(Duration::from_secs(5));
    }

    pub fn capture(&self, device: &str, records: &[library::Record], values: &mut Snapshot) {
        // Keep one revision per originating device. Simultaneous edits remain
        // available in the journal even when only one is used for playback.
        for record in records
            .iter()
            .filter(|record| record.music && record.favorite)
        {
            if let Some((id, text)) = self.scan.lyrics.get(&record.path)
                && record
                    .online
                    .as_ref()
                    .map_or_else(|| id.starts_with("file/"), |song| *id == online_id(song))
            {
                values.insert(format!("{PREFIX}{id}/{device}"), serde_json::json!(text));
            }
        }
    }

    pub fn versions<'a>(
        &self,
        document: &'a Document,
        song: Option<&crate::music_sources::OnlineSong>,
        source: Option<&Path>,
    ) -> Vec<(u64, &'a str)> {
        let id = song.map(online_id).or_else(|| {
            self.scan
                .fingerprints
                .get(source?.to_str()?)
                .map(|entry| entry.id.clone())
        });
        let Some(id) = id else {
            return Vec::new();
        };
        let prefix = format!("{PREFIX}{id}/");
        let mut versions: Vec<_> = document
            .entries
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
            .filter_map(|(key, e)| Some((e.time, key, e.value.as_ref()?.as_str()?)))
            .filter(|(_, _, text)| text.len() <= MAX_LRC_BYTES)
            .collect();
        versions.sort_unstable_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
        let mut seen = std::collections::HashSet::new();
        versions
            .into_iter()
            .filter(|(_, _, text)| seen.insert(*text) && valid_lrc(text))
            .map(|(time, _, text)| (time, text))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncs_only_favorites_and_finds_renamed_audio_with_both_lyric_versions() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("home.flac");
        let moved = dir.join("laptop.flac");
        let other = dir.join("unfavorited.flac");
        for path in [&file, &moved, &other] {
            std::fs::write(path, b"audio fixture").unwrap();
        }
        std::fs::write(file.with_extension("lrc"), "[00:01.00]Home").unwrap();
        std::fs::write(other.with_extension("lrc"), "[00:01.00]Keep private").unwrap();
        let make_record = |path: &Path, favorite| library::Record {
            path: path.to_str().unwrap().into(),
            music: true,
            favorite,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut state = State::default();
        state.poll(
            &ctx,
            &[make_record(&file, true), make_record(&other, false)],
            Some(&moved),
            true,
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.job.is_some() && Instant::now() < deadline {
            state.poll(&ctx, &[], Some(&moved), true);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut values = Snapshot::new();
        state.capture("home", &[make_record(&file, false)], &mut values);
        assert!(values.is_empty());
        state.capture(
            "home",
            &[make_record(&file, true), make_record(&other, false)],
            &mut values,
        );
        assert_eq!(values.len(), 1);
        let (key, value) = values.iter().next().unwrap();
        assert!(key.starts_with("lyric/file/") && key.ends_with("/home"));
        assert!(!key.contains("flac"));
        assert_eq!(value, "[00:01.00]Home");
        let mut journal = crate::sync::Journal::load(&dir.join("journal.json"), "home").unwrap();
        journal.write(key.clone(), Some(value.clone()));
        let alternate = key.trim_end_matches("home").to_owned() + "laptop";
        journal.write(alternate, Some(serde_json::json!("[00:01.00]Laptop")));
        let versions = state.versions(&journal.document, None, Some(&moved));
        assert_eq!(versions.len(), 2);
        assert!(versions.iter().any(|(_, value)| *value == "[00:01.00]Home"));
        assert!(
            versions
                .iter()
                .any(|(_, value)| *value == "[00:01.00]Laptop")
        );
        assert!(!moved.with_extension("lrc").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
