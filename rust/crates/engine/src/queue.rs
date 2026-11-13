//! The play queue, kept in memory and mirrored to `queue_item`.
//!
//! Order is a `REAL` position: inserting or moving an item writes one row with a value
//! between its neighbours. When the gap gets too small all positions are renumbered.

use dap_library::DbError;
use rusqlite::{Connection, params};

use crate::db_writer::DbWriter;

/// Below this spacing positions are renumbered before inserting.
const MIN_GAP: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueEntry {
    /// `queue_item.id`; stable while the item stays in the queue.
    pub id: i64,
    pub track_id: i64,
}

#[derive(Debug, Clone, Copy)]
struct Item {
    entry: QueueEntry,
    pos: f64,
}

#[derive(Debug, Default)]
pub struct Queue {
    items: Vec<Item>,
}

type Result<T> = std::result::Result<T, DbError>;

fn writer_gone() -> DbError {
    DbError::File("database writer thread is gone".into())
}

impl Queue {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut st = conn.prepare("SELECT id, track_id, pos FROM queue_item ORDER BY pos, id")?;
        let items = st
            .query_map([], |r| {
                Ok(Item {
                    entry: QueueEntry {
                        id: r.get(0)?,
                        track_id: r.get(1)?,
                    },
                    pos: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(Self { items })
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<QueueEntry> {
        self.items.get(index).map(|i| i.entry)
    }

    pub fn index_of(&self, id: i64) -> Option<usize> {
        self.items.iter().position(|i| i.entry.id == id)
    }

    pub fn entries(&self) -> Vec<QueueEntry> {
        self.items.iter().map(|i| i.entry).collect()
    }

    /// Positions for `n` new items placed before index `at`. Renumbers (in memory) first
    /// when there is no room; the returned flag tells the caller to persist that.
    fn make_room(&mut self, at: usize, n: usize) -> (Vec<f64>, bool) {
        let compute = |items: &[Item]| -> Option<Vec<f64>> {
            let lo = at.checked_sub(1).and_then(|i| items.get(i)).map(|i| i.pos);
            let hi = items.get(at).map(|i| i.pos);
            let n_f = n as f64;
            Some(match (lo, hi) {
                (None, None) => (1..=n).map(|k| k as f64).collect(),
                (Some(l), None) => (1..=n).map(|k| l + k as f64).collect(),
                (None, Some(h)) => (0..n).map(|k| h - n_f + k as f64).collect(),
                (Some(l), Some(h)) => {
                    let step = (h - l) / (n_f + 1.0);
                    if step < MIN_GAP {
                        return None;
                    }
                    (1..=n).map(|k| l + step * k as f64).collect()
                }
            })
        };
        if let Some(p) = compute(&self.items) {
            return (p, false);
        }
        for (i, item) in self.items.iter_mut().enumerate() {
            item.pos = (i + 1) as f64;
        }
        (compute(&self.items).expect("room after renumbering"), true)
    }

    fn renumber_job(&self) -> Vec<(i64, f64)> {
        self.items.iter().map(|i| (i.entry.id, i.pos)).collect()
    }

    /// Puts the items from index `from` on in the order of `ids`, which must be exactly
    /// those items. Returns false (and changes nothing) when they are not.
    pub fn reorder_tail(&mut self, db: &DbWriter, from: usize, ids: &[i64]) -> bool {
        let from = from.min(self.items.len());
        let mut tail: Vec<i64> = self.items[from..].iter().map(|i| i.entry.id).collect();
        let mut wanted = ids.to_vec();
        tail.sort_unstable();
        wanted.sort_unstable();
        if tail != wanted {
            return false;
        }
        let by_id: std::collections::HashMap<i64, Item> = self.items[from..]
            .iter()
            .map(|i| (i.entry.id, *i))
            .collect();
        self.items.truncate(from);
        self.items.extend(ids.iter().map(|id| by_id[id]));
        // Fresh positions for everything: one write, no gaps to worry about.
        for (n, item) in self.items.iter_mut().enumerate() {
            item.pos = n as f64;
        }
        let rows = self.renumber_job();
        db.run(move |conn| {
            let _ = (|| -> Result<()> {
                let tx = conn.transaction()?;
                for (id, pos) in rows {
                    tx.execute(
                        "UPDATE queue_item SET pos = ?2 WHERE id = ?1",
                        params![id, pos],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })();
        });
        true
    }

    /// Inserts tracks before index `at` (clamped to the end).
    pub fn insert(
        &mut self,
        db: &DbWriter,
        at: usize,
        track_ids: &[i64],
        origin: &'static str,
    ) -> Result<()> {
        if track_ids.is_empty() {
            return Ok(());
        }
        let at = at.min(self.items.len());
        let (positions, renumbered) = self.make_room(at, track_ids.len());
        let renumber = renumbered.then(|| self.renumber_job());
        let rows: Vec<(i64, f64)> = track_ids
            .iter()
            .copied()
            .zip(positions.iter().copied())
            .collect();
        let ids = db
            .call(move |conn| -> Result<Vec<i64>> {
                let tx = conn.transaction()?;
                if let Some(all) = renumber {
                    for (id, pos) in all {
                        tx.execute(
                            "UPDATE queue_item SET pos = ?2 WHERE id = ?1",
                            params![id, pos],
                        )?;
                    }
                }
                let mut ids = Vec::with_capacity(rows.len());
                for (track_id, pos) in rows {
                    tx.execute(
                        "INSERT INTO queue_item (track_id, pos, origin) VALUES (?1, ?2, ?3)",
                        params![track_id, pos, origin],
                    )?;
                    ids.push(tx.last_insert_rowid());
                }
                tx.commit()?;
                Ok(ids)
            })
            .ok_or_else(writer_gone)??;
        let new = ids
            .into_iter()
            .zip(track_ids)
            .zip(positions)
            .map(|((id, &track_id), pos)| Item {
                entry: QueueEntry { id, track_id },
                pos,
            });
        self.items.splice(at..at, new);
        Ok(())
    }

    /// Moves the item at `from` so that it ends up at index `to`.
    pub fn move_item(&mut self, db: &DbWriter, from: usize, to: usize) -> Result<()> {
        if from >= self.items.len() || from == to {
            return Ok(());
        }
        let item = self.items.remove(from);
        let to = to.min(self.items.len());
        let (pos, renumbered) = self.make_room(to, 1);
        let renumber = renumbered.then(|| self.renumber_job());
        let id = item.entry.id;
        let pos = pos[0];
        self.items.insert(to, Item { pos, ..item });
        db.run(move |conn| {
            let _ = (|| -> Result<()> {
                let tx = conn.transaction()?;
                for (rid, rpos) in renumber.unwrap_or_default() {
                    tx.execute(
                        "UPDATE queue_item SET pos = ?2 WHERE id = ?1",
                        params![rid, rpos],
                    )?;
                }
                tx.execute(
                    "UPDATE queue_item SET pos = ?2 WHERE id = ?1",
                    params![id, pos],
                )?;
                tx.commit()?;
                Ok(())
            })();
        });
        Ok(())
    }

    pub fn remove(&mut self, db: &DbWriter, index: usize) -> Option<QueueEntry> {
        if index >= self.items.len() {
            return None;
        }
        let item = self.items.remove(index);
        let id = item.entry.id;
        db.run(move |conn| {
            let _ = conn.execute("DELETE FROM queue_item WHERE id = ?1", [id]);
        });
        Some(item.entry)
    }

    pub fn clear(&mut self, db: &DbWriter) {
        self.items.clear();
        db.run(|conn| {
            let _ = conn.execute("DELETE FROM queue_item", []);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDb(PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("dap-queue-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDb(dir)
        }
        fn path(&self) -> PathBuf {
            self.0.join("q.db")
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writer with tracks 1..=n in the catalog.
    fn writer_with_tracks(tmp: &TempDb, n: i64) -> DbWriter {
        let (db, _thread) = DbWriter::open(&tmp.path()).unwrap();
        db.call(move |c| {
            c.execute("INSERT INTO library_root (id, path, added_at) VALUES (1, '/m', 0)", []).unwrap();
            for i in 1..=n {
                c.execute(
                    "INSERT INTO file (id, root_id, rel_path, size, mtime, format, scanned_at)
                     VALUES (?1, 1, ?1 || '.mp3', 1, 1, 'mp3', 0)",
                    [i],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO track (id, file_id, title, title_sort, added_at) VALUES (?1, ?1, 't', 't', 0)",
                    [i],
                )
                .unwrap();
            }
        })
        .unwrap();
        db
    }

    fn tracks(q: &Queue) -> Vec<i64> {
        q.entries().iter().map(|e| e.track_id).collect()
    }

    fn reload(db: &DbWriter) -> Vec<i64> {
        db.call(|c| tracks(&Queue::load(c).unwrap())).unwrap()
    }

    #[test]
    fn insert_move_remove_persist() {
        let tmp = TempDb::new("ops");
        let db = writer_with_tracks(&tmp, 6);
        let mut q = Queue::default();
        q.insert(&db, 0, &[1, 2, 3], "user").unwrap();
        q.insert(&db, 1, &[4, 5], "user").unwrap(); // between 1 and 2
        q.insert(&db, 0, &[6], "user").unwrap(); // at the front
        assert_eq!(tracks(&q), [6, 1, 4, 5, 2, 3]);
        assert_eq!(reload(&db), tracks(&q));

        q.move_item(&db, 5, 0).unwrap();
        q.move_item(&db, 1, 3).unwrap();
        assert_eq!(tracks(&q), [3, 1, 4, 6, 5, 2]);
        assert_eq!(q.remove(&db, 2).unwrap().track_id, 4);
        assert_eq!(reload(&db), [3, 1, 6, 5, 2]);

        q.clear(&db);
        assert!(reload(&db).is_empty());
    }

    #[test]
    fn many_inserts_at_the_same_spot_renumber() {
        let tmp = TempDb::new("renumber");
        let db = writer_with_tracks(&tmp, 3);
        let mut q = Queue::default();
        q.insert(&db, 0, &[1, 3], "user").unwrap();
        // Halving the gap 80 times exhausts f64 precision and forces a renumber.
        for _ in 0..80 {
            q.insert(&db, 1, &[2], "user").unwrap();
        }
        let t = tracks(&q);
        assert_eq!((t[0], t[t.len() - 1], t.len()), (1, 3, 82));
        assert_eq!(reload(&db), t);
    }
}
