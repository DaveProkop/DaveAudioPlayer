//! The control thread: owns the queue and player state, drives the mixer and the deck
//! decoder threads, and talks to the UI through commands and events.
//!
//! Position is not streamed: [`PlayerState`] carries position + timestamp + rate and is
//! sent only when something changes; the UI extrapolates.
//!
//! Transitions: with crossfade on, the control thread plans a [`TransitionPlan`] for the
//! current deck as soon as its exact length is known; consecutive tracks of one album stay
//! gapless. Loudness normalization sets each deck's gain from the stored EBU R128 analysis.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use dap_analysis::normalization_gain_db;
use dap_dsp::gain::db_to_gain;
use dap_dsp::eq::{BAND_COUNT, MAX_GAIN_DB};
use dap_library::catalog::now_ms;
use dap_library::db::open_reader;
use dap_library::{DbError, ScanJob, ScanReport, add_root};
use dap_mixer::{MixerCommand, MixerControl, MixerEvent, TransitionPlan};
use rusqlite::{Connection, OptionalExtension, params};

use crate::analyzer::{Analyzer, stored_loudness};
use crate::db_writer::DbWriter;
use crate::watcher::FolderWatcher;
use crate::deck::{DeckInfo, spawn_deck};
use crate::output::{self, Output, OutputConfig};
use crate::queue::{Queue, QueueEntry};

/// "Previous" restarts the current track when it has played longer than this.
const RESTART_THRESHOLD_MS: u64 = 3000;
const TICK: Duration = Duration::from_millis(10);

const SETTING_CURRENT: &str = "player.current_queue_item";
const SETTING_POSITION: &str = "player.position_ms";
const SETTING_CROSSFADE: &str = "player.crossfade_ms";
const SETTING_NORMALIZE: &str = "player.normalize";
const SETTING_VOLUME: &str = "player.volume";
const SETTING_EQ_ENABLED: &str = "eq.enabled";
const SETTING_EQ_GAINS: &str = "eq.gains_db";
const SETTING_EQ_PREAMP: &str = "eq.preamp_db";
/// Output level while another app speaks (navigation, notification).
const DUCK_GAIN: f32 = 0.3;

pub const DEFAULT_CROSSFADE_MS: u32 = 6000;
pub const MAX_CROSSFADE_MS: u32 = 12_000;
/// Shorter fades than this are not worth it: the tracks just follow each other.
const MIN_FADE_MS: u64 = 10;

pub struct EngineConfig {
    pub db_path: PathBuf,
    pub output: OutputConfig,
    /// Rescan the library on start and whenever the music folders change.
    pub watch_folders: bool,
}

#[derive(Debug, Clone)]
pub enum Command {
    Play,
    Pause,
    TogglePlayPause,
    Next,
    Previous,
    Seek {
        position_ms: u64,
    },
    /// Starts playing the queue item at `index`.
    PlayIndex(usize),
    /// Inserts tracks before `at` (`None` = append).
    Enqueue {
        track_ids: Vec<i64>,
        at: Option<usize>,
    },
    MoveQueueItem {
        from: usize,
        to: usize,
    },
    RemoveQueueItem(usize),
    ClearQueue,
    /// Linear volume 0.0–1.0 (stored).
    SetVolume(f32),
    /// Temporarily lowers the output (audio focus ducking); not stored.
    Duck(bool),
    SetEq {
        gains_db: [f32; BAND_COUNT],
        preamp_db: f32,
    },
    SetEqEnabled(bool),
    /// Crossfade length between tracks; 0 = gapless.
    SetCrossfade {
        ms: u32,
    },
    /// Volume normalization to a common loudness (EBU R128).
    SetNormalization(bool),
    /// Stores a free-form UI preference (key without prefix; saved as `ui.<key>`).
    SetUiSetting {
        key: String,
        value: String,
    },
    /// Registers a music folder and rescans the library.
    AddFolder(PathBuf),
    /// Rescans every registered folder.
    RescanLibrary,
    CancelScan,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone)]
pub struct PlayerState {
    pub status: Status,
    /// Index of the current item in the queue.
    pub index: Option<usize>,
    pub entry: Option<QueueEntry>,
    /// Position at `at`; while playing it advances at `rate`.
    pub position_ms: u64,
    pub at: Instant,
    pub rate: f64,
    pub duration_ms: Option<u64>,
}

/// Playback preferences, stored in the database.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerSettings {
    pub crossfade_ms: u32,
    pub normalize: bool,
    /// Linear 0.0–1.0.
    pub volume: f32,
    pub eq_enabled: bool,
    pub eq_gains_db: [f32; BAND_COUNT],
    pub eq_preamp_db: f32,
}

