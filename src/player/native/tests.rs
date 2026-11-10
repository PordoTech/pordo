use super::*;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
fn fixture(name: &str) -> String {
    PathBuf::from(
        pordo_core::environment::var("PORDO_NATIVE_FIXTURES").expect("set PORDO_NATIVE_FIXTURES"),
    )
    .join(name)
    .to_string_lossy()
    .into_owned()
}

#[test]
#[ignore = "requires native fixtures; validates media-clock speed, paused changes and split EOF"]
fn native_media_tempo_keeps_split_video_audio_and_seek_in_sync() {
    let mut p = Player::open_split(fixture("av.mp4"), Some(fixture("audio.wav")), None).unwrap();
    p.set_volume(0.0);
    until(&mut p, Duration::from_secs(10), |p, _| p.position() > 0.1);
    for speed in [1.25, 1.5, 2.0, 3.0, 1.0] {
        p.set_playing(false);
        until(&mut p, Duration::from_secs(2), |p, _| {
            p.snapshot().state == PlaybackState::Paused
        });
        p.seek(0.2);
        p.set_speed(speed);
        until(&mut p, Duration::from_secs(5), |p, _| {
            p.snapshot().state == PlaybackState::Paused && (p.position() - 0.2).abs() < 0.05
        });
        let position = p.position();
        std::thread::sleep(Duration::from_millis(80));
        assert!((p.position() - position).abs() < 0.02);
        assert_eq!(p.speed(), speed);
        p.set_playing(true);
        until(&mut p, Duration::from_secs(5), |p, _| {
            let rendered = *p.shared.output.rendered.lock().unwrap();
            p.position() > 0.25 && rendered.epoch == p.snapshot().epoch && rendered.rate > 0
        });
        // Validate media time per rendered PCM second. Host wall time also
        // includes decoder starvation and delayed simulated-device scheduling,
        // which makes this a CI throughput benchmark rather than a tempo check.
        // Production clock timestamps remain unchanged, including stall freezes.
        let start = *p.shared.output.rendered.lock().unwrap();
        assert_eq!(start.epoch, p.snapshot().epoch);
        let wall_start = Instant::now();
        let mut sample = start;
        until(&mut p, Duration::from_secs(5), |p, _| {
            sample = *p.shared.output.rendered.lock().unwrap();
            assert_eq!(
                sample.epoch, start.epoch,
                "speed measurement must not cross a seek"
            );
            assert_eq!(sample.rate, start.rate);
            sample.frames - start.frames >= u64::from(start.rate) / 4
        });
        let elapsed = (sample.frames - start.frames) as f64 / f64::from(start.rate);
        let measured = (sample.media_end - start.media_end) / elapsed;
        println!(
            "speed {speed}: measured {measured:.4} over {elapsed:.4} rendered PCM seconds ({:.4}s wall)",
            wall_start.elapsed().as_secs_f64()
        );
        assert!(
            (measured - speed as f64).abs() < 0.3,
            "{speed}: measured {measured}"
        );
    }
    p.set_speed(3.0);
    p.seek(p.duration() - 0.2);
    until(&mut p, Duration::from_secs(6), |p, _| {
        p.snapshot().state == PlaybackState::Ended
    });
    p.toggle_play();
    until(&mut p, Duration::from_secs(6), |p, _| {
        p.position() > 0.1 && p.position() < 1.5 && p.snapshot().state != PlaybackState::Ended
    });
}
#[test]
#[ignore = "requires staged remdia components, audio device and native fixtures"]
fn native_media_separate_audio_video_share_seeks_pause_and_eof() {
    // The primary fixture also has audio: only its video may be emitted when
    // the external audio input is supplied, so no duplicate audio is queued.
    let mut p = Player::open_split(fixture("av.mp4"), Some(fixture("audio.wav")), None).unwrap();
    p.set_volume(0.0);
    until(&mut p, Duration::from_secs(10), |p, _| p.position() > 0.15);
    assert!(p.snapshot().has_audio);
    assert_eq!(p.snapshot().media.unwrap().kind, crate::media::Kind::Video);
    p.set_playing(false);
    until(&mut p, Duration::from_secs(3), |p, _| {
        p.snapshot().state == PlaybackState::Paused
    });
    let paused = p.position();
    std::thread::sleep(Duration::from_millis(100));
    assert!((p.position() - paused).abs() < 0.03);
    p.seek(1.5);
    let id = p.seek(0.5);
    until(&mut p, Duration::from_secs(10), |p, events| {
        events
            .iter()
            .any(|e| matches!(e,Event::SeekCompleted {id:done,..} if *done==id))
            && p.snapshot().state == PlaybackState::Paused
    });
    assert!((p.position() - 0.5).abs() < 0.04);
    p.set_playing(true);
    until(&mut p, Duration::from_secs(5), |p, _| p.position() > 0.6);
    p.seek((p.duration() - 0.2).max(0.0));
    until(&mut p, Duration::from_secs(10), |p, _| {
        p.snapshot().state == PlaybackState::Ended
    });
    p.toggle_play();
    until(&mut p, Duration::from_secs(5), |p, _| {
        p.position() > 0.1 && p.position() < 1.0 && p.snapshot().state != PlaybackState::Ended
    });
}

