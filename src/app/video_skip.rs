//! OP/ED lookup and media-clock-driven skipping, scoped to the active Bilibili P.
use super::*;
use crate::{
    bilibili::segments::{self, Kind, Segment},
    jobs::Job,
};

#[derive(Default)]
pub(super) struct State {
    key: Option<(String, u64)>,
    job: Option<Job<Vec<Segment>>>,
    pub(super) playback: segments::Playback,
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
}
impl App {
    pub(super) fn poll_video_skip(&mut self, ctx: &egui::Context) {
        let source = self.bilibili_skip_source();
        let expected_duration = source.as_ref().map(|(_, _, duration)| *duration);
        self.video_skip.poll(ctx, source);
        let Some(player) = &self.player else { return };
        if !self.bilibili_controls_available()
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
            .target(player.position(), &self.settings.video.skip)
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
        for segment in &self.video_skip.playback.segments {
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
        assert_eq!(
            state.playback.target(
                6.0,
                &pordo_core::video_sources::SkipSettings {
                    intro: true,
                    outro: true
                }
            ),
            None
        );
        state.job = Some(Job::spawn(|_| std::future::pending()));
        state.poll(&ctx, None);
        assert!(state.key.is_none() && state.job.is_none() && state.playback.segments.is_empty());
    }
}
