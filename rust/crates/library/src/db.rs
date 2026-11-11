//! Opening the library database and migrating its schema.
//!
//! The database runs in WAL mode with a single writer connection and any number of
//! read-only connections. The schema version lives in `PRAGMA user_version`.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Migration `i` upgrades the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[include_str!("../../../../db/schema.sql")];

/// Schema version this build understands.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    /// The database was created by a newer version of the app.
    TooNew {
        found: i64,
        supported: i64,
    },
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Sqlite(e) => write!(f, "database error: {e}"),
            DbError::TooNew { found, supported } => write!(
                f,
                "database schema version {found} is newer than supported version {supported}"
            ),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Sqlite(e) => Some(e),
            DbError::TooNew { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e)
    }
}

pub type Result<T> = std::result::Result<T, DbError>;

/// Opens (creating if needed) the writer connection and migrates the schema.
/// Only one writer connection should exist per database.
pub fn open_writer(path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Opens a read-only connection. The writer must have created and migrated the database first.
pub fn open_reader(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    check_version(schema_version(&conn)?)?;
    Ok(conn)
}

/// In-memory database with the full schema (for tests and tools).
pub fn open_in_memory() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // journal_mode returns the resulting mode; in-memory databases report "memory".
    let _mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

fn check_version(found: i64) -> Result<()> {
    if found > SCHEMA_VERSION {
        Err(DbError::TooNew {
            found,
            supported: SCHEMA_VERSION,
        })
    } else {
        Ok(())
    }
}

/// Applies all pending migrations, each in its own transaction.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    let current = schema_version(conn)?;
    check_version(current)?;
    for (from, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", from as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDb(PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("dap-library-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDb(dir)
        }
        fn path(&self) -> PathBuf {
            self.0.join("library.db")
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = ?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }

    #[test]
    fn fresh_database_gets_full_schema() {
        let conn = open_in_memory().unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        for t in [
            "library_root",
            "file",
            "track",
            "tag_edit",
            "analysis",
            "queue_item",
            "track_fts",
            "track_fts_tri",
        ] {
            assert!(table_exists(&conn, t), "missing table {t}");
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_is_rejected() {
        let mut conn = open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(DbError::TooNew { .. })));
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let conn = open_in_memory().unwrap();
        let err = conn.execute(
            "INSERT INTO file (root_id, rel_path, size, mtime, format, scanned_at)
             VALUES (999, 'a.mp3', 1, 1, 'mp3', 1)",
            [],
        );
        assert!(err.is_err());
    }

    #[test]
    fn file_database_uses_wal_and_readers_work() {
        let tmp = TempDb::new("wal");
        let writer = open_writer(&tmp.path()).unwrap();
        let mode: String = writer
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute(
                "INSERT INTO setting (key, value) VALUES ('eq.preset', '\"flat\"')",
                [],
            )
            .unwrap();

        let reader = open_reader(&tmp.path()).unwrap();
        let v: String = reader
            .query_row(
                "SELECT value FROM setting WHERE key = 'eq.preset'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "\"flat\"");
        assert!(
            reader.execute("DELETE FROM setting", []).is_err(),
            "reader must be read-only"
        );
        drop(reader);
        drop(writer);

        // Reopening an existing database keeps the data.
        let writer = open_writer(&tmp.path()).unwrap();
        let n: i64 = writer
            .query_row("SELECT count(*) FROM setting", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn fts_ignores_diacritics_and_matches_prefixes() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO track_fts (rowid, title, artists, album, genres, lyrics)
             VALUES (1, 'Příliš žluťoučký kůň', 'Dvořák', 'Praha', 'klasika', '')",
            [],
        )
        .unwrap();
        let hits = |q: &str| -> Vec<i64> {
            let mut st = conn
                .prepare("SELECT rowid FROM track_fts WHERE track_fts MATCH ?1")
                .unwrap();
            st.query_map([q], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(hits("zlutoucky"), [1]);
        assert_eq!(hits("dvorak"), [1]);
        assert_eq!(hits("pra*"), [1]);
        assert!(hits("rock").is_empty());

        conn.execute("DELETE FROM track_fts WHERE rowid = 1", [])
            .unwrap();
        assert!(hits("dvorak").is_empty());
    }

    #[test]
    fn trigram_index_matches_substrings_without_diacritics() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO track_fts_tri (rowid, title, artists, album)
             VALUES (7, 'Příliš žluťoučký kůň', 'Dvořák', 'Praha')",
            [],
        )
        .unwrap();
        let hit: i64 = conn
            .query_row(
                "SELECT rowid FROM track_fts_tri WHERE track_fts_tri MATCH 'utouck'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, 7);
    }
}