#[test]
#[ignore = "requires staged components and fixtures; simulates a stalled separate audio download"]
fn native_media_split_network_stall_freezes_clock_and_latest_seek_recovers() {
    use axum::{
        Router,
        body::Body,
        http::{HeaderMap, Response},
        routing::get,
    };
    use std::sync::atomic::AtomicUsize;
    let data = Arc::new(std::fs::read(fixture("audio.wav")).unwrap());
    let available = Arc::new(AtomicUsize::new(data.len() / 3));
    let cancel = tokio_util::sync::CancellationToken::new();
    let (url, server) = crate::jobs::block_on({
        let data = data.clone();
        let available = available.clone();
        let cancel = cancel.clone();
        async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route("/audio.wav", get(move |headers: HeaderMap| {
                let data = data.clone(); let available = available.clone(); let cancel = cancel.clone();
                async move {
                    let start = headers.get("range").and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("bytes=")).and_then(|h| h.split('-').next()).and_then(|h| h.parse::<usize>().ok()).unwrap_or(0).min(data.len());
                    let length = data.len();
                    let body = futures_util::stream::unfold((start, data, available, cancel), |(mut offset, data, available, cancel)| async move {
                        while offset < data.len() {
                            let end = (offset + 8192).min(available.load(Ordering::Acquire)).min(data.len());
                            if end > offset {
                                let bytes = data[offset..end].to_vec(); offset = end;
                                return Some((Ok::<_, std::io::Error>(bytes), (offset, data, available, cancel)));
                            }
                            tokio::select! {_ = cancel.cancelled() => return None, _ = tokio::time::sleep(Duration::from_millis(5)) => {}}
                        }
                        None
                    });
                    Response::builder().status(206).header("Content-Type", "audio/wav").header("Accept-Ranges", "bytes")
                        .header("Content-Length", length - start).header("Content-Range", format!("bytes {start}-{}/{length}", length-1))
                        .body(Body::from_stream(body)).unwrap()
                }
            }));
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            Ok((format!("http://{address}/audio.wav"), server))
        }
    }).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut player = Player::open_split(fixture("av.mp4"), Some(url), None).unwrap();
        until(&mut player, Duration::from_secs(8), |p, _| {
            p.snapshot().state == PlaybackState::Buffering
        });
        let frozen = player.position();
        std::thread::sleep(Duration::from_millis(150));
        pump(&mut player);
        assert!((player.position() - frozen).abs() < 0.03);
        player.seek(2.5);
        std::thread::sleep(Duration::from_millis(100));
        let id = player.seek(0.0);
        until(&mut player, Duration::from_secs(4), |_, events| {
            events
                .iter()
                .any(|event| matches!(event, Event::SeekCompleted {id: done, ..} if *done == id))
        });
        available.store(data.len(), Ordering::Release);
        until(&mut player, Duration::from_secs(5), |p, _| {
            p.position() > 1.5
        });
    }));
    cancel.cancel();
    server.abort();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[test]
