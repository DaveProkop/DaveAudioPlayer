//! Manual playlists: `playlist` and its ordered `playlist_item`s.
//!
//! Item order is a `REAL` position: adding appends after the last one, moving writes one
//! row with a value between its new neighbours (renumbering only when the gap is tiny).
//! Write functions run on the writer connection; the list functions on any.

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog::now_ms;
use crate::db::Result;

/// Below this spacing the items of a playlist are renumbered before a move.
const MIN_GAP: f64 = 1e-9;

/// A playlist with its size and total length (playable tracks only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistSummary {
    pub id: i64,
    pub name: String,
    pub track_count: u32,
    pub duration_ms: i64,
    /// Rules (JSON) of a smart playlist; its counts above are 0, see [`crate::smart`].
    pub rules: Option<String>,
}

/// One entry of a playlist (the same track may appear more than once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaylistEntry {
    pub item_id: i64,
    pub track_id: i64,
}

/// Creates an empty playlist at the end of the list and returns its id.
pub fn create(conn: &Connection, name: &str) -> Result<i64> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO playlist (name, sort_order, created_at, updated_at)
         VALUES (?1, coalesce((SELECT max(sort_order) FROM playlist), 0) + 1, ?2, ?2)",
        params![name.trim(), now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Creates a smart playlist from rules (JSON, [`crate::smart::SmartRules`]).
pub fn create_smart(conn: &Connection, name: &str, rules: &str) -> Result<i64> {
    crate::smart::SmartRules::from_json(rules)?;
    let id = create(conn, name)?;
    conn.execute(
        "UPDATE playlist SET kind = 'smart', rules = ?2 WHERE id = ?1",
        params![id, rules],
    )?;
    Ok(id)
}

/// Replaces the rules of a smart playlist.
pub fn set_rules(conn: &Connection, id: i64, rules: &str) -> Result<()> {
    crate::smart::SmartRules::from_json(rules)?;
    conn.execute(
        "UPDATE playlist SET rules = ?2, updated_at = ?3 WHERE id = ?1 AND kind = 'smart'",
        params![id, rules, now_ms()],
    )?;
    Ok(())
}

pub fn rename(conn: &Connection, id: i64, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE playlist SET name = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, name.trim(), now_ms()],
    )?;
    Ok(())
}

/// Deletes a playlist with its items (the tracks stay in the library).
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM playlist WHERE id = ?1", [id])?;
    Ok(())
}

