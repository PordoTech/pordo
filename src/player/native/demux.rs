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
        let worker = shared.clone();
        let stop = shared.stop.clone();
        let thread = std::thread::Builder::new()
            .name("pordo-remdia-graph".into())
            .spawn(move || {
                if let Err(e) = run(&path, &worker, rate, &tx, commands, &worker_waiting)
                    && !worker.stop.load(Ordering::Acquire)
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
fn send(tx: &Sender<Message>, mut message: Message, s: &Shared) -> bool {
    while !s.stop.load(Ordering::Acquire) {
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
fn run(
    path: &str,
    s: &Shared,
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
    let mut external_audio = if let Some(source) = &s.audio_source {
        let mut audio = request.clone();
        audio.source = source.clone();
        Some(
            unsafe { engine.open(audio, s.cancel.clone()) }
                .context("open separate audio stream")?,
        )
    } else {
        None
    };
    let mut pending = [None, None];
    let mut finished = [false; 2];
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
        has_audio: external_audio
            .as_ref()
            .unwrap_or(&session)
            .info()
            .tracks
            .iter()
            .any(|t| t.kind == "audio"),
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
    while !s.stop.load(Ordering::Acquire) {
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
                if let Some(source) = &s.audio_source {
                    let mut audio = updated.clone();
                    audio.source = source.clone();
                    let opened = unsafe { engine.open(audio, s.cancel.clone()) };
                    if s.requested.load(Ordering::Acquire) > next.epoch {
                        continue;
                    }
                    external_audio = Some(opened?);
                }
                request = updated;
            }
            waiting.store(true, Ordering::Release);
            pending = [None, None];
            finished = [false; 2];
            let target_us = (next.target * 1e6).round() as i64;
            let result = session.seek(target_us).and_then(|_| {
                if let Some(audio) = &mut external_audio {
                    audio.seek(target_us)?;
                }
                Ok(())
            });
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
        let result = read_inputs(
            &mut session,
            external_audio.as_mut(),
            &mut pending,
            &mut finished,
        );
        waiting.store(false, Ordering::Release);
        if s.stop.load(Ordering::Acquire) {
            break;
        }
        if s.requested.load(Ordering::Acquire) > epoch {
            continue;
        }
        match result? {
            Some(frame) => {
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

// At most one lookahead frame per input. Both streams share the session clock,
// seek epoch and cancellation; neither can run ahead into an unbounded queue.
fn read_inputs(
    video: &mut remdia::Session,
    audio: Option<&mut remdia::Session>,
    pending: &mut [Option<remdia::Frame>; 2],
    ended: &mut [bool; 2],
) -> Result<Option<remdia::Frame>> {
    let Some(audio) = audio else {
        return video.read();
    };
    for (i, source) in [video, audio].into_iter().enumerate() {
        while pending[i].is_none() && !ended[i] {
            match source.read()? {
                Some(frame)
                    if matches!(
                        (i, frame.info()),
                        (0, remdia::FrameInfo::Video { .. }) | (1, remdia::FrameInfo::Audio { .. })
                    ) =>
                {
                    pending[i] = Some(frame)
                }
                Some(_) => {}
                None => ended[i] = true,
            }
        }
    }
    let timestamp = |frame: &remdia::Frame| frame.info().pts_us().unwrap_or(0);
    let index = match (&pending[0], &pending[1]) {
        (Some(v), Some(a)) => usize::from(timestamp(a) < timestamp(v)),
        (Some(_), None) => 0,
        (None, Some(_)) => 1,
        (None, None) => return Ok(None),
    };
    Ok(pending[index].take())
}