#[derive(Debug, Clone)]
pub enum Event {
    State(PlayerState),
    Settings(PlayerSettings),
    Queue(Vec<QueueEntry>),
    /// A track could not be played and was skipped.
    TrackFailed {
        track_id: i64,
        error: String,
    },
    Error(String),
    ScanProgress {
        folder: PathBuf,
        processed: usize,
        total: usize,
    },
    /// A library scan finished; library views should refresh.
    ScanFinished(ScanSummary),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub moved: usize,
    pub missing: usize,
    pub failed: usize,
    pub cancelled: bool,
}

impl ScanSummary {
    fn add(&mut self, r: &ScanReport) {
        self.added += r.added;
        self.updated += r.updated;
        self.unchanged += r.unchanged;
        self.moved += r.moved;
        self.missing += r.missing;
        self.failed += r.failed.len();
        self.cancelled |= r.cancelled;
    }
}

/// Handle to a running engine. Dropping it shuts the engine down.
pub struct Engine {
    tx: Sender<Command>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start(config: EngineConfig) -> Result<Self, String> {
        let (tx, rx) = unbounded();
        let (event_tx, events) = unbounded();
        let (ready_tx, ready_rx) = bounded(1);
        let thread = std::thread::Builder::new()
            .name("engine-control".into())
            .spawn(move || match Control::new(config, event_tx) {
                Ok(mut control) => {
                    let _ = ready_tx.send(Ok(()));
                    control.run(rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                tx,
                events,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("engine thread died during startup".into()),
        }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// A command sender for other threads (OS media controls and the like).
    pub fn commands(&self) -> Sender<Command> {
        self.tx.clone()
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Saves the player state and stops all threads.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = self.tx.send(Command::Shutdown);
            let _ = t.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A track loaded into a deck.
struct Loaded {
    token: u64,
    entry: QueueEntry,
    info: Arc<DeckInfo>,
    duration_ms: Option<u64>,
    /// Normalization gain in dB (applied only while normalization is on).
    gain_db: f64,
    album: Option<AlbumPos>,
    /// Follows the previous deck on the same album: gapless, and the same gain.
    continues_album: bool,
}

/// Where a track sits on its album.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AlbumPos {
    album_id: i64,
    disc: i64,
    track: i64,
}

impl AlbumPos {
    fn is_followed_by(self, next: AlbumPos) -> bool {
        self.album_id == next.album_id
            && ((next.disc == self.disc && next.track == self.track + 1)
                || (next.disc == self.disc + 1 && next.track == 1))
    }
}

struct Control {
    db: DbWriter,
    db_thread: Option<JoinHandle<()>>,
    reader: Connection,
    queue: Queue,
    mixer: MixerControl,
    output: Option<Output>,
    sample_rate: u32,
    channels: usize,
    events: Sender<Event>,
    status: Status,
    current: Option<Loaded>,
    next: Option<Loaded>,
    /// The current queue item, also when nothing is loaded (stopped, restored).
    cursor: Option<i64>,
    position_frames: u64,
    /// Where to start the cursor item when nothing is loaded (restored state).
    resume_ms: u64,
    next_token: u64,
    scan: ScanState,
    analyzer: Option<Analyzer>,
    watcher: Option<FolderWatcher>,
    /// Where scans store cover images.
    covers: PathBuf,
    settings: PlayerSettings,
    ducked: bool,
    /// The transition plan last sent to the mixer.
    plan_sent: Option<TransitionPlan>,
}

/// Which library roots a scan covers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum ScanScope {
    #[default]
    Nothing,
    Roots(BTreeSet<i64>),
    All,
}

impl ScanScope {
    fn merge(&mut self, other: ScanScope) {
        *self = match (std::mem::take(self), other) {
            (ScanScope::All, _) | (_, ScanScope::All) => ScanScope::All,
            (ScanScope::Nothing, o) | (o, ScanScope::Nothing) => o,
            (ScanScope::Roots(mut a), ScanScope::Roots(b)) => {
                a.extend(b);
                ScanScope::Roots(a)
            }
        };
    }

    fn includes(&self, root_id: i64) -> bool {
        match self {
            ScanScope::Nothing => false,
            ScanScope::Roots(r) => r.contains(&root_id),
            ScanScope::All => true,
        }
    }
}

/// Scan requests waiting for the scan thread. Requests arriving during a scan are merged
/// and run as one more pass.
#[derive(Default)]
struct ScanQueue {
    scope: ScanScope,
    /// Every merged request came from the app itself (start, folder watching), not the
    /// user: an unreachable folder is then not reported as an error.
    automatic: bool,
    running: bool,
}

#[derive(Default)]
struct ScanState {
    queue: Arc<Mutex<ScanQueue>>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Control {
    fn new(config: EngineConfig, events: Sender<Event>) -> Result<Self, String> {
        let (db, db_thread) = DbWriter::open(&config.db_path).map_err(|e| e.to_string())?;
        let reader = open_reader(&config.db_path).map_err(|e| e.to_string())?;
        let queue = Queue::load(&reader).map_err(|e| e.to_string())?;
        let error_tx = events.clone();
        let started = output::start(&config.output, move |e| {
            let _ = error_tx.send(Event::Error(format!("audio output: {e}")));
        })?;

        let saved_cursor =
            read_setting(&reader, SETTING_CURRENT).filter(|&id| queue.index_of(id).is_some());
        let resume_ms = saved_cursor
            .and(read_setting(&reader, SETTING_POSITION))
            .unwrap_or(0)
            .max(0) as u64;
        let settings = PlayerSettings {
            crossfade_ms: read_setting(&reader, SETTING_CROSSFADE)
                .map_or(DEFAULT_CROSSFADE_MS, |v| v.clamp(0, i64::from(MAX_CROSSFADE_MS)) as u32),
            normalize: read_setting(&reader, SETTING_NORMALIZE).is_none_or(|v| v != 0),
            volume: read_setting_str(&reader, SETTING_VOLUME)
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map_or(1.0, |v| v.clamp(0.0, 1.0)),
            eq_enabled: read_setting(&reader, SETTING_EQ_ENABLED).is_some_and(|v| v != 0),
            eq_gains_db: read_setting_str(&reader, SETTING_EQ_GAINS)
                .map(|v| parse_gains(&v))
                .unwrap_or_default(),
            eq_preamp_db: read_setting_str(&reader, SETTING_EQ_PREAMP)
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map_or(0.0, |v| v.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)),
        };
        // Without analysis playback still works, just without measured loudness.
        let analyzer = match Analyzer::start(&config.db_path, db.clone()) {
            Ok(a) => Some(a),
            Err(e) => {
                let _ = events.send(Event::Error(format!("loudness analysis: {e}")));
                None
            }
        };

        let control = Self {
            db,
            db_thread: Some(db_thread),
            reader,
            queue,
            mixer: started.control,
            output: Some(started.output),
            sample_rate: started.sample_rate,
            channels: started.channels,
            events,
            status: Status::Stopped,
            current: None,
            next: None,
            cursor: saved_cursor,
            position_frames: 0,
            resume_ms,
            next_token: 1,
            scan: ScanState::default(),
            analyzer,
            watcher: None,
            covers: dap_library::covers::covers_dir(&config.db_path),
            settings,
            ducked: false,
            plan_sent: None,
        };
        let mut control = control;
        // The mixer starts at defaults; bring it to the stored settings.
        let s = control.settings;
        control.send_mixer(MixerCommand::SetVolume(s.volume));
        control.send_mixer(MixerCommand::SetEq {
            gains_db: s.eq_gains_db,
            preamp_db: s.eq_preamp_db,
        });
        control.send_mixer(MixerCommand::SetEqEnabled(s.eq_enabled));
        control.emit(Event::Settings(control.settings));
        control.emit(Event::Queue(control.queue.entries()));
        control.emit_state();
        control.prioritize_analysis();
        if config.watch_folders {
            // Watching starts before the scan, so nothing changed meanwhile is missed.
            match FolderWatcher::start(&config.db_path, control.covers.clone()) {
                Ok(w) => control.watcher = Some(w),
                Err(e) => control.emit(Event::Error(e)),
            }
            let has_roots = control
                .reader
                .query_row("SELECT EXISTS (SELECT 1 FROM library_root)", [], |r| {
                    r.get::<_, bool>(0)
                })
                .unwrap_or(false);
            if has_roots {
                control.start_scan(ScanScope::All, true);
            }
        }
        Ok(control)
    }

    fn run(&mut self, commands: Receiver<Command>) {
        loop {
            match commands.recv_timeout(TICK) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.drain_mixer_events();
            self.drain_analysis();
            self.drain_folder_changes();
            self.update_transition();
        }
        if self.status == Status::Playing {
            self.status = Status::Paused;
        }
        self.save_state();
        self.scan.cancel.store(true, Ordering::Relaxed);
        if let Some(t) = self.scan.thread.take() {
            let _ = t.join(); // it holds a writer handle
        }
        self.watcher = None;
        self.analyzer = None; // stops and joins; it writes through the writer
        self.output = None; // stop audio before the decks go away
        self.current = None;
        self.next = None;
        // Closing the writer channel ends the writer thread after pending jobs.
        let db_thread = self.db_thread.take();
        self.db = DbWriter::closed();
        if let Some(t) = db_thread {
            let _ = t.join();
        }
    }

    // ----- helpers -------------------------------------------------------------------

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn position_ms(&self) -> u64 {
        if self.current.is_some() {
            self.position_frames * 1000 / u64::from(self.sample_rate)
        } else {
            self.resume_ms
        }
    }

    fn cursor_index(&self) -> Option<usize> {
        self.cursor.and_then(|id| self.queue.index_of(id))
    }

    fn emit_state(&self) {
        let index = self.cursor_index();
        let duration_ms = match &self.current {
            Some(l) => l.duration_ms,
            None => index
                .and_then(|i| self.queue.get(i))
                .and_then(|e| self.track_file(e.track_id).1),
        };
        self.emit(Event::State(PlayerState {
            status: self.status,
            index,
            entry: index.and_then(|i| self.queue.get(i)),
            position_ms: self.position_ms(),
            at: Instant::now(),
            rate: if self.status == Status::Playing {
                1.0
            } else {
                0.0
            },
            duration_ms,
        }));
    }

    fn send_mixer(&mut self, mut cmd: MixerCommand) {
        // The audio thread drains the queue every few ms; a full queue is transient.
        for _ in 0..200 {
            match self.mixer.send(cmd) {
                Ok(()) => return,
                Err(c) => {
                    cmd = c;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        self.emit(Event::Error("audio thread is not responding".into()));
    }

    /// Absolute path and duration of a track's file; path is `None` when it is missing.
    fn track_file(&self, track_id: i64) -> (Option<PathBuf>, Option<u64>) {
        let row: Option<(String, String, Option<i64>, bool)> = self
            .reader
            .query_row(
                "SELECT r.path, f.rel_path, f.duration_ms, f.missing
                 FROM track t JOIN file f ON f.id = t.file_id JOIN library_root r ON r.id = f.root_id
                 WHERE t.id = ?1",
                [track_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .ok()
            .flatten();
        match row {
            Some((root, rel, duration, missing)) => {
                let path = (!missing).then(|| PathBuf::from(root).join(rel));
                (path, duration.map(|d| d.max(0) as u64))
            }
            None => (None, None),
        }
    }

    fn album_pos(&self, track_id: i64) -> Option<AlbumPos> {
        self.reader
            .query_row(
                "SELECT album_id, coalesce(disc_no, 1), track_no FROM track
                 WHERE id = ?1 AND album_id IS NOT NULL AND track_no IS NOT NULL",
                [track_id],
                |r| {
                    Ok(AlbumPos {
                        album_id: r.get(0)?,
                        disc: r.get(1)?,
                        track: r.get(2)?,
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Normalization gain from the stored analysis (a typical master when not analyzed).
    fn track_gain_db(&self, track_id: i64) -> f64 {
        let l = stored_loudness(&self.reader, track_id).unwrap_or_default();
        normalization_gain_db(l.integrated_lufs, l.true_peak_db)
    }

    /// The linear gain a deck plays at.
    fn deck_gain(&self, gain_db: f64) -> f32 {
        if self.settings.normalize {
            db_to_gain(gain_db as f32)
        } else {
            1.0
        }
    }

    /// Loads a queue entry into a new deck. `after` is the deck it will follow, if any: the
    /// next track of the same album keeps that deck's gain so the album plays as mastered.
    fn load(
        &mut self,
        entry: QueueEntry,
        start_ms: u64,
        after: Option<&Loaded>,
    ) -> (Loaded, dap_mixer::DeckInput) {
        let token = self.next_token;
        self.next_token += 1;
        let (path, duration_ms) = self.track_file(entry.track_id);
        let album = self.album_pos(entry.track_id);
        let continues_album = match (after.and_then(|a| a.album), album) {
            (Some(prev), Some(next)) => prev.is_followed_by(next),
            _ => false,
        };
        let gain_db = match after {
            Some(prev) if continues_album => prev.gain_db,
            _ => self.track_gain_db(entry.track_id),
        };
        let (input, info) = spawn_deck(
            token,
            path,
            start_ms,
            self.deck_gain(gain_db),
            self.sample_rate,
            self.channels,
        );
        (
            Loaded {
                token,
                entry,
                info,
                duration_ms,
                gain_db,
                album,
                continues_album,
            },
            input,
        )
    }

    /// Makes queue item `index` current, starting at `start_ms`.
    fn play_index(&mut self, index: usize, start_ms: u64, autoplay: bool) {
        let Some(entry) = self.queue.get(index) else {
            return;
        };
        let (loaded, input) = self.load(entry, start_ms, None);
        self.cursor = Some(entry.id);
        self.position_frames = input.start_frame;
        self.resume_ms = 0;
        self.current = Some(loaded);
        self.send_mixer(MixerCommand::SetCurrent(input));
        self.preload_next();
        if autoplay {
            self.send_mixer(MixerCommand::Play);
            self.status = Status::Playing;
        } else if self.status == Status::Stopped {
            self.status = Status::Paused;
        }
        self.emit_state();
    }

    /// Ensures the deck after the current item holds the right track (gapless preload).
    fn preload_next(&mut self) {
        let want = self.cursor_index().and_then(|i| self.queue.get(i + 1));
        let have = self.next.as_ref().map(|l| l.entry.id);
        if want.map(|e| e.id) == have {
            return;
        }
        match want {
            Some(entry) if self.current.is_some() => {
                let current = self.current.take();
                let (loaded, input) = self.load(entry, 0, current.as_ref());
                self.current = current;
                self.next = Some(loaded);
                self.send_mixer(MixerCommand::SetNext(input));
            }
            _ => {
                if self.next.take().is_some() {
                    self.send_mixer(MixerCommand::ClearNext);
                }
            }
        }
        self.prioritize_analysis();
    }

    /// The upcoming tracks are measured first, so they play normalized.
    fn prioritize_analysis(&self) {
        let Some(analyzer) = &self.analyzer else {
            return;
        };
        let start = self.cursor_index().unwrap_or(0);
        let mut ids: Vec<i64> = (start + 1..start + 21)
            .filter_map(|i| self.queue.get(i))
            .map(|e| e.track_id)
            .collect();
        if let Some(e) = self.queue.get(start) {
            ids.push(e.track_id);
        }
        analyzer.prioritize(ids);
    }

    /// The crossfade the mixer should do out of the current deck, if any.
    fn wanted_plan(&self) -> Option<TransitionPlan> {
        if self.settings.crossfade_ms == 0 {
            return None;
        }
        let current = self.current.as_ref()?;
        let next = self.next.as_ref()?;
        if next.continues_album || current.info.failure.get().is_some() {
            return None;
        }
        let end = current.info.end_frame()?;
        let rate = u64::from(self.sample_rate);
        // Short tracks fade for at most half their length.
        let mut len = (u64::from(self.settings.crossfade_ms) * rate / 1000).min(end / 2);
        if let Some(next_end) = next.info.end_frame() {
            len = len.min(next_end / 2);
        }
        if len < MIN_FADE_MS * rate / 1000 {
            return None;
        }
        Some(TransitionPlan {
            token: current.token,
            start_frame: end - len,
            end_frame: end,
        })
    }

    fn update_transition(&mut self) {
        let want = self.wanted_plan();
        if want != self.plan_sent {
            self.plan_sent = want;
            self.send_mixer(match want {
                Some(plan) => MixerCommand::SetTransition(plan),
                None => MixerCommand::ClearTransition,
            });
        }
    }

    /// A freshly analyzed track that is loaded but not playing yet gets its real gain.
    fn drain_analysis(&mut self) {
        let Some(analyzer) = &self.analyzer else {
            return;
        };
        let done: Vec<i64> = analyzer.done.try_iter().collect();
        for track_id in done {
            let Some(next) = self.next.as_ref() else {
                continue;
            };
            if next.entry.track_id != track_id || next.continues_album {
                continue;
            }
            let gain_db = self.track_gain_db(track_id);
            let gain = self.deck_gain(gain_db);
            let token = next.token;
            if let Some(next) = self.next.as_mut() {
                next.gain_db = gain_db;
            }
            self.send_mixer(MixerCommand::SetDeckGain { token, gain });
        }
    }

    /// Changed library folders are rescanned (incrementally).
    fn drain_folder_changes(&mut self) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        let mut roots = BTreeSet::new();
        for changed in watcher.changed.try_iter() {
            roots.extend(changed);
        }
        if !roots.is_empty() {
            self.start_scan(ScanScope::Roots(roots), true);
        }
    }

    fn apply_volume(&mut self) {
        let v = self.settings.volume * if self.ducked { DUCK_GAIN } else { 1.0 };
        self.send_mixer(MixerCommand::SetVolume(v));
    }

    fn save_setting(&self, key: &'static str, value: String) {
        self.db.run(move |c| {
            let _ = c.execute(
                "INSERT INTO setting (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = ?2",
                params![key, value],
            );
        });
    }

    fn set_normalization(&mut self, on: bool) {
        if self.settings.normalize == on {
            return;
        }
        self.settings.normalize = on;
        self.save_setting(SETTING_NORMALIZE, i64::from(on).to_string());
        let decks: Vec<(u64, f64)> = [self.current.as_ref(), self.next.as_ref()]
            .into_iter()
            .flatten()
            .map(|l| (l.token, l.gain_db))
            .collect();
        for (token, gain_db) in decks {
            let gain = self.deck_gain(gain_db);
            self.send_mixer(MixerCommand::SetDeckGain { token, gain });
        }
        self.emit(Event::Settings(self.settings));
    }

    fn set_crossfade(&mut self, ms: u32) {
        let ms = ms.min(MAX_CROSSFADE_MS);
        if self.settings.crossfade_ms == ms {
            return;
        }
        self.settings.crossfade_ms = ms;
        self.save_setting(SETTING_CROSSFADE, ms.to_string());
        self.emit(Event::Settings(self.settings));
        // The plan follows on the next tick.
    }

    fn stop(&mut self) {
        self.send_mixer(MixerCommand::Pause);
        self.send_mixer(MixerCommand::ClearCurrent);
        self.send_mixer(MixerCommand::ClearNext);
        self.current = None;
        self.next = None;
        self.status = Status::Stopped;
        self.resume_ms = 0;
        self.position_frames = 0;
    }

    fn save_state(&self) {
        let cursor = self.cursor;
        let position = self.position_ms() as i64;
        self.db.run(move |c| {
            let set = |k: &str, v: String| {
                let _ = c.execute(
                    "INSERT INTO setting (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = ?2",
                    params![k, v],
                );
            };
            set(
                SETTING_CURRENT,
                cursor.map_or("null".into(), |id| id.to_string()),
            );
            set(SETTING_POSITION, position.to_string());
        });
    }

    fn record_play(&self, loaded: &Loaded) {
        let track_id = loaded.entry.track_id;
        let played = loaded.duration_ms.unwrap_or(0) as i64;
        let device = if cfg!(target_os = "android") {
            "android"
        } else {
            "pc"
        };
        self.db.run(move |c| {
            let now = now_ms();
            let _ = c.execute(
                "UPDATE track SET play_count = play_count + 1, last_played = ?2 WHERE id = ?1",
                params![track_id, now],
            );
            let _ = c.execute(
                "INSERT INTO play_history (track_id, started_at, played_ms, completed, skipped, device)
                 VALUES (?1, ?2, ?3, 1, 0, ?4)",
                params![track_id, now - played, played, device],
            );
        });
    }

    // ----- commands ------------------------------------------------------------------

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::TogglePlayPause => {
                if self.status == Status::Playing {
                    self.pause()
                } else {
                    self.play()
                }
            }
            Command::Next => match self.cursor_index() {
                Some(i) if i + 1 < self.queue.len() => {
                    self.play_index(i + 1, 0, self.status == Status::Playing)
                }
                _ => {
                    self.stop();
                    self.emit_state();
                }
            },
            Command::Previous => match self.cursor_index() {
                Some(i) if i > 0 && self.position_ms() <= RESTART_THRESHOLD_MS => {
                    self.play_index(i - 1, 0, self.status == Status::Playing)
                }
                Some(i) => self.play_index(i, 0, self.status == Status::Playing),
                None => {}
            },
            Command::Seek { position_ms } => {
                if let Some(i) = self.cursor_index() {
                    if self.current.is_some() {
                        self.play_index(i, position_ms, self.status == Status::Playing);
                    } else {
                        self.resume_ms = position_ms;
                        self.emit_state();
                    }
                }
            }
            Command::PlayIndex(i) => self.play_index(i, 0, true),
            Command::Enqueue { track_ids, at } => {
                let at = at.unwrap_or(self.queue.len());
                if let Err(e) = self.queue.insert(&self.db, at, &track_ids, "user") {
                    self.emit(Event::Error(e.to_string()));
                }
                self.queue_changed();
            }
            Command::MoveQueueItem { from, to } => {
                if let Err(e) = self.queue.move_item(&self.db, from, to) {
                    self.emit(Event::Error(e.to_string()));
                }
                self.queue_changed();
            }
            Command::RemoveQueueItem(index) => {
                let removing_current = self.cursor_index() == Some(index);
                if removing_current {
                    let playing = self.status == Status::Playing;
                    let had_deck = self.current.is_some();
                    self.queue.remove(&self.db, index);
                    if index < self.queue.len() && had_deck {
                        self.play_index(index, 0, playing);
                    } else {
                        self.stop();
                        self.cursor = self.queue.get(index).map(|e| e.id);
                    }
                } else {
                    self.queue.remove(&self.db, index);
                }
                self.queue_changed();
            }
            Command::ClearQueue => {
                self.stop();
                self.cursor = None;
                self.queue.clear(&self.db);
                self.queue_changed();
            }
            Command::SetVolume(v) => {
                let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 };
                self.settings.volume = v;
                self.apply_volume();
                self.save_setting(SETTING_VOLUME, v.to_string());
                self.emit(Event::Settings(self.settings));
            }
            Command::Duck(on) => {
                self.ducked = on;
                self.apply_volume();
            }
            Command::SetEq {
                gains_db,
                preamp_db,
            } => {
                let clamp = |g: f32| {
                    if g.is_finite() {
                        g.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
                    } else {
                        0.0
                    }
                };
                let gains_db = gains_db.map(clamp);
                let preamp_db = clamp(preamp_db);
                self.settings.eq_gains_db = gains_db;
                self.settings.eq_preamp_db = preamp_db;
                self.send_mixer(MixerCommand::SetEq {
                    gains_db,
                    preamp_db,
                });
                let list: Vec<String> = gains_db.iter().map(|g| g.to_string()).collect();
                self.save_setting(SETTING_EQ_GAINS, list.join(","));
                self.save_setting(SETTING_EQ_PREAMP, preamp_db.to_string());
                self.emit(Event::Settings(self.settings));
            }
            Command::SetEqEnabled(on) => {
                self.settings.eq_enabled = on;
                self.send_mixer(MixerCommand::SetEqEnabled(on));
                self.save_setting(SETTING_EQ_ENABLED, i64::from(on).to_string());
                self.emit(Event::Settings(self.settings));
            }
            Command::SetUiSetting { key, value } => {
                let key = format!("ui.{key}");
                self.db.run(move |c| {
                    let _ = c.execute(
                        "INSERT INTO setting (key, value) VALUES (?1, ?2)
                         ON CONFLICT (key) DO UPDATE SET value = ?2",
                        params![key, value],
                    );
                });
            }
            Command::SetCrossfade { ms } => self.set_crossfade(ms),
            Command::SetNormalization(on) => self.set_normalization(on),
            Command::AddFolder(path) => {
                let result = self.db.call(move |c| add_root(c, &path));
                match result {
                    Some(Ok(_)) => {
                        if let Some(w) = &self.watcher {
                            w.refresh();
                        }
                        self.start_scan(ScanScope::All, false)
                    }
                    Some(Err(e)) => self.emit(Event::Error(e.to_string())),
                    None => {}
                }
            }
            Command::RescanLibrary => self.start_scan(ScanScope::All, false),
            Command::CancelScan => self.scan.cancel.store(true, Ordering::Relaxed),
            Command::Shutdown => {}
        }
    }

    /// Scans folders on a coordinator thread; each batch runs as its own writer job.
    /// A request during a running scan is merged into one more pass.
    fn start_scan(&mut self, scope: ScanScope, automatic: bool) {
        {
            let Ok(mut q) = self.scan.queue.lock() else {
                return;
            };
            q.automatic = if q.scope == ScanScope::Nothing {
                automatic
            } else {
                q.automatic && automatic
            };
            q.scope.merge(scope);
            if q.running {
                return;
            }
            q.running = true;
        }
        if let Some(t) = self.scan.thread.take() {
            let _ = t.join();
        }
        self.scan.cancel.store(false, Ordering::Relaxed);
        let db = self.db.clone();
        let events = self.events.clone();
        let covers = self.covers.clone();
        let (queue, cancel) = (self.scan.queue.clone(), self.scan.cancel.clone());
        let spawned = std::thread::Builder::new()
            .name("library-scan".into())
            .spawn(move || {
                loop {
                    let (scope, automatic) = {
                        let Ok(mut q) = queue.lock() else { return };
                        if q.scope == ScanScope::Nothing || cancel.load(Ordering::Relaxed) {
                            q.scope = ScanScope::Nothing;
                            q.running = false;
                            return;
                        }
                        (std::mem::take(&mut q.scope), q.automatic)
                    };
                    let summary = scan_roots(&db, &events, &cancel, &covers, &scope, automatic);
                    let _ = events.send(Event::ScanFinished(summary));
                }
            });
        match spawned {
            Ok(t) => self.scan.thread = Some(t),
            Err(e) => {
                if let Ok(mut q) = self.scan.queue.lock() {
                    q.scope = ScanScope::Nothing;
                    q.running = false;
                }
                self.emit(Event::Error(format!("cannot start scan: {e}")));
            }
        }
    }

    fn play(&mut self) {
        if self.status == Status::Playing || self.queue.is_empty() {
            return;
        }
        if self.current.is_some() {
            self.send_mixer(MixerCommand::Play);
            self.status = Status::Playing;
            self.emit_state();
        } else {
            let index = self.cursor_index().unwrap_or(0);
            let start = self.resume_ms;
            self.play_index(index, start, true);
        }
    }

    fn pause(&mut self) {
        if self.status != Status::Playing {
            return;
        }
        self.send_mixer(MixerCommand::Pause);
        self.status = Status::Paused;
        self.save_state();
        self.emit_state();
    }

    fn queue_changed(&mut self) {
        if self.current.is_some() {
            self.preload_next();
        }
        self.emit(Event::Queue(self.queue.entries()));
        self.emit_state();
        self.prioritize_analysis();
    }

    // ----- mixer events --------------------------------------------------------------

    fn drain_mixer_events(&mut self) {
        while let Some(event) = self.mixer.try_event() {
            match event {
                MixerEvent::Position { token, frames } => {
                    if self.current.as_ref().is_some_and(|l| l.token == token) {
                        self.position_frames = frames;
                    }
                }
                MixerEvent::Ended { token } => self.on_ended(token),
                MixerEvent::Started { token } => self.on_started(token),
                MixerEvent::Idle => {
                    // With a next deck on its way the mixer will pick it up.
                    if self.current.is_none()
                        && self.next.is_none()
                        && self.status == Status::Playing
                    {
                        self.status = Status::Stopped;
                        self.emit_state();
                    }
                }
                MixerEvent::Underrun { .. } => {}
                MixerEvent::Trash(deck) => drop(deck),
            }
        }
    }

    fn on_ended(&mut self, token: u64) {
        if !self.current.as_ref().is_some_and(|l| l.token == token) {
            return;
        }
        let ended = self.current.take().expect("current");
        match ended.info.failure.get() {
            Some(error) => self.emit(Event::TrackFailed {
                track_id: ended.entry.track_id,
                error: error.clone(),
            }),
            None => self.record_play(&ended),
        }
        if self.next.is_none() {
            // End of the queue: stay on the last item, ready to play again from the start.
            self.send_mixer(MixerCommand::Pause);
            self.status = Status::Stopped;
            self.position_frames = 0;
            self.resume_ms = 0;
            self.save_state();
            self.emit_state();
        }
    }

    fn on_started(&mut self, token: u64) {
        if self.next.as_ref().is_some_and(|l| l.token == token) {
            // Gapless switch or the start of a crossfade, done by the mixer. In a crossfade
            // the previous track is still fading out: it counts as played now, and its own
            // end event (no longer current) is ignored.
            if let Some(previous) = self.current.take()
                && previous.info.failure.get().is_none()
            {
                self.record_play(&previous);
            }
            let next = self.next.take().expect("next");
            self.cursor = Some(next.entry.id);
            self.position_frames = 0;
            self.current = Some(next);
            self.preload_next();
            self.save_state();
            self.emit_state();
        }
    }
}

fn read_setting_str(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM setting WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .ok()
    .flatten()
}

fn read_setting(conn: &Connection, key: &str) -> Option<i64> {
    read_setting_str(conn, key).and_then(|v| v.trim().parse().ok())
}

/// Stored EQ gains ("1.5,0,-2,…"); missing or bad values count as 0 dB.
fn parse_gains(v: &str) -> [f32; BAND_COUNT] {
    let mut gains = [0.0; BAND_COUNT];
    for (g, part) in gains.iter_mut().zip(v.split(',')) {
        if let Ok(x) = part.trim().parse::<f32>()
            && x.is_finite()
        {
            *g = x.clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
        }
    }
    gains
}

fn scan_roots(
    db: &DbWriter,
    events: &Sender<Event>,
    cancel: &Arc<AtomicBool>,
    covers: &std::path::Path,
    scope: &ScanScope,
    automatic: bool,
) -> ScanSummary {
    let mut summary = ScanSummary::default();
    let roots: Vec<(i64, String)> = db
        .call(|c| {
            c.prepare("SELECT id, path FROM library_root WHERE enabled = 1 ORDER BY id")
                .and_then(|mut st| {
                    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    for (root_id, path) in roots.into_iter().filter(|(id, _)| scope.includes(*id)) {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        let folder = PathBuf::from(&path);
        // Walking the folder is file-system work only; it stays off the writer thread.
        let mut job = match ScanJob::walk(root_id, folder.clone()) {
            Ok(j) => j.with_covers(covers.to_path_buf()),
            // An unplugged drive is normal when the app scans on its own.
            Err(DbError::RootUnavailable(_)) if automatic => continue,
            Err(e) => {
                let _ = events.send(Event::Error(e.to_string()));
                continue;
            }
        };
        loop {
            let c = cancel.clone();
            let Some((returned, result)) = db.call(move |conn| {
                let r = job.step(conn, &c, |_| {});
                (job, r)
            }) else {
                return summary; // writer gone: shutting down
            };
            job = returned;
            let (processed, total) = job.progress();
            let _ = events.send(Event::ScanProgress {
                folder: folder.clone(),
                processed,
                total,
            });
            match result {
                Ok(false) => continue,
                Ok(true) => break,
                Err(e) => {
                    let _ = events.send(Event::Error(e.to_string()));
                    break;
                }
            }
        }
        summary.add(job.report());
    }
    summary
}
