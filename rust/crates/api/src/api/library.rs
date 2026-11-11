//! Read-only library queries for the UI (a read connection; writes go through the engine).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use dap_library::covers::{cover_files, covers_dir};
use dap_library::db::open_reader;
use rusqlite::Connection;

use super::player::DB_PATH;

static READER: Mutex<Option<Connection>> = Mutex::new(None);

pub struct TrackInfo {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_no: Option<u32>,
    pub duration_ms: Option<i64>,
    pub missing: bool,
    /// Cover image files (64 px, 256 px, up to 1024 px); `None` when the track has none.
    pub cover_small: Option<String>,
    pub cover_medium: Option<String>,
    pub cover_large: Option<String>,
}

/// Closes the read connection (on engine stop, so the database file is released).
pub(crate) fn close_reader() {
    if let Ok(mut guard) = READER.lock() {
        *guard = None;
    }
}

/// The covers folder of the running engine's database.
fn current_covers_dir() -> Result<PathBuf> {
    DB_PATH
        .lock()
        .map_err(|_| anyhow!("lock poisoned"))?
        .as_deref()
        .map(covers_dir)
        .ok_or_else(|| anyhow!("engine is not running"))
}

fn with_reader<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
    let mut guard = READER.lock().map_err(|_| anyhow!("reader lock poisoned"))?;
    if guard.is_none() {
        let path = DB_PATH
            .lock()
            .map_err(|_| anyhow!("lock poisoned"))?
            .clone()
            .ok_or_else(|| anyhow!("engine is not running"))?;
        *guard = Some(open_reader(&path).map_err(|e| anyhow!(e.to_string()))?);
    }
    Ok(f(guard.as_ref().expect("reader"))?)
}

const TRACK_COLUMNS: &str = "t.id, t.title,
    coalesce((SELECT group_concat(a.name, ', ') FROM track_artist ta
              JOIN artist a ON a.id = ta.artist_id WHERE ta.track_id = t.id), ''),
    coalesce(al.title, ''), t.track_no, f.duration_ms, f.missing, c.hash";

const TRACK_JOINS: &str = "FROM track t
    JOIN file f ON f.id = t.file_id
    LEFT JOIN album al ON al.id = t.album_id
    LEFT JOIN artist aa ON aa.id = al.album_artist_id
    LEFT JOIN cover c ON c.id = coalesce(t.cover_id, al.cover_id)";

fn row_to_track(r: &rusqlite::Row<'_>, covers: &Path) -> rusqlite::Result<TrackInfo> {
    let files = r
        .get::<_, Option<Vec<u8>>>(7)?
        .map(|hash| cover_files(covers, &hash));
    let path = |p: &Path| p.to_string_lossy().into_owned();
    Ok(TrackInfo {
        id: r.get(0)?,
        title: r.get(1)?,
        artist: r.get(2)?,
        album: r.get(3)?,
        track_no: r.get(4)?,
        duration_ms: r.get(5)?,
        missing: r.get(6)?,
        cover_small: files.as_ref().map(|f| path(&f.small)),
        cover_medium: files.as_ref().map(|f| path(&f.medium)),
        cover_large: files.as_ref().map(|f| path(&f.large)),
    })
}

/// Number of playable tracks.
pub fn track_count() -> Result<u32> {
    with_reader(|c| {
        c.query_row(
            "SELECT count(*) FROM track t JOIN file f ON f.id = t.file_id WHERE f.missing = 0",
            [],
            |r| r.get(0),
        )
    })
}

/// One page of the library, ordered by album artist, album, disc and track.
pub fn library_page(offset: u32, limit: u32) -> Result<Vec<TrackInfo>> {
    let covers = current_covers_dir()?;
    with_reader(|c| {
        let sql = format!(
            "SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE f.missing = 0
             ORDER BY coalesce(aa.name_sort, ''), coalesce(al.title_sort, ''), t.disc_no,
                      t.track_no, t.title_sort
             LIMIT ?1 OFFSET ?2"
        );
        let mut st = c.prepare_cached(&sql)?;
        let rows = st.query_map([limit, offset], |r| row_to_track(r, &covers))?;
        rows.collect()
    })
}

pub fn search(query: String, limit: u32) -> Result<Vec<TrackInfo>> {
    let ids = with_reader(|c| Ok(dap_library::search_tracks(c, &query, limit as usize)))?
        .map_err(|e| anyhow!(e.to_string()))?;
    tracks_by_ids(ids)
}

/// One track by id, on any connection (also used by the media controls).
pub(crate) fn query_track(
    c: &Connection,
    covers: &Path,
    id: i64,
) -> rusqlite::Result<Option<TrackInfo>> {
    let sql = format!("SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE t.id = ?1");
    let mut st = c.prepare_cached(&sql)?;
    match st.query_row([id], |r| row_to_track(r, covers)) {
        Ok(t) => Ok(Some(t)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Tracks in the order of `ids` (unknown ids are skipped).
pub fn tracks_by_ids(ids: Vec<i64>) -> Result<Vec<TrackInfo>> {
    let covers = current_covers_dir()?;
    with_reader(|c| {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.extend(query_track(c, &covers, id)?);
        }
        Ok(out)
    })
}

/// Registered music folders.
pub fn library_folders() -> Result<Vec<String>> {
    with_reader(|c| {
        let mut st = c.prepare("SELECT path FROM library_root ORDER BY id")?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect()
    })
}
