//! Scrobbling to ListenBrainz: finished plays are queued in `scrobble` (by the engine)
//! and sent in batches by a background thread, retried while offline.

use std::path::PathBuf;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use dap_engine::DbWriter;
use dap_metadata::listenbrainz::{self, Listen};
use rusqlite::{Connection, OptionalExtension, params};

use super::library::with_reader;
use super::player::write;

/// Where the user's ListenBrainz token is stored.
pub(crate) const TOKEN_SETTING: &str = "scrobble.listenbrainz_token";
const SERVICE: &str = "listenbrainz";
const BATCH: i64 = 50;
const MAX_ATTEMPTS: i64 = 10;
const EVERY: Duration = Duration::from_secs(60);

/// Wakes the sender (a new token, "send now"); dropped to stop it.
static WAKE: Mutex<Option<Sender<()>>> = Mutex::new(None);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub struct ScrobbleStatus {
    /// A token is set.
    pub configured: bool,
    pub pending: u32,
    pub sent: u32,
    /// The last failure, if the last attempt failed.
    pub last_error: Option<String>,
}

/// Starts the sender thread (with the engine).
pub(crate) fn start(db_path: PathBuf, db: DbWriter) -> Option<JoinHandle<()>> {
    let (tx, rx) = bounded(1);
    if let Ok(mut wake) = WAKE.lock() {
        *wake = Some(tx);
    }
    std::thread::Builder::new()
        .name("scrobbler".into())
        .spawn(move || run(&db_path, &db, &rx))
        .ok()
}

/// Stops the sender thread; join the handle afterwards.
pub(crate) fn stop() {
    if let Ok(mut wake) = WAKE.lock() {
        *wake = None;
    }
}

fn wake() {
    if let Ok(wake) = WAKE.lock() {
        if let Some(tx) = wake.as_ref() {
            let _ = tx.try_send(());
        }
    }
}

fn set_error(e: Option<String>) {
    if let Ok(mut last) = LAST_ERROR.lock() {
        *last = e;
    }
}

fn run(db_path: &std::path::Path, db: &DbWriter, wake: &Receiver<()>) {
    // A short pause first: startup has more urgent work.
    if let Err(RecvTimeoutError::Disconnected) = wake.recv_timeout(Duration::from_secs(5)) {
        return;
    }
    let Ok(reader) = dap_library::db::open_reader(db_path) else {
        return;
    };
    loop {
        send_pending(&reader, db);
        match wake.recv_timeout(EVERY) {
            Err(RecvTimeoutError::Disconnected) => return,
            _ => continue,
        }
    }
}

fn token(c: &Connection) -> Option<String> {
    c.query_row("SELECT value FROM setting WHERE key = ?1", [TOKEN_SETTING], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .ok()
    .flatten()
    .filter(|t| !t.trim().is_empty())
}

fn send_pending(reader: &Connection, db: &DbWriter) {
    let Some(token) = token(reader) else {
        return;
    };
    let pending: Vec<(i64, Listen)> = reader
        .prepare_cached(
            "SELECT s.history_id, h.started_at / 1000, t.title,
                    coalesce((SELECT group_concat(a.name, ', ') FROM track_artist ta
                              JOIN artist a ON a.id = ta.artist_id
                              WHERE ta.track_id = t.id AND ta.role = 'main'), ''),
                    al.title, f.duration_ms
             FROM scrobble s
             JOIN play_history h ON h.id = s.history_id
             JOIN track t ON t.id = h.track_id
             JOIN file f ON f.id = t.file_id
             LEFT JOIN album al ON al.id = t.album_id
             WHERE s.service = ?1 AND s.status = 'pending' AND s.attempts < ?2
             ORDER BY h.started_at LIMIT ?3",
        )
        .and_then(|mut st| {
            st.query_map(params![SERVICE, MAX_ATTEMPTS, BATCH], |r| {
                Ok((
                    r.get(0)?,
                    Listen {
                        listened_at: r.get(1)?,
                        title: r.get(2)?,
                        artist: r.get(3)?,
                        album: r.get(4)?,
                        duration_ms: r.get(5)?,
                    },
                ))
            })?
            .collect()
        })
        .unwrap_or_default();
    // ListenBrainz needs an artist; such plays cannot be scrobbled.
    let (ok, unusable): (Vec<_>, Vec<_>) = pending.into_iter().partition(|(_, l)| !l.artist.is_empty());
    let mark = |ids: Vec<i64>, status: &'static str| {
        db.run(move |c| {
            for id in ids {
                let _ = c.execute(
                    "UPDATE scrobble SET status = ?3, attempts = attempts + 1
                     WHERE history_id = ?1 AND service = ?2",
                    params![id, SERVICE, status],
                );
            }
        });
    };
    mark(unusable.into_iter().map(|(id, _)| id).collect(), "failed");
    if ok.is_empty() {
        return;
    }
    let ids: Vec<i64> = ok.iter().map(|(id, _)| *id).collect();
    let listens: Vec<Listen> = ok.into_iter().map(|(_, l)| l).collect();
    let result = dap_metadata::client().and_then(|c| listenbrainz::submit(&c, &token, &listens));
    match result {
        Ok(()) => {
            set_error(None);
            mark(ids, "sent");
        }
        Err(e) => {
            set_error(Some(e.to_string()));
            // Still pending: counted, retried on the next round.
            mark(ids, "pending");
        }
    }
}

/// Sets (or with an empty token removes) the ListenBrainz token. The token is checked
/// first; returns the ListenBrainz user name.
pub fn set_listenbrainz_token(token: String) -> Result<Option<String>> {
    let token = token.trim().to_string();
    if token.is_empty() {
        write(|c| c.execute("DELETE FROM setting WHERE key = ?1", [TOKEN_SETTING]))??;
        set_error(None);
        return Ok(None);
    }
    let client = dap_metadata::client().map_err(|e| anyhow!(e.to_string()))?;
    let user = listenbrainz::validate_token(&client, &token)
        .map_err(|e| anyhow!(e.to_string()))?
        .ok_or_else(|| anyhow!("ListenBrainz token neplatí"))?;
    write(move |c| {
        c.execute(
            "INSERT INTO setting (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = ?2",
            params![TOKEN_SETTING, token],
        )
    })??;
    wake();
    Ok(Some(user))
}

/// Sends queued plays now instead of within the minute.
pub fn scrobble_now() {
    wake();
}

pub fn scrobble_status() -> Result<ScrobbleStatus> {
    let (configured, pending, sent) = with_reader(|c| {
        Ok((
            token(c).is_some(),
            c.query_row(
                "SELECT count(*) FROM scrobble WHERE service = ?1 AND status = 'pending'",
                [SERVICE],
                |r| r.get(0),
            )?,
            c.query_row(
                "SELECT count(*) FROM scrobble WHERE service = ?1 AND status = 'sent'",
                [SERVICE],
                |r| r.get(0),
            )?,
        ))
    })?;
    Ok(ScrobbleStatus {
        configured,
        pending,
        sent,
        last_error: LAST_ERROR.lock().ok().and_then(|e| e.clone()),
    })
}
