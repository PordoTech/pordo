//! OP/ED lookup and media-clock-driven skipping, scoped to the active Bilibili P.
use super::*;
use crate::{
    bilibili::segments::{self, Kind, Segment},
    jobs::Job,
};

mod analysis;

#[derive(Clone, Copy, PartialEq, Eq)]
struct MissingMarkers {
    intro: bool,
    outro: bool,
}

#[derive(Default)]
pub(super) struct State {
    key: Option<(String, u64)>,
    job: Option<Job<Vec<Segment>>>,
    pub(super) playback: segments::Playback,
    community_ready: bool,
    ai_request: Option<(PathBuf, crate::ai::ModelRef, MissingMarkers)>,
    ai_job: Option<analysis::Analysis>,
    ai_playback: segments::Playback,
    manual_position: Option<f64>,
}
impl State {
    #[cfg(test)]
    pub(in crate::app) fn seeded(key: (String, u64), markers: Vec<Segment>) -> Self {
        Self {
            key: Some(key),
            playback: segments::Playback::new(markers),
            ..Default::default()
        }
    }
    pub(super) fn manual_seek(&mut self, position: f64) {
        self.manual_position = Some(position);
        self.playback.manual_seek(position);
        self.ai_playback.manual_seek(position);
    }
    pub(super) fn reset_pass(&mut self) {
        self.manual_position = None;
        self.playback.reset_pass();
        self.ai_playback.reset_pass();
    }
    fn poll(&mut self, ctx: &egui::Context, source: Option<(String, u64, f64)>) {
        let key = source.as_ref().map(|(bvid, cid, _)| (bvid.clone(), *cid));
        if self.key != key {
            *self = Self {
                key,
                ..Default::default()
            };
            if let Some((bvid, cid, duration)) = source {
                let ctx = ctx.clone();
                self.job = Some(Job::spawn(move |_| async move {
                    let result = segments::fetch(&bvid, cid, duration).await;
                    ctx.request_repaint();
                    result
                }));
            }
        }
        if let Some(result) = self.job.as_ref().and_then(Job::poll) {
            self.job = None;
            match result {
                Ok(segments) => {
                    self.community_ready = true;
                    self.playback = segments::Playback::new(segments);
                    if let Some(position) = self.manual_position {
                        self.playback.manual_seek(position);
                    }
                }
                Err(error) => eprintln!("Bilibili OP/ED markers unavailable: {error}"),
            }
        }
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn poll_ai(
        &mut self,
        ctx: &egui::Context,
        settings: &Settings,
        source: Option<PathBuf>,
        duration: f64,
    ) {
        let ai = &settings.video_skip_ai;
        let missing = MissingMarkers {
            intro: self.playback.segments.iter().all(|s| s.kind != Kind::Intro),
            outro: self.playback.segments.iter().all(|s| s.kind != Kind::Outro),
        };
        let request = source
            .filter(|_| {
                ai.enabled
                    && settings.video.skip
                    && self.community_ready
                    && (missing.intro || missing.outro)
            })
            .map(|source| (source, ai.selected.clone(), missing));
        if self.ai_request != request {
            self.ai_job = None;
            self.ai_playback = Default::default();
            self.ai_request = None;
            if let Some((source, selected, wanted)) = &request
                && let Some(key) = &self.key
                && let Ok(connection) = settings.ai.resolve(selected)
            {
                self.ai_job = Some(analysis::Analysis::start(
                    ctx.clone(),
                    source.clone(),
                    key,
                    duration,
                    *wanted,
                    connection,
                ));
                self.ai_request = request;
            }
        }
        if let Some(job) = &self.ai_job {
            let done = job.job.poll();
            for marker in job.markers.try_iter() {
                let existing = self
                    .playback
                    .segments
                    .iter()
                    .chain(&self.ai_playback.segments);
                if existing
                    .clone()
                    .any(|s| s.start < marker.end && marker.start < s.end)
                    || existing.map(|s| s.end - s.start).sum::<f64>() + marker.end - marker.start
                        > duration - analysis::MIN_SEGMENT
                {
                    continue;
                }
                self.ai_playback.add(marker);
                if let Some(position) = self.manual_position {
                    self.ai_playback.manual_seek(position);
                }
            }
            if let Some(result) = done {
                if result.is_err() {
                    eprintln!("Bilibili AI audio analysis unavailable");
                }
                self.ai_job = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    pub(super) fn ai_marker_at(&self, position: f64) -> Option<Kind> {
        self.ai_playback
            .segments
            .iter()
            .find(|s| s.start <= position && position < s.end)
            .map(|s| s.kind)
    }
}
impl App {
    pub(super) fn poll_video_skip(&mut self, ctx: &egui::Context) {
        let source = self.bilibili_skip_source();
        let expected_duration = source.as_ref().map(|(_, _, duration)| *duration);
        self.video_skip.poll(ctx, source);
        if let Some(duration) =
            expected_duration.filter(|d| d.is_finite() && *d >= 2.0 * analysis::MIN_SEGMENT)
        {
            let audio = self
                .player
                .as_ref()
                .filter(|p| {
                    self.settings.video_skip_ai.enabled
                        && self.workspace.page == workspace::Page::Playing
                        && !self.loading
                        && p.duration() > 0.0
                        && p.duration().is_finite()
                        && (duration - p.duration()).abs() <= 2.0
                })
                .and_then(|_| self.bilibili_skip_audio_source());
            self.video_skip
                .poll_ai(ctx, &self.settings, audio, duration);
        }
        let Some(player) = &self.player else { return };
        if !self.bilibili_controls_available()
            || !self.settings.video.skip
            || self.loading
            || self.scrubbing
            || self.pending_seek.is_some()
            || player.snapshot().state != PlaybackState::Playing
            || player.duration() <= 0.0
            || expected_duration.is_some_and(|duration| (duration - player.duration()).abs() > 2.0)
        {
            return;
        }
        if let Some(target) = self
            .video_skip
            .playback
            .target(player.position())
            .or_else(|| self.video_skip.ai_playback.target(player.position()))
        {
            // Keep an ED ending exactly at EOF: native seek-to-duration drains
            // playback normally, preserving pause/EOF/queue behavior.
            let target = target.min(player.duration());
            if let Some(job) = &self.subtitle_job {
                job.prioritize(target);
            }
            self.pending_seek = Some((player.seek(target), target));
        }
    }

    pub(super) fn paint_video_skip_markers(
        &self,
        ui: &mut Ui,
        rect: Rect,
        duration: f64,
        alpha: f32,
    ) {
        if !self.bilibili_controls_available() || !duration.is_finite() || duration <= 0.0 {
            return;
        }
        for segment in self
            .video_skip
            .playback
            .segments
            .iter()
            .chain(&self.video_skip.ai_playback.segments)
        {
            if segment.end > duration + 0.5 {
                continue;
            }
            let color = match segment.kind {
                Kind::Intro => Color32::from_rgb(234, 190, 105),
                Kind::Outro => Color32::from_rgb(170, 160, 240),
            };
            let color = color.gamma_multiply(alpha);
            let x =
                |time: f64| rect.left() + rect.width() * (time / duration).clamp(0.0, 1.0) as f32;
            let range = Rect::from_min_max(
                pos2(x(segment.start), rect.center().y - 1.5),
                pos2(x(segment.end), rect.center().y + 1.5),
            );
            ui.painter().rect_filled(range, 0, color);
            for time in [segment.start, segment.end] {
                let point = pos2(x(time), rect.center().y);
                ui.painter().line_segment(
                    [point - vec2(0.0, 4.5), point + vec2(0.0, 4.5)],
                    Stroke::new(1.5, color),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_markers_preserve_manual_rewatch_and_leaving_cancels_pending_data() {
        let ctx = egui::Context::default();
        let mut state = State::seeded(("BV1GJ411x7h7".into(), 11), vec![]);
        state.job = Some(Job::spawn(|_| async {
            Ok(vec![Segment {
                kind: Kind::Intro,
                start: 0.0,
                end: 30.0,
            }])
        }));
        state.manual_seek(5.0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.job.is_some() && Instant::now() < deadline {
            state.poll(&ctx, Some(("BV1GJ411x7h7".into(), 11, 300.0)));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(state.playback.segments.len(), 1);
        assert_eq!(state.playback.target(6.0), None);
        state.job = Some(Job::spawn(|_| std::future::pending()));
        state.poll(&ctx, None);
        assert!(state.key.is_none() && state.job.is_none() && state.playback.segments.is_empty());
    }
}
