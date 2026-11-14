use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::clock::Clock;
#[cfg(all(not(test), not(windows)))]
use anyhow::Context;
use anyhow::Result;
#[cfg(not(test))]
use anyhow::bail;
#[cfg(all(not(test), not(windows)))]
use cpal::traits::HostTrait;
#[cfg(not(test))]
use cpal::traits::{DeviceTrait, StreamTrait};

use rtrb::{Producer, RingBuffer};

#[cfg(all(windows, not(test)))]
#[path = "device.rs"]
mod device;

#[cfg(test)]
thread_local! { static INITIAL_OPEN_FAILURES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(super) fn simulate_initial_failures(count: u32) {
    INITIAL_OPEN_FAILURES.with(|n| n.set(count));
}

#[derive(Clone, Copy)]
pub struct Sample {
    pub epoch: u64,
    pub pts: f64,
    pub pcm: [f32; 2],
    pub duration: f64,
}

/// Test-device observation: only PCM actually consumed, excluding scheduling
/// gaps and underrun silence. Keep its frame count and media endpoint coherent.
#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub struct RenderedAudio {
    pub epoch: u64,
    pub frames: u64,
    pub media_end: f64,
    pub rate: u32,
}

/// Control is read only by the callback. A generation change invalidates queued audio.
pub struct OutputControl {
    pub epoch: AtomicU64,
    pub enabled: AtomicBool,
    pub failed: AtomicBool,
    pub underruns: AtomicU64,
    pub device_error: Mutex<Option<String>>,
    stream_token: AtomicU64,
    #[cfg(test)]
    pub simulated_device: AtomicU64,
    #[cfg(test)]
    pub simulated_rate: AtomicU32,
    #[cfg(test)]
    pub open_attempts: AtomicU32,
    #[cfg(test)]
    pub simulated_open_failures: AtomicU32,
    #[cfg(test)]
    pub rendered: Mutex<RenderedAudio>,
}
impl OutputControl {
    pub fn new() -> Self {
        Self {
            epoch: AtomicU64::new(1),
            enabled: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            device_error: Mutex::new(None),
            stream_token: AtomicU64::new(0),
            #[cfg(test)]
            simulated_device: AtomicU64::new(1),
            #[cfg(test)]
            simulated_rate: AtomicU32::new(48000),
            #[cfg(test)]
            open_attempts: AtomicU32::new(0),
            #[cfg(test)]
            simulated_open_failures: AtomicU32::new(INITIAL_OPEN_FAILURES.with(|n| n.replace(0))),
            #[cfg(test)]
            rendered: Mutex::new(RenderedAudio::default()),
        }
    }
    fn begin_stream(&self) -> u64 {
        let mut error = self.device_error.lock().unwrap_or_else(|e| e.into_inner());
        *error = None;
        self.failed.store(false, Ordering::Release);
        self.stream_token.fetch_add(1, Ordering::AcqRel) + 1
    }
    fn report_error(&self, token: u64, error: cpal::Error) {
        if token != self.stream_token.load(Ordering::Acquire) {
            return;
        }
        match error.kind() {
            cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied => {}
            cpal::ErrorKind::Xrun => {
                self.underruns.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                let mut saved = self.device_error.lock().unwrap_or_else(|e| e.into_inner());
                if token != self.stream_token.load(Ordering::Acquire) {
                    return;
                }
                *saved = Some(error.to_string());
                self.enabled.store(false, Ordering::Release);
                self.failed.store(true, Ordering::Release);
            }
        }
    }
}
pub struct AudioOut {
    pub rate: u32,
    pub producer: Producer<Sample>,
    pub capacity: usize,
    _stream: Option<cpal::Stream>,
    #[cfg(all(windows, not(test)))]
    device_id: cpal::DeviceId,
    #[cfg(test)]
    device_id: u64,
    #[cfg(test)]
    simulation_stop: Arc<AtomicBool>,
}
#[cfg(not(test))]
pub fn open_output(
    clock: Arc<Clock>,
    control: Arc<OutputControl>,
    volume: Arc<AtomicU32>,
) -> Result<AudioOut> {
    let token = control.begin_stream();
    #[cfg(windows)]
    let (device, device_id) = device::open_default()?;
    #[cfg(not(windows))]
    let device = cpal::default_host()
        .default_output_device()
        .context("no output device")?;
    let supported = device.default_output_config()?;
    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let config = supported.config();
    let capacity = (rate as usize / 2).max(1024);
    let (producer, consumer) = RingBuffer::<Sample>::new(capacity);
    let err_control = control.clone();
    let err_cb = move |error: cpal::Error| err_control.report_error(token, error);

    macro_rules! build {
        ($ty:ty) => {{
            let mut consumer = consumer;
            device.build_output_stream(
                config,
                move |data: &mut [$ty], info: &cpal::OutputCallbackInfo| {
                    use cpal::Sample as _;
                    let epoch = control.epoch.load(Ordering::Acquire);
                    // Only stale generations are discarded; pause retains current PCM.
                    for _ in 0..capacity {
                        if consumer.peek().is_ok_and(|s| s.epoch < epoch) {
                            let _ = consumer.pop();
                        } else {
                            break;
                        }
                    }
                    let enabled = control.enabled.load(Ordering::Acquire);
                    let gain = f32::from_bits(volume.load(Ordering::Relaxed));
                    let mut last = None;
                    let mut produced = 0;
                    for (i, dst) in data.chunks_mut(channels).enumerate() {
                        let sample = if enabled && consumer.peek().is_ok_and(|s| s.epoch == epoch) {
                            consumer.pop().ok()
                        } else {
                            None
                        };
                        if let Some(s) = sample {
                            for (ch, v) in dst.iter_mut().enumerate() {
                                let value = if channels == 1 {
                                    (s.pcm[0] + s.pcm[1]) * 0.5
                                } else if ch < 2 {
                                    s.pcm[ch]
                                } else {
                                    0.0
                                };
                                *v = value.mul_add(gain, 0.0).clamp(-1.0, 1.0).to_sample::<$ty>();
                            }
                            last = Some((s.pts + s.duration, i + 1));
                            produced += 1;
                        } else {
                            dst.fill(<$ty as cpal::Sample>::EQUILIBRIUM);
                        }
                    }
                    if enabled && produced < data.len() / channels {
                        control.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Some((end, offset)) = last {
                        let ts = info.timestamp();
                        let latency = ts
                            .playback
                            .as_nanos()
                            .saturating_sub(ts.callback.as_nanos())
                            as f64
                            / 1e9;
                        clock.progress.publish(
                            epoch,
                            end,
                            clock.wall() + latency + offset as f64 / rate as f64,
                        );
                    }
                },
                err_cb,
                None,
            )?
        }};
    }
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::F64 => build!(f64),
        other => bail!("unsupported audio device format: {other}"),
    };
    stream.play()?;
    Ok(AudioOut {
        rate,
        producer,
        capacity,
        _stream: Some(stream),
        #[cfg(windows)]
        device_id,
    })
}
#[cfg(test)]
pub fn open_output(
    clock: Arc<Clock>,
    control: Arc<OutputControl>,
    _volume: Arc<AtomicU32>,
) -> Result<AudioOut> {
    control.begin_stream();
    control.open_attempts.fetch_add(1, Ordering::AcqRel);
    if control
        .simulated_open_failures
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
        .is_ok()
    {
        anyhow::bail!("simulated audio output open failure");
    }
    let rate = control.simulated_rate.load(Ordering::Acquire);
    let capacity = rate as usize / 2;
    let (producer, mut consumer) = RingBuffer::<Sample>::new(capacity);
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let device_id = control.simulated_device.load(Ordering::Acquire);
    std::thread::spawn(move || {
        let mut tick = std::time::Instant::now();
        let mut remainder = 0.0;
        while !worker_stop.load(Ordering::Acquire) {
            let now = std::time::Instant::now();
            let samples = now.duration_since(tick).as_secs_f64() * rate as f64 + remainder;
            tick = now;
            remainder = samples.fract();
            // Simulate a device at its sample rate even when CI coalesces the
            // five-millisecond sleeps. A fixed 240 frames per wake slows the
            // media clock whenever thread scheduling is delayed.
            let due = (samples as usize).min(capacity);
            let epoch = control.epoch.load(Ordering::Acquire);
            while consumer.peek().is_ok_and(|s| s.epoch < epoch) {
                let _ = consumer.pop();
            }
            if control.enabled.load(Ordering::Acquire) {
                let mut last = None;
                let mut produced = 0;
                for _ in 0..due {
                    if consumer.peek().is_ok_and(|s| s.epoch == epoch) {
                        last = consumer.pop().ok();
                        produced += 1;
                    } else {
                        break;
                    }
                }
                if let Some(sample) = last {
                    assert!(sample.pcm.iter().all(|v| v.is_finite()));
                    clock
                        .progress
                        .publish(epoch, sample.pts + sample.duration, clock.wall());
                    let mut rendered = control.rendered.lock().unwrap();
                    if rendered.epoch != epoch {
                        *rendered = RenderedAudio {
                            epoch,
                            rate,
                            ..Default::default()
                        };
                    }
                    rendered.frames += produced;
                    rendered.media_end = sample.pts + sample.duration;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
    Ok(AudioOut {
        rate,
        capacity,
        producer,
        _stream: None,
        device_id,
        simulation_stop: stop,
    })
}
impl AudioOut {
    #[cfg(any(windows, test))]
    pub fn default_changed(&self, _control: &OutputControl) -> bool {
        #[cfg(test)]
        {
            _control.simulated_device.load(Ordering::Acquire) != self.device_id
        }
        #[cfg(all(windows, not(test)))]
        {
            device::default_id().as_ref() != Some(&self.device_id)
        }
    }
}
#[cfg(test)]
impl Drop for AudioOut {
    fn drop(&mut self) {
        self.simulation_stop.store(true, Ordering::Release);
    }
}

pub struct AudioPipeline {
    pub pending: VecDeque<Sample>,
    pub end: f64,
    pub submitted_end: Option<f64>,
    rate: u32,
    queued_until: f64,
    discard_before: f64,
    speed: f64,
    stretch: Option<wsola::TimeStretch>,
    stretch_buffer: Vec<f32>,
    stretch_position: f64,
    input_end: Option<f64>,
    finished: bool,
}
impl AudioPipeline {
    pub fn new(rate: u32) -> Self {
        Self {
            pending: VecDeque::new(),
            end: 0.,
            submitted_end: None,
            rate,
            queued_until: 0.,
            discard_before: 0.,
            speed: 1.0,
            stretch: None,
            stretch_buffer: Vec::new(),
            stretch_position: 0.0,
            input_end: None,
            finished: false,
        }
    }
    pub fn reset(&mut self, target: f64) {
        self.pending.clear();
        self.end = target;
        self.submitted_end = None;
        self.queued_until = target;
        self.discard_before = target;
        self.stretch_position = target;
        self.input_end = None;
        self.finished = false;
        if let Some(stretch) = &mut self.stretch {
            stretch.reset();
        }
    }
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed as f64;
        self.stretch = (speed != 1.0).then(|| {
            let mut stretch =
                wsola::TimeStretch::new(self.rate.try_into().unwrap(), 2.try_into().unwrap())
                    .expect("valid PCM format");
            stretch.set_tempo(speed);
            self.stretch_buffer
                .resize(stretch.min_output_buff_size() * 8, 0.0);
            stretch
        });
    }
    pub fn append(&mut self, frame: &remdia::Frame, epoch: u64, target: f64) -> Result<()> {
        let remdia::FrameInfo::Audio {
            pts_us,
            rate,
            channels,
            samples,
            ..
        } = *frame.info()
        else {
            return Ok(());
        };
        anyhow::ensure!(
            rate == self.rate && channels == 2,
            "remdia/device PCM format mismatch"
        );
        let pts = pts_us as f64 / 1e6;
        let skip = ((target.max(self.discard_before) - pts).max(0.) * rate as f64).ceil() as usize;
        anyhow::ensure!(
            self.pending.len() + samples <= self.rate as usize * 2,
            "decoded audio exceeded two-second pending budget"
        );
        if samples <= skip {
            return Ok(());
        }
        let start = pts + skip as f64 / rate as f64;
        let end = pts + samples as f64 / rate as f64;
        if self.stretch.is_some() {
            if self
                .input_end
                .is_some_and(|last| (last - start).abs() > 1.5 / rate as f64)
            {
                self.finish(epoch)?;
                self.stretch.as_mut().unwrap().reset();
                self.finished = false;
                self.input_end = None;
            }
            if self.input_end.is_none() {
                self.stretch_position = start;
            }
            let pcm: Vec<f32> = frame
                .bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .skip(skip * 2)
                .map(|v| f32::from_le_bytes(*v))
                .collect();
            self.stretch.as_mut().unwrap().push(&pcm);
            self.input_end = Some(end);
            self.end = end;
            return self.pull_stretched(epoch);
        }
        for (i, pair) in frame
            .bytes()
            .as_chunks::<8>()
            .0
            .iter()
            .enumerate()
            .skip(skip)
        {
            let left = f32::from_le_bytes(pair[..4].try_into().unwrap());
            let right = f32::from_le_bytes(pair[4..].try_into().unwrap());
            self.pending.push_back(Sample {
                epoch,
                pts: pts + i as f64 / rate as f64,
                pcm: [left, right],
                duration: 1.0 / rate as f64,
            });
        }
        if samples > skip {
            self.end = pts + samples as f64 / rate as f64;
        }
        Ok(())
    }
    fn pull_stretched(&mut self, epoch: u64) -> Result<()> {
        let Some(stretch) = &mut self.stretch else {
            return Ok(());
        };
        loop {
            let count = match stretch.pull(&mut self.stretch_buffer) {
                Ok(n) => n,
                Err(wsola::Error::NotEnoughInput) => break,
                Err(e) => return Err(e.into()),
            };
            if count == 0 {
                break;
            }
            for pair in self.stretch_buffer[..count].as_chunks::<2>().0 {
                if self.stretch_position >= self.end {
                    break;
                }
                let duration =
                    (self.speed / self.rate as f64).min(self.end - self.stretch_position);
                self.pending.push_back(Sample {
                    epoch,
                    pts: self.stretch_position,
                    pcm: [pair[0], pair[1]],
                    duration,
                });
                self.stretch_position += duration;
            }
        }
        anyhow::ensure!(
            self.pending.len() <= self.rate as usize * 2,
            "stretched audio exceeded pending budget"
        );
        Ok(())
    }
    pub fn finish(&mut self, epoch: u64) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        if let Some(stretch) = &mut self.stretch {
            stretch.set_draining();
            self.pull_stretched(epoch)?;
            // WSOLA works in complete windows. Complete the sub-window tail so
            // the audio clock reaches the exact source endpoint, including EOF.
            while self.input_end.is_some() && self.stretch_position + 1e-9 < self.end {
                let duration =
                    (self.speed / self.rate as f64).min(self.end - self.stretch_position);
                self.pending.push_back(Sample {
                    epoch,
                    pts: self.stretch_position,
                    pcm: [0.0; 2],
                    duration,
                });
                self.stretch_position += duration;
            }
        }
        self.finished = true;
        Ok(())
    }
    pub fn pump(&mut self, out: &mut AudioOut) {
        for _ in 0..out.producer.slots() {
            let Some(next) = self.pending.front() else {
                break;
            };
            let sample = if next.pts > self.queued_until + 1.5 * self.speed / self.rate as f64 {
                Sample {
                    epoch: next.epoch,
                    pts: self.queued_until,
                    pcm: [0.; 2],
                    duration: (self.speed / self.rate as f64).min(next.pts - self.queued_until),
                }
            } else {
                self.pending.pop_front().unwrap()
            };
            self.queued_until = sample.pts + sample.duration;
            let result = out.producer.push(sample);
            debug_assert!(result.is_ok());
            self.submitted_end = Some(self.queued_until);
        }
    }
    pub fn catch_up(&mut self, position: f64) {
        self.discard_before = position;
        self.queued_until = position;
        while self.pending.front().is_some_and(|s| s.pts < position) {
            self.pending.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn audio_device_notifications_distinguish_xruns_routes_and_stale_stream_errors() {
        let control = OutputControl::new();
        control.stream_token.store(2, Ordering::Release);
        control.enabled.store(true, Ordering::Release);
        for kind in [
            cpal::ErrorKind::Xrun,
            cpal::ErrorKind::DeviceChanged,
            cpal::ErrorKind::RealtimeDenied,
        ] {
            control.report_error(2, kind.into());
        }
        assert!(!control.failed.load(Ordering::Acquire));
        assert!(control.enabled.load(Ordering::Acquire));
        assert_eq!(control.underruns.load(Ordering::Acquire), 1);
        control.report_error(1, cpal::ErrorKind::DeviceNotAvailable.into());
        assert!(
            !control.failed.load(Ordering::Acquire),
            "a replaced stream cannot invalidate its replacement"
        );
        control.report_error(2, cpal::ErrorKind::StreamInvalidated.into());
        assert!(control.failed.load(Ordering::Acquire));
        assert!(!control.enabled.load(Ordering::Acquire));
        assert!(control.device_error.lock().unwrap().is_some());
    }

    use super::*;

    #[test]
    fn tempo_preserves_pitch_maps_media_time_and_flushes_tail_at_every_speed() {
        let rate = 16000;
        let frames = rate * 2 + 137; // Deliberately not a WSOLA window multiple.
        let input: Vec<_> = (0..frames)
            .flat_map(|i| {
                let sample = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                [sample, sample]
            })
            .collect();
        for speed in [1.25, 1.5, 2.0, 3.0] {
            let mut pipeline = AudioPipeline::new(rate);
            pipeline.set_speed(speed);
            pipeline.reset(12.0);
            for chunk in input.chunks(1024) {
                pipeline.stretch.as_mut().unwrap().push(chunk);
                pipeline.end += chunk.len() as f64 / 2.0 / rate as f64;
                pipeline.input_end = Some(pipeline.end);
                pipeline.pull_stretched(7).unwrap();
            }
            pipeline.finish(7).unwrap();
            let expected = (frames as f64 / speed as f64).ceil() as usize;
            assert!(
                pipeline.pending.len().abs_diff(expected) <= 1,
                "{speed}: {} vs {expected}",
                pipeline.pending.len()
            );
            let samples: Vec<_> = pipeline.pending.iter().collect();
            assert!(
                samples
                    .iter()
                    .all(|s| s.epoch == 7 && s.pcm[0].is_finite() && s.pcm[0] == s.pcm[1])
            );
            assert!(
                (samples.last().unwrap().pts + samples.last().unwrap().duration - pipeline.end)
                    .abs()
                    < 1e-8
            );
            let center = &samples[rate as usize / 10..samples.len() - rate as usize / 10];
            let crossings = center
                .windows(2)
                .filter(|s| s[0].pcm[0] <= 0.0 && s[1].pcm[0] > 0.0)
                .count();
            let hz = crossings as f64 / (center.len() as f64 / rate as f64);
            assert!((hz - 440.0).abs() < 8.0, "{speed}: pitch {hz}");
            pipeline.reset(30.0);
            assert!(pipeline.pending.is_empty());
            assert_eq!(pipeline.stretch.as_ref().unwrap().buffered(), 0);
            assert_eq!(pipeline.stretch_position, 30.0);
        }
    }
}
