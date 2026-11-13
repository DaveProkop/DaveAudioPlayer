//! End-to-end engine tests with the null output: real files, real decoder threads,
//! real mixer, audio captured instead of played.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dap_engine::{
    Command, Engine, EngineConfig, Event, OutputConfig, PlayerState, QueueEntry, Status,
    TransitionKind,
};
use dap_library::db::open_writer;
use dap_library::{add_root, scan_root};

const RATE: u32 = 44_100;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("dap-engine-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("music")).unwrap();
        TempDir(dir)
    }
    fn music(&self) -> PathBuf {
        self.0.join("music")
    }
    fn db(&self) -> PathBuf {
        self.0.join("library.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Sample `i` of track `k`: never zero, distinct per track, exact in 16-bit.
fn sample(k: u32, i: u32) -> i16 {
    ((k * 7919 + i) % 30_000 + 1) as i16
}

fn write_wav(path: &Path, k: u32, frames: u32) {
    let data_len = frames * 2;
    let mut f = std::fs::File::create(path).unwrap();
    let mut w = |b: &[u8]| f.write_all(b).unwrap();
    w(b"RIFF");
    w(&(36 + data_len).to_le_bytes());
    w(b"WAVEfmt ");
    w(&16u32.to_le_bytes());
    w(&1u16.to_le_bytes());
    w(&1u16.to_le_bytes());
    w(&RATE.to_le_bytes());
    w(&(RATE * 2).to_le_bytes());
    w(&2u16.to_le_bytes());
    w(&16u16.to_le_bytes());
    w(b"data");
    w(&data_len.to_le_bytes());
    w(&(0..frames)
        .flat_map(|i| sample(k, i).to_le_bytes())
        .collect::<Vec<_>>());
}

/// Scans the music folder and returns track ids by file name.
fn scan(dir: &TempDir) -> impl Fn(&str) -> i64 {
    let mut conn = open_writer(&dir.db()).unwrap();
    let root = add_root(&conn, &dir.music()).unwrap();
    scan_root(&mut conn, root, &AtomicBool::new(false), |_| {}).unwrap();
    let ids: Vec<(String, i64)> = conn
        .prepare("SELECT f.rel_path, t.id FROM track t JOIN file f ON f.id = t.file_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    move |name| {
        ids.iter()
            .find(|(p, _)| p == name)
            .unwrap_or_else(|| panic!("{name} not scanned"))
            .1
    }
}

fn start_with(dir: &TempDir, capture: Option<Arc<Mutex<Vec<f32>>>>) -> Engine {
    Engine::start(EngineConfig {
        db_path: dir.db(),
        output: OutputConfig::Null {
            sample_rate: RATE,
            channels: 1,
            speed: 8.0,
            capture,
        },
        watch_folders: false,
    })
    .unwrap()
}

/// An engine that plays the samples untouched: gapless, no normalization.
fn start(dir: &TempDir, capture: Option<Arc<Mutex<Vec<f32>>>>) -> Engine {
    let engine = start_with(dir, capture);
    engine.send(Command::SetCrossfade { ms: 0 });
    engine.send(Command::SetNormalization(false));
    engine
}

/// Collects events until `done` returns true (or panics after a timeout).
fn wait_for(engine: &Engine, mut done: impl FnMut(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Ok(e) = engine.events().recv_timeout(Duration::from_millis(50)) {
            let stop = done(&e);
            seen.push(e);
            if stop {
                return seen;
            }
        }
    }
    panic!("timed out; events: {seen:#?}");
}

fn last_state(events: &[Event]) -> PlayerState {
    events
        .iter()
        .rev()
        .find_map(|e| {
            if let Event::State(s) = e {
                Some(s.clone())
            } else {
                None
            }
        })
        .expect("a state event")
}

#[test]
fn plays_queue_gaplessly_skips_broken_file_and_counts_plays() {
    let dir = TempDir::new("gapless");
    // Long enough for the control thread to preload the next track at 8x speed
    // (real tracks last seconds to minutes).
    let lengths = [44_100u32, 50_001, 30_870];
    for (k, len) in lengths.iter().enumerate() {
        write_wav(&dir.music().join(format!("{k}.wav")), k as u32, *len);
    }
    // A track whose file disappeared after scanning cannot be played.
    write_wav(&dir.music().join("gone.wav"), 9, 1000);
    let id = scan(&dir);
    let gone_id = id("gone.wav");
    std::fs::remove_file(dir.music().join("gone.wav")).unwrap();

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start(&dir, Some(capture.clone()));
    // The unplayable track first: it is skipped before any audio, so the captured stream
    // must be exactly the three good tracks.
    engine.send(Command::Enqueue {
        track_ids: vec![gone_id, id("0.wav"), id("1.wav"), id("2.wav")],
        at: None,
    });
    engine.send(Command::Play);

    let mut started_playing = false;
    let events = wait_for(&engine, |e| match e {
        Event::State(s) if s.status == Status::Playing => {
            started_playing = true;
            false
        }
        Event::State(s) => started_playing && s.status == Status::Stopped,
        _ => false,
    });
    engine.shutdown();

    let failed: Vec<i64> = events
        .iter()
        .filter_map(|e| {
            if let Event::TrackFailed { track_id, .. } = e {
                Some(*track_id)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        failed,
        [gone_id],
        "the unplayable track is skipped, not fatal"
    );

    // The captured stream must be exactly the three tracks back to back.
    let expected: Vec<f32> = lengths
        .iter()
        .enumerate()
        .flat_map(|(k, &len)| (0..len).map(move |i| f32::from(sample(k as u32, i)) / 32768.0))
        .collect();
    let out = capture.lock().unwrap();
    let start = out
        .iter()
        .position(|&s| s != 0.0)
        .expect("audio was played");
    let fade = (RATE / 100) as usize; // 10 ms fade-in on Play
    assert!(
        out.len() >= start + expected.len(),
        "captured too little: {} < {}",
        out.len() - start,
        expected.len()
    );
    let played = &out[start..start + expected.len()];
    if let Some(i) = (fade..expected.len()).find(|&i| played[i] != expected[i]) {
        let around = |v: &[f32]| v[i.saturating_sub(2)..(i + 3).min(v.len())].to_vec();
        panic!(
            "first difference at frame {i} (track boundaries at {}, {}): played {:?}, expected {:?}",
            lengths[0],
            lengths[0] + lengths[1],
            around(played),
            around(&expected)
        );
    }
    assert!(
        out[start + expected.len()..].iter().all(|&s| s == 0.0),
        "silence after the queue"
    );

    let conn = open_writer(&dir.db()).unwrap();
    let plays: i64 = conn
        .query_row("SELECT sum(play_count) FROM track", [], |r| r.get(0))
        .unwrap();
    let history: i64 = conn
        .query_row("SELECT count(*) FROM play_history", [], |r| r.get(0))
        .unwrap();
    assert_eq!((plays, history), (3, 3));
}

#[test]
fn queue_edits_and_position_survive_a_restart() {
    let dir = TempDir::new("restore");
    for k in 0..4u32 {
        write_wav(&dir.music().join(format!("{k}.wav")), k, RATE * 3);
    }
    let id = scan(&dir);
    let ids: Vec<i64> = (0..4).map(|k| id(&format!("{k}.wav"))).collect();

    let engine = start(&dir, None);
    engine.send(Command::Enqueue {
        track_ids: ids.clone(),
        at: None,
    });
    engine.send(Command::MoveQueueItem { from: 3, to: 0 }); // 3 0 1 2
    engine.send(Command::RemoveQueueItem(2)); // 3 0 2
    engine.send(Command::PlayIndex(1)); // track 0
    wait_for(
        &engine,
        |e| matches!(e, Event::State(s) if s.status == Status::Playing),
    );
    std::thread::sleep(Duration::from_millis(150)); // ~1.2 s of audio at 8x speed
    engine.send(Command::Pause);
    let mut paused_events = wait_for(
        &engine,
        |e| matches!(e, Event::State(s) if s.status == Status::Paused),
    );
    // The last positions may still arrive after the pause.
    std::thread::sleep(Duration::from_millis(300));
    paused_events.extend(engine.events().try_iter());
    let paused = last_state(&paused_events);
    assert!(paused.position_ms > 200, "{paused:?}");
    engine.shutdown();

    let engine = start(&dir, None);
    let events = wait_for(&engine, |e| matches!(e, Event::State(_)));
    let queue: Vec<i64> = events
        .iter()
        .find_map(|e| {
            if let Event::Queue(q) = e {
                Some(q.iter().map(|x: &QueueEntry| x.track_id).collect())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(queue, [ids[3], ids[0], ids[2]]);
    let restored = last_state(&events);
    assert_eq!(restored.index, Some(1));
    assert_eq!(restored.status, Status::Stopped);
    assert!(
        restored.position_ms.abs_diff(paused.position_ms) < 150,
        "restored {} vs paused {}",
        restored.position_ms,
        paused.position_ms
    );

    // Play resumes there; Next and Previous move through the queue.
    engine.send(Command::Play);
    let s = last_state(&wait_for(
        &engine,
        |e| matches!(e, Event::State(s) if s.status == Status::Playing),
    ));
    assert_eq!(s.entry.map(|e| e.track_id), Some(ids[0]));
    assert!(s.position_ms >= paused.position_ms.saturating_sub(150));
    engine.send(Command::Next);
    let s = last_state(&wait_for(
        &engine,
        |e| matches!(e, Event::State(s) if s.index == Some(2)),
    ));
    assert_eq!(s.entry.map(|e| e.track_id), Some(ids[2]));
    engine.send(Command::Previous);
    let s = last_state(&wait_for(
        &engine,
        |e| matches!(e, Event::State(s) if s.index == Some(1)),
    ));
    assert_eq!(s.position_ms, 0);

    engine.send(Command::Seek { position_ms: 2_000 });
    let s = last_state(&wait_for(
        &engine,
        |e| matches!(e, Event::State(s) if s.position_ms >= 2_000),
    ));
    assert_eq!(s.position_ms, 2_000);
}

#[test]
fn add_folder_scans_in_batches_while_queue_edits_go_through() {
    let dir = TempDir::new("scan");
    for k in 0..120u32 {
        write_wav(&dir.music().join(format!("{k:03}.wav")), k, 500);
    }
    // Create the database first (the engine opens an existing or new file).
    drop(open_writer(&dir.db()).unwrap());
    let engine = start(&dir, None);
    engine.send(Command::AddFolder(dir.music()));

    let mut progress_events = 0;
    let events = wait_for(&engine, |e| {
        if matches!(e, Event::ScanProgress { .. }) {
            progress_events += 1;
        }
        matches!(e, Event::ScanFinished(_))
    });
    let summary = events
        .iter()
        .find_map(|e| {
            if let Event::ScanFinished(s) = e {
                Some(s.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        (summary.added, summary.failed, summary.cancelled),
        (120, 0, false)
    );
    assert!(
        progress_events >= 3,
        "120 files in batches of 50 → 3 steps, got {progress_events}"
    );

    // A second request rescans: everything unchanged.
    engine.send(Command::RescanLibrary);
    let events = wait_for(&engine, |e| matches!(e, Event::ScanFinished(_)));
    let Some(Event::ScanFinished(s)) = events.last() else {
        unreachable!()
    };
    assert_eq!((s.added, s.unchanged), (0, 120));
}

fn captured_from_first_sound(capture: &Mutex<Vec<f32>>) -> Vec<f32> {
    let out = capture.lock().unwrap();
    let start = out
        .iter()
        .position(|&s| s != 0.0)
        .expect("audio was played");
    out[start..].to_vec()
}

fn play_to_the_end(engine: &Engine) -> Vec<Event> {
    let mut started_playing = false;
    wait_for(engine, |e| match e {
        Event::State(s) if s.status == Status::Playing => {
            started_playing = true;
            false
        }
        Event::State(s) => started_playing && s.status == Status::Stopped,
        _ => false,
    })
}

#[test]
fn crossfades_between_tracks_and_counts_both_plays() {
    let dir = TempDir::new("crossfade");
    let len = 2 * RATE; // 2 s each
    write_wav(&dir.music().join("a.wav"), 1, len);
    write_wav(&dir.music().join("b.wav"), 2, len);
    let id = scan(&dir);

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start(&dir, Some(capture.clone()));
    engine.send(Command::SetCrossfade { ms: 500 });
    engine.send(Command::Enqueue {
        track_ids: vec![id("a.wav"), id("b.wav")],
        at: None,
    });
    engine.send(Command::Play);
    play_to_the_end(&engine);
    engine.shutdown();

    let fade = (RATE / 2) as usize;
    let len = len as usize;
    let out = captured_from_first_sound(&capture);
    let a = |i: usize| f32::from(sample(1, i as u32)) / 32768.0;
    let b = |i: usize| f32::from(sample(2, i as u32)) / 32768.0;

    // Track a alone, then the overlap with equal-power curves, then track b alone.
    let ramp = (RATE / 100) as usize; // 10 ms fade-in on Play
    assert_eq!(out[ramp + 1000], a(ramp + 1000));
    let start = len - fade;
    for k in [0, fade / 4, fade / 2, fade - 1] {
        let t = k as f64 / fade as f64 * std::f64::consts::FRAC_PI_2;
        let want = a(start + k) * t.cos() as f32 + b(k) * t.sin() as f32;
        assert!(
            (out[start + k] - want).abs() < 1e-5,
            "overlap frame {k}: {} vs {want}",
            out[start + k]
        );
    }
    assert_eq!(out[len], b(fade));
    let total = 2 * len - fade;
    assert_eq!(out[total - 1], b(len - 1), "b plays to its last frame");
    assert!(out[total..].iter().all(|&s| s == 0.0));

    let conn = open_writer(&dir.db()).unwrap();
    let plays: i64 = conn
        .query_row("SELECT sum(play_count) FROM track", [], |r| r.get(0))
        .unwrap();
    assert_eq!(plays, 2);
}

#[test]
fn consecutive_album_tracks_stay_gapless_with_crossfade_on() {
    let dir = TempDir::new("album");
    let len = 2 * RATE;
    write_wav(&dir.music().join("01.wav"), 1, len);
    write_wav(&dir.music().join("02.wav"), 2, len);
    let id = scan(&dir);
    // WAV files carry no tags here; put both on one album as tracks 1 and 2.
    {
        let conn = open_writer(&dir.db()).unwrap();
        conn.execute(
            "INSERT INTO album (title, title_sort) VALUES ('Live', 'live')",
            [],
        )
        .unwrap();
        let album = conn.last_insert_rowid();
        for (name, no) in [("01.wav", 1), ("02.wav", 2)] {
            conn.execute(
                "UPDATE track SET album_id = ?1, track_no = ?2 WHERE id = ?3",
                rusqlite::params![album, no, id(name)],
            )
            .unwrap();
        }
    }

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start(&dir, Some(capture.clone()));
    engine.send(Command::SetCrossfade { ms: 500 });
    engine.send(Command::Enqueue {
        track_ids: vec![id("01.wav"), id("02.wav")],
        at: None,
    });
    engine.send(Command::Play);
    play_to_the_end(&engine);
    engine.shutdown();

    let out = captured_from_first_sound(&capture);
    let len = len as usize;
    assert_eq!(out[len - 1], f32::from(sample(1, len as u32 - 1)) / 32768.0);
    assert_eq!(out[len], f32::from(sample(2, 0)) / 32768.0, "no overlap");
    assert!(out[2 * len..].iter().all(|&s| s == 0.0));
}

#[test]
fn normalization_applies_the_measured_loudness() {
    let dir = TempDir::new("loudness");
    let len = 2 * RATE;
    write_wav(&dir.music().join("loud.wav"), 1, len);
    let id = scan(&dir);
    let track = id("loud.wav");

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start_with(&dir, Some(capture.clone()));
    engine.send(Command::SetCrossfade { ms: 0 });
    engine.send(Command::Enqueue {
        track_ids: vec![track],
        at: None,
    });

    // The analyzer measures queued tracks first; wait for the stored result.
    let conn = open_writer(&dir.db()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let (lufs, peak) = loop {
        let row: Option<(Option<f64>, Option<f64>)> = conn
            .query_row(
                "SELECT loudness_lufs, true_peak_db FROM analysis WHERE track_id = ?1",
                [track],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some(r) = row {
            break r;
        }
        assert!(Instant::now() < deadline, "analysis did not finish");
        std::thread::sleep(Duration::from_millis(50));
    };
    let lufs = lufs.expect("measured loudness");

    engine.send(Command::Play);
    play_to_the_end(&engine);
    engine.shutdown();

    // The test ramp is mostly infrasound, which K-weighting barely counts: it measures
    // quiet, and the true peak limits how far it may be turned up.
    let gain_db = dap_analysis::normalization_gain_db(Some(lufs), peak);
    assert!(
        lufs < -18.0 && gain_db.abs() > 0.1,
        "{lufs} LUFS, {gain_db} dB"
    );
    let gain = 10f32.powf(gain_db as f32 / 20.0);
    let out = captured_from_first_sound(&capture);
    let i = 10_000;
    let want = f32::from(sample(1, i as u32)) / 32768.0 * gain;
    assert!((out[i] - want).abs() < 1e-6, "{} vs {want}", out[i]);
}

fn next_scan(engine: &Engine) -> dap_engine::ScanSummary {
    let events = wait_for(engine, |e| matches!(e, Event::ScanFinished(_)));
    match events.last() {
        Some(Event::ScanFinished(s)) => s.clone(),
        _ => unreachable!(),
    }
}

#[test]
fn watched_folders_are_rescanned_on_start_and_on_change() {
    let dir = TempDir::new("watch");
    write_wav(&dir.music().join("old.wav"), 1, 4410);
    let _ = scan(&dir);
    // Added while the app was not running: found by the scan on start.
    write_wav(&dir.music().join("offline.wav"), 2, 4410);

    let engine = Engine::start(EngineConfig {
        db_path: dir.db(),
        output: OutputConfig::Null {
            sample_rate: RATE,
            channels: 1,
            speed: 8.0,
            capture: None,
        },
        watch_folders: true,
    })
    .unwrap();
    let s = next_scan(&engine);
    assert_eq!((s.added, s.unchanged), (1, 1), "{s:?}");

    // Changes while running: picked up after the folder has been quiet for a moment.
    let started = Instant::now();
    std::fs::create_dir_all(dir.music().join("new album")).unwrap();
    write_wav(&dir.music().join("new album/01.wav"), 3, 4410);
    write_wav(&dir.music().join("new album/02.wav"), 4, 4410);
    let s = next_scan(&engine);
    assert_eq!(s.added, 2, "{s:?}");
    assert!(started.elapsed() >= dap_engine::watcher::QUIET);

    std::fs::remove_file(dir.music().join("old.wav")).unwrap();
    let s = next_scan(&engine);
    assert_eq!(s.missing, 1, "{s:?}");

    // Files the library ignores do not trigger a scan.
    std::fs::write(dir.music().join("notes.txt"), b"hello").unwrap();
    let quiet_until = Instant::now() + dap_engine::watcher::QUIET + Duration::from_secs(2);
    while Instant::now() < quiet_until {
        if let Ok(e) = engine.events().recv_timeout(Duration::from_millis(100)) {
            assert!(!matches!(e, Event::ScanFinished(_)), "unexpected scan");
        }
    }
    engine.shutdown();
}

fn first_settings(engine: &Engine) -> dap_engine::PlayerSettings {
    match wait_for(engine, |e| matches!(e, Event::Settings(_))).last() {
        Some(Event::Settings(s)) => *s,
        _ => unreachable!(),
    }
}

#[test]
fn volume_and_equalizer_survive_a_restart_but_ducking_does_not() {
    let dir = TempDir::new("settings");
    let engine = start_with(&dir, None);
    let defaults = first_settings(&engine);
    assert_eq!((defaults.volume, defaults.eq_enabled), (1.0, false));

    let mut gains = [0.0f32; 10];
    gains[0] = 6.0;
    gains[9] = -20.0; // clamped to -12
    engine.send(Command::SetVolume(0.4));
    engine.send(Command::Duck(true));
    engine.send(Command::SetEq {
        gains_db: gains,
        preamp_db: -3.0,
    });
    engine.send(Command::SetEqEnabled(true));
    engine.send(Command::SetUiSetting {
        key: "theme".into(),
        value: "light".into(),
    });
    wait_for(&engine, |e| matches!(e, Event::Settings(s) if s.eq_enabled));
    engine.shutdown();

    let engine = start_with(&dir, None);
    let s = first_settings(&engine);
    engine.shutdown();
    assert_eq!(s.volume, 0.4, "the user volume, not the ducked one");
    assert!(s.eq_enabled);
    assert_eq!(s.eq_gains_db[0], 6.0);
    assert_eq!(s.eq_gains_db[9], -12.0);
    assert_eq!(s.eq_preamp_db, -3.0);
    let conn = open_writer(&dir.db()).unwrap();
    let theme: String = conn
        .query_row(
            "SELECT value FROM setting WHERE key = 'ui.theme'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(theme, "light");
}

#[test]
fn removing_a_folder_drops_its_tracks_from_the_queue() {
    let dir = TempDir::new("remove-folder");
    write_wav(&dir.music().join("a.wav"), 1, 4410);
    let id = scan(&dir);
    let engine = start(&dir, None);
    engine.send(Command::Enqueue {
        track_ids: vec![id("a.wav")],
        at: None,
    });
    wait_for(&engine, |e| matches!(e, Event::Queue(q) if q.len() == 1));
    let conn = open_writer(&dir.db()).unwrap();
    let root: i64 = conn
        .query_row("SELECT id FROM library_root", [], |r| r.get(0))
        .unwrap();
    drop(conn);

    engine.send(Command::RemoveFolder(root));
    wait_for(&engine, |e| matches!(e, Event::Queue(q) if q.is_empty()));
    wait_for(&engine, |e| matches!(e, Event::LibraryChanged));
    engine.shutdown();

    let conn = open_writer(&dir.db()).unwrap();
    let tracks: i64 = conn
        .query_row("SELECT count(*) FROM track", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tracks, 0);
}

#[test]
fn analysis_detects_keys_but_keeps_keys_from_tags() {
    let dir = TempDir::new("keys");
    write_wav(&dir.music().join("tagged.wav"), 1, 2 * RATE);
    write_wav(&dir.music().join("plain.wav"), 2, 2 * RATE);
    let id = scan(&dir);
    {
        // As the scanner stores a key read from tags.
        let conn = open_writer(&dir.db()).unwrap();
        conn.execute(
            "INSERT INTO analysis (track_id, algo_version, key_camelot, key_confidence, analyzed_at)
             VALUES (?1, 0, '3B', 1.0, 0)",
            [id("tagged.wav")],
        )
        .unwrap();
    }
    let engine = start(&dir, None);
    engine.send(Command::Enqueue {
        track_ids: vec![id("tagged.wav"), id("plain.wav")],
        at: None,
    });
    let conn = open_writer(&dir.db()).unwrap();
    let key_of = |name: &str| -> Option<(Option<String>, i64)> {
        conn.query_row(
            "SELECT key_camelot, algo_version FROM analysis WHERE track_id = ?1",
            [id(name)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while ![key_of("tagged.wav"), key_of("plain.wav")].iter().all(|k| {
        k.as_ref()
            .is_some_and(|(_, v)| *v >= dap_engine::analyzer::ANALYSIS_ALGO)
    }) {
        assert!(Instant::now() < deadline, "analysis did not finish");
        std::thread::sleep(Duration::from_millis(50));
    }
    engine.shutdown();
    assert_eq!(
        key_of("tagged.wav").unwrap().0.as_deref(),
        Some("3B"),
        "tag key kept"
    );
    let detected = key_of("plain.wav").unwrap().0;
    assert!(
        detected
            .as_deref()
            .is_some_and(|k| dap_analysis::Camelot::parse(k).is_some()),
        "{detected:?}"
    );
}

#[test]
fn upcoming_items_can_be_reordered_and_the_order_persists() {
    let dir = TempDir::new("reorder");
    for name in ["a", "b", "c"] {
        write_wav(&dir.music().join(format!("{name}.wav")), 1, 4410);
    }
    let id = scan(&dir);
    let engine = start(&dir, None);
    engine.send(Command::Enqueue {
        track_ids: vec![id("a.wav"), id("b.wav"), id("c.wav")],
        at: None,
    });
    let queued = match wait_for(&engine, |e| matches!(e, Event::Queue(q) if q.len() == 3)).last() {
        Some(Event::Queue(q)) => q.clone(),
        _ => unreachable!(),
    };
    let ids: Vec<i64> = queued.iter().map(|e| e.id).collect();

    // A list that is not exactly the upcoming items is refused.
    engine.send(Command::ReorderUpcoming(vec![ids[0], ids[1]]));
    wait_for(&engine, |e| matches!(e, Event::Error(_)));

    let wanted = vec![ids[2], ids[0], ids[1]];
    engine.send(Command::ReorderUpcoming(wanted.clone()));
    wait_for(
        &engine,
        |e| matches!(e, Event::Queue(q) if q.iter().map(|x| x.id).collect::<Vec<_>>() == wanted),
    );
    engine.shutdown();

    let engine = start(&dir, None);
    let restored = match wait_for(&engine, |e| matches!(e, Event::Queue(_))).last() {
        Some(Event::Queue(q)) => q.iter().map(|x| x.id).collect::<Vec<_>>(),
        _ => unreachable!(),
    };
    engine.shutdown();
    assert_eq!(restored, wanted);
}

/// A mono WAV of 2 ms 2 kHz clicks at `beats` (seconds), silent otherwise.
fn write_clicks(path: &Path, seconds: f64, beats: impl Iterator<Item = f64>) {
    let frames = (seconds * f64::from(RATE)) as usize;
    let mut pcm = vec![0i16; frames];
    let click = (RATE / 500) as usize;
    for t in beats {
        let at = (t * f64::from(RATE)).round() as usize;
        for i in 0..click.min(frames.saturating_sub(at)) {
            let phase = 2.0 * std::f64::consts::PI * 2000.0 * i as f64 / f64::from(RATE);
            // Starts at a non-zero sample, so the click onset is exact.
            pcm[at + i] = (20_000.0 * (phase + 0.5).sin()) as i16;
        }
    }
    let data_len = (frames * 2) as u32;
    let mut f = std::fs::File::create(path).unwrap();
    let mut w = |b: &[u8]| f.write_all(b).unwrap();
    w(b"RIFF");
    w(&(36 + data_len).to_le_bytes());
    w(b"WAVEfmt ");
    w(&16u32.to_le_bytes());
    w(&1u16.to_le_bytes());
    w(&1u16.to_le_bytes());
    w(&RATE.to_le_bytes());
    w(&(RATE * 2).to_le_bytes());
    w(&2u16.to_le_bytes());
    w(&16u16.to_le_bytes());
    w(b"data");
    w(&data_len.to_le_bytes());
    w(&pcm.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<_>>());
}

#[test]
fn beatmatch_lands_the_incoming_beats_on_the_outgoing_grid() {
    let dir = TempDir::new("beatmatch");
    // a: 120 BPM, first beat 0.25 s; clicks only in its first 2 s so that later the
    // incoming track can be heard alone. b: 125 BPM, first beat 0.4 s.
    write_clicks(
        &dir.music().join("a.wav"),
        20.0,
        (0..4).map(|k| 0.25 + 0.5 * k as f64),
    );
    write_clicks(
        &dir.music().join("b.wav"),
        20.0,
        (0..40).map(|k| 0.4 + 0.48 * k as f64),
    );
    let id = scan(&dir);
    {
        let conn = open_writer(&dir.db()).unwrap();
        for (name, bpm, first) in [("a.wav", 120.0, 250), ("b.wav", 125.0, 400)] {
            conn.execute(
                "INSERT INTO analysis (track_id, algo_version, bpm, bpm_confidence,
                                       first_beat_ms, analyzed_at)
                 VALUES (?1, ?2, ?3, 1.0, ?4, 0)",
                rusqlite::params![id(name), dap_engine::analyzer::ANALYSIS_ALGO, bpm, first],
            )
            .unwrap();
        }
    }

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start(&dir, Some(capture.clone()));
    engine.send(Command::SetCrossfade { ms: 500 });
    engine.send(Command::Enqueue {
        track_ids: vec![id("a.wav"), id("b.wav")],
        at: None,
    });
    engine.send(Command::Play);
    let events = play_to_the_end(&engine);
    engine.shutdown();

    // The UI is told what comes: a beatmatch on a's beat 16 (8.25 s into the file).
    let announced = events.iter().find_map(|e| match e {
        Event::State(s) => s.transition.filter(|t| t.kind == TransitionKind::Beatmatch),
        _ => None,
    });
    assert_eq!(announced.map(|t| t.start_ms), Some(8250), "{announced:?}");

    // Index 0 = a's first beat. The transition starts on a's 16-beat phrase boundary
    // that leaves room for 16 beats before a's end: beat 16 (8 s after the first beat).
    let out = captured_from_first_sound(&capture);
    let beat = f64::from(RATE) / 2.0;
    let tolerance = f64::from(RATE) * 0.005;
    // The incoming fade starts from silence: check from its third beat on.
    for j in 2..16 {
        let expected = (16 + j) as f64 * beat;
        let (lo, hi) = (
            (expected - 0.4 * beat) as usize,
            (expected + 0.4 * beat) as usize,
        );
        let peak = lo
            + out[lo..hi]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0;
        assert!(
            (peak as f64 - expected).abs() < tolerance,
            "beat {j}: click at {peak}, expected {expected} ({:.1} ms off)",
            (peak as f64 - expected) / f64::from(RATE) * 1000.0
        );
        assert!(out[peak].abs() > 0.05, "beat {j}: no click ({})", out[peak]);
    }
}

#[test]
fn crossfade_skips_trailing_and_leading_silence() {
    let dir = TempDir::new("trim");
    write_wav(&dir.music().join("a.wav"), 1, 5 * RATE);
    write_wav(&dir.music().join("b.wav"), 2, 4 * RATE);
    let id = scan(&dir);
    {
        // As if the analysis found: a's sound ends at 3 s, b's starts at 1 s.
        let conn = open_writer(&dir.db()).unwrap();
        for (name, start, end) in [("a.wav", 0, 3000), ("b.wav", 1000, 4000)] {
            conn.execute(
                "INSERT INTO analysis (track_id, algo_version, audio_start_ms, audio_end_ms,
                                       analyzed_at)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                rusqlite::params![id(name), dap_engine::analyzer::ANALYSIS_ALGO, start, end],
            )
            .unwrap();
        }
    }

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start(&dir, Some(capture.clone()));
    engine.send(Command::SetCrossfade { ms: 500 });
    engine.send(Command::Enqueue {
        track_ids: vec![id("a.wav"), id("b.wav")],
        at: None,
    });
    engine.send(Command::Play);
    play_to_the_end(&engine);
    engine.shutdown();

    let out = captured_from_first_sound(&capture);
    let r = RATE as usize;
    let a = |i: usize| f32::from(sample(1, i as u32)) / 32768.0;
    let b = |i: usize| f32::from(sample(2, i as u32)) / 32768.0;
    // The fade runs from 2.5 s to a's end of sound at 3 s; b enters at its first sound.
    assert_eq!(out[2 * r], a(2 * r));
    let mid = 2 * r + r / 2 + r / 4;
    let t = std::f64::consts::FRAC_PI_4;
    let want = a(mid) * t.cos() as f32 + b(r + r / 4) * t.sin() as f32;
    assert!((out[mid] - want).abs() < 1e-5, "{} vs {want}", out[mid]);
    assert_eq!(out[3 * r], b(r + r / 2), "b alone after a's end of sound");
    let total = 2 * r + r / 2 + 3 * r;
    assert_eq!(out[total - 1], b(4 * r - 1), "b plays to its last frame");
    assert!(out[total..].iter().all(|&s| s == 0.0));
}

/// Peak position (frame) in `out` within ±40% of a beat around `expected`.
fn peak_near(out: &[f32], expected: f64, beat: f64) -> usize {
    let (lo, hi) = (
        (expected - 0.4 * beat) as usize,
        (expected + 0.4 * beat) as usize,
    );
    lo + out[lo..hi]
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0
}

#[test]
fn clashing_tempo_and_key_echo_out_and_the_next_track_lands_on_the_beat() {
    let dir = TempDir::new("echo");
    // a: 120 BPM in 8A, clicks on every beat. b: 140 BPM in 3B (far apart), first beat
    // at 0.8 s.
    write_clicks(
        &dir.music().join("a.wav"),
        20.0,
        (0..40).map(|k| 0.25 + 0.5 * k as f64),
    );
    let b_beat = 60.0 / 140.0;
    write_clicks(
        &dir.music().join("b.wav"),
        20.0,
        (0..40).map(|k| 0.8 + b_beat * k as f64),
    );
    let id = scan(&dir);
    {
        let conn = open_writer(&dir.db()).unwrap();
        for (name, bpm, first, key) in [("a.wav", 120.0, 250, "8A"), ("b.wav", 140.0, 800, "3B")] {
            conn.execute(
                "INSERT INTO analysis (track_id, algo_version, bpm, bpm_confidence,
                                       first_beat_ms, key_camelot, key_confidence, analyzed_at)
                 VALUES (?1, ?2, ?3, 1.0, ?4, ?5, 1.0, 0)",
                rusqlite::params![
                    id(name),
                    dap_engine::analyzer::ANALYSIS_ALGO,
                    bpm,
                    first,
                    key
                ],
            )
            .unwrap();
        }
    }

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start(&dir, Some(capture.clone()));
    engine.send(Command::SetCrossfade { ms: 500 });
    engine.send(Command::Enqueue {
        track_ids: vec![id("a.wav"), id("b.wav")],
        at: None,
    });
    engine.send(Command::Play);
    play_to_the_end(&engine);
    engine.shutdown();

    // Index 0 = a's first beat. The echo (1 beat + 4 beats of echoes) starts on the last
    // bar line that leaves room: a's beat 32. b's first beat follows one beat later.
    let out = captured_from_first_sound(&capture);
    let rate = f64::from(RATE);
    let beat = rate / 2.0;
    let tolerance = rate * 0.005;
    let first_b = 33.0 * beat;
    // Off a's grid, so neither a nor its echoes are there: b's second and third beats.
    for j in 1..3 {
        let expected = first_b + j as f64 * b_beat * rate;
        let peak = peak_near(&out, expected, b_beat * rate);
        assert!(
            (peak as f64 - expected).abs() < tolerance && out[peak].abs() > 0.3,
            "b beat {j}: click at {peak} ({}), expected {expected}",
            out[peak]
        );
    }
    // a's dry clicks stop after its last beat: at beat 35 only a fading echo is left.
    let echo = out[peak_near(&out, 35.0 * beat, b_beat * rate / 2.0)].abs();
    let dry = out[peak_near(&out, 30.0 * beat, b_beat * rate / 2.0)].abs();
    assert!(echo < dry * 0.6, "echo {echo} vs dry {dry}");
}

#[test]
fn two_energetic_tracks_of_different_tempo_cut_on_the_phrase() {
    let dir = TempDir::new("cut");
    write_clicks(
        &dir.music().join("a.wav"),
        20.0,
        (0..40).map(|k| 0.25 + 0.5 * k as f64),
    );
    let b_beat = 60.0 / 140.0;
    write_clicks(
        &dir.music().join("b.wav"),
        20.0,
        (0..40).map(|k| 0.8 + b_beat * k as f64),
    );
    let id = scan(&dir);
    {
        // Loud masters at 120 and 140 BPM: both energetic, too far apart to beatmatch.
        let conn = open_writer(&dir.db()).unwrap();
        for (name, bpm, first) in [("a.wav", 120.0, 250), ("b.wav", 140.0, 800)] {
            conn.execute(
                "INSERT INTO analysis (track_id, algo_version, bpm, bpm_confidence,
                                       first_beat_ms, loudness_lufs, analyzed_at)
                 VALUES (?1, ?2, ?3, 1.0, ?4, -7.0, 0)",
                rusqlite::params![id(name), dap_engine::analyzer::ANALYSIS_ALGO, bpm, first],
            )
            .unwrap();
        }
    }

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start(&dir, Some(capture.clone()));
    engine.send(Command::SetCrossfade { ms: 500 });
    engine.send(Command::Enqueue {
        track_ids: vec![id("a.wav"), id("b.wav")],
        at: None,
    });
    engine.send(Command::Play);
    let events = play_to_the_end(&engine);
    engine.shutdown();

    // The cut comes on a's last phrase boundary: beat 32, 16.25 s into the file.
    let announced = events.iter().find_map(|e| match e {
        Event::State(s) => s.transition.filter(|t| t.kind == TransitionKind::HardCut),
        _ => None,
    });
    assert_eq!(announced.map(|t| t.start_ms), Some(16_250), "{announced:?}");

    // Index 0 = a's first beat; b's first beat lands right on the cut.
    let out = captured_from_first_sound(&capture);
    let rate = f64::from(RATE);
    let cut = 32.0 * rate / 2.0;
    let tolerance = rate * 0.005;
    for j in 1..3 {
        let expected = cut + j as f64 * b_beat * rate;
        let peak = peak_near(&out, expected, b_beat * rate);
        assert!(
            (peak as f64 - expected).abs() < tolerance && out[peak].abs() > 0.3,
            "b beat {j}: click at {peak} ({}), expected {expected}",
            out[peak]
        );
    }
    // a is gone after the cut: nothing on its beat 34.
    let at_a = out[peak_near(&out, 34.0 * rate / 2.0, b_beat * rate / 2.0)].abs();
    assert!(at_a < 0.01, "a still playing: {at_a}");
}

#[test]
fn a_chosen_transition_wins_over_the_settings_and_persists() {
    let dir = TempDir::new("override");
    let len = 2 * RATE;
    write_wav(&dir.music().join("a.wav"), 1, len);
    write_wav(&dir.music().join("b.wav"), 2, len);
    let id = scan(&dir);

    let capture = Arc::new(Mutex::new(Vec::new()));
    let engine = start(&dir, Some(capture.clone()));
    engine.send(Command::SetCrossfade { ms: 500 });
    // Gapless for this pair although crossfade is on.
    engine.send(Command::SetTransitionOverride {
        from_track: id("a.wav"),
        to_track: id("b.wav"),
        kind: Some(TransitionKind::Gapless),
    });
    engine.send(Command::Enqueue {
        track_ids: vec![id("a.wav"), id("b.wav")],
        at: None,
    });
    engine.send(Command::Play);
    let events = play_to_the_end(&engine);
    engine.shutdown();

    let announced = events.iter().find_map(|e| match e {
        Event::State(s) => s.transition,
        _ => None,
    });
    assert!(
        announced.is_some_and(|t| t.kind == TransitionKind::Gapless && t.forced),
        "{announced:?}"
    );
    let out = captured_from_first_sound(&capture);
    let len = len as usize;
    assert_eq!(out[len - 1], f32::from(sample(1, len as u32 - 1)) / 32768.0);
    assert_eq!(out[len], f32::from(sample(2, 0)) / 32768.0, "no overlap");

    let conn = open_writer(&dir.db()).unwrap();
    let kind: String = conn
        .query_row("SELECT kind FROM transition_override", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kind, "gapless");
}

#[test]
fn a_mix_mode_sets_its_profile_and_survives_a_restart() {
    use dap_engine::MixMode;
    let dir = TempDir::new("mixmode");
    let _ = scan(&dir);
    // Before modes existed: crossfade off means the Album mode.
    {
        let conn = open_writer(&dir.db()).unwrap();
        conn.execute("INSERT INTO setting (key, value) VALUES ('player.crossfade_ms', '0')", [])
            .unwrap();
    }
    let engine = start_with(&dir, None);
    let s = first_settings(&engine);
    assert_eq!((s.mix_mode, s.crossfade_ms), (MixMode::Album, 0));

    engine.send(Command::SetMixMode(MixMode::Chill));
    let s = first_settings(&engine);
    assert_eq!(
        (s.mix_mode, s.crossfade_ms, s.smart_transitions),
        (MixMode::Chill, 10_000, true)
    );
    engine.shutdown();

    let engine = start_with(&dir, None);
    let s = first_settings(&engine);
    assert_eq!((s.mix_mode, s.crossfade_ms), (MixMode::Chill, 10_000));
    engine.send(Command::SetMixMode(MixMode::Radio));
    let s = first_settings(&engine);
    assert_eq!(
        (s.mix_mode, s.crossfade_ms, s.smart_transitions),
        (MixMode::Radio, 6000, false)
    );
    engine.shutdown();
}

#[test]
fn the_chosen_output_device_is_stored() {
    let dir = TempDir::new("device");
    let _ = scan(&dir);
    let engine = start(&dir, None);
    engine.send(Command::SetOutputDevice(Some("Sluchátka".into())));
    engine.shutdown();
    let conn = open_writer(&dir.db()).unwrap();
    let stored: String = conn
        .query_row("SELECT value FROM setting WHERE key = 'player.output_device'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, "Sluchátka");
}

#[test]
#[ignore = "plays (silence) on the machine's real sound devices"]
fn switching_the_output_device_keeps_playing() {
    let dir = TempDir::new("switch");
    write_clicks(&dir.music().join("quiet.wav"), 20.0, std::iter::empty());
    let id = scan(&dir);
    let engine = Engine::start(EngineConfig {
        db_path: dir.db(),
        output: OutputConfig::Device,
        watch_folders: false,
    })
    .unwrap();
    engine.send(Command::Enqueue {
        track_ids: vec![id("quiet.wav")],
        at: None,
    });
    engine.send(Command::Play);
    wait_for(&engine, |e| matches!(e, Event::State(s) if s.status == Status::Playing));
    std::thread::sleep(Duration::from_millis(1500));
    for name in dap_engine::output_devices().into_iter().map(Some).chain([None]) {
        engine.send(Command::SetOutputDevice(name.clone()));
        let events = wait_for(&engine, |e| matches!(e, Event::State(s) if s.status == Status::Playing));
        assert!(
            !events.iter().any(|e| matches!(e, Event::Error(_))),
            "{name:?}: {events:?}"
        );
        let s = last_state(&events);
        assert!(s.position_ms >= 1000, "{name:?} resumed at {}", s.position_ms);
        std::thread::sleep(Duration::from_millis(500));
    }
    engine.shutdown();
}
