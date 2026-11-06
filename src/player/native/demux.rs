//! A dedicated SDK worker executes the public demux/decoder/filter DAG.
use super::Shared;
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, SendTimeoutError, Sender, bounded, unbounded};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub struct Header {
    pub duration: f64,
    pub has_audio: bool,
    pub has_video: bool,
    pub info: Arc<crate::media::Info>,
}
pub enum Message {
    Opened(Header),
    Frame(u64, remdia::Frame),
    Seeked(u64),
    Eof(u64),
    Error(String),
}
#[derive(Clone, Copy)]
pub struct Seek {
    pub epoch: u64,
    pub target: f64,
    pub rate: Option<u32>,
}
pub struct Demux {
    pub rx: Receiver<Message>,
    pub seeks: Sender<Seek>,
    pub waiting: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    cancel: remdia::Cancellation,
}
impl Demux {
    pub fn spawn(path: String, shared: &Arc<Shared>, rate: u32) -> Result<Self> {
        let (tx, rx) = bounded(4);
        let (seeks, commands) = unbounded();
        let waiting = Arc::new(AtomicBool::new(false));
        let worker_waiting = waiting.clone();
        let worker = Input::new(shared);
        let audio = shared.audio_source.clone();
        let stop = shared.stop.clone();
        let thread = std::thread::Builder::new()
            .name("pordo-remdia-graph".into())
            .spawn(move || {
                if let Err(e) = if let Some(audio) = audio {
                    super::split::run(path, audio, &worker, rate, &tx, commands, &worker_waiting)
                } else {
                    run(&path, &worker, rate, &tx, commands, &worker_waiting)
                } && !worker.stop.load(Ordering::Acquire)
                {
                    let _ = send(&tx, Message::Error(format!("{e:#}")), &worker);
                }
            })?;
        Ok(Self {
            rx,
            seeks,
            waiting,
            stop,
            thread: Some(thread),
            cancel: shared.cancel.clone(),
        })
    }
}
impl Drop for Demux {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
pub(super) fn send(tx: &Sender<Message>, mut message: Message, s: &Input) -> bool {
    while !s.stop.load(Ordering::Acquire) && !s.cancel.is_cancelled() {
        if matches!(&message,Message::Frame(id,_) if *id<s.requested.load(Ordering::Acquire)) {
            return true;
        }
        match tx.send_timeout(message, Duration::from_millis(5)) {
            Ok(()) => return true,
            Err(SendTimeoutError::Timeout(m)) => message = m,
            Err(SendTimeoutError::Disconnected(_)) => break,
        }
    }
    false
}
pub(super) fn run(
    path: &str,
    s: &Input,
    rate: u32,
    tx: &Sender<Message>,
    commands: Receiver<Seek>,
    waiting: &AtomicBool,
) -> Result<()> {
    let prepared = crate::media_source::prepare(path, &s.stop, s.source_key.as_deref())?;
    let engine = crate::media_backend::engine()?;
    let mut request = remdia::Request::playback(prepared.path());
    request.audio_rate = rate;
    request.timeout_ms = if s.progressive { 300000 } else { 30000 };
    request.hardware = pordo_core::environment::var_os("PORDO_SOFTWARE").is_none();
    let mut session = unsafe { engine.open(request.clone(), s.cancel.clone()) }
        .with_context(|| format!("open media '{path}' through remdia"))?;
    #[cfg(test)]
    let trace = crate::media_source::is_qq(std::path::Path::new(path));
    #[cfg(test)]
    if trace {
        eprintln!(
            "QQ source graph: {}",
            serde_json::to_string(session.plan())?
        );
    }
    let header = Header {
        duration: session
            .info()
            .duration_us
            .filter(|d| *d > 0)
            .map(|d| d as f64 / 1e6)
            .or(s.duration_hint.filter(|d| d.is_finite() && *d > 0.0))
            .unwrap_or(0.0),
        has_audio: session.info().tracks.iter().any(|t| t.kind == "audio"),
        has_video: session
            .info()
            .tracks
            .iter()
            .any(|t| t.kind == "video" && !t.attached_picture),
        info: Arc::new(crate::media::remdia_info(
            &mut session,
            std::path::Path::new(path),
        )),
    };
    if !send(tx, Message::Opened(header), s) {
        return Ok(());
    }
    s.cancel.acknowledge_generation(1);
    let mut epoch = 1;
    let mut eof = false;
    let stats = pordo_core::environment::var_os("PORDO_MEDIA_STATS").is_some();
    let kind = if session.info().tracks.iter().any(|t| t.kind == "video") {
        "video"
    } else {
        "audio"
    };
    let started = std::time::Instant::now();
    let mut trace_frames = 0;
    while !s.stop.load(Ordering::Acquire) && !s.cancel.is_cancelled() {
        let mut seek = None;
        while let Ok(next) = commands.try_recv() {
            seek = Some(next);
        }
        if let Some(next) = seek {
            if s.requested.load(Ordering::Acquire) > next.epoch {
                continue;
            }
            s.cancel.acknowledge_generation(next.epoch);
            if let Some(rate) = next.rate
                && rate != request.audio_rate
            {
                let mut updated = request.clone();
                updated.audio_rate = rate;
                s.cancel.begin();
                let opened = unsafe { engine.open(updated.clone(), s.cancel.clone()) };
                if s.requested.load(Ordering::Acquire) > next.epoch {
                    continue;
                }
                session = opened?;
                request = updated;
            }
            waiting.store(true, Ordering::Release);
            let target_us = (next.target * 1e6).round() as i64;
            let seek_started = std::time::Instant::now();
            if stats {
                eprintln!(
                    "[input] {kind} seek epoch={} target={:.3} begin",
                    next.epoch, next.target
                );
            }
            let result = session.seek(target_us);
            if stats {
                eprintln!(
                    "[input] {kind} seek epoch={} reset_ms={:.2}",
                    next.epoch,
                    seek_started.elapsed().as_secs_f64() * 1000.0
                );
                eprintln!("[sdk] {kind} metrics={:?}", session.metrics());
            }
            waiting.store(false, Ordering::Release);
            if s.requested.load(Ordering::Acquire) > next.epoch {
                continue;
            }
            result.context("seek remdia graph")?;
            #[cfg(test)]
            if trace {
                eprintln!(
                    "QQ seek epoch={} target={} generation={}",
                    next.epoch,
                    next.target,
                    session.generation()
                );
            }
            epoch = next.epoch;
            trace_frames = 0;
            eof = false;
            if !send(tx, Message::Seeked(epoch), s) {
                break;
            }
        }
        if s.requested.load(Ordering::Acquire) > epoch || eof {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        waiting.store(true, Ordering::Release);
        let result = session.read();
        waiting.store(false, Ordering::Release);
        if s.stop.load(Ordering::Acquire) {
            break;
        }
        if s.requested.load(Ordering::Acquire) > epoch {
            continue;
        }
        match result? {
            Some(frame) => {
                if stats && kind == "video" && trace_frames < 24 {
                    eprintln!(
                        "[input] video frame pts={:?} after_ms={:.2}",
                        frame.info().pts_us(),
                        started.elapsed().as_secs_f64() * 1000.0
                    );
                    trace_frames += 1;
                }
                #[cfg(test)]
                if trace {
                    eprintln!("QQ frame epoch={epoch}: {:?}", frame.info());
                }
                if !send(tx, Message::Frame(epoch, frame), s) {
                    break;
                }
            }
            None => {
                #[cfg(test)]
                if trace {
                    eprintln!("QQ EOF epoch={epoch}");
                }
                eof = true;
                if !send(tx, Message::Eof(epoch), s) {
                    break;
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct Input {
    pub stop: Arc<AtomicBool>,
    pub requested: Arc<std::sync::atomic::AtomicU64>,
    pub cancel: remdia::Cancellation,
    progressive: bool,
    source_key: Option<String>,
    duration_hint: Option<f64>,
}
impl Input {
    fn new(shared: &Shared) -> Self {
        Self {
            stop: shared.stop.clone(),
            requested: shared.requested.clone(),
            cancel: shared.cancel.clone(),
            progressive: shared.progressive,
            source_key: shared.source_key.clone(),
            duration_hint: shared.duration_hint,
        }
    }
    pub fn independent(&self) -> Self {
        Self {
            cancel: remdia::Cancellation::with_requested_generation(self.requested.clone()),
            ..self.clone()
        }
    }
}