#[ignore = "requires PORDO_CADENCE_FILE pointing to a six-second 60fps clip"]
fn playback_profile_sixty_fps_keeps_future_frames_until_presentation() {
    let path = std::env::var("PORDO_CADENCE_FILE").unwrap();
    let mut player = Player::open(path).unwrap();
    player.set_volume(0.0);
    let start = Instant::now();
    let mut count = 0;
    while start.elapsed() < Duration::from_secs(12) {
        while let Some(event) = player.poll_event() {
            if let Event::Error(error) = event {
                panic!("{error}");
            }
        }
        if player.next_frame(player.sync_time()).is_some() {
            count += 1;
        }
        if player.snapshot().state == PlaybackState::Ended {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(player.snapshot().state, PlaybackState::Ended);
    println!("60fps presentation: {count}/360 frames");
    assert!(
        count >= 342,
        "more than 5% of future frames were lost: {count}/360"
    );
}

#[test]
#[ignore = "requires PORDO_BFRAME_FILE; compares local and HTTP decoded picture/timestamp associations"]
fn playback_profile_b_frame_timestamps_match_reference_pictures() {
    use axum::{
        Router,
        body::Body,
        http::{HeaderMap, Response},
        routing::get,
    };
    let path = std::env::var("PORDO_BFRAME_FILE").unwrap();
    let data = Arc::new(std::fs::read(&path).unwrap());
    let (url, server) = crate::jobs::block_on(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Router::new().route(
            "/video.mp4",
            get(move |headers: HeaderMap| {
                let data = data.clone();
                async move {
                    let range = headers
                        .get("range")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|h| h.strip_prefix("bytes="))
                        .and_then(|h| h.split_once('-'));
                    let start = range
                        .and_then(|(start, _)| start.parse::<usize>().ok())
                        .unwrap_or(0)
                        .min(data.len());
                    let end = range
                        .and_then(|(_, end)| end.parse::<usize>().ok())
                        .map(|end| end.saturating_add(1))
                        .unwrap_or(data.len())
                        .min(data.len());
                    Response::builder()
                        .status(206)
                        .header("Content-Type", "video/mp4")
                        .header("Accept-Ranges", "bytes")
                        .header("Content-Length", end.saturating_sub(start))
                        .header(
                            "Content-Range",
                            format!("bytes {start}-{}/{}", end.saturating_sub(1), data.len()),
                        )
                        .body(Body::from(data[start..end].to_vec()))
                        .unwrap()
                }
            }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok((format!("http://{address}/video.mp4"), server))
    })
    .unwrap();
    let pictures = |source| {
        let engine = crate::media_backend::engine().unwrap();
        let mut session = unsafe {
            engine.open(
                remdia::Request::playback(source),
                remdia::Cancellation::default(),
            )
        }
        .unwrap();
        let mut pictures = std::collections::BTreeMap::new();
        while pictures.len() < 500 {
            let Some(frame) = session.read().unwrap() else {
                break;
            };
            if let remdia::FrameInfo::Video {
                pts_us: Some(pts), ..
            } = frame.info()
            {
                let bytes: Vec<_> = (0..4)
                    .flat_map(|index| frame.buffer().plane(index).iter().copied())
                    .collect();
                pictures.insert(*pts, crate::crypto::sha256_hex(&bytes));
            }
        }
        pictures
    };
    let reference = pictures(path);
    let network = pictures(url);
    server.abort();
    assert!(reference.len() >= 100);
    assert_eq!(
        reference.len(),
        network.len(),
        "both paths must produce every picture"
    );
    let mismatches: Vec<_> = network
        .iter()
        .filter(|(pts, hash)| reference.get(pts) != Some(hash))
        .map(|(pts, _)| *pts)
        .collect();
    assert!(
        mismatches.is_empty(),
        "mismatched timestamps: {mismatches:?}; reference={} network={}",
        reference.len(),
        network.len()
    );
    println!("MP4 picture/timestamp match: {} frames", reference.len());
}
fn pump(player: &mut Player) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(e) = player.poll_event() {
        if let Event::Error(message) = &e {
            panic!("{message}");
        }
        events.push(e);
    }
    player.next_frame(player.sync_time());
    events
}
fn until(
    player: &mut Player,
    limit: Duration,
    mut ready: impl FnMut(&mut Player, &[Event]) -> bool,
) {
    let start = Instant::now();
    loop {
        let events = pump(player);
        if ready(player, &events) {
            return;
        }
        assert!(
            start.elapsed() < limit,
            "state={:?}, position={}",
            player.snapshot(),
            player.position()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
#[ignore = "requires staged remdia component DLLs and native fixtures"]
fn native_media_music_pause_seek_eof_and_replay() {
    for source in ["audio.wav", "cover.flac", "audio.mp3", "audio.opus"] {
        let mut p = Player::open(fixture(source)).unwrap();
        until(&mut p, Duration::from_secs(5), |p, _| p.position() > 0.1);
        assert_eq!(
            p.snapshot().media.as_ref().unwrap().kind,
            crate::media::Kind::Music
        );
        assert_eq!(p.queued(), 0);
        if source == "cover.flac" {
            assert!(p.snapshot().media.as_ref().unwrap().artwork.is_some());
        }
        p.set_playing(false);
        until(&mut p, Duration::from_secs(2), |p, _| {
            p.snapshot().state == PlaybackState::Paused
        });
        let paused = p.position();
        std::thread::sleep(Duration::from_millis(100));
        assert!((p.position() - paused).abs() < 0.03);
        p.seek(1.5);
        let id = p.seek(0.3);
        until(&mut p, Duration::from_secs(5), |p, e| {
            e.iter()
                .any(|e| matches!(e,Event::SeekCompleted {id:done,..} if *done==id))
                && p.snapshot().state == PlaybackState::Paused
        });
        p.seek(2.8);
        p.set_playing(true);
        until(&mut p, Duration::from_secs(5), |p, _| {
            p.snapshot().state == PlaybackState::Ended
        });
        p.toggle_play();
        until(&mut p, Duration::from_secs(5), |p, _| {
            p.position() > 0.1 && p.position() < 1. && p.snapshot().state != PlaybackState::Ended
        });
    }
}
#[test]
#[ignore = "requires staged remdia component DLLs and native fixtures"]
fn native_media_device_recovery() {
    let mut p = Player::open(fixture("audio.wav")).unwrap();
    until(&mut p, Duration::from_secs(5), |p, _| p.position() > 0.15);
    let epoch = p.snapshot().epoch;
    p.shared.output.failed.store(true, Ordering::Release);
    let mut recovered = false;
    until(&mut p, Duration::from_secs(6), |p, e| {
        recovered |= e.iter().any(|e| matches!(e, Event::Recovered));
        recovered && p.snapshot().epoch > epoch && p.position() > 0.2
    });
}

#[test]
#[ignore = "requires staged remdia components and native video fixture"]
fn native_media_paused_device_loss_defers_recovery_until_resume() {
    let mut player = Player::open(fixture("av.mp4")).unwrap();
    until(&mut player, Duration::from_secs(5), |p, _| {
        p.position() > 0.1
    });
    player.set_playing(false);
    until(&mut player, Duration::from_secs(2), |p, _| {
        p.snapshot().state == PlaybackState::Paused
    });
    let attempts = player.shared.output.open_attempts.load(Ordering::Acquire);
    player
        .shared
        .output
        .simulated_open_failures
        .store(2, Ordering::Release);
    player.shared.output.failed.store(true, Ordering::Release);
    until(&mut player, Duration::from_secs(2), |p, events| {
        assert!(!events.iter().any(|e| matches!(e, Event::Warning(_))));
        !p.snapshot().has_audio
    });
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        player.shared.output.open_attempts.load(Ordering::Acquire),
        attempts
    );
    assert!(
        !pump(&mut player)
            .iter()
            .any(|e| matches!(e, Event::Warning(_)))
    );
    player.set_playing(true);
    let mut recovered = false;
    until(&mut player, Duration::from_secs(6), |p, events| {
        recovered |= events.iter().any(|e| matches!(e, Event::Recovered));
        recovered && p.snapshot().has_audio && p.position() > 0.2
    });
    assert!(player.shared.output.open_attempts.load(Ordering::Acquire) >= attempts + 3);
}

#[test]
#[ignore = "requires staged remdia components and native video fixture"]
fn native_media_initial_audio_failure_recovers_without_reopening_video() {
    audio::simulate_initial_failures(2);
    let mut player = Player::open(fixture("av.mp4")).unwrap();
    let mut recovered = false;
    until(&mut player, Duration::from_secs(6), |p, events| {
        assert!(!events.iter().any(|e| matches!(e, Event::Warning(_))));
        recovered |= events.iter().any(|e| matches!(e, Event::Recovered));
        recovered && p.snapshot().has_audio && p.position() > 0.1
    });
    assert_eq!(
        player.shared.output.open_attempts.load(Ordering::Acquire),
        3
    );
}
#[test]
#[ignore = "requires staged remdia component DLLs"]
fn native_media_drop_interrupts_blocked_network_open() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        accepted.send(()).unwrap();
        let _ = wait.recv_timeout(Duration::from_secs(5));
    });
    let p = Player::open(format!("http://{address}/blocked.mp4")).unwrap();
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    let shared = p.shared.clone();
    let start = Instant::now();
    drop(p);
    assert!(start.elapsed() < Duration::from_millis(200));
    while Arc::strong_count(&shared) > 1 && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let reaped = Arc::strong_count(&shared) == 1;
    let _ = release.send(());
    server.join().unwrap();
    assert!(reaped, "native graph worker was not reclaimed");
}
#[cfg(any(windows, target_os = "macos"))]
#[test]
#[ignore = "requires staged platform video components"]
fn native_media_video_playback_pause_seek_and_drain() {
    let mut p = Player::open(fixture("av.mp4")).unwrap();
    until(&mut p, Duration::from_secs(8), |p, _| p.position() > 0.15);
    assert_eq!(
        p.snapshot().media.as_ref().unwrap().kind,
        crate::media::Kind::Video
    );
    p.set_playing(false);
    let id = p.seek(1.25);
    let mut preview = false;
    let start = Instant::now();
    let mut completed = false;
    while start.elapsed() < Duration::from_secs(5) {
        for e in pump(&mut p) {
            if matches!(e,Event::SeekCompleted {id:done,..} if done==id) {
                completed = true;
            }
        }
        if let Some(f) = p.next_frame(p.sync_time()) {
            assert_eq!(f.epoch, id);
            assert!(f.pts >= 1.25 && f.pts < 1.25 + 1. / 24. + 0.001);
            assert_eq!((f.image().width(), f.image().height()), (160, 90));
            preview = true;
        }
        if completed {
            let presented = f64::from_bits(p.shared.presented.load(Ordering::Acquire));
            assert!(presented < 1.25 + 1. / 24. + 0.001);
            preview |= presented >= 1.25;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(completed && preview);
    p.set_playing(true);
    p.seek(2.8);
    until(&mut p, Duration::from_secs(5), |p, _| {
        p.snapshot().state == PlaybackState::Ended
    });
    p.toggle_play();
    until(&mut p, Duration::from_secs(5), |p, _| {
        p.position() > 0.1 && p.position() < 1.
    });
}

#[test]
#[ignore = "requires staged remdia platform components"]
fn native_media_video_open_and_paused_seek_survive_audio_preroll() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/av-audio-preroll.mkv")
        .to_string_lossy()
        .into_owned();
    let mut player = Player::open(source).unwrap();
    until(&mut player, Duration::from_secs(6), |p, _| {
        p.snapshot().state == PlaybackState::Playing && p.position() > 0.1
    });
    player.set_playing(false);
    let seek = player.seek(0.25);
    until(&mut player, Duration::from_secs(6), |p, events| {
        p.snapshot().state == PlaybackState::Paused
            && events
                .iter()
                .any(|e| matches!(e,Event::SeekCompleted {id,..} if *id==seek))
    });
    assert!((player.position() - 0.25).abs() < 0.03);
}

#[test]
#[ignore = "requires remdia component DLLs and native fixtures"]
fn native_media_progressive_audio_buffers_and_new_seek_preempts_missing_data() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::atomic::AtomicUsize,
    };
    let data = Arc::new(std::fs::read(fixture("audio.wav")).unwrap());
    let available = Arc::new(AtomicUsize::new(80_000));
    let stop = Arc::new(AtomicBool::new(false));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server_data = data.clone();
    let server_available = available.clone();
    let server_stop = stop.clone();
    let server = std::thread::spawn(move || {
        let mut children = Vec::new();
        while !server_stop.load(Ordering::Acquire) {
            let (mut socket, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            };
            let data = server_data.clone();
            let available = server_available.clone();
            let stop = server_stop.clone();
            children.push(std::thread::spawn(move||{
                let _=socket.set_read_timeout(Some(Duration::from_secs(2)));
                let _=socket.set_write_timeout(Some(Duration::from_secs(1)));
                let mut request=Vec::new();let mut part=[0;1024];
                while request.len()<16384&&!request.ends_with(b"\r\n\r\n"){
                    match socket.read(&mut part){Ok(0)|Err(_)=>return,Ok(n)=>request.extend_from_slice(&part[..n])}
                }
                let request=String::from_utf8_lossy(&request);
                let offset=request.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("range: bytes=").and_then(|v|v.split('-').next()).and_then(|v|v.parse::<usize>().ok())).unwrap_or(0);
                if offset>=data.len(){let _=write!(socket,"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\n\r\n");return;}
                if write!(socket,"HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Type: audio/wav\r\nContent-Range: bytes {offset}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",data.len()-1,data.len(),data.len()-offset).is_err(){return;}
                let mut position=offset;
                while position<data.len()&&!stop.load(Ordering::Acquire){
                    let end=(position+8192).min(available.load(Ordering::Acquire)).min(data.len());
                    if end<=position{std::thread::sleep(Duration::from_millis(5));continue;}
                    if socket.write_all(&data[position..end]).is_err(){return;}position=end;
                }
            }));
        }
        for child in children {
            let _ = child.join();
        }
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut player = Player::open_progressive(format!("http://{address}/audio.wav")).unwrap();
        until(&mut player, Duration::from_secs(8), |p, _| {
            p.snapshot().state == PlaybackState::Buffering
        });
        let frozen = player.position();
        std::thread::sleep(Duration::from_millis(150));
        pump(&mut player);
        assert!((player.position() - frozen).abs() < 0.03);
        player.seek(2.5);
        std::thread::sleep(Duration::from_millis(150));
        let id = player.seek(0.);
        until(&mut player, Duration::from_secs(3), |_, events| {
            events
                .iter()
                .any(|e| matches!(e,Event::SeekCompleted {id:done,..} if *done==id))
        });
        // Recover while a second distant seek is blocked. Recovery gets a new
        // media generation but must retain the user's target and acknowledgement.
        player.set_playing(false);
        let pending = player.seek(2.5);
        until(&mut player, Duration::from_secs(3), |p, _| {
            p.snapshot().epoch == pending && p.snapshot().state == PlaybackState::Seeking
        });
        player
            .shared
            .output
            .simulated_rate
            .store(44100, Ordering::Release);
        player.shared.output.failed.store(true, Ordering::Release);
        until(&mut player, Duration::from_secs(3), |p, events| {
            events.iter().any(|e| matches!(e, Event::Recovered))
                && p.shared.requested.load(Ordering::Acquire) > pending
        });
        available.store(data.len(), Ordering::Release);
        until(&mut player, Duration::from_secs(5), |p, events| {
            events.iter().any(|e| {
                matches!(e, Event::SeekCompleted { id, position }
                if *id == pending && (*position - 2.5).abs() < 0.001)
            }) && p.snapshot().state == PlaybackState::Paused
        });
        assert!((player.position() - 2.5).abs() < 0.03);
        player.set_playing(true);
        until(&mut player, Duration::from_secs(5), |p, _| {
            p.position() > 2.6
        });
    }));
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
