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
