//! End-to-end engine tests with the null output: real files, real decoder threads,
//! real mixer, audio captured instead of played.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dap_engine::{
    Command, Engine, EngineConfig, Event, OutputConfig, PlayerState, QueueEntry, Status,
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
    let paused = last_state(&wait_for(
        &engine,
        |e| matches!(e, Event::State(s) if s.status == Status::Paused),
    ));
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
    let start = out.iter().position(|&s| s != 0.0).expect("audio was played");
    out[start..].to_vec()
}

fn play_to_the_end(engine: &Engine) {
    let mut started_playing = false;
    wait_for(engine, |e| match e {
        Event::State(s) if s.status == Status::Playing => {
            started_playing = true;
            false
        }
        Event::State(s) => started_playing && s.status == Status::Stopped,
        _ => false,
    });
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
        conn.execute("INSERT INTO album (title, title_sort) VALUES ('Live', 'live')", [])
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
    assert!(lufs < -18.0 && gain_db.abs() > 0.1, "{lufs} LUFS, {gain_db} dB");
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
        .query_row("SELECT value FROM setting WHERE key = 'ui.theme'", [], |r| r.get(0))
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
