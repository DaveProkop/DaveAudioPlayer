//! Loudness analysis thread (`analysis`): decodes tracks and stores their EBU R128
//! loudness and true peak in the `analysis` table.
//!
//! Tracks the control thread asks for (the queue ahead of the cursor) go first. On desktop
//! the rest of the library follows in the background; on Android only queued tracks are
//! analyzed, to spare the battery.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use dap_analysis::LoudnessMeter;
use dap_audio_io::Decoder;
use dap_library::catalog::now_ms;
use dap_library::db::open_reader;
use rusqlite::{Connection, OptionalExtension, params};

use crate::db_writer::DbWriter;

/// Bumped when the loudness measurement changes; older rows are measured again.
pub const LOUDNESS_ALGO: i64 = 1;
/// How often an idle analyzer looks for new tracks (after a scan, for example).
const IDLE_POLL: Duration = Duration::from_secs(10);
const BACKGROUND: bool = !cfg!(target_os = "android");

/// Loudness of a track as stored, `None` values when unknown.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrackLoudness {
    pub integrated_lufs: Option<f64>,
    pub true_peak_db: Option<f64>,
}

/// Stored loudness of a track; `None` when it has not been analyzed (by this algorithm).
pub fn stored_loudness(conn: &Connection, track_id: i64) -> Option<TrackLoudness> {
    conn.query_row(
        "SELECT loudness_lufs, true_peak_db FROM analysis
         WHERE track_id = ?1 AND algo_version >= ?2",
        params![track_id, LOUDNESS_ALGO],
        |r| {
            Ok(TrackLoudness {
                integrated_lufs: r.get(0)?,
                true_peak_db: r.get(1)?,
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

pub struct Analyzer {
    priority: Arc<Mutex<Vec<i64>>>,
    wake: Sender<()>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Track ids whose analysis was just stored.
    pub done: Receiver<i64>,
}

impl Analyzer {
    pub fn start(db_path: &Path, db: DbWriter) -> Result<Self, String> {
        let reader = open_reader(db_path).map_err(|e| e.to_string())?;
        let priority = Arc::new(Mutex::new(Vec::new()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (wake, woken) = bounded(1);
        let (done_tx, done) = unbounded();
        let worker = Worker {
            reader,
            db,
            priority: priority.clone(),
            cancel: cancel.clone(),
            done: done_tx,
        };
        let thread = std::thread::Builder::new()
            .name("analysis".into())
            .spawn(move || worker.run(woken))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            priority,
            wake,
            cancel,
            thread: Some(thread),
            done,
        })
    }

    /// Tracks to analyze first, in order (replaces the previous list).
    pub fn prioritize(&self, track_ids: Vec<i64>) {
        if let Ok(mut p) = self.priority.lock() {
            *p = track_ids;
        }
        let _ = self.wake.try_send(());
    }
}

impl Drop for Analyzer {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.wake.try_send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Worker {
    reader: Connection,
    db: DbWriter,
    priority: Arc<Mutex<Vec<i64>>>,
    cancel: Arc<AtomicBool>,
    done: Sender<i64>,
}

impl Worker {
    fn run(self, woken: Receiver<()>) {
        while !self.cancel.load(Ordering::Relaxed) {
            match self.next_track() {
                Some((track_id, path)) => {
                    let result = measure(&path, &self.cancel);
                    if self.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    // A file that cannot be decoded is stored without values, so it is not
                    // retried on every pass; playback treats it as unknown loudness.
                    let loudness = result.unwrap_or_default();
                    let stored = self.db.call(move |c| {
                        c.execute(
                            "INSERT INTO analysis (track_id, algo_version, loudness_lufs,
                                                   true_peak_db, analyzed_at)
                             VALUES (?1, ?2, ?3, ?4, ?5)
                             ON CONFLICT (track_id) DO UPDATE SET
                                 algo_version = ?2, loudness_lufs = ?3, true_peak_db = ?4,
                                 analyzed_at = ?5",
                            params![
                                track_id,
                                LOUDNESS_ALGO,
                                loudness.integrated_lufs,
                                loudness.true_peak_db,
                                now_ms()
                            ],
                        )
                    });
                    match stored {
                        Some(Ok(_)) => {
                            let _ = self.done.send(track_id);
                        }
                        Some(Err(_)) => {
                            // The track was deleted meanwhile, or the database is busy;
                            // try again later rather than spinning on it.
                            let _ = woken.recv_timeout(IDLE_POLL);
                        }
                        None => break, // writer gone: shutting down
                    }
                }
                None => {
                    let _ = woken.recv_timeout(IDLE_POLL);
                }
            }
        }
    }

    /// The first prioritized track that still needs analysis, else (on desktop) any.
    fn next_track(&self) -> Option<(i64, PathBuf)> {
        let wanted: Vec<i64> = self.priority.lock().map(|p| p.clone()).unwrap_or_default();
        for id in wanted {
            if stored_loudness(&self.reader, id).is_none()
                && let Some(path) = self.track_path(id)
            {
                return Some((id, path));
            }
        }
        if !BACKGROUND {
            return None;
        }
        let id: i64 = self
            .reader
            .query_row(
                "SELECT t.id FROM track t
                 JOIN file f ON f.id = t.file_id
                 LEFT JOIN analysis a ON a.track_id = t.id
                 WHERE f.missing = 0 AND (a.track_id IS NULL OR a.algo_version < ?1)
                 ORDER BY t.id LIMIT 1",
                [LOUDNESS_ALGO],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()?;
        Some((id, self.track_path(id)?))
    }

    fn track_path(&self, track_id: i64) -> Option<PathBuf> {
        self.reader
            .query_row(
                "SELECT r.path, f.rel_path FROM track t
                 JOIN file f ON f.id = t.file_id JOIN library_root r ON r.id = f.root_id
                 WHERE t.id = ?1 AND f.missing = 0",
                [track_id],
                |r| Ok(PathBuf::from(r.get::<_, String>(0)?).join(r.get::<_, String>(1)?)),
            )
            .optional()
            .ok()
            .flatten()
    }
}

/// Decodes the whole file and measures it. `None` on a decoding error or cancel.
fn measure(path: &Path, cancel: &AtomicBool) -> Option<TrackLoudness> {
    let mut decoder = Decoder::open(path).ok()?;
    let info = decoder.info().clone();
    let mut meter = LoudnessMeter::new(info.sample_rate, info.channels).ok()?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        match decoder.next_chunk() {
            Ok(Some(chunk)) => meter.push(chunk).ok()?,
            Ok(None) => break,
            // What decoded before a damaged spot is still a fair measurement.
            Err(_) => break,
        }
    }
    let l = meter.finish();
    Some(TrackLoudness {
        integrated_lufs: l.integrated_lufs,
        true_peak_db: l.true_peak_db,
    })
}
