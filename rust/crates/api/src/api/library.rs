//! Read-only library queries for the UI (a read connection; writes go through the engine).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use dap_engine::analyzer::ANALYSIS_ALGO;
use dap_library::covers::{cover_files, covers_dir};
use dap_library::db::open_reader;
use rusqlite::{Connection, OptionalExtension};

use super::player::DB_PATH;

static READER: Mutex<Option<Connection>> = Mutex::new(None);

pub struct TrackInfo {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<i64>,
    pub track_no: Option<u32>,
    pub duration_ms: Option<i64>,
    pub missing: bool,
    /// Musical key in Camelot notation ("8A"), from tags or detected; `None` until known.
    pub key: Option<String>,
    /// Tempo in BPM, from tags or detected.
    pub bpm: Option<f64>,
    /// Cover image files (64 px, 256 px, up to 1024 px); `None` when the track has none.
    pub cover_small: Option<String>,
    pub cover_medium: Option<String>,
    pub cover_large: Option<String>,
}

pub struct AlbumInfo {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub year: Option<i32>,
    pub track_count: u32,
    pub duration_ms: i64,
    pub cover_small: Option<String>,
    pub cover_medium: Option<String>,
    pub cover_large: Option<String>,
}

pub struct ArtistInfo {
    pub id: i64,
    pub name: String,
    pub album_count: u32,
    pub track_count: u32,
    /// A cover from one of the artist's albums.
    pub cover_medium: Option<String>,
}

pub struct FolderInfo {
    pub id: i64,
    pub path: String,
    pub track_count: u32,
    /// False when the folder cannot be reached (unplugged drive, removed SD card).
    pub available: bool,
}

pub struct LibraryStats {
    pub tracks: u32,
    pub albums: u32,
    pub artists: u32,
    pub total_ms: i64,
    /// Tracks whose loudness has not been measured yet.
    pub pending_analysis: u32,
}

