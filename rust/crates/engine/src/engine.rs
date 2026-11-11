//! The control thread: owns the queue and player state, drives the mixer and the deck
//! decoder threads, and talks to the UI through commands and events.
//!
//! Position is not streamed: [`PlayerState`] carries position + timestamp + rate and is
//! sent only when something changes; the UI extrapolates.

use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use dap_dsp::eq::BAND_COUNT;
use dap_library::catalog::now_ms;
use dap_library::db::open_reader;
use dap_mixer::{MixerCommand, MixerControl, MixerEvent};
use rusqlite::{Connection, OptionalExtension, params};

use crate::db_writer::DbWriter;
use crate::deck::{DeckFailure, spawn_deck};
use crate::output::{self, Output, OutputConfig};
use crate::queue::{Queue, QueueEntry};

/// "Previous" restarts the current track when it has played longer than this.
const RESTART_THRESHOLD_MS: u64 = 3000;
const TICK: Duration = Duration::from_millis(10);

const SETTING_CURRENT: &str = "player.current_queue_item";
const SETTING_POSITION: &str = "player.position_ms";

pub struct EngineConfig {
    pub db_path: PathBuf,
    pub output: OutputConfig,
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
    SetVolume(f32),
    SetEq {
        gains_db: [f32; BAND_COUNT],
        preamp_db: f32,
    },
    SetEqEnabled(bool),
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

#[derive(Debug, Clone)]
pub enum Event {
    State(PlayerState),
    Queue(Vec<QueueEntry>),
    /// A track could not be played and was skipped.
    TrackFailed {
        track_id: i64,
        error: String,
    },
    Error(String),
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
    failure: DeckFailure,
    duration_ms: Option<u64>,
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
        };
        control.emit(Event::Queue(control.queue.entries()));
        control.emit_state();
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
        }
        if self.status == Status::Playing {
            self.status = Status::Paused;
        }
        self.save_state();
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

    fn load(&mut self, entry: QueueEntry, start_ms: u64) -> (Loaded, dap_mixer::DeckInput) {
        let token = self.next_token;
        self.next_token += 1;
        let (path, duration_ms) = self.track_file(entry.track_id);
        let (input, failure) = spawn_deck(token, path, start_ms, self.sample_rate, self.channels);
        (
            Loaded {
                token,
                entry,
                failure,
                duration_ms,
            },
            input,
        )
    }

    /// Makes queue item `index` current, starting at `start_ms`.
    fn play_index(&mut self, index: usize, start_ms: u64, autoplay: bool) {
        let Some(entry) = self.queue.get(index) else {
            return;
        };
        let (loaded, input) = self.load(entry, start_ms);
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
                let (loaded, input) = self.load(entry, 0);
                self.next = Some(loaded);
                self.send_mixer(MixerCommand::SetNext(input));
            }
            _ => {
                if self.next.take().is_some() {
                    self.send_mixer(MixerCommand::ClearNext);
                }
            }
        }
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
            Command::SetVolume(v) => self.send_mixer(MixerCommand::SetVolume(v)),
            Command::SetEq {
                gains_db,
                preamp_db,
            } => self.send_mixer(MixerCommand::SetEq {
                gains_db,
                preamp_db,
            }),
            Command::SetEqEnabled(on) => self.send_mixer(MixerCommand::SetEqEnabled(on)),
            Command::Shutdown => {}
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
        match ended.failure.get() {
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
            // Gapless switch done by the mixer.
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

fn read_setting(conn: &Connection, key: &str) -> Option<i64> {
    conn.query_row("SELECT value FROM setting WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse().ok())
}
