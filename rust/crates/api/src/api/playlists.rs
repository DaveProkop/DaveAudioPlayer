//! Playlists: listing, editing and their tracks. Edits run on the database writer thread.

use anyhow::{Result, anyhow};
use dap_library::playlists;
use dap_library::smart::{self, SmartRules};

use super::library::{TrackInfo, current_covers_dir, query_track, with_reader};
use super::player::write;

/// A playlist with its size, length and the cover of its first track.
pub struct PlaylistInfo {
    pub id: i64,
    pub name: String,
    pub track_count: u32,
    pub duration_ms: i64,
    pub cover_medium: Option<String>,
    /// Rules (JSON) of a smart playlist; `None` for a manual one.
    pub smart_rules: Option<String>,
}

/// One entry of a playlist; `item_id` identifies it for moving and removing (a track may
/// be in a playlist more than once).
pub struct PlaylistTrack {
    pub item_id: i64,
    pub track: TrackInfo,
}

fn db<T>(r: std::result::Result<T, dap_library::DbError>) -> Result<T> {
    r.map_err(|e| anyhow!(e.to_string()))
}

pub fn playlists() -> Result<Vec<PlaylistInfo>> {
    let covers = current_covers_dir()?;
    let list = with_reader(|c| Ok(playlists::list(c)))?;
    let list = db(list)?;
    with_reader(|c| {
        list.into_iter()
            .map(|p| {
                let (mut count, mut duration) = (p.track_count, p.duration_ms);
                // The first track that has a cover stands for the playlist.
                let first: Option<i64> = match p.rules.as_deref().map(SmartRules::from_json) {
                    Some(Ok(rules)) => {
                        let ids = smart::evaluate(c, &rules).unwrap_or_default();
                        count = ids.len() as u32;
                        duration = 0;
                        let mut first = None;
                        for &id in &ids {
                            if let Some(t) = query_track(c, &covers, id)? {
                                duration += t.duration_ms.unwrap_or(0);
                                if first.is_none() && t.cover_medium.is_some() {
                                    first = Some(id);
                                }
                            }
                        }
                        first
                    }
                    Some(Err(_)) => None,
                    None => c
                        .query_row(
                            "SELECT i.track_id FROM playlist_item i
                             JOIN track t ON t.id = i.track_id
                             LEFT JOIN album al ON al.id = t.album_id
                             WHERE i.playlist_id = ?1 AND coalesce(t.cover_id, al.cover_id) IS NOT NULL
                             ORDER BY i.pos LIMIT 1",
                            [p.id],
                            |r| r.get(0),
                        )
                        .ok(),
                };
                let cover_medium = match first {
                    Some(id) => query_track(c, &covers, id)?.and_then(|t| t.cover_medium),
                    None => None,
                };
                Ok(PlaylistInfo {
                    id: p.id,
                    name: p.name,
                    track_count: count,
                    duration_ms: duration,
                    cover_medium,
                    smart_rules: p.rules,
                })
            })
            .collect()
    })
}

/// The tracks of a playlist in order (files that disappeared are left out). A smart
/// playlist is evaluated now; its entries have item id 0 (they cannot be moved).
pub fn playlist_tracks(playlist_id: i64) -> Result<Vec<PlaylistTrack>> {
    let covers = current_covers_dir()?;
    let rules: Option<String> = with_reader(|c| {
        use rusqlite::OptionalExtension;
        c.query_row(
            "SELECT rules FROM playlist WHERE id = ?1 AND kind = 'smart'",
            [playlist_id],
            |r| r.get(0),
        )
        .optional()
        .map(Option::flatten)
    })?;
    let entries = match rules {
        Some(json) => {
            let rules = db(SmartRules::from_json(&json))?;
            db(with_reader(|c| Ok(smart::evaluate(c, &rules)))?)?
                .into_iter()
                .map(|track_id| playlists::PlaylistEntry { item_id: 0, track_id })
                .collect()
        }
        None => db(with_reader(|c| Ok(playlists::entries(c, playlist_id)))?)?,
    };
    with_reader(|c| {
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            match query_track(c, &covers, e.track_id)? {
                Some(track) if !track.missing => out.push(PlaylistTrack {
                    item_id: e.item_id,
                    track,
                }),
                _ => {}
            }
        }
        Ok(out)
    })
}

/// Creates a playlist, optionally filled with tracks (e.g. the current queue); returns its id.
pub fn create_playlist(name: String, track_ids: Vec<i64>) -> Result<i64> {
    db(write(move |c| {
        let id = playlists::create(c, &name)?;
        playlists::add_tracks(c, id, &track_ids)?;
        Ok(id)
    })?)
}

/// Creates a smart playlist from rules (JSON: `match_all`, `rules` of `field`/`op`/`value`,
/// `order`, `limit`); returns its id.
pub fn create_smart_playlist(name: String, rules_json: String) -> Result<i64> {
    db(write(move |c| playlists::create_smart(c, &name, &rules_json))?)
}

pub fn set_smart_rules(playlist_id: i64, rules_json: String) -> Result<()> {
    db(write(move |c| playlists::set_rules(c, playlist_id, &rules_json))?)
}

/// How many tracks rules select now (for the editor).
pub fn smart_preview_count(rules_json: String) -> Result<u32> {
    let rules = db(SmartRules::from_json(&rules_json))?;
    Ok(db(with_reader(|c| Ok(smart::evaluate(c, &rules)))?)?.len() as u32)
}

pub fn rename_playlist(playlist_id: i64, name: String) -> Result<()> {
    db(write(move |c| playlists::rename(c, playlist_id, &name))?)
}

/// Deletes a playlist (its tracks stay in the library).
pub fn delete_playlist(playlist_id: i64) -> Result<()> {
    db(write(move |c| playlists::delete(c, playlist_id))?)
}

/// Appends tracks to a playlist.
pub fn add_to_playlist(playlist_id: i64, track_ids: Vec<i64>) -> Result<()> {
    db(write(move |c| playlists::add_tracks(c, playlist_id, &track_ids))?)
}

pub fn remove_from_playlist(item_ids: Vec<i64>) -> Result<()> {
    db(write(move |c| playlists::remove_items(c, &item_ids))?)
}

/// Moves a playlist entry to index `to` (in the list without it).
pub fn move_playlist_item(item_id: i64, to: u32) -> Result<()> {
    db(write(move |c| playlists::move_item(c, item_id, to as usize))?)
}