/// Technical details of one track (now playing view).
pub struct TrackDetails {
    pub format: String,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    pub year: Option<i32>,
    pub loudness_lufs: Option<f64>,
    /// Camelot key and whether it comes from the file's tags (else it was detected).
    pub key: Option<String>,
    pub key_from_tag: bool,
    pub bpm: Option<f64>,
    pub bpm_from_tag: bool,
    /// Energy 0–1, from loudness and tempo (once the loudness is measured).
    pub energy: Option<f64>,
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

struct Covers {
    small: Option<String>,
    medium: Option<String>,
    large: Option<String>,
}

fn covers_for(dir: &Path, hash: Option<Vec<u8>>) -> Covers {
    let files = hash.map(|h| cover_files(dir, &h));
    let path = |p: &Path| p.to_string_lossy().into_owned();
    Covers {
        small: files.as_ref().map(|f| path(&f.small)),
        medium: files.as_ref().map(|f| path(&f.medium)),
        large: files.as_ref().map(|f| path(&f.large)),
    }
}

const TRACK_COLUMNS: &str = "t.id, t.title,
    coalesce((SELECT group_concat(a.name, ', ') FROM track_artist ta
              JOIN artist a ON a.id = ta.artist_id
              WHERE ta.track_id = t.id AND ta.role = 'main'), ''),
    coalesce(al.title, ''), t.track_no, f.duration_ms, f.missing, c.hash, t.album_id,
    an.key_camelot, an.bpm";

const TRACK_JOINS: &str = "FROM track t
    JOIN file f ON f.id = t.file_id
    LEFT JOIN album al ON al.id = t.album_id
    LEFT JOIN artist aa ON aa.id = al.album_artist_id
    LEFT JOIN cover c ON c.id = coalesce(t.cover_id, al.cover_id)
    LEFT JOIN analysis an ON an.track_id = t.id";

/// Library order: album artist, album, disc, track.
const TRACK_ORDER: &str = "ORDER BY coalesce(aa.name_sort, ''), coalesce(al.title_sort, ''),
    t.disc_no, t.track_no, t.title_sort";

fn row_to_track(r: &rusqlite::Row<'_>, covers: &Path) -> rusqlite::Result<TrackInfo> {
    let c = covers_for(covers, r.get(7)?);
    Ok(TrackInfo {
        id: r.get(0)?,
        title: r.get(1)?,
        artist: r.get(2)?,
        album: r.get(3)?,
        album_id: r.get(8)?,
        track_no: r.get(4)?,
        duration_ms: r.get(5)?,
        missing: r.get(6)?,
        key: r.get(9)?,
        bpm: r.get(10)?,
        cover_small: c.small,
        cover_medium: c.medium,
        cover_large: c.large,
    })
}

fn query_tracks(
    where_clause: &str,
    order: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<TrackInfo>> {
    let covers = current_covers_dir()?;
    with_reader(|c| {
        let sql = format!("SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE {where_clause} {order}");
        let mut st = c.prepare_cached(&sql)?;
        let rows = st.query_map(params, |r| row_to_track(r, &covers))?;
        rows.collect()
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
    query_tracks(
        "f.missing = 0",
        &format!("{TRACK_ORDER} LIMIT ?1 OFFSET ?2"),
        [limit, offset],
    )
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
    st.query_row([id], |r| row_to_track(r, covers)).optional()
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

const ALBUM_QUERY: &str = "SELECT al.id, al.title, coalesce(aa.name, ''), al.year,
        count(t.id), coalesce(sum(f.duration_ms), 0), c.hash
    FROM album al
    JOIN track t ON t.album_id = al.id
    JOIN file f ON f.id = t.file_id AND f.missing = 0
    LEFT JOIN artist aa ON aa.id = al.album_artist_id
    LEFT JOIN cover c ON c.id = al.cover_id";

fn query_albums(
    where_clause: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<AlbumInfo>> {
    let covers = current_covers_dir()?;
    with_reader(|c| {
        let sql = format!(
            "{ALBUM_QUERY} WHERE {where_clause} GROUP BY al.id
             ORDER BY coalesce(aa.name_sort, ''), al.year, al.title_sort"
        );
        let mut st = c.prepare_cached(&sql)?;
        let rows = st.query_map(params, |r| {
            let cv = covers_for(&covers, r.get(6)?);
            Ok(AlbumInfo {
                id: r.get(0)?,
                title: r.get(1)?,
                artist: r.get(2)?,
                year: r.get(3)?,
                track_count: r.get(4)?,
                duration_ms: r.get(5)?,
                cover_small: cv.small,
                cover_medium: cv.medium,
                cover_large: cv.large,
            })
        })?;
        rows.collect()
    })
}

/// Albums with at least one playable track, by album artist and year.
pub fn albums() -> Result<Vec<AlbumInfo>> {
    query_albums("1 = 1", [])
}

pub fn album(album_id: i64) -> Result<Option<AlbumInfo>> {
    Ok(query_albums("al.id = ?1", [album_id])?.into_iter().next())
}

/// An album's tracks in disc and track order.
pub fn album_tracks(album_id: i64) -> Result<Vec<TrackInfo>> {
    query_tracks(
        "t.album_id = ?1 AND f.missing = 0",
        "ORDER BY t.disc_no, t.track_no, t.title_sort",
        [album_id],
    )
}

/// Artists credited on playable tracks, by sort name.
pub fn artists() -> Result<Vec<ArtistInfo>> {
    let covers = current_covers_dir()?;
    with_reader(|c| {
        let mut st = c.prepare_cached(
            "SELECT a.id, a.name, count(DISTINCT t.album_id), count(DISTINCT t.id), max(cv.hash)
             FROM artist a
             JOIN track_artist ta ON ta.artist_id = a.id AND ta.role = 'main'
             JOIN track t ON t.id = ta.track_id
             JOIN file f ON f.id = t.file_id AND f.missing = 0
             LEFT JOIN album al ON al.id = t.album_id
             LEFT JOIN cover cv ON cv.id = coalesce(al.cover_id, t.cover_id)
             GROUP BY a.id
             ORDER BY a.name_sort",
        )?;
        let rows = st.query_map([], |r| {
            Ok(ArtistInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                album_count: r.get(2)?,
                track_count: r.get(3)?,
                cover_medium: covers_for(&covers, r.get(4)?).medium,
            })
        })?;
        rows.collect()
    })
}

/// Albums of an artist: as album artist, or with tracks by them.
pub fn artist_albums(artist_id: i64) -> Result<Vec<AlbumInfo>> {
    query_albums(
        "(al.album_artist_id = ?1 OR EXISTS (
            SELECT 1 FROM track t2 JOIN track_artist ta ON ta.track_id = t2.id
            WHERE t2.album_id = al.id AND ta.artist_id = ?1))",
        [artist_id],
    )
}

/// All playable tracks by an artist, in library order.
pub fn artist_tracks(artist_id: i64) -> Result<Vec<TrackInfo>> {
    query_tracks(
        "f.missing = 0 AND EXISTS (SELECT 1 FROM track_artist ta
            WHERE ta.track_id = t.id AND ta.artist_id = ?1)",
        TRACK_ORDER,
        [artist_id],
    )
}

/// Registered music folders.
pub fn library_folders() -> Result<Vec<FolderInfo>> {
    let rows: Vec<(i64, String, u32)> = with_reader(|c| {
        let mut st = c.prepare(
            "SELECT r.id, r.path,
                    (SELECT count(*) FROM file f WHERE f.root_id = r.id AND f.missing = 0)
             FROM library_root r ORDER BY r.id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    })?;
    Ok(rows
        .into_iter()
        .map(|(id, path, track_count)| FolderInfo {
            id,
            available: Path::new(&path).is_dir(),
            path,
            track_count,
        })
        .collect())
}

pub fn library_stats() -> Result<LibraryStats> {
    with_reader(|c| {
        let (tracks, total_ms): (u32, i64) = c.query_row(
            "SELECT count(*), coalesce(sum(f.duration_ms), 0)
             FROM track t JOIN file f ON f.id = t.file_id WHERE f.missing = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let albums: u32 = c.query_row(
            "SELECT count(DISTINCT t.album_id) FROM track t JOIN file f ON f.id = t.file_id
             WHERE f.missing = 0",
            [],
            |r| r.get(0),
        )?;
        let artists: u32 = c.query_row(
            "SELECT count(DISTINCT ta.artist_id) FROM track_artist ta
             JOIN track t ON t.id = ta.track_id JOIN file f ON f.id = t.file_id
             WHERE f.missing = 0 AND ta.role = 'main'",
            [],
            |r| r.get(0),
        )?;
        let pending_analysis: u32 = c.query_row(
            "SELECT count(*) FROM track t JOIN file f ON f.id = t.file_id
             LEFT JOIN analysis a ON a.track_id = t.id
             WHERE f.missing = 0 AND (a.track_id IS NULL OR a.algo_version < ?1)",
            [ANALYSIS_ALGO],
            |r| r.get(0),
        )?;
        Ok(LibraryStats {
            tracks,
            albums,
            artists,
            total_ms,
            pending_analysis,
        })
    })
}

pub fn track_details(track_id: i64) -> Result<Option<TrackDetails>> {
    with_reader(|c| {
        c.query_row(
            "SELECT f.format, f.sample_rate, f.bit_depth, f.bitrate, t.year, a.loudness_lufs,
                    a.key_camelot, coalesce(a.key_confidence, 0) >= 1.0,
                    a.bpm, coalesce(a.bpm_confidence, 0) >= 1.0
             FROM track t JOIN file f ON f.id = t.file_id
             LEFT JOIN analysis a ON a.track_id = t.id
             WHERE t.id = ?1",
            [track_id],
            |r| {
                let lufs: Option<f64> = r.get(5)?;
                let bpm: Option<f64> = r.get(8)?;
                Ok(TrackDetails {
                    energy: lufs.map(|l| dap_analysis::track_energy(l, bpm)),
                    format: r.get(0)?,
                    sample_rate: r.get(1)?,
                    bit_depth: r.get(2)?,
                    bitrate_kbps: r.get(3)?,
                    year: r.get(4)?,
                    loudness_lufs: r.get(5)?,
                    key: r.get(6)?,
                    key_from_tag: r.get(7)?,
                    bpm: r.get(8)?,
                    bpm_from_tag: r.get(9)?,
                })
            },
        )
        .optional()
    })
}

/// Smart Mix: an order for the upcoming queue items (`item_ids`, playing `track_ids` in
/// the same order) where each track mixes well into the next by key, tempo and energy, starting
/// after `after_track` (the playing one). Returns the item ids in the proposed order.
pub fn suggest_mix_order(
    after_track: Option<i64>,
    item_ids: Vec<i64>,
    track_ids: Vec<i64>,
) -> Result<Vec<i64>> {
    use dap_analysis::{Camelot, MixTrack, mix_order};
    let info = |c: &Connection, id: i64| -> rusqlite::Result<MixTrack> {
        let row: Option<(Option<String>, Option<f64>, Option<f64>)> = c
            .query_row(
                "SELECT key_camelot, bpm, loudness_lufs FROM analysis WHERE track_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (key, bpm, lufs) = row.unwrap_or_default();
        Ok(MixTrack {
            key: key.as_deref().and_then(Camelot::parse),
            bpm,
            energy: lufs.map(|l| dap_analysis::track_energy(l, bpm)),
        })
    };
    let (start, tracks) = with_reader(|c| {
        let start = after_track.map(|id| info(c, id)).transpose()?;
        let tracks = track_ids
            .iter()
            .map(|&id| info(c, id))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((start, tracks))
    })?;
    if tracks.len() != item_ids.len() {
        return Err(anyhow!("item_ids and track_ids differ in length"));
    }
    Ok(mix_order(start.as_ref(), &tracks)
        .into_iter()
        .map(|i| item_ids[i])
        .collect())
}

/// A UI preference stored with [`super::player::set_ui_setting`].
pub fn ui_setting(key: String) -> Result<Option<String>> {
    with_reader(|c| {
        c.query_row(
            "SELECT value FROM setting WHERE key = ?1",
            [format!("ui.{key}")],
            |r| r.get(0),
        )
        .optional()
    })
}
