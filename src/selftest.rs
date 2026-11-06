use crate::player::{Event, PlaybackState, Player};
use anyhow::{Context, Result, bail, ensure};
use std::time::{Duration, Instant};
pub fn music(path: String) -> Result<()> {
    let mut player = Player::open(path)?;
    player.set_volume(0.0);
    let until = Instant::now() + Duration::from_secs(10);
    while player.position() < 0.2 {
        pump(&mut player, 10)?;
        ensure!(Instant::now() < until, "music clock did not advance");
    }
    let info = player.snapshot().media.context("missing media metadata")?;
    ensure!(
        info.kind == crate::media::Kind::Music,
        "not detected as music"
    );
    println!(
        "music detected: title={:?}, artist={:?}, album={:?}, artwork={}",
        info.title,
        info.artist,
        info.album,
        info.artwork.is_some()
    );
    player.set_playing(false);
    pump(&mut player, 80)?;
    let position = player.position();
    pump(&mut player, 100)?;
    ensure!(
        (player.position() - position).abs() < 0.03,
        "pause moved clock"
    );
    player.seek(1.5);
    let id = player.seek(0.3);
    let until = Instant::now() + Duration::from_secs(5);
    let mut done = false;
    while Instant::now() < until {
        while let Some(event) = player.poll_event() {
            match event {
                Event::Error(error) => bail!(error),
                Event::SeekCompleted { id: current, .. } if current == id => done = true,
                _ => {}
            }
        }
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    ensure!(done && !player.is_playing(), "paused music seek failed");
    ensure!(player.queued() == 0, "music produced video frames");
    player.seek((player.duration() - 0.2).max(0.0));
    player.set_playing(true);
    let until = Instant::now() + Duration::from_secs(5);
    while player.snapshot().state != PlaybackState::Ended {
        pump(&mut player, 10)?;
        ensure!(
            Instant::now() < until,
            "music did not finish after audio drain"
        );
    }
    player.toggle_play();
    let until = Instant::now() + Duration::from_secs(5);
    while player.position() < 0.1 || player.snapshot().state == PlaybackState::Ended {
        pump(&mut player, 10)?;
        ensure!(Instant::now() < until, "music replay failed");
    }
    println!("PASS: real audio device, music clock, pause, rapid seek, EOF drain and replay");
    Ok(())
}

fn pump(p: &mut Player, millis: u64) -> Result<Vec<f64>> {
    let until = Instant::now() + Duration::from_millis(millis);
    let mut pts = Vec::new();
    while Instant::now() < until {
        while let Some(event) = p.poll_event() {
            if let Event::Error(e) = event {
                bail!(e);
            }
        }
        if let Some(f) = p.next_frame(p.sync_time()) {
            pts.push(f.pts);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    Ok(pts)
}
fn seek(p: &mut Player, target: f64) -> Result<()> {
    let id = p.seek(target);
    let start = Instant::now();
    let mut completed = false;
    let mut preview = None;
    while start.elapsed() < Duration::from_secs(10) {
        while let Some(event) = p.poll_event() {
            match event {
                Event::Error(e) => bail!(e),
                Event::SeekCompleted { id: done, position } if done == id => {
                    ensure!(
                        (position - target).abs() < 0.01,
                        "wrong seek completion position"
                    );
                    completed = true;
                }
                _ => {}
            }
        }
        if let Some(frame) = p.next_frame(p.sync_time()) {
            // Device recovery may start a newer generation while preserving
            // the caller's seek ID. Only an older generation is stale.
            ensure!(frame.epoch >= id, "stale generation after seek");
            ensure!(
                frame.pts + 0.001 >= target,
                "pre-target seek frame: {} < {target}",
                frame.pts
            );
            preview.get_or_insert(frame.pts);
        }
        if completed && (preview.is_some() || p.snapshot().state == PlaybackState::Ended) {
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    ensure!(completed, "seek completion timed out: {:?}", p.snapshot());
    ensure!(
        preview.is_some() || p.snapshot().state == PlaybackState::Ended,
        "seek preview timed out: {:?}, pos={}, queued={}, pending={:?}",
        p.snapshot(),
        p.position(),
        p.queued(),
        p.pending_pts()
    );
    println!("seek {target:.3}: id={id} first={preview:?}");
    Ok(())
}
pub fn run(path: String) -> Result<()> {
    let open_start = Instant::now();
    exercise(Player::open(path)?, open_start)
}
pub fn bilibili(id: String) -> Result<()> {
    use crate::bilibili::{Bilibili, start};
    let source = crate::jobs::block_on(async move {
        let api = std::sync::Arc::new(Bilibili::new(None)?);
        let id = crate::bilibili::video_id(&id).context("provide a BV ID or Bilibili video URL")?;
        let detail = api.detail(&id).await?;
        let playback = api.resolve(&id, detail.parts[0].cid, 16).await?;
        ensure!(
            playback.audio.is_some(),
            "sample has no separate audio stream"
        );
        start(api, playback).await
    })?;
    let open_start = Instant::now();
    exercise(
        Player::open_split(
            source.video.clone(),
            source.audio.clone(),
            Some(source.duration),
        )?,
        open_start,
    )
}
pub fn profile_bilibili(id: String) -> Result<()> {
    use crate::{
        bilibili::{Bilibili, start},
        credentials::{SystemVault, Vault},
    };
    let (source, quality) = crate::jobs::block_on(async move {
        let saved = SystemVault.get(&crate::settings::Settings::bilibili_target())?;
        let session = saved
            .as_deref()
            .map(serde_json::from_str::<crate::bilibili::Session>)
            .transpose()?;
        let api = std::sync::Arc::new(Bilibili::new(session.as_ref())?);
        let id = crate::bilibili::video_id(&id).context("provide a BV ID")?;
        let detail = api.detail(&id).await?;
        let playback = api
            .resolve(&id, detail.parts[0].cid, crate::bilibili::DEFAULT_QUALITY)
            .await?;
        let quality = playback.quality;
        Ok::<_, anyhow::Error>((start(api, playback).await?, quality))
    })?;
    let player = Player::open_split(
        source.video.clone(),
        source.audio.clone(),
        Some(source.duration),
    )?;
    println!("profile source quality={quality}");
    profile_player(player)
}
pub fn profile_media(path: String) -> Result<()> {
    profile_player(Player::open(path)?)
}
fn profile_player(mut player: Player) -> Result<()> {
    player.set_volume(0.0);
    let start = Instant::now();
    let mut began = None;
    let mut last_frame = None;
    let mut frames = 0;
    let mut gaps = 0;
    let mut buffering = 0;
    let mut state = PlaybackState::Opening;
    let mut first_pts = None;
    let mut last_pts = 0.0;
    let mut last_clock = 0.0f64;
    let mut max_clock_step = 0.0f64;
    let mut max_clock_error = 0.0f64;
    let mut clock_wall = Instant::now();
    while start.elapsed() < Duration::from_secs(50) {
        while let Some(event) = player.poll_event() {
            if let Event::Error(error) = event {
                bail!(error);
            }
        }
        let current = player.snapshot().state;
        if current == PlaybackState::Buffering && state != current {
            buffering += 1;
        }
        state = current;
        let clock = player.sync_time();
        max_clock_step = max_clock_step.max(clock - last_clock);
        max_clock_error =
            max_clock_error.max((clock - last_clock - clock_wall.elapsed().as_secs_f64()).abs());
        last_clock = clock;
        clock_wall = Instant::now();
        if let Some(frame) = player.next_frame(clock) {
            let now = Instant::now();
            began.get_or_insert(now);
            first_pts.get_or_insert(frame.pts);
            last_pts = frame.pts;
            if last_frame
                .is_some_and(|last: Instant| now.duration_since(last) > Duration::from_millis(100))
            {
                gaps += 1;
            }
            last_frame = Some(now);
            frames += 1;
        }
        if current == PlaybackState::Ended
            || began.is_some_and(|began: Instant| began.elapsed() >= Duration::from_secs(15))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let elapsed = began
        .context("no video frames during profile")?
        .elapsed()
        .as_secs_f64();
    println!(
        "profile: frames={frames} fps={:.2} decoded={} dropped={} pts_span={:.3} gaps_over_100ms={gaps} buffering_entries={buffering} underruns={} hardware={}",
        frames as f64 / elapsed,
        player.snapshot().decoded_frames,
        player.snapshot().dropped_frames,
        last_pts - first_pts.unwrap_or(0.0),
        player.underruns(),
        player.snapshot().hardware
    );
    println!(
        "clock: max_step_ms={:.2} max_error_ms={:.2}",
        max_clock_step * 1000.0,
        max_clock_error * 1000.0
    );
    ensure!(
        frames > 0 && last_pts > first_pts.unwrap_or(0.0),
        "playback did not advance"
    );
    Ok(())
}
fn exercise(mut p: Player, open_start: Instant) -> Result<()> {
    p.set_volume(0.0);
    ensure!(
        open_start.elapsed() < Duration::from_millis(500),
        "open blocked caller"
    );
    let startup = Instant::now();
    while matches!(
        p.snapshot().state,
        PlaybackState::Opening | PlaybackState::Seeking
    ) && startup.elapsed() < Duration::from_secs(30)
    {
        pump(&mut p, 10)?;
    }
    ensure!(p.snapshot().state != PlaybackState::Failed, "open failed");
    println!("opened: {:?}", p.snapshot());
    let frames = pump(&mut p, 600)?;
    ensure!(!frames.is_empty(), "no video frames during playback");
    let before_stall = p.position();
    std::thread::sleep(Duration::from_millis(700));
    ensure!(
        p.position() > before_stall + 0.4,
        "stalled video consumer blocked playback clock"
    );
    pump(&mut p, 30)?;
    let duration = p.duration();
    ensure!(
        duration > 1.5,
        "selftest needs media longer than 1.5 seconds"
    );
    seek(&mut p, duration * 0.5)?;
    p.set_playing(false);
    pump(&mut p, 100)?;
    let paused = p.position();
    pump(&mut p, 200)?;
    ensure!((p.position() - paused).abs() < 0.005, "pause clock moved");
    seek(&mut p, duration * 0.25)?;
    ensure!(!p.is_playing(), "paused seek resumed unexpectedly");
    p.seek(duration * 0.7);
    p.seek(duration * 0.1);
    seek(&mut p, duration * 0.4)?;
    p.set_playing(true);
    pump(&mut p, 300)?;
    ensure!(
        p.position() > duration * 0.4 + 0.05,
        "resume did not advance"
    );
    seek(&mut p, (duration - 0.5).max(0.0))?;
    // The last video frame must survive a temporarily stalled presentation loop.
    std::thread::sleep(Duration::from_millis(800));
    let timeout = Instant::now();
    while p.snapshot().state != PlaybackState::Ended && timeout.elapsed() < Duration::from_secs(5) {
        pump(&mut p, 10)?;
    }
    ensure!(
        p.snapshot().state == PlaybackState::Ended,
        "EOF did not drain: {:?} pos={}",
        p.snapshot(),
        p.position()
    );
    ensure!(!p.is_playing(), "ended still playing");
    seek(&mut p, duration * 0.2)?;
    pump(&mut p, 200)?;
    ensure!(
        p.position() > duration * 0.2 + 0.05,
        "seek after EOF did not restart"
    );
    seek(&mut p, duration)?;
    pump(&mut p, 100)?;
    ensure!(
        p.snapshot().state == PlaybackState::Ended,
        "seek to EOF did not end"
    );
    p.toggle_play();
    let replay_started = Instant::now();
    let mut restarted = Vec::new();
    while (restarted.is_empty() || p.position() < 0.1)
        && replay_started.elapsed() < Duration::from_secs(10)
    {
        restarted.extend(pump(&mut p, 10)?);
    }
    ensure!(
        !restarted.is_empty() && p.position() < 1.0,
        "replay from end failed"
    );
    println!(
        "replay first frames after_ms={:.2}",
        replay_started.elapsed().as_secs_f64() * 1000.0
    );
    println!(
        "PASS: async open, playback, seek, pause, paused seek, rapid seek, drain, seek after EOF, replay; underruns={}",
        p.underruns()
    );
    Ok(())
}
