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
use dap_analysis::{KeyDetector, LoudnessMeter, TempoDetector};
use dap_audio_io::Decoder;
use dap_library::catalog::now_ms;
use dap_library::db::open_reader;
use rusqlite::{Connection, OptionalExtension, params};

use crate::db_writer::DbWriter;

/// Rows from this version on hold a valid loudness measurement.
pub const LOUDNESS_ALGO: i64 = 1;
/// Bumped when the analysis changes; older rows are analyzed again. 2: adds the key,
/// 3: key from spectral peaks only, 4: adds the tempo.
pub const ANALYSIS_ALGO: i64 = 4;
/// Detected keys stay below the confidence that marks a key from tags.
const MAX_DETECTED_KEY_CONFIDENCE: f64 = 0.99;
/// How often an idle analyzer looks for new tracks (after a scan, for example).
const IDLE_POLL: Duration = Duration::from_secs(10);
/// Whether the rest of the library is analyzed in the background by default. Android
/// starts with queued tracks only; the app turns the rest on while charging.
const BACKGROUND_DEFAULT: bool = !cfg!(target_os = "android");

/// Loudness of a track as stored, `None` values when unknown.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrackLoudness {
    pub integrated_lufs: Option<f64>,
    pub true_peak_db: Option<f64>,
}

/// What one analysis pass measures.
#[derive(Debug, Clone, Default, PartialEq)]
struct Measured {
    loudness: TrackLoudness,
    /// Camelot key and confidence.
    key: Option<(String, f64)>,
    /// BPM and confidence.
    tempo: Option<(f64, f64)>,
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
    background: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Track ids whose analysis was just stored.
    pub done: Receiver<i64>,
}

impl Analyzer {
    pub fn start(db_path: &Path, db: DbWriter) -> Result<Self, String> {
        let reader = open_reader(db_path).map_err(|e| e.to_string())?;
        let priority = Arc::new(Mutex::new(Vec::new()));
        let cancel = Arc::new(AtomicBool::new(false));
        let background = Arc::new(AtomicBool::new(BACKGROUND_DEFAULT));
        let (wake, woken) = bounded(1);
        let (done_tx, done) = unbounded();
        let worker = Worker {
            reader,
            db,
            priority: priority.clone(),
            cancel: cancel.clone(),
            background: background.clone(),
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
            background,
            thread: Some(thread),
            done,
        })
    }

    /// Analyze the whole library, not only the prioritized tracks.
    pub fn set_background(&self, on: bool) {
        self.background.store(on, Ordering::Relaxed);
        let _ = self.wake.try_send(());
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
    background: Arc<AtomicBool>,
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
                    let m = result.unwrap_or_default();
                    let (key, key_confidence) = m.key.unzip();
                    let (bpm, bpm_confidence) =
                        choose_bpm(self.tag_bpm(track_id), m.tempo).unzip();
                    let stored = self.db.call(move |c| {
                        // A key from tags (confidence 1) beats a detected one.
                        c.execute(
                            "INSERT INTO analysis (track_id, algo_version, loudness_lufs,
                                                   true_peak_db, key_camelot, key_confidence,
                                                   bpm, bpm_confidence, analyzed_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                             ON CONFLICT (track_id) DO UPDATE SET
                                 algo_version = ?2, loudness_lufs = ?3, true_peak_db = ?4,
                                 key_camelot = CASE WHEN analysis.key_confidence >= 1.0
                                     THEN analysis.key_camelot ELSE ?5 END,
                                 key_confidence = CASE WHEN analysis.key_confidence >= 1.0
                                     THEN analysis.key_confidence ELSE ?6 END,
                                 bpm = ?7, bpm_confidence = ?8,
                                 analyzed_at = ?9",
                            params![
                                track_id,
                                ANALYSIS_ALGO,
                                m.loudness.integrated_lufs,
                                m.loudness.true_peak_db,
                                key,
                                key_confidence,
                                bpm,
                                bpm_confidence,
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
            if self.needs_analysis(id)
                && let Some(path) = self.track_path(id)
            {
                return Some((id, path));
            }
        }
        if !self.background.load(Ordering::Relaxed) {
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
                [ANALYSIS_ALGO],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()?;
        Some((id, self.track_path(id)?))
    }

    /// The tempo stored from the file's tags, if any.
    fn tag_bpm(&self, track_id: i64) -> Option<f64> {
        self.reader
            .query_row(
                "SELECT bpm FROM analysis WHERE track_id = ?1 AND bpm_confidence >= 1.0",
                [track_id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    fn needs_analysis(&self, track_id: i64) -> bool {
        self.reader
            .query_row(
                "SELECT 1 FROM analysis WHERE track_id = ?1 AND algo_version >= ?2",
                params![track_id, ANALYSIS_ALGO],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_none()
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

/// BPM tags are often wrong (some taggers write a default like 90), so a tag counts only
/// when the detection agrees with it (also at half / double tempo) or is unsure; a
/// confident detection that disagrees wins.
fn choose_bpm(tag: Option<f64>, detected: Option<(f64, f64)>) -> Option<(f64, f64)> {
    match (tag, detected) {
        (Some(tag), Some((bpm, conf))) => {
            let agrees = [1.0, 2.0, 0.5].iter().any(|r| (bpm / (tag * r) - 1.0).abs() <= 0.04);
            if agrees || conf < 0.5 { Some((tag, 1.0)) } else { Some((bpm, conf)) }
        }
        (Some(tag), None) => Some((tag, 1.0)),
        (None, d) => d,
    }
}

/// Decodes the whole file once and measures loudness and key. `None` on a decoding error
/// or cancel.
fn measure(path: &Path, cancel: &AtomicBool) -> Option<Measured> {
    let mut decoder = Decoder::open(path).ok()?;
    let info = decoder.info().clone();
    let mut meter = LoudnessMeter::new(info.sample_rate, info.channels).ok()?;
    let mut keys = KeyDetector::new(info.sample_rate, info.channels);
    let mut tempo = TempoDetector::new(info.sample_rate, info.channels);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                meter.push(chunk).ok()?;
                keys.push(chunk);
                tempo.push(chunk);
            }
            Ok(None) => break,
            // What decoded before a damaged spot is still a fair measurement.
            Err(_) => break,
        }
    }
    let l = meter.finish();
    Some(Measured {
        loudness: TrackLoudness {
            integrated_lufs: l.integrated_lufs,
            true_peak_db: l.true_peak_db,
        },
        key: keys
            .finish()
            .map(|(k, c)| (k.camelot().to_string(), c.min(MAX_DETECTED_KEY_CONFIDENCE))),
        tempo: tempo
            .finish()
            .map(|(b, c)| (b, c.min(MAX_DETECTED_KEY_CONFIDENCE))),
    })
}

#[cfg(test)]
mod tests {
    use super::choose_bpm;

    #[test]
    fn bpm_tags_count_only_when_the_detection_agrees() {
        assert_eq!(choose_bpm(Some(128.0), Some((127.6, 0.9))), Some((128.0, 1.0)));
        assert_eq!(choose_bpm(Some(87.0), Some((174.2, 0.9))), Some((87.0, 1.0)), "half time");
        assert_eq!(choose_bpm(Some(90.0), Some((124.0, 0.9))), Some((124.0, 0.9)), "wrong tag");
        assert_eq!(choose_bpm(Some(90.0), Some((124.0, 0.2))), Some((90.0, 1.0)), "unsure");
        assert_eq!(choose_bpm(Some(90.0), None), Some((90.0, 1.0)));
        assert_eq!(choose_bpm(None, Some((124.0, 0.3))), Some((124.0, 0.3)));
    }
}
