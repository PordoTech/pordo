//! Playback tags and background artwork share a cache, but only a completed
//! background probe counts as an attempt to obtain a video thumbnail.
use crate::media::{Info, Kind};
use crossbeam_channel::Sender;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Default)]
pub(super) struct Cache {
    entries: HashMap<String, Option<Arc<Info>>>,
    requested: HashSet<String>,
}
impl Cache {
    pub fn get(&self, path: &str) -> Option<&Option<Arc<Info>>> {
        self.entries.get(path)
    }

    pub fn remove(&mut self, path: &str) {
        self.entries.remove(path);
        self.requested.remove(path);
    }

    pub fn request(&mut self, path: &str, sender: &Sender<String>) {
        if self
            .entries
            .get(path)
            .and_then(Option::as_ref)
            .is_some_and(|info| info.kind == Kind::Music || info.artwork.is_some())
            || self.requested.contains(path)
        {
            return;
        }
        self.requested.insert(path.to_owned());
        self.entries.entry(path.to_owned()).or_insert(None);
        if sender.send(path.to_owned()).is_err() {
            self.requested.remove(path);
        }
    }

    pub fn playback(&mut self, path: String, info: Arc<Info>) -> Arc<Info> {
        self.merge(path, Some(info)).as_ref().unwrap().clone()
    }

    pub fn complete(&mut self, path: String, info: Option<Arc<Info>>) {
        self.requested.insert(path.clone());
        self.merge(path, info);
    }

    fn merge(&mut self, path: String, info: Option<Arc<Info>>) -> &Option<Arc<Info>> {
        let cached = self.entries.entry(path).or_default();
        if let Some(mut info) = info {
            if let Some(previous) = cached.as_ref() {
                let info = Arc::make_mut(&mut info);
                for (tag, known) in [
                    (&mut info.title, &previous.title),
                    (&mut info.artist, &previous.artist),
                    (&mut info.album, &previous.album),
                ] {
                    if tag.as_deref().is_none_or(|text| text.trim().is_empty()) {
                        tag.clone_from(known);
                    }
                }
                if info.artwork.is_none() {
                    info.artwork.clone_from(&previous.artwork);
                }
            }
            *cached = Some(info);
        }
        cached
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Artwork;

    #[test]
    fn partial_playback_and_artwork_results_preserve_known_music_tags() {
        let mut cache = Cache::default();
        let path = "online://fixture:42";
        let tags = Info {
            kind: Kind::Music,
            title: Some("Track".into()),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            artwork: None,
        };
        cache.playback(path.into(), Arc::new(tags));
        let cover = Arc::new(Artwork {
            width: 1,
            height: 1,
            rgba: vec![50, 100, 150, 255],
        });
        cache.complete(
            path.into(),
            Some(Arc::new(Info {
                kind: Kind::Music,
                title: None,
                artist: None,
                album: None,
                artwork: Some(cover),
            })),
        );
        cache.playback(
            path.into(),
            Arc::new(Info {
                kind: Kind::Music,
                title: None,
                artist: Some(" ".into()),
                album: Some("Updated album".into()),
                artwork: None,
            }),
        );
        let current = cache.get(path).unwrap().as_ref().unwrap();
        assert_eq!(current.title.as_deref(), Some("Track"));
        assert_eq!(current.artist.as_deref(), Some("Artist"));
        assert_eq!(current.album.as_deref(), Some("Updated album"));
        assert!(current.artwork.is_some());
    }

    fn video(cover: bool) -> Arc<Info> {
        Arc::new(Info {
            kind: Kind::Video,
            title: Some("Video".into()),
            artist: None,
            album: None,
            artwork: cover.then(|| {
                Arc::new(Artwork {
                    width: 1,
                    height: 1,
                    rgba: vec![50, 100, 150, 255],
                })
            }),
        })
    }

    #[test]
    fn playback_tags_do_not_prevent_video_thumbnail_requests_or_erase_results() {
        for playback_first in [true, false] {
            let mut cache = Cache::default();
            let (tx, rx) = crossbeam_channel::unbounded();
            if playback_first {
                cache.playback("video.mkv".into(), video(false));
            }
            cache.request("video.mkv", &tx);
            cache.request("video.mkv", &tx);
            assert_eq!(rx.try_iter().collect::<Vec<_>>(), ["video.mkv"]);
            cache.complete("video.mkv".into(), Some(video(true)));
            cache.playback("video.mkv".into(), video(false));
            cache.complete("video.mkv".into(), None);
            assert!(
                cache
                    .get("video.mkv")
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .artwork
                    .is_some()
            );
            cache.request("video.mkv", &tx);
            assert!(rx.is_empty());
        }
    }

    #[test]
    fn failed_thumbnail_is_not_requeued_each_frame() {
        let mut cache = Cache::default();
        let (tx, rx) = crossbeam_channel::unbounded();
        cache.playback("broken.mkv".into(), video(false));
        cache.request("broken.mkv", &tx);
        assert!(rx.try_recv().is_ok());
        cache.complete("broken.mkv".into(), None);
        for _ in 0..10 {
            cache.request("broken.mkv", &tx);
        }
        assert!(rx.is_empty());
        assert!(cache.get("broken.mkv").unwrap().is_some());
    }
}
