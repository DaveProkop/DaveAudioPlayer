//! Lyrics and other metadata from the files' surroundings and online services.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use dap_library::catalog::now_ms;
use dap_metadata::{lrclib, parse_lrc};
use rusqlite::{OptionalExtension, params};

use super::library::with_reader;
use super::player::write;

/// A failed lookup is not repeated for this long.
const MISS_TTL_MS: i64 = 30 * 24 * 3600 * 1000;

pub struct LyricLine {
    /// Start in ms, or -1 for plain (untimed) lyrics.
    pub ms: i64,
    pub text: String,
}

pub struct LyricsDto {
    /// Lines carry times and can follow the playback.
    pub synced: bool,
    pub lines: Vec<LyricLine>,
    /// embedded | lrc_file | lrclib | user
    pub source: String,
}

/// What a lyrics lookup needs to know about a track.
struct TrackFacts {
    path: PathBuf,
    title: String,
    artist: String,
    album: String,
    duration_ms: i64,
}

fn facts(track_id: i64) -> Result<Option<TrackFacts>> {
    with_reader(|c| {
        c.query_row(
            "SELECT r.path, f.rel_path, t.title,
                    coalesce((SELECT group_concat(a.name, ', ') FROM track_artist ta
                              JOIN artist a ON a.id = ta.artist_id
                              WHERE ta.track_id = t.id AND ta.role = 'main'), ''),
                    coalesce(al.title, ''), coalesce(f.duration_ms, 0)
             FROM track t JOIN file f ON f.id = t.file_id
             JOIN library_root r ON r.id = f.root_id
             LEFT JOIN album al ON al.id = t.album_id
             WHERE t.id = ?1",
            [track_id],
            |r| {
                Ok(TrackFacts {
                    path: PathBuf::from(r.get::<_, String>(0)?).join(r.get::<_, String>(1)?),
                    title: r.get(2)?,
                    artist: r.get(3)?,
                    album: r.get(4)?,
                    duration_ms: r.get(5)?,
                })
            },
        )
        .optional()
    })
}

fn to_dto(body: &str, source: String) -> LyricsDto {
    let timed = parse_lrc(body);
    if timed.is_empty() {
        LyricsDto {
            synced: false,
            lines: body
                .lines()
                .map(|l| LyricLine {
                    ms: -1,
                    text: l.trim().to_string(),
                })
                .collect(),
            source,
        }
    } else {
        LyricsDto {
            synced: true,
            lines: timed
                .into_iter()
                .map(|l| LyricLine {
                    ms: l.ms as i64,
                    text: l.text,
                })
                .collect(),
            source,
        }
    }
}

fn store(track_id: i64, body: String, source: &'static str) -> Result<()> {
    let synced = !parse_lrc(&body).is_empty();
    write(move |c| {
        c.execute(
            "INSERT INTO lyrics (track_id, synced, body, source) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (track_id) DO UPDATE SET synced = ?2, body = ?3, source = ?4",
            params![track_id, synced, body, source],
        )
    })??;
    Ok(())
}

