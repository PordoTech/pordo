//! Each separate media input owns its SDK session on its own thread. A blocked
//! CDN read cannot prevent the other input from decoding into its bounded queue.
use super::demux::{self, Header, Input, Message, Seek};
use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct Worker {
    rx: Receiver<Message>,
    commands: Sender<Seek>,
    cancel: remdia::Cancellation,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    fn spawn(path: String, input: &Input, rate: u32) -> Result<Self> {
        let input = input.independent();
        let cancel = input.cancel.clone();
        let (tx, rx) = bounded(4);
        let (commands, seeks) = unbounded();
        let thread = std::thread::Builder::new()
            .name("pordo-split-input".into())
            .spawn(move || {
                let waiting = AtomicBool::new(false);
                if let Err(error) = demux::run(&path, &input, rate, &tx, seeks, &waiting)
                    && !input.stop.load(Ordering::Acquire)
                    && !input.cancel.is_cancelled()
                {
                    demux::send(&tx, Message::Error(format!("{error:#}")), &input);
                }
            })?;
        Ok(Self {
            rx,
            commands,
            cancel,
            thread: Some(thread),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Merge {
    epoch: u64,
    ack: [bool; 2],
    ended: [bool; 2],
    pending: [Option<remdia::Frame>; 2],
    last: [i64; 2],
}
impl Merge {
    fn new(epoch: u64, target: f64) -> Self {
        Self {
            epoch,
            ack: [epoch == 1; 2],
            ended: [false; 2],
            pending: [None, None],
            last: [(target * 1e6) as i64; 2],
        }
    }
    fn accept(&mut self, index: usize, message: Message) -> Result<()> {
        match message {
            Message::Frame(epoch, frame) if epoch == self.epoch && self.ack[index] => {
                if matches!(
                    (index, frame.info()),
                    (0, remdia::FrameInfo::Video { .. }) | (1, remdia::FrameInfo::Audio { .. })
                ) {
                    self.pending[index] = Some(frame);
                }
            }
            Message::Seeked(epoch) if epoch == self.epoch => self.ack[index] = true,
            Message::Eof(epoch) if epoch == self.epoch => self.ended[index] = true,
            Message::Error(error) => bail!(error),
            _ => {}
        }
        Ok(())
    }
    fn next(&mut self) -> Option<remdia::Frame> {
        if !self.ack.iter().all(|ack| *ack) {
            return None;
        }
        let timestamp = |frame: &remdia::Frame| frame.info().pts_us().unwrap_or(0);
        let index = match (&self.pending[0], &self.pending[1]) {
            (Some(v), Some(a)) => usize::from(timestamp(a) < timestamp(v)),
            (Some(v), None) if self.ended[1] || timestamp(v) <= self.last[1] + 500_000 => 0,
            (None, Some(a)) if self.ended[0] || timestamp(a) <= self.last[0] + 500_000 => 1,
            _ => return None,
        };
        let frame = self.pending[index].take()?;
        self.last[index] = timestamp(&frame);
        Some(frame)
    }
    fn done(&self) -> bool {
        self.ended.iter().all(|ended| *ended) && self.pending.iter().all(Option::is_none)
    }
}

pub(super) fn run(
    video: String,
    audio: String,
    input: &Input,
    rate: u32,
    tx: &Sender<Message>,
    commands: Receiver<Seek>,
    waiting: &AtomicBool,
) -> Result<()> {
    let workers = [
        Worker::spawn(video, input, rate)?,
        Worker::spawn(audio, input, rate)?,
    ];
    let mut headers: [Option<Header>; 2] = [None, None];
    while headers.iter().any(Option::is_none) {
        if input.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        for (index, worker) in workers.iter().enumerate() {
            if headers[index].is_none() {
                match worker.rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(Message::Opened(header)) => headers[index] = Some(header),
                    Ok(Message::Error(error)) => bail!(error),
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        bail!("Separate input exited during open")
                    }
                    _ => {}
                }
            }
        }
    }
    let mut header = headers[0].take().unwrap();
    header.has_audio = headers[1].take().unwrap().has_audio;
    if !demux::send(tx, Message::Opened(header), input) {
        return Ok(());
    }
    let mut merge = Merge::new(1, 0.0);
    let mut reported_eof = false;
    let mut reported_seek = 1;
    while !input.stop.load(Ordering::Acquire) {
        let mut seek = None;
        while let Ok(next) = commands.try_recv() {
            seek = Some(next);
        }
        if let Some(seek) = seek {
            merge = Merge::new(seek.epoch, seek.target);
            reported_eof = false;
            for worker in &workers {
                worker.commands.send(seek).context("seek separate input")?;
            }
        }
        if input.requested.load(Ordering::Acquire) > merge.epoch {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        for (index, worker) in workers.iter().enumerate() {
            if merge.pending[index].is_none() && !merge.ended[index] {
                match worker.rx.try_recv() {
                    Ok(message) => merge.accept(index, message)?,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        bail!("Separate input exited unexpectedly")
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {}
                }
            }
        }
        if reported_seek != merge.epoch && merge.ack.iter().all(|ack| *ack) {
            if !demux::send(tx, Message::Seeked(merge.epoch), input) {
                return Ok(());
            }
            reported_seek = merge.epoch;
        }
        if let Some(frame) = merge.next() {
            waiting.store(false, Ordering::Release);
            if !demux::send(tx, Message::Frame(merge.epoch, frame), input) {
                return Ok(());
            }
        } else if merge.done() && !reported_eof {
            if !demux::send(tx, Message::Eof(merge.epoch), input) {
                return Ok(());
            }
            reported_eof = true;
        } else {
            waiting.store(!reported_eof, Ordering::Release);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(audio: bool, epoch: u64, pts: i64) -> Message {
        let info = if audio {
            remdia::FrameInfo::Audio {
                format: "f32le".into(),
                pts_us: pts,
                rate: 48000,
                channels: 2,
                samples: 1,
            }
        } else {
            remdia::FrameInfo::Video {
                pts_us: Some(pts),
                duration_us: 16667,
                width: 2,
                height: 2,
                format: "rgba".into(),
                planes: vec![remdia::graph::Plane { stride: 8, rows: 2 }],
                sample_aspect_ratio: remdia::Rational { num: 1, den: 1 },
                color_space: "bt709".into(),
                color_range: "limited".into(),
                color_primaries: String::new(),
                color_transfer: String::new(),
                hardware_decoded: false,
            }
        };
        let bytes = vec![0; if audio { 8 } else { 16 }];
        Message::Frame(
            epoch,
            remdia::Frame::from_buffer(remdia::MediaBuffer::new(
                remdia::BufferHeader {
                    stream_id: u32::from(audio),
                    generation: epoch,
                    info,
                },
                vec![bytes],
            ))
            .unwrap(),
        )
    }
    #[test]
    fn blocked_input_allows_bounded_progress_without_unlimited_lead_or_premature_eof() {
        let mut merge = Merge::new(1, 0.0);
        merge.accept(0, frame(false, 1, 400_000)).unwrap();
        assert_eq!(merge.next().unwrap().info().pts_us(), Some(400_000));
        merge.accept(0, frame(false, 1, 600_000)).unwrap();
        assert!(merge.next().is_none());
        merge.accept(1, frame(true, 1, 250_000)).unwrap();
        assert_eq!(merge.next().unwrap().info().pts_us(), Some(250_000));
        assert_eq!(merge.next().unwrap().info().pts_us(), Some(600_000));
        merge.accept(1, Message::Eof(1)).unwrap();
        assert!(!merge.done());
        merge.accept(0, frame(false, 1, 2_000_000)).unwrap();
        assert!(
            merge.next().is_some(),
            "remaining input drains after peer EOF"
        );
        merge.accept(0, Message::Eof(1)).unwrap();
        assert!(merge.done());
    }
    #[test]
    fn seek_requires_both_acks_and_never_reuses_old_frames_or_duplicate_primary_audio() {
        let mut merge = Merge::new(3, 2.0);
        merge.accept(0, Message::Seeked(2)).unwrap();
        merge.accept(0, frame(false, 2, 2_000_000)).unwrap();
        assert!(merge.pending[0].is_none());
        merge.accept(0, Message::Seeked(3)).unwrap();
        merge.accept(0, frame(true, 3, 2_000_000)).unwrap();
        assert!(merge.pending[0].is_none());
        merge.accept(0, frame(false, 3, 2_000_000)).unwrap();
        assert!(merge.next().is_none());
        merge.accept(1, Message::Seeked(3)).unwrap();
        assert_eq!(merge.next().unwrap().generation(), 3);
        merge.accept(1, Message::Eof(2)).unwrap();
        assert!(!merge.ended[1]);
        assert!(
            merge
                .accept(1, Message::Error("read failed".into()))
                .is_err()
        );
    }
}
