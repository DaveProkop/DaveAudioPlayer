//! Incremental folder scanner.
//!
//! - unchanged files (same size and mtime) are skipped without reading them
//! - new files whose `quick_hash` matches a missing file are treated as moves, keeping
//!   the track with its rating, stats and playlist entries
//! - files that disappeared are flagged `missing`, never deleted
//! - an unreachable root (unplugged drive) is an error and marks nothing as missing
//! - covers (embedded, else an image in the folder) are stored when a covers folder is
//!   given; a root scanned before covers existed gets them once for all its files
//!
//! Database work runs on the single DB writer thread, in batches (see [`ScanJob`]).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, params};
use walkdir::WalkDir;

use crate::catalog::{self, NameCache, now_ms};
use crate::covers::{FolderCovers, store_cover};
use crate::db::{DbError, Result};

/// Extensions the player can decode (Opus is not supported yet).
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "m4a", "m4b", "mp4", "aac", "ogg", "oga",
];

/// Files per step (one transaction). Small enough that other writes wait at most
/// a moment while a large library is being scanned.
const BATCH: usize = 50;
/// Bytes hashed from the start and from the end of a file.
const HASH_SPAN: u64 = 16 * 1024;
/// Bumped when cover extraction changes: every root then gets its covers again once.
/// Version 2 also stores the key from tags, 3 the tempo; unchanged files are read once
/// more for them.
const COVERS_VERSION: &str = "3";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub moved: usize,
    pub missing: usize,
    /// Files that could not be read, with the reason. They are retried on the next scan.
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct ScanProgress<'a> {
    pub processed: usize,
    pub total: usize,
    pub current: &'a Path,
}

/// Registers a library folder (idempotent) and returns its id.
struct Found {
    abs: PathBuf,
    rel: String,
    size: u64,
    mtime: i64,
}

struct Known {
    id: i64,
    size: u64,
    mtime: i64,
    missing: bool,
}

/// Scans a whole root in one go (tests, tools). The app uses [`ScanJob`] so other
/// writes can run between batches.
pub fn scan_root(
    conn: &mut Connection,
    root_id: i64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&ScanProgress<'_>),
) -> Result<ScanReport> {
    let root = root_path(conn, root_id)?;
    let mut job = ScanJob::walk(root_id, root)?;
    while !job.step(conn, cancel, &mut progress)? {}
    Ok(job.into_report())
}

/// Path of a registered library folder.
pub fn root_path(conn: &Connection, root_id: i64) -> Result<PathBuf> {
    let p: String = conn.query_row(
        "SELECT path FROM library_root WHERE id = ?1",
        [root_id],
        |r| r.get(0),
    )?;
    Ok(PathBuf::from(p))
}

/// A scan split into steps. [`ScanJob::walk`] touches only the file system and can run
/// on any thread; each [`ScanJob::step`] does one batch on the writer connection, so the
/// writer thread can serve other jobs (queue edits, play stats) in between.
pub struct ScanJob {
    root_id: i64,
    root: PathBuf,
    found: Vec<Found>,
    next: usize,
    known: Option<HashMap<String, Known>>,
    gone: Vec<i64>,
    names: NameCache,
    report: ScanReport,
    covers: Option<Covers>,
}

/// Cover handling during a scan.
struct Covers {
    dir: PathBuf,
    folders: FolderCovers,
    /// Also unchanged files get their covers (first scan with covers).
    refresh: bool,
}

fn covers_setting(root_id: i64) -> String {
    format!("scan.covers_version.{root_id}")
}

impl ScanJob {
    /// Lists the audio files under `root`. Fails if the folder is not reachable.
    pub fn walk(root_id: i64, root: PathBuf) -> Result<Self> {
        if !root.is_dir() {
            return Err(DbError::RootUnavailable(root));
        }
        let mut report = ScanReport::default();
        let found = walk(&root, &mut report);
        Ok(Self {
            root_id,
            root,
            found,
            next: 0,
            known: None,
            gone: Vec::new(),
            names: NameCache::default(),
            report,
            covers: None,
        })
    }