/// Lyrics of a track: stored ones (from the file's tags or an earlier lookup), else an
/// `.lrc` file next to the audio file, else (when `online`) LRCLIB. Found lyrics are
/// stored; an online miss is remembered for 30 days. The `ui.lyrics_online` setting
/// (`false`) turns the online part off.
pub fn lyrics(track_id: i64, online: bool) -> Result<Option<LyricsDto>> {
    let stored: Option<(String, String)> = with_reader(|c| {
        c.query_row(
            "SELECT body, source FROM lyrics WHERE track_id = ?1",
            [track_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
    })?;
    if let Some((body, source)) = stored {
        return Ok(Some(to_dto(&body, source)));
    }
    let Some(track) = facts(track_id)? else {
        return Ok(None);
    };
    let sidecar = track.path.with_extension("lrc");
    if let Ok(bytes) = std::fs::read(&sidecar) {
        let body = String::from_utf8_lossy(&bytes).into_owned();
        store(track_id, body.clone(), "lrc_file")?;
        return Ok(Some(to_dto(&body, "lrc_file".into())));
    }
    // The user may have turned online lookups off (settings).
    let allowed = with_reader(|c| {
        c.query_row(
            "SELECT value FROM setting WHERE key = 'ui.lyrics_online'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
    })?
    .is_none_or(|v| v != "false");
    if !online || !allowed || track.title.is_empty() || track.artist.is_empty() {
        return Ok(None);
    }
    let key = track_id.to_string();
    let missed_recently = with_reader(|c| {
        c.query_row(
            "SELECT fetched_at FROM metadata_cache WHERE provider = 'lrclib' AND lookup_key = ?1",
            [&key],
            |r| r.get::<_, i64>(0),
        )
        .optional()
    })?
    .is_some_and(|at| now_ms() - at < MISS_TTL_MS);
    if missed_recently {
        return Ok(None);
    }
    let client = dap_metadata::client().map_err(|e| anyhow!(e.to_string()))?;
    let found = lrclib::lookup(
        &client,
        &track.artist,
        &track.title,
        &track.album,
        track.duration_ms as f64 / 1000.0,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    match found.and_then(|l| l.synced.or(l.plain)).filter(|b| !b.trim().is_empty()) {
        Some(body) => {
            store(track_id, body.clone(), "lrclib")?;
            Ok(Some(to_dto(&body, "lrclib".into())))
        }
        None => {
            write(move |c| {
                c.execute(
                    "INSERT INTO metadata_cache (provider, lookup_key, response, fetched_at)
                     VALUES ('lrclib', ?1, '{}', ?2)
                     ON CONFLICT (provider, lookup_key) DO UPDATE SET fetched_at = ?2",
                    params![key, now_ms()],
                )
            })??;
            Ok(None)
        }
    }
}

/// The editable facts of a track.
pub struct TrackFieldsDto {
    pub title: String,
    pub artists: Vec<String>,
    /// Empty = no album.
    pub album: String,
    pub album_artist: String,
    pub year: Option<i64>,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub genres: Vec<String>,
}

pub fn track_fields(track_id: i64) -> Result<Option<TrackFieldsDto>> {
    let f = with_reader(|c| {
        dap_library::catalog::track_fields(c, track_id).map_err(|e| match e {
            dap_library::DbError::Sqlite(e) => e,
            other => rusqlite::Error::ToSqlConversionFailure(other.to_string().into()),
        })
    })?;
    Ok(f.map(|f| TrackFieldsDto {
        title: f.title,
        artists: f.artists,
        album: f.album,
        album_artist: f.album_artist,
        year: f.year,
        track_no: f.track_no,
        disc_no: f.disc_no,
        genres: f.genres,
    }))
}

/// Saves a manual edit. Changed fields are locked, so rescans keep them. With
/// `write_to_file` they are also written into the file's tags; returns the error text
/// when that failed (the edit itself is kept either way).
pub fn edit_track(
    track_id: i64,
    fields: TrackFieldsDto,
    write_to_file: bool,
) -> Result<Option<String>> {
    let new = dap_library::catalog::TrackFields {
        title: fields.title,
        artists: fields.artists,
        album: fields.album,
        album_artist: fields.album_artist,
        year: fields.year,
        track_no: fields.track_no,
        disc_no: fields.disc_no,
        genres: fields.genres,
    };
    let status = if write_to_file { "pending_write" } else { "db_only" };
    let changes = write(move |c| dap_library::catalog::edit_track(c, track_id, &new, status))?
        .map_err(|e| anyhow!(e.to_string()))?;
    if !write_to_file || changes.is_empty() {
        return Ok(None);
    }
    let Some(track) = facts(track_id)? else {
        return Ok(None);
    };
    let values: Vec<(&str, Option<String>)> =
        changes.iter().map(|(f, _, new)| (*f, new.clone())).collect();
    let error = dap_tags::write_fields(&track.path, &values)
        .err()
        .map(|e| e.to_string());
    let status_error = error.clone();
    write(move |c| dap_library::catalog::set_edit_status(c, track_id, status_error.as_deref()))?
        .map_err(|e| anyhow!(e.to_string()))?;
    Ok(error)
}

/// An album as far as an online lookup needs it.
pub struct AlbumRef {
    pub id: i64,
    pub title: String,
    pub artist: String,
}

/// Albums (with playable tracks) that have no cover yet.
pub fn albums_without_cover() -> Result<Vec<AlbumRef>> {
    with_reader(|c| {
        let mut st = c.prepare_cached(
            "SELECT al.id, al.title,
                    coalesce(aa.name, (SELECT a.name FROM track t2
                                       JOIN track_artist ta ON ta.track_id = t2.id AND ta.role = 'main'
                                       JOIN artist a ON a.id = ta.artist_id
                                       WHERE t2.album_id = al.id LIMIT 1), '')
             FROM album al LEFT JOIN artist aa ON aa.id = al.album_artist_id
             WHERE al.cover_id IS NULL
               AND EXISTS (SELECT 1 FROM track t JOIN file f ON f.id = t.file_id
                           WHERE t.album_id = al.id AND f.missing = 0)
             ORDER BY al.title_sort",
        )?;
        let rows = st.query_map([], |r| {
            Ok(AlbumRef {
                id: r.get(0)?,
                title: r.get(1)?,
                artist: r.get(2)?,
            })
        })?;
        rows.collect()
    })
}

/// Looks an album's front cover up on MusicBrainz / Cover Art Archive and gives it to the
/// album. Returns whether a cover was found. A miss is remembered for 30 days unless
/// `force`.
pub fn find_album_cover(album_id: i64, force: bool) -> Result<bool> {
    let album: Option<(String, String, Option<String>)> = with_reader(|c| {
        c.query_row(
            "SELECT al.title,
                    coalesce(aa.name, (SELECT a.name FROM track t2
                                       JOIN track_artist ta ON ta.track_id = t2.id AND ta.role = 'main'
                                       JOIN artist a ON a.id = ta.artist_id
                                       WHERE t2.album_id = al.id LIMIT 1), ''),
                    al.mbid
             FROM album al LEFT JOIN artist aa ON aa.id = al.album_artist_id
             WHERE al.id = ?1",
            [album_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
    })?;
    let Some((title, artist, known_mbid)) = album else {
        return Ok(false);
    };
    let key = album_id.to_string();
    if !force {
        let missed = with_reader(|c| {
            c.query_row(
                "SELECT fetched_at FROM metadata_cache WHERE provider = 'caa' AND lookup_key = ?1",
                [&key],
                |r| r.get::<_, i64>(0),
            )
            .optional()
        })?
        .is_some_and(|at| now_ms() - at < MISS_TTL_MS);
        if missed {
            return Ok(false);
        }
    }
    let client = dap_metadata::client().map_err(|e| anyhow!(e.to_string()))?;
    let net = |e: dap_metadata::MetadataError| anyhow!(e.to_string());
    let mbid = match known_mbid {
        Some(id) => Some(id),
        None if !artist.is_empty() => {
            dap_metadata::musicbrainz::find_release(&client, &artist, &title).map_err(net)?
        }
        None => None,
    };
    let image = match &mbid {
        Some(id) => dap_metadata::musicbrainz::front_cover(&client, id).map_err(net)?,
        None => None,
    };
    let dir = super::library::current_covers_dir()?;
    let found = match image {
        Some(bytes) => write(move |c| -> std::result::Result<bool, dap_library::DbError> {
            let Some(cover_id) = dap_library::covers::store_cover(c, &dir, &bytes, "coverartarchive")?
            else {
                return Ok(false);
            };
            // mbid is unique: another album may hold it already.
            c.execute(
                "UPDATE album SET cover_id = ?2,
                     mbid = coalesce(mbid, (SELECT ?3 WHERE NOT EXISTS
                                            (SELECT 1 FROM album WHERE mbid = ?3)))
                 WHERE id = ?1",
                params![album_id, cover_id, mbid],
            )?;
            Ok(true)
        })?
        .map_err(|e| anyhow!(e.to_string()))?,
        None => false,
    };
    if !found {
        write(move |c| {
            c.execute(
                "INSERT INTO metadata_cache (provider, lookup_key, response, fetched_at)
                 VALUES ('caa', ?1, '{}', ?2)
                 ON CONFLICT (provider, lookup_key) DO UPDATE SET fetched_at = ?2",
                params![key, now_ms()],
            )
        })??;
    }
    Ok(found)
}
