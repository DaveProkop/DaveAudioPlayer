//! Read-only library queries for the UI (a read connection; writes go through the engine).

use std::sync::Mutex;

use anyhow::{anyhow, Result};
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
}

/// Closes the read connection (on engine stop, so the database file is released).
pub(crate) fn close_reader() {
    if let Ok(mut guard) = READER.lock() {
        *guard = None;
    }
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
    coalesce(al.title, ''), t.track_no, f.duration_ms, f.missing";

const TRACK_JOINS: &str = "FROM track t
    JOIN file f ON f.id = t.file_id
    LEFT JOIN album al ON al.id = t.album_id
    LEFT JOIN artist aa ON aa.id = al.album_artist_id";

fn row_to_track(r: &rusqlite::Row<'_>) -> rusqlite::Result<TrackInfo> {
    Ok(TrackInfo {
        id: r.get(0)?,
        title: r.get(1)?,
        artist: r.get(2)?,
        album: r.get(3)?,
        track_no: r.get(4)?,
        duration_ms: r.get(5)?,
        missing: r.get(6)?,
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
    with_reader(|c| {
        let sql = format!(
            "SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE f.missing = 0
             ORDER BY coalesce(aa.name_sort, ''), coalesce(al.title_sort, ''), t.disc_no,
                      t.track_no, t.title_sort
             LIMIT ?1 OFFSET ?2"
        );
        let mut st = c.prepare_cached(&sql)?;
        let rows = st.query_map([limit, offset], row_to_track)?;
        rows.collect()
    })
}

pub fn search(query: String, limit: u32) -> Result<Vec<TrackInfo>> {
    let ids = with_reader(|c| Ok(dap_library::search_tracks(c, &query, limit as usize)))?
        .map_err(|e| anyhow!(e.to_string()))?;
    tracks_by_ids(ids)
}

/// Tracks in the order of `ids` (unknown ids are skipped).
pub fn tracks_by_ids(ids: Vec<i64>) -> Result<Vec<TrackInfo>> {
    with_reader(|c| {
        let sql = format!("SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE t.id = ?1");
        let mut st = c.prepare_cached(&sql)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match st.query_row([id], row_to_track) {
                Ok(t) => out.push(t),
                Err(rusqlite::Error::QueryReturnedNoRows) => {}
                Err(e) => return Err(e),
            }
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
