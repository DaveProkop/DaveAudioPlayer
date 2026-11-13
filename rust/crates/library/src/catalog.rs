//! Mapping file tags onto the catalog tables (artist, album, genre, track) and keeping
//! the full-text indexes in sync.
//!
//! Fields that have a row in `tag_edit` are locked: a rescan never overwrites them.

use std::collections::{HashMap, HashSet};

use dap_tags::TrackTags;
use rusqlite::{Connection, OptionalExtension, params};

use crate::db::Result;

/// Field names used in `tag_edit.field` that lock catalog values.
pub mod field {
    pub const TITLE: &str = "title";
    pub const ARTIST: &str = "artist";
    pub const ALBUM: &str = "album";
    pub const ALBUM_ARTIST: &str = "album_artist";
    pub const YEAR: &str = "year";
    pub const GENRE: &str = "genre";
    pub const TRACK_NO: &str = "track_no";
    pub const DISC_NO: &str = "disc_no";
    pub const COMMENT: &str = "comment";
    pub const LYRICS: &str = "lyrics";
    pub const COVER: &str = "cover";
}

const VARIOUS_ARTISTS: &str = "Various Artists";

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Sort key: a leading English article moves to the end ("The Beatles" → "Beatles, The").
pub fn sort_name(name: &str) -> String {
    let trimmed = name.trim();
    for article in ["The ", "A ", "An "] {
        if let Some(head) = trimmed.get(..article.len())
            && head.eq_ignore_ascii_case(article)
            && trimmed.len() > article.len()
        {
            let rest = &trimmed[article.len()..];
            return format!("{rest}, {}", head.trim_end());
        }
    }
    trimmed.to_owned()
}

/// Unicode case-insensitive name → id lookups for artists, genres and albums.
///
/// SQLite's `NOCASE` only folds ASCII ("KABÁT" ≠ "Kabát"), and the name columns have no
/// index, so lookups go through this cache. Keep one instance per writer connection.
#[derive(Debug, Default)]
pub struct NameCache {
    artists: Option<HashMap<String, i64>>,
    genres: Option<HashMap<String, i64>>,
    albums: Option<HashMap<(String, Option<i64>), i64>>,
}

fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

impl NameCache {
    pub fn artist_id(&mut self, conn: &Connection, name: &str) -> Result<i64> {
        let map = match &mut self.artists {
            Some(m) => m,
            None => self
                .artists
                .insert(load_names(conn, "SELECT id, name FROM artist")?),
        };
        if let Some(&id) = map.get(&key(name)) {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO artist (name, name_sort) VALUES (?1, ?2)",
            params![name, sort_name(name)],
        )?;
        let id = conn.last_insert_rowid();
        map.insert(key(name), id);
        Ok(id)
    }

    pub fn genre_id(&mut self, conn: &Connection, name: &str) -> Result<i64> {
        let map = match &mut self.genres {
            Some(m) => m,
            None => self
                .genres
                .insert(load_names(conn, "SELECT id, name FROM genre")?),
        };
        if let Some(&id) = map.get(&key(name)) {
            return Ok(id);
        }
        conn.execute("INSERT OR IGNORE INTO genre (name) VALUES (?1)", [name])?;
        let id = conn.query_row("SELECT id FROM genre WHERE name = ?1", [name], |r| r.get(0))?;
        map.insert(key(name), id);
        Ok(id)
    }

    fn album_lookup(
        &mut self,
        conn: &Connection,
        title: &str,
        artist: Option<i64>,
    ) -> Result<Option<i64>> {
        let map = match &mut self.albums {
            Some(m) => m,
            None => {
                let mut st = conn.prepare("SELECT id, title, album_artist_id FROM album")?;
                let rows = st.query_map([], |r| {
                    Ok(((key(&r.get::<_, String>(1)?), r.get(2)?), r.get(0)?))
                })?;
                self.albums
                    .insert(rows.collect::<std::result::Result<_, _>>()?)
            }
        };
        Ok(map.get(&(key(title), artist)).copied())
    }

    fn album_inserted(&mut self, title: &str, artist: Option<i64>, id: i64) {
        if let Some(m) = &mut self.albums {
            m.insert((key(title), artist), id);
        }
    }
}

