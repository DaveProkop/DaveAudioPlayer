//! Engine lifecycle, player and queue commands, and the event stream.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use dap_dsp::eq::BAND_COUNT;
use dap_engine::{Command, Engine, EngineConfig, Event, OutputConfig, PlayerState, Status};
use flutter_rust_bridge::frb;

use crate::frb_generated::StreamSink;

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
pub(crate) static DB_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

/// Opens the library database (creating it if needed) and starts the audio engine on the
/// default output device. Calling it again while running does nothing.
pub fn start_engine(db_path: String) -> Result<()> {
    let mut guard = ENGINE.lock().map_err(|_| anyhow!("engine lock poisoned"))?;
    if guard.is_some() {
        return Ok(());
    }
    let path = PathBuf::from(&db_path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let engine = Engine::start(EngineConfig {
        db_path: path.clone(),
        output: OutputConfig::Device,
    })
    .map_err(|e| anyhow!(e))?;
    *guard = Some(engine);
    *DB_PATH.lock().map_err(|_| anyhow!("lock poisoned"))? = Some(path);
    Ok(())
}

/// Saves the player state and stops all engine threads.
pub fn stop_engine() {
    if let Ok(mut guard) = ENGINE.lock() {
        if let Some(engine) = guard.take() {
            engine.shutdown();
        }
    }
    super::library::close_reader();
    if let Ok(mut path) = DB_PATH.lock() {
        *path = None;
    }
}

fn send(cmd: Command) -> Result<()> {
    let guard = ENGINE.lock().map_err(|_| anyhow!("engine lock poisoned"))?;
    guard
        .as_ref()
        .ok_or_else(|| anyhow!("engine is not running"))?
        .send(cmd);
    Ok(())
}

// ----- player ----------------------------------------------------------------------

pub fn play() -> Result<()> {
    send(Command::Play)
}

pub fn pause() -> Result<()> {
    send(Command::Pause)
}

pub fn toggle_play_pause() -> Result<()> {
    send(Command::TogglePlayPause)
}

pub fn next() -> Result<()> {
    send(Command::Next)
}

pub fn previous() -> Result<()> {
    send(Command::Previous)
}

pub fn seek(position_ms: i64) -> Result<()> {
    send(Command::Seek {
        position_ms: position_ms.max(0) as u64,
    })
}

/// Linear volume 0.0–1.0.
pub fn set_volume(volume: f64) -> Result<()> {
    send(Command::SetVolume(volume as f32))
}

/// Ten band gains in dB (31 Hz … 16 kHz), each clamped to ±12 dB.
pub fn set_eq(gains_db: Vec<f64>, preamp_db: f64) -> Result<()> {
    let mut gains = [0.0f32; BAND_COUNT];
    for (dst, src) in gains.iter_mut().zip(&gains_db) {
        *dst = *src as f32;
    }
    send(Command::SetEq {
        gains_db: gains,
        preamp_db: preamp_db as f32,
    })
}

pub fn set_eq_enabled(enabled: bool) -> Result<()> {
    send(Command::SetEqEnabled(enabled))
}

// ----- queue -----------------------------------------------------------------------

pub fn play_index(index: u32) -> Result<()> {
    send(Command::PlayIndex(index as usize))
}

/// Inserts tracks before `at` (`None` = append).
pub fn enqueue(track_ids: Vec<i64>, at: Option<u32>) -> Result<()> {
    send(Command::Enqueue {
        track_ids,
        at: at.map(|a| a as usize),
    })
}

pub fn move_queue_item(from: u32, to: u32) -> Result<()> {
    send(Command::MoveQueueItem {
        from: from as usize,
        to: to as usize,
    })
}

pub fn remove_queue_item(index: u32) -> Result<()> {
    send(Command::RemoveQueueItem(index as usize))
}

pub fn clear_queue() -> Result<()> {
    send(Command::ClearQueue)
}

// ----- library scanning --------------------------------------------------------------

pub fn add_folder(path: String) -> Result<()> {
    send(Command::AddFolder(PathBuf::from(path)))
}

pub fn rescan_library() -> Result<()> {
    send(Command::RescanLibrary)
}

pub fn cancel_scan() -> Result<()> {
    send(Command::CancelScan)
}

// ----- events ----------------------------------------------------------------------

pub enum PlayStatus {
    Stopped,
    Playing,
    Paused,
}

/// Player state at a point in time. The UI computes the live position as
/// `position_ms + (now - at_epoch_ms) * rate`; no per-frame updates cross the bridge.
pub struct PlayerStateDto {
    pub status: PlayStatus,
    pub index: Option<u32>,
    pub queue_item_id: Option<i64>,
    pub track_id: Option<i64>,
    pub position_ms: i64,
    pub at_epoch_ms: i64,
    pub rate: f64,
    pub duration_ms: Option<i64>,
}

pub struct QueueItemDto {
    pub id: i64,
    pub track_id: i64,
}

pub struct ScanSummaryDto {
    pub added: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub moved: u32,
    pub missing: u32,
    pub failed: u32,
    pub cancelled: bool,
}

pub enum AppEventKind {
    State,
    Queue,
    TrackFailed,
    Error,
    ScanProgress,
    ScanFinished,
}

/// One event from the engine; `kind` says which fields are set.
pub struct AppEvent {
    pub kind: AppEventKind,
    pub state: Option<PlayerStateDto>,
    pub queue: Option<Vec<QueueItemDto>>,
    pub track_id: Option<i64>,
    pub message: Option<String>,
    pub scan_folder: Option<String>,
    pub scan_processed: Option<u32>,
    pub scan_total: Option<u32>,
    pub scan_summary: Option<ScanSummaryDto>,
}

impl AppEvent {
    fn new(kind: AppEventKind) -> Self {
        Self {
            kind,
            state: None,
            queue: None,
            track_id: None,
            message: None,
            scan_folder: None,
            scan_processed: None,
            scan_total: None,
            scan_summary: None,
        }
    }
}

fn epoch_ms(at: Instant) -> i64 {
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    now_epoch - Instant::now().saturating_duration_since(at).as_millis() as i64
}

fn state_dto(s: PlayerState) -> PlayerStateDto {
    PlayerStateDto {
        status: match s.status {
            Status::Stopped => PlayStatus::Stopped,
            Status::Playing => PlayStatus::Playing,
            Status::Paused => PlayStatus::Paused,
        },
        index: s.index.map(|i| i as u32),
        queue_item_id: s.entry.map(|e| e.id),
        track_id: s.entry.map(|e| e.track_id),
        position_ms: s.position_ms as i64,
        at_epoch_ms: epoch_ms(s.at),
        rate: s.rate,
        duration_ms: s.duration_ms.map(|d| d as i64),
    }
}

fn to_dto(event: Event) -> AppEvent {
    match event {
        Event::State(s) => AppEvent {
            state: Some(state_dto(s)),
            ..AppEvent::new(AppEventKind::State)
        },
        Event::Queue(q) => AppEvent {
            queue: Some(
                q.into_iter()
                    .map(|e| QueueItemDto {
                        id: e.id,
                        track_id: e.track_id,
                    })
                    .collect(),
            ),
            ..AppEvent::new(AppEventKind::Queue)
        },
        Event::TrackFailed { track_id, error } => AppEvent {
            track_id: Some(track_id),
            message: Some(error),
            ..AppEvent::new(AppEventKind::TrackFailed)
        },
        Event::Error(e) => AppEvent {
            message: Some(e),
            ..AppEvent::new(AppEventKind::Error)
        },
        Event::ScanProgress {
            folder,
            processed,
            total,
        } => AppEvent {
            scan_folder: Some(folder.to_string_lossy().into_owned()),
            scan_processed: Some(processed as u32),
            scan_total: Some(total as u32),
            ..AppEvent::new(AppEventKind::ScanProgress)
        },
        Event::ScanFinished(s) => AppEvent {
            scan_summary: Some(ScanSummaryDto {
                added: s.added as u32,
                updated: s.updated as u32,
                unchanged: s.unchanged as u32,
                moved: s.moved as u32,
                missing: s.missing as u32,
                failed: s.failed as u32,
                cancelled: s.cancelled,
            }),
            ..AppEvent::new(AppEventKind::ScanFinished)
        },
    }
}

/// Streams engine events to Dart until the engine stops. Subscribe once, right after
/// [`start_engine`]; the engine sends the current queue and state on start.
pub fn app_events(sink: StreamSink<AppEvent>) -> Result<()> {
    let events = {
        let guard = ENGINE.lock().map_err(|_| anyhow!("engine lock poisoned"))?;
        guard
            .as_ref()
            .ok_or_else(|| anyhow!("engine is not running"))?
            .events()
            .clone()
    };
    std::thread::Builder::new()
        .name("dart-events".into())
        .spawn(move || {
            for event in events {
                if sink.add(to_dto(event)).is_err() {
                    break; // Dart side closed the stream
                }
            }
        })?;
    Ok(())
}