/// All playlists in their list order.
pub fn list(conn: &Connection) -> Result<Vec<PlaylistSummary>> {
    let mut st = conn.prepare_cached(
        "SELECT p.id, p.name, count(f.id), coalesce(sum(f.duration_ms), 0),
                CASE WHEN p.kind = 'smart' THEN p.rules END
         FROM playlist p
         LEFT JOIN playlist_item i ON i.playlist_id = p.id
         LEFT JOIN track t ON t.id = i.track_id
         LEFT JOIN file f ON f.id = t.file_id AND f.missing = 0
         GROUP BY p.id
         ORDER BY p.sort_order, p.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(PlaylistSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            track_count: r.get(2)?,
            duration_ms: r.get(3)?,
            rules: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The entries of a playlist in order.
pub fn entries(conn: &Connection, playlist_id: i64) -> Result<Vec<PlaylistEntry>> {
    let mut st = conn.prepare_cached(
        "SELECT id, track_id FROM playlist_item WHERE playlist_id = ?1 ORDER BY pos, id",
    )?;
    let rows = st.query_map([playlist_id], |r| {
        Ok(PlaylistEntry {
            item_id: r.get(0)?,
            track_id: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Appends tracks to a playlist, in the given order.
pub fn add_tracks(conn: &mut Connection, playlist_id: i64, track_ids: &[i64]) -> Result<()> {
    let tx = conn.transaction()?;
    let last: f64 = tx.query_row(
        "SELECT coalesce(max(pos), 0) FROM playlist_item WHERE playlist_id = ?1",
        [playlist_id],
        |r| r.get(0),
    )?;
    let now = now_ms();
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO playlist_item (playlist_id, track_id, pos, added_at)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (i, &track_id) in track_ids.iter().enumerate() {
            st.execute(params![playlist_id, track_id, last + 1.0 + i as f64, now])?;
        }
    }
    touch(&tx, playlist_id)?;
    tx.commit()?;
    Ok(())
}

/// Removes entries (by item id) from their playlists.
pub fn remove_items(conn: &mut Connection, item_ids: &[i64]) -> Result<()> {
    let tx = conn.transaction()?;
    for &id in item_ids {
        let playlist: Option<i64> = tx
            .query_row("SELECT playlist_id FROM playlist_item WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        tx.execute("DELETE FROM playlist_item WHERE id = ?1", [id])?;
        if let Some(p) = playlist {
            touch(&tx, p)?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Moves an entry to index `to` (counted in the list without the moved entry).
pub fn move_item(conn: &mut Connection, item_id: i64, to: usize) -> Result<()> {
    let tx = conn.transaction()?;
    let Some(playlist_id): Option<i64> = tx
        .query_row("SELECT playlist_id FROM playlist_item WHERE id = ?1", [item_id], |r| {
            r.get(0)
        })
        .optional()?
    else {
        return Ok(());
    };
    let mut others = positions(&tx, playlist_id, item_id)?;
    let gap_ok = |o: &[(i64, f64)]| o.windows(2).all(|w| w[1].1 - w[0].1 > MIN_GAP);
    if !gap_ok(&others) {
        for (i, (id, pos)) in others.iter_mut().enumerate() {
            *pos = (i + 1) as f64;
            tx.execute("UPDATE playlist_item SET pos = ?2 WHERE id = ?1", params![*id, *pos])?;
        }
    }
    let to = to.min(others.len());
    let pos = match (to.checked_sub(1).map(|i| others[i].1), others.get(to).map(|o| o.1)) {
        (None, None) => 1.0,
        (None, Some(next)) => next - 1.0,
        (Some(prev), None) => prev + 1.0,
        (Some(prev), Some(next)) => (prev + next) / 2.0,
    };
    tx.execute("UPDATE playlist_item SET pos = ?2 WHERE id = ?1", params![item_id, pos])?;
    touch(&tx, playlist_id)?;
    tx.commit()?;
    Ok(())
}

/// Positions of a playlist's entries except `skip`, in order.
fn positions(conn: &Connection, playlist_id: i64, skip: i64) -> Result<Vec<(i64, f64)>> {
    let mut st = conn.prepare_cached(
        "SELECT id, pos FROM playlist_item WHERE playlist_id = ?1 AND id != ?2 ORDER BY pos, id",
    )?;
    let rows = st.query_map(params![playlist_id, skip], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn touch(conn: &Connection, playlist_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE playlist SET updated_at = ?2 WHERE id = ?1",
        params![playlist_id, now_ms()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    /// A library with `n` playable tracks of 1 s each; returns their ids.
    fn library(conn: &Connection, n: usize) -> Vec<i64> {
        conn.execute(
            "INSERT INTO library_root (path, added_at) VALUES ('/music', 0)",
            [],
        )
        .unwrap();
        (0..n)
            .map(|i| {
                conn.execute(
                    "INSERT INTO file (root_id, rel_path, size, mtime, format, scanned_at,
                                       duration_ms)
                     VALUES (1, ?1, 0, 0, 'mp3', 0, 1000)",
                    [format!("{i}.mp3")],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO track (file_id, title, title_sort, added_at)
                     VALUES (?1, 't', 't', 0)",
                    [conn.last_insert_rowid()],
                )
                .unwrap();
                conn.last_insert_rowid()
            })
            .collect()
    }

    fn tracks(conn: &Connection, playlist: i64) -> Vec<i64> {
        entries(conn, playlist).unwrap().iter().map(|e| e.track_id).collect()
    }

    #[test]
    fn create_fill_reorder_and_delete() {
        let mut conn = open_in_memory().unwrap();
        let t = library(&conn, 4);
        let a = create(&conn, " Na běhání ").unwrap();
        let b = create(&conn, "Večer").unwrap();
        add_tracks(&mut conn, a, &[t[0], t[1], t[2]]).unwrap();
        add_tracks(&mut conn, a, &[t[0]]).unwrap(); // the same track twice is fine
        assert_eq!(tracks(&conn, a), [t[0], t[1], t[2], t[0]]);

        let list_now = list(&conn).unwrap();
        assert_eq!(list_now[0].name, "Na běhání");
        assert_eq!((list_now[0].track_count, list_now[0].duration_ms), (4, 4000));
        assert_eq!((list_now[1].id, list_now[1].track_count), (b, 0));

        let e = entries(&conn, a).unwrap();
        move_item(&mut conn, e[2].item_id, 0).unwrap();
        assert_eq!(tracks(&conn, a), [t[2], t[0], t[1], t[0]]);
        move_item(&mut conn, e[2].item_id, 9).unwrap();
        assert_eq!(tracks(&conn, a), [t[0], t[1], t[0], t[2]]);

        remove_items(&mut conn, &[e[0].item_id]).unwrap();
        assert_eq!(tracks(&conn, a), [t[1], t[0], t[2]]);

        rename(&conn, b, "Noc").unwrap();
        delete(&conn, a).unwrap();
        let left = list(&conn).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "Noc");
        let items: i64 = conn
            .query_row("SELECT count(*) FROM playlist_item", [], |r| r.get(0))
            .unwrap();
        assert_eq!(items, 0, "items go with their playlist");
    }

    #[test]
    fn many_moves_to_the_same_spot_renumber() {
        let mut conn = open_in_memory().unwrap();
        let t = library(&conn, 3);
        let p = create(&conn, "p").unwrap();
        add_tracks(&mut conn, p, &t).unwrap();
        let e = entries(&conn, p).unwrap();
        // Alternately move the last two between the first two: the gap halves each time.
        for i in 0..80 {
            move_item(&mut conn, e[1 + i % 2].item_id, 1).unwrap();
        }
        let got = tracks(&conn, p);
        assert_eq!(got[0], t[0]);
        assert_eq!(got.len(), 3);
    }
}