fn load_names(conn: &Connection, sql: &str) -> Result<HashMap<String, i64>> {
    let mut st = conn.prepare(sql)?;
    let rows = st.query_map([], |r| Ok((key(&r.get::<_, String>(1)?), r.get(0)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn album_id(
    conn: &Connection,
    cache: &mut NameCache,
    title: &str,
    album_artist_id: Option<i64>,
    tags: &TrackTags,
) -> Result<i64> {
    let existing = cache.album_lookup(conn, title, album_artist_id)?;
    if let Some(id) = existing {
        // Fill gaps from later tracks of the same album.
        conn.execute(
            "UPDATE album SET year = coalesce(year, ?2), total_discs = coalesce(total_discs, ?3),
                 mbid = coalesce(mbid, ?4),
                 is_compilation = max(is_compilation, ?5)
             WHERE id = ?1",
            params![
                id,
                tags.year,
                tags.disc_total,
                tags.mb_release_id,
                tags.compilation
            ],
        )?;
        return Ok(id);
    }
    // mbid is UNIQUE; a conflicting release id goes unrecorded rather than failing the scan.
    conn.execute(
        "INSERT INTO album (title, title_sort, album_artist_id, year, total_discs, mbid, is_compilation)
         VALUES (?1, ?2, ?3, ?4, ?5,
                 (SELECT ?6 WHERE NOT EXISTS (SELECT 1 FROM album WHERE mbid = ?6)), ?7)",
        params![
            title,
            sort_name(title),
            album_artist_id,
            tags.year,
            tags.disc_total,
            tags.mb_release_id,
            tags.compilation
        ],
    )?;
    let id = conn.last_insert_rowid();
    cache.album_inserted(title, album_artist_id, id);
    Ok(id)
}

/// Fields of `track_id` locked by manual or online edits.
pub fn locked_fields(conn: &Connection, track_id: i64) -> Result<HashSet<String>> {
    let mut st = conn.prepare_cached("SELECT DISTINCT field FROM tag_edit WHERE track_id = ?1")?;
    let rows = st.query_map([track_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Key confidence that marks a key read from tags (detected keys stay below it).
pub const TAG_KEY_CONFIDENCE: f64 = 1.0;

/// Stores the key from the file's tags (in `analysis`, next to detected values). A track
/// whose tag no longer has a key loses a tag key but keeps a detected one.
pub fn set_tag_key(conn: &Connection, track_id: i64, tag: Option<&str>) -> Result<()> {
    match tag.and_then(dap_analysis::parse_key_tag) {
        Some(key) => {
            conn.execute(
                "INSERT INTO analysis (track_id, algo_version, key_camelot, key_confidence, analyzed_at)
                 VALUES (?1, 0, ?2, ?3, ?4)
                 ON CONFLICT (track_id) DO UPDATE SET
                     key_camelot = excluded.key_camelot, key_confidence = excluded.key_confidence",
                params![track_id, key.to_string(), TAG_KEY_CONFIDENCE, now_ms()],
            )?;
        }
        None => {
            conn.execute(
                "UPDATE analysis SET key_camelot = NULL, key_confidence = NULL
                 WHERE track_id = ?1 AND key_confidence >= ?2",
                params![track_id, TAG_KEY_CONFIDENCE],
            )?;
        }
    }
    Ok(())
}

/// Stores the tempo from the file's tags, like [`set_tag_key`] (confidence 1 = from tags).
pub fn set_tag_bpm(conn: &Connection, track_id: i64, tag: Option<&str>) -> Result<()> {
    match tag.and_then(dap_analysis::parse_bpm_tag) {
        Some(bpm) => {
            conn.execute(
                "INSERT INTO analysis (track_id, algo_version, bpm, bpm_confidence, analyzed_at)
                 VALUES (?1, 0, ?2, ?3, ?4)
                 ON CONFLICT (track_id) DO UPDATE SET
                     bpm = excluded.bpm, bpm_confidence = excluded.bpm_confidence
                 -- A detected tempo that overruled a wrong tag stays (see the analyzer).
                 WHERE analysis.bpm IS NULL OR analysis.bpm_confidence >= 1.0",
                params![track_id, bpm, TAG_KEY_CONFIDENCE, now_ms()],
            )?;
        }
        None => {
            conn.execute(
                "UPDATE analysis SET bpm = NULL, bpm_confidence = NULL
                 WHERE track_id = ?1 AND bpm_confidence >= ?2",
                params![track_id, TAG_KEY_CONFIDENCE],
            )?;
        }
    }
    Ok(())
}

/// Sets a track's own cover (unless the user locked it) and gives its album that cover
/// when the album has none yet.
pub fn set_track_cover(conn: &Connection, track_id: i64, cover_id: Option<i64>) -> Result<()> {
    if locked_fields(conn, track_id)?.contains(field::COVER) {
        return Ok(());
    }
    conn.execute(
        "UPDATE track SET cover_id = ?2 WHERE id = ?1",
        params![track_id, cover_id],
    )?;
    if let Some(cover_id) = cover_id {
        conn.execute(
            "UPDATE album SET cover_id = ?2
             WHERE id = (SELECT album_id FROM track WHERE id = ?1) AND cover_id IS NULL",
            params![track_id, cover_id],
        )?;
    }
    Ok(())
}

/// Creates the track for `file_id`, or updates it while respecting locked fields.
/// `fallback_title` is used when the file has no title tag (usually the file name).
pub fn upsert_track(
    conn: &Connection,
    cache: &mut NameCache,
    file_id: i64,
    tags: &TrackTags,
    fallback_title: &str,
) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM track WHERE file_id = ?1 AND start_ms = 0",
            [file_id],
            |r| r.get(0),
        )
        .optional()?;
    let locked = match existing {
        Some(id) => locked_fields(conn, id)?,
        None => HashSet::new(),
    };
    let is_locked = |f: &str| locked.contains(f);

    let title = tags
        .title
        .clone()
        .unwrap_or_else(|| fallback_title.to_owned());

    let album = if is_locked(field::ALBUM) || is_locked(field::ALBUM_ARTIST) {
        None // keep current album_id
    } else {
        match &tags.album {
            Some(album_title) => {
                let album_artist = tags
                    .album_artist
                    .clone()
                    .or_else(|| tags.compilation.then(|| VARIOUS_ARTISTS.to_owned()))
                    .or_else(|| tags.artists.first().cloned());
                let aa_id = album_artist
                    .map(|a| cache.artist_id(conn, &a))
                    .transpose()?;
                Some(Some(album_id(conn, cache, album_title, aa_id, tags)?))
            }
            None => Some(None),
        }
    };

    let track_id = match existing {
        None => {
            conn.execute(
                "INSERT INTO track (file_id, title, title_sort, album_id, track_no, disc_no, year,
                                    comment, mbid, acoustid, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    file_id,
                    title,
                    sort_name(&title),
                    album.flatten(),
                    tags.track_no,
                    tags.disc_no,
                    tags.year,
                    tags.comment,
                    tags.mb_recording_id,
                    tags.acoustid,
                    now_ms()
                ],
            )?;
            conn.last_insert_rowid()
        }
        Some(id) => {
            let set = |col: &str, f: &str, value: &dyn rusqlite::ToSql| -> Result<()> {
                if !is_locked(f) {
                    conn.execute(
                        &format!("UPDATE track SET {col} = ?2 WHERE id = ?1"),
                        params![id, value],
                    )?;
                }
                Ok(())
            };
            set("title", field::TITLE, &title)?;
            set("title_sort", field::TITLE, &sort_name(&title))?;
            set("track_no", field::TRACK_NO, &tags.track_no)?;
            set("disc_no", field::DISC_NO, &tags.disc_no)?;
            set("year", field::YEAR, &tags.year)?;
            set("comment", field::COMMENT, &tags.comment)?;
            if let Some(album_id) = album {
                conn.execute(
                    "UPDATE track SET album_id = ?2 WHERE id = ?1",
                    params![id, album_id],
                )?;
            }
            conn.execute(
                "UPDATE track SET mbid = coalesce(?2, mbid), acoustid = coalesce(?3, acoustid) WHERE id = ?1",
                params![id, tags.mb_recording_id, tags.acoustid],
            )?;
            id
        }
    };

    if !is_locked(field::ARTIST) {
        conn.execute(
            "DELETE FROM track_artist WHERE track_id = ?1 AND role = 'main'",
            [track_id],
        )?;
        for (pos, name) in tags.artists.iter().enumerate() {
            let aid = cache.artist_id(conn, name)?;
            conn.execute(
                "INSERT OR IGNORE INTO track_artist (track_id, artist_id, role, position)
                 VALUES (?1, ?2, 'main', ?3)",
                params![track_id, aid, pos as i64],
            )?;
        }
    }
    if !is_locked(field::GENRE) {
        conn.execute("DELETE FROM track_genre WHERE track_id = ?1", [track_id])?;
        for name in &tags.genres {
            let gid = cache.genre_id(conn, name)?;
            conn.execute(
                "INSERT OR IGNORE INTO track_genre (track_id, genre_id) VALUES (?1, ?2)",
                params![track_id, gid],
            )?;
        }
    }
    if !is_locked(field::LYRICS) {
        // Only embedded lyrics are owned by the scanner; LRCLIB/user lyrics stay.
        match &tags.lyrics {
            Some(body) => {
                conn.execute(
                    "INSERT INTO lyrics (track_id, synced, body, source) VALUES (?1, ?2, ?3, 'embedded')
                     ON CONFLICT (track_id) DO UPDATE SET synced = ?2, body = ?3
                     WHERE source = 'embedded'",
                    params![track_id, is_lrc(body), body],
                )?;
            }
            None => {
                conn.execute(
                    "DELETE FROM lyrics WHERE track_id = ?1 AND source = 'embedded'",
                    [track_id],
                )?;
            }
        }
    }

    reindex_track(conn, track_id)?;
    Ok(track_id)
}

/// True for LRC lyrics with `[mm:ss` timestamps.
fn is_lrc(body: &str) -> bool {
    body.lines().any(|l| {
        let b = l.trim_start().as_bytes();
        b.len() >= 6
            && b[0] == b'['
            && b[1].is_ascii_digit()
            && b[2].is_ascii_digit()
            && b[3] == b':'
    })
}

/// Rebuilds both FTS rows of a track from the catalog tables.
pub fn reindex_track(conn: &Connection, track_id: i64) -> Result<()> {
    let row: Option<(String, String, String, String, String)> = conn
        .query_row(
            "SELECT t.title,
                    coalesce((SELECT group_concat(a.name, ' ') FROM track_artist ta
                              JOIN artist a ON a.id = ta.artist_id WHERE ta.track_id = t.id), ''),
                    coalesce(al.title, '') || ' ' || coalesce(aa.name, ''),
                    coalesce((SELECT group_concat(g.name, ' ') FROM track_genre tg
                              JOIN genre g ON g.id = tg.genre_id WHERE tg.track_id = t.id), ''),
                    coalesce((SELECT body FROM lyrics WHERE track_id = t.id), '')
             FROM track t
             LEFT JOIN album al ON al.id = t.album_id
             LEFT JOIN artist aa ON aa.id = al.album_artist_id
             WHERE t.id = ?1",
            [track_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    conn.execute("DELETE FROM track_fts WHERE rowid = ?1", [track_id])?;
    conn.execute("DELETE FROM track_fts_tri WHERE rowid = ?1", [track_id])?;
    if let Some((title, artists, album, genres, lyrics)) = row {
        conn.execute(
            "INSERT INTO track_fts (rowid, title, artists, album, genres, lyrics) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![track_id, title, artists, album, genres, lyrics],
        )?;
        conn.execute(
            "INSERT INTO track_fts_tri (rowid, title, artists, album) VALUES (?1, ?2, ?3, ?4)",
            params![track_id, title, artists, album],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn sort_names() {
        assert_eq!(sort_name("The Beatles"), "Beatles, The");
        assert_eq!(sort_name("Theatre of Tragedy"), "Theatre of Tragedy");
        assert_eq!(sort_name("A"), "A");
        assert_eq!(sort_name("  Kabát "), "Kabát");
        assert_eq!(sort_name("Ché"), "Ché");
    }

    #[test]
    fn artists_and_genres_are_deduplicated_case_insensitively() {
        let conn = open_in_memory().unwrap();
        let mut cache = NameCache::default();
        let a = cache.artist_id(&conn, "Kabát").unwrap();
        assert_eq!(cache.artist_id(&conn, "KABÁT").unwrap(), a);
        let g = cache.genre_id(&conn, "Rock").unwrap();
        assert_eq!(cache.genre_id(&conn, "rock").unwrap(), g);
        // A fresh cache sees what earlier ones wrote.
        assert_eq!(NameCache::default().artist_id(&conn, "kabát").unwrap(), a);
    }

    #[test]
    fn lrc_detection() {
        assert!(is_lrc("[00:12.30] Hello"));
        assert!(!is_lrc("Hello\nWorld"));
    }
}
