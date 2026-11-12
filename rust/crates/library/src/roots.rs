//! Library folders (roots). Roots never overlap: a file belongs to exactly one root, or it
//! would show up twice in the library.
//!
//! - adding a folder inside an existing root changes nothing
//! - adding a parent of existing roots takes their files over (tracks keep their ids, so
//!   ratings, play counts and queue entries stay)
//! - [`repair_overlapping_roots`] merges roots that overlap from before this rule

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog::now_ms;
use crate::db::Result;

/// Path components, separators of both kinds accepted.
fn components(p: &str) -> Vec<&str> {
    p.split(['/', '\\']).filter(|c| !c.is_empty()).collect()
}

fn same_component(a: &str, b: &str) -> bool {
    // Windows paths are case-insensitive.
    if cfg!(windows) { a.to_lowercase() == b.to_lowercase() } else { a == b }
}

/// `child` relative to `parent` ("" when equal, "a/b" when inside), `None` when outside.
pub fn relative_to(child: &str, parent: &str) -> Option<String> {
    let c = components(child);
    let p = components(parent);
    // An absolute Unix path and a relative one must not match.
    if child.starts_with('/') != parent.starts_with('/') || p.len() > c.len() {
        return None;
    }
    if c.iter().zip(&p).all(|(a, b)| same_component(a, b)) {
        Some(c[p.len()..].join("/"))
    } else {
        None
    }
}