    /// Stores covers in `dir` while scanning (see [`crate::covers`]).
    pub fn with_covers(mut self, dir: PathBuf) -> Self {
        self.covers = Some(Covers {
            dir,
            folders: FolderCovers::default(),
            refresh: false,
        });
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `(processed, total)` files.
    pub fn progress(&self) -> (usize, usize) {
        (self.next, self.found.len())
    }

    pub fn report(&self) -> &ScanReport {
        &self.report
    }

    pub fn into_report(self) -> ScanReport {
        self.report
    }

    /// Processes the next batch. Returns `true` when the scan is complete (or cancelled).
    pub fn step(
        &mut self,
        conn: &mut Connection,
        cancel: &AtomicBool,
        mut progress: impl FnMut(&ScanProgress<'_>),
    ) -> Result<bool> {
        if self.known.is_none() {
            self.prepare(conn)?;
        }
        let total = self.found.len();
        let end = (self.next + BATCH).min(total);
        let tx = conn.transaction()?;
        let known = self.known.as_mut().expect("prepared");
        while self.next < end {
            if cancel.load(Ordering::Relaxed) {
                self.report.cancelled = true;
                break;
            }
            let f = &self.found[self.next];
            progress(&ScanProgress {
                processed: self.next,
                total,
                current: &f.abs,
            });
            let k = known.remove(&f.rel);
            if let Err(e) = process_file(
                &tx,
                &mut self.names,
                self.root_id,
                f,
                k.as_ref(),
                &mut self.report,
                self.covers.as_mut(),
            ) {
                match e {
                    DbError::Sqlite(_) | DbError::TooNew { .. } => return Err(e),
                    other => self.report.failed.push((f.abs.clone(), other.to_string())),
                }
            }
            self.next += 1;
        }
        tx.commit()?;

        if self.next < total && !self.report.cancelled {
            return Ok(false);
        }
        // Gone files that did not turn up elsewhere stay missing.
        for &id in &self.gone {
            let still_missing: bool =
                conn.query_row("SELECT missing FROM file WHERE id = ?1", [id], |r| r.get(0))?;
            self.report.missing += usize::from(still_missing);
        }
        if !self.report.cancelled {
            if self.covers.is_some() {
                conn.execute(
                    "INSERT INTO setting (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = ?2",
                    params![covers_setting(self.root_id), COVERS_VERSION],
                )?;
            }
            progress(&ScanProgress {
                processed: total,
                total,
                current: &self.root,
            });
        }
        Ok(true)
    }

    /// Loads what the database knows about this root and flags files gone from disk
    /// first, so that a file moved within this scan is recognized as a move when its new
    /// location is processed. Folders that could not be read are skipped: their files are
    /// not known to be gone.
    fn prepare(&mut self, conn: &mut Connection) -> Result<()> {
        if let Some(covers) = self.covers.as_mut() {
            let done: Option<String> = conn
                .query_row(
                    "SELECT value FROM setting WHERE key = ?1",
                    [covers_setting(self.root_id)],
                    |r| r.get(0),
                )
                .optional()?;
            covers.refresh = done.as_deref() != Some(COVERS_VERSION);
        }
        let known: HashMap<String, Known> = {
            let mut st = conn.prepare(
                "SELECT id, rel_path, size, mtime, missing FROM file WHERE root_id = ?1",
            )?;
            let rows = st.query_map([self.root_id], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    Known {
                        id: r.get(0)?,
                        size: r.get::<_, i64>(2)? as u64,
                        mtime: r.get(3)?,
                        missing: r.get(4)?,
                    },
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let unreadable: Vec<String> = self
            .report
            .failed
            .iter()
            .filter_map(|(p, _)| rel_path(&self.root, p))
            .collect();
        let present: std::collections::HashSet<&str> =
            self.found.iter().map(|f| f.rel.as_str()).collect();
        let tx = conn.transaction()?;
        for (rel, k) in &known {
            let in_unreadable = unreadable
                .iter()
                .any(|u| u.is_empty() || rel.starts_with(&format!("{u}/")));
            if !k.missing && !present.contains(rel.as_str()) && !in_unreadable {
                tx.execute("UPDATE file SET missing = 1 WHERE id = ?1", [k.id])?;
                self.gone.push(k.id);
            }
        }
        tx.commit()?;
        self.known = Some(known);
        Ok(())
    }
}

fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn walk(root: &Path, report: &mut ScanReport) -> Vec<Found> {
    let mut found = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let path = e
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.to_path_buf());
                report.failed.push((path, e.to_string()));
                continue;
            }
        };
        if !entry.file_type().is_file() || !is_audio(entry.path()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Some(rel) = rel_path(root, entry.path()) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);
        found.push(Found {
            abs: entry.path().to_path_buf(),
            rel,
            size: meta.len(),
            mtime,
        });
    }
    // Stable order: albums are scanned together, progress is predictable.
    found.sort_by(|a, b| a.rel.cmp(&b.rel));
    found
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn process_file(
    conn: &Connection,
    names: &mut NameCache,
    root_id: i64,
    f: &Found,
    known: Option<&Known>,
    report: &mut ScanReport,
    covers: Option<&mut Covers>,
) -> Result<()> {
    if let Some(k) = known
        && !k.missing
        && k.size == f.size
        && k.mtime == f.mtime
    {
        report.unchanged += 1;
        if let Some(c) = covers
            && c.refresh
        {
            let track_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM track WHERE file_id = ?1 AND start_ms = 0",
                    [k.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(track_id) = track_id
                && let Ok(tags) = dap_tags::read(&f.abs)
            {
                assign_cover(conn, c, track_id, &f.abs, tags.cover.as_deref())?;
                catalog::set_tag_key(conn, track_id, tags.tags.initial_key.as_deref())?;
                catalog::set_tag_bpm(conn, track_id, tags.tags.bpm.as_deref())?;
            }
        }
        return Ok(());
    }

    let hash = quick_hash(&f.abs, f.size).map_err(DbError::Io)?;

    if known.is_none()
        && let Some(file_id) = find_moved(conn, &hash, f.size)?
    {
        conn.execute(
            "UPDATE file SET root_id = ?2, rel_path = ?3, mtime = ?4, missing = 0, scanned_at = ?5 WHERE id = ?1",
            params![file_id, root_id, f.rel, f.mtime, now_ms()],
        )?;
        report.moved += 1;
        return Ok(());
    }

    let tags = dap_tags::read(&f.abs).map_err(|e| DbError::File(e.to_string()))?;
    let p = &tags.properties;
    let file_id = match known {
        Some(k) => {
            conn.execute(
                "UPDATE file SET size = ?2, mtime = ?3, quick_hash = ?4, format = ?5, duration_ms = ?6,
                        sample_rate = ?7, channels = ?8, bit_depth = ?9, bitrate = ?10,
                        missing = 0, scanned_at = ?11
                 WHERE id = ?1",
                params![
                    k.id,
                    f.size as i64,
                    f.mtime,
                    hash,
                    extension(&f.abs),
                    p.duration_ms as i64,
                    p.sample_rate,
                    p.channels,
                    p.bit_depth,
                    p.bitrate_kbps,
                    now_ms()
                ],
            )?;
            report.updated += 1;
            k.id
        }
        None => {
            conn.execute(
                "INSERT INTO file (root_id, rel_path, size, mtime, quick_hash, format, duration_ms,
                                   sample_rate, channels, bit_depth, bitrate, scanned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    root_id,
                    f.rel,
                    f.size as i64,
                    f.mtime,
                    hash,
                    extension(&f.abs),
                    p.duration_ms as i64,
                    p.sample_rate,
                    p.channels,
                    p.bit_depth,
                    p.bitrate_kbps,
                    now_ms()
                ],
            )?;
            report.added += 1;
            conn.last_insert_rowid()
        }
    };

    let fallback_title = f
        .abs
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let track_id = catalog::upsert_track(conn, names, file_id, &tags.tags, &fallback_title)?;
    if known.is_some() {
        // The audio may have changed: measure loudness, key and tempo again.
        conn.execute("UPDATE analysis SET algo_version = 0 WHERE track_id = ?1", [track_id])?;
    }
    catalog::set_tag_key(conn, track_id, tags.tags.initial_key.as_deref())?;
    catalog::set_tag_bpm(conn, track_id, tags.tags.bpm.as_deref())?;
    if let Some(c) = covers {
        assign_cover(conn, c, track_id, &f.abs, tags.cover.as_deref())?;
    }
    Ok(())
}

/// The embedded cover if there is a readable one, else the folder's image.
fn assign_cover(
    conn: &Connection,
    covers: &mut Covers,
    track_id: i64,
    file: &Path,
    embedded: Option<&[u8]>,
) -> Result<()> {
    let mut cover_id = match embedded {
        Some(bytes) => store_cover(conn, &covers.dir, bytes, "embedded")?,
        None => None,
    };
    if cover_id.is_none()
        && let Some(dir) = file.parent()
    {
        cover_id = covers.folders.get(conn, &covers.dir, dir)?;
    }
    catalog::set_track_cover(conn, track_id, cover_id)
}

fn find_moved(conn: &Connection, hash: &[u8], size: u64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM file WHERE missing = 1 AND quick_hash = ?1 AND size = ?2 LIMIT 1",
            params![hash, size as i64],
            |r| r.get(0),
        )
        .optional()?)
}

/// FNV-1a 64 over the size and the first and last [`HASH_SPAN`] bytes. Stable across
/// versions and platforms (unlike `std`'s hasher), cheap enough for every new file.
fn quick_hash(path: &Path, size: u64) -> std::io::Result<Vec<u8>> {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
    };
    feed(&size.to_le_bytes());

    let mut file = File::open(path)?;
    let mut buf = vec![0u8; HASH_SPAN as usize];
    let head = read_up_to(&mut file, &mut buf)?;
    feed(&buf[..head]);
    if size > 2 * HASH_SPAN {
        file.seek(SeekFrom::End(-(HASH_SPAN as i64)))?;
        let tail = read_up_to(&mut file, &mut buf)?;
        feed(&buf[..tail]);
    }
    Ok(h.to_le_bytes().to_vec())
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}