fn roots(conn: &Connection) -> Result<Vec<(i64, String)>> {
    let mut st = conn.prepare("SELECT id, path FROM library_root ORDER BY id")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Registers a music folder and returns its root id. A folder inside an existing root
/// returns that root; a parent of existing roots absorbs them.
pub fn add_root(conn: &Connection, path: &Path) -> Result<i64> {
    let p = path.to_string_lossy().into_owned();
    let existing = roots(conn)?;
    if let Some((id, _)) = existing.iter().find(|(_, r)| relative_to(&p, r).is_some()) {
        return Ok(*id);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO library_root (path, added_at) VALUES (?1, ?2)",
        params![p, now_ms()],
    )?;
    let id = tx.last_insert_rowid();
    for (child, child_path) in &existing {
        if let Some(prefix) = relative_to(child_path, &p) {
            absorb(&tx, id, *child, &prefix)?;
        }
    }
    tx.commit()?;
    Ok(id)
}

/// Moves the files of root `child` (which lies at `prefix` inside `parent`) into
/// `parent` and deletes `child`. A file both roots already know is kept once.
fn absorb(conn: &Connection, parent: i64, child: i64, prefix: &str) -> Result<()> {
    let files: Vec<(i64, String)> = {
        let mut st = conn.prepare("SELECT id, rel_path FROM file WHERE root_id = ?1")?;
        let rows = st.query_map([child], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, rel) in files {
        let new_rel = if prefix.is_empty() { rel } else { format!("{prefix}/{rel}") };
        let twin: Option<i64> = conn
            .query_row(
                "SELECT id FROM file WHERE root_id = ?1 AND rel_path = ?2",
                params![parent, new_rel],
                |r| r.get(0),
            )
            .optional()?;
        match twin {
            // The older row stays: it has the longer history.
            Some(other) if other < id => {
                conn.execute("DELETE FROM file WHERE id = ?1", [id])?;
            }
            Some(other) => {
                conn.execute("DELETE FROM file WHERE id = ?1", [other])?;
                conn.execute(
                    "UPDATE file SET root_id = ?2, rel_path = ?3 WHERE id = ?1",
                    params![id, parent, new_rel],
                )?;
            }
            None => {
                conn.execute(
                    "UPDATE file SET root_id = ?2, rel_path = ?3 WHERE id = ?1",
                    params![id, parent, new_rel],
                )?;
            }
        }
    }
    drop_root(conn, child)
}

fn drop_root(conn: &Connection, id: i64) -> Result<()> {
    // Files, tracks and everything hanging on them go by ON DELETE CASCADE.
    conn.execute("DELETE FROM library_root WHERE id = ?1", [id])?;
    conn.execute(
        "DELETE FROM setting WHERE key = ?1",
        [format!("scan.covers_version.{id}")],
    )?;
    Ok(())
}

/// Removes a music folder from the library (the files on disk stay).
pub fn remove_root(conn: &Connection, id: i64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    drop_root(&tx, id)?;
    tx.commit()?;
    Ok(())
}

/// Merges roots that overlap (one inside another, or the same folder twice). Returns how
/// many roots were merged away.
pub fn repair_overlapping_roots(conn: &Connection) -> Result<usize> {
    let mut merged = 0;
    loop {
        let all = roots(conn)?;
        // The outermost parent first, so each child moves only once.
        let pair = all.iter().find_map(|(child, cp)| {
            all.iter()
                .filter(|(parent, pp)| {
                    parent != child
                        && relative_to(cp, pp).is_some()
                        // Same folder twice: the older root stays.
                        && !(relative_to(pp, cp).is_some() && parent > child)
                })
                .min_by_key(|(_, pp)| components(pp).len())
                .map(|(parent, pp)| (*child, *parent, relative_to(cp, pp).unwrap_or_default()))
        });
        let Some((child, parent, prefix)) = pair else {
            return Ok(merged);
        };
        let tx = conn.unchecked_transaction()?;
        absorb(&tx, parent, child, &prefix)?;
        tx.commit()?;
        merged += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn add_file(conn: &Connection, root: i64, rel: &str) -> i64 {
        conn.execute(
            "INSERT INTO file (root_id, rel_path, size, mtime, quick_hash, format, scanned_at)
             VALUES (?1, ?2, 1, 1, x'00', 'mp3', 0)",
            params![root, rel],
        )
        .unwrap();
        let file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO track (file_id, title, title_sort, added_at) VALUES (?1, ?2, ?2, 0)",
            params![file, rel],
        )
        .unwrap();
        file
    }

    fn files(conn: &Connection) -> Vec<(String, String)> {
        let mut st = conn
            .prepare(
                "SELECT r.path, f.rel_path FROM file f JOIN library_root r ON r.id = f.root_id
                 ORDER BY f.rel_path",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative_to("/s/0/Music/A", "/s/0"), Some("Music/A".into()));
        assert_eq!(relative_to("/s/0/", "/s/0"), Some(String::new()));
        assert_eq!(relative_to("/s/0/Musical", "/s/0/Music"), None);
        assert_eq!(relative_to("/s/0", "/s/0/Music"), None);
        assert_eq!(relative_to("C:\\Users\\D\\Music", "C:/Users/D"), Some("Music".into()));
        if cfg!(windows) {
            assert_eq!(relative_to("c:\\users\\d\\music", "C:\\Users\\D"), Some("music".into()));
        }
    }

    #[test]
    fn a_folder_inside_a_root_is_not_added_again() {
        let conn = open_in_memory().unwrap();
        let root = add_root(&conn, Path::new("/sd")).unwrap();
        assert_eq!(add_root(&conn, Path::new("/sd/Music/Party")).unwrap(), root);
        assert_eq!(add_root(&conn, Path::new("/sd/")).unwrap(), root);
        assert_eq!(roots(&conn).unwrap().len(), 1);
    }

    #[test]
    fn a_parent_folder_takes_over_its_children_and_keeps_tracks() {
        let conn = open_in_memory().unwrap();
        let party = add_root(&conn, Path::new("/sd/Music/Party")).unwrap();
        let file = add_file(&conn, party, "a.mp3");
        let track: i64 = conn
            .query_row("SELECT id FROM track WHERE file_id = ?1", [file], |r| r.get(0))
            .unwrap();
        let other = add_root(&conn, Path::new("/elsewhere")).unwrap();
        add_file(&conn, other, "b.mp3");

        let sd = add_root(&conn, Path::new("/sd")).unwrap();
        assert_ne!(sd, party);
        assert_eq!(
            files(&conn),
            [
                ("/sd".into(), "Music/Party/a.mp3".into()),
                ("/elsewhere".into(), "b.mp3".into())
            ]
        );
        let kept: i64 = conn
            .query_row("SELECT id FROM track WHERE file_id = ?1", [file], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, track, "the track survives with its id");
        assert_eq!(roots(&conn).unwrap().len(), 2);
    }

    #[test]
    fn overlapping_roots_from_before_are_merged_without_duplicates() {
        let conn = open_in_memory().unwrap();
        // As found on a phone: the same files under a folder and under its parent.
        let insert = |p: &str| {
            conn.execute(
                "INSERT INTO library_root (path, added_at) VALUES (?1, 0)",
                [p],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let party = insert("/sd/Music/Party");
        let sd = insert("/sd");
        let music = insert("/int/Music");
        let int = insert("/int/");
        let older = add_file(&conn, party, "x.mp3");
        let newer = add_file(&conn, sd, "Music/Party/x.mp3");
        add_file(&conn, sd, "Other/y.mp3");
        add_file(&conn, music, "z.mp3");
        let _ = int;

        assert_eq!(repair_overlapping_roots(&conn).unwrap(), 2);
        assert_eq!(
            files(&conn),
            [
                ("/sd".into(), "Music/Party/x.mp3".into()),
                ("/int/".into(), "Music/z.mp3".into()),
                ("/sd".into(), "Other/y.mp3".into()),
            ]
        );
        let survivor: i64 = conn
            .query_row("SELECT id FROM file WHERE rel_path = 'Music/Party/x.mp3'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(survivor, older, "the older row of a duplicate stays");
        assert_ne!(survivor, newer);
        let tracks: i64 = conn.query_row("SELECT count(*) FROM track", [], |r| r.get(0)).unwrap();
        assert_eq!(tracks, 3);
        assert_eq!(repair_overlapping_roots(&conn).unwrap(), 0, "nothing left to merge");
    }

    #[test]
    fn removing_a_root_removes_its_tracks() {
        let conn = open_in_memory().unwrap();
        let a = add_root(&conn, Path::new("/a")).unwrap();
        let b = add_root(&conn, Path::new("/b")).unwrap();
        add_file(&conn, a, "1.mp3");
        add_file(&conn, b, "2.mp3");
        remove_root(&conn, a).unwrap();
        assert_eq!(files(&conn), [("/b".into(), "2.mp3".into())]);
        let tracks: i64 = conn.query_row("SELECT count(*) FROM track", [], |r| r.get(0)).unwrap();
        assert_eq!(tracks, 1);
    }
}
