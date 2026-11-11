//! End-to-end scanner tests on real files in a temporary folder.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use dap_library::db::open_in_memory;
use dap_library::{DbError, ScanReport, add_root, scan_root, search_tracks};
use lofty::config::WriteOptions;
use lofty::prelude::{Accessor, ItemKey, TagExt};
use lofty::tag::{Tag, TagType};
use rusqlite::Connection;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dap-scan-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Meta<'a> {
    title: &'a str,
    artist: &'a str,
    album: &'a str,
    track: u32,
    genre: &'a str,
}

/// Writes a short WAV whose samples depend on `seed` (distinct content → distinct hash).
fn write_track(path: &Path, seed: u8, m: &Meta) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let frames = 4410u32;
    let data_len = frames * 2;
    let mut f = std::fs::File::create(path).unwrap();
    let mut w = |b: &[u8]| f.write_all(b).unwrap();
    w(b"RIFF");
    w(&(36 + data_len).to_le_bytes());
    w(b"WAVEfmt ");
    w(&16u32.to_le_bytes());
    w(&1u16.to_le_bytes());
    w(&1u16.to_le_bytes());
    w(&44_100u32.to_le_bytes());
    w(&88_200u32.to_le_bytes());
    w(&2u16.to_le_bytes());
    w(&16u16.to_le_bytes());
    w(b"data");
    w(&data_len.to_le_bytes());
    w(&(0..data_len)
        .map(|i| (i as u8).wrapping_mul(seed))
        .collect::<Vec<_>>());
    drop(f);
    retag(path, m);
}

fn retag(path: &Path, m: &Meta) {
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(m.title.into());
    tag.set_artist(m.artist.into());
    tag.set_album(m.album.into());
    tag.set_track(m.track);
    tag.set_genre(m.genre.into());
    tag.insert_text(ItemKey::RecordingDate, "1997".into());
    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

fn scan(conn: &mut Connection, root: i64) -> ScanReport {
    scan_root(conn, root, &AtomicBool::new(false), |_| {}).unwrap()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn title_hits(conn: &Connection, q: &str) -> Vec<String> {
    search_tracks(conn, q, 50)
        .unwrap()
        .into_iter()
        .map(|id| {
            conn.query_row("SELECT title FROM track WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        })
        .collect()
}

/// Library with one album of two tracks plus a single, and some non-audio files.
fn fixture(dir: &TempDir) {
    let album = |title, track| Meta {
        title,
        artist: "Kabát",
        album: "Colorado",
        track,
        genre: "Rock",
    };
    write_track(&dir.path("Kabát/Colorado/01.wav"), 3, &album("Colorado", 1));
    write_track(&dir.path("Kabát/Colorado/02.wav"), 5, &album("Žízeň", 2));
    write_track(
        &dir.path("Singles/song.wav"),
        7,
        &Meta {
            title: "Příliš žluťoučký kůň",
            artist: "Dvořák",
            album: "Single",
            track: 1,
            genre: "Classical",
        },
    );
    std::fs::write(dir.path("Kabát/Colorado/cover.jpg"), b"jpeg").unwrap();
    std::fs::write(dir.path("Singles/notes.txt"), b"text").unwrap();
    std::fs::write(dir.path("Singles/unsupported.opus"), b"opus").unwrap();
}

#[test]
fn first_scan_builds_catalog_and_search_index() {
    let dir = TempDir::new("first");
    fixture(&dir);
    let mut conn = open_in_memory().unwrap();
    let root = add_root(&conn, &dir.0).unwrap();
    assert_eq!(
        add_root(&conn, &dir.0).unwrap(),
        root,
        "add_root is idempotent"
    );

    let r = scan(&mut conn, root);
    assert_eq!((r.added, r.updated, r.unchanged, r.missing), (3, 0, 0, 0));
    assert!(r.failed.is_empty(), "{:?}", r.failed);

    assert_eq!(count(&conn, "SELECT count(*) FROM track"), 3);
    assert_eq!(count(&conn, "SELECT count(*) FROM album"), 2);
    assert_eq!(
        count(&conn, "SELECT count(*) FROM artist"),
        2,
        "artists are shared"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM album al JOIN artist a ON a.id = al.album_artist_id
             WHERE al.title = 'Colorado' AND a.name = 'Kabát' AND al.year = 1997"
        ),
        1
    );
    let rel: String = conn
        .query_row(
            "SELECT rel_path FROM file f JOIN track t ON t.file_id = f.id WHERE t.title = 'Žízeň'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        rel, "Kabát/Colorado/02.wav",
        "relative paths use '/' on every platform"
    );

    assert_eq!(
        title_hits(&conn, "zizen"),
        ["Žízeň"],
        "diacritics are ignored"
    );
    assert_eq!(
        title_hits(&conn, "kab col").len(),
        2,
        "prefixes over artist and album"
    );
    assert_eq!(title_hits(&conn, "dvorak zlut"), ["Příliš žluťoučký kůň"]);
    assert_eq!(
        title_hits(&conn, "utouc"),
        ["Příliš žluťoučký kůň"],
        "substring fallback"
    );
    assert!(title_hits(&conn, "metallica").is_empty());
}

#[test]
fn rescan_skips_unchanged_and_picks_up_edits_but_respects_locks() {
    let dir = TempDir::new("rescan");
    fixture(&dir);
    let mut conn = open_in_memory().unwrap();
    let root = add_root(&conn, &dir.0).unwrap();
    scan(&mut conn, root);

    let r = scan(&mut conn, root);
    assert_eq!((r.added, r.updated, r.unchanged), (0, 0, 3));

    // A manual title edit locks the title of track "Colorado".
    let id: i64 = conn
        .query_row("SELECT id FROM track WHERE title = 'Colorado'", [], |r| {
            r.get(0)
        })
        .unwrap();
    conn.execute(
        "INSERT INTO tag_edit (track_id, field, old_value, new_value, source, status, created_at)
         VALUES (?1, 'title', 'Colorado', 'Colorado (live)', 'user', 'db_only', 0)",
        [id],
    )
    .unwrap();
    conn.execute(
        "UPDATE track SET title = 'Colorado (live)', rating = 9 WHERE id = ?1",
        [id],
    )
    .unwrap();

    // Another program retags the file: new title (locked) and new genre (not locked).
    std::thread::sleep(std::time::Duration::from_millis(20));
    retag(
        &dir.path("Kabát/Colorado/01.wav"),
        &Meta {
            title: "Colorado 2024",
            artist: "Kabát",
            album: "Colorado",
            track: 1,
            genre: "Hard Rock",
        },
    );
    let r = scan(&mut conn, root);
    assert_eq!((r.updated, r.unchanged), (1, 2));

    let (title, rating): (String, i64) = conn
        .query_row("SELECT title, rating FROM track WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(title, "Colorado (live)", "locked field survives the rescan");
    assert_eq!(rating, 9, "user data survives the rescan");
    let genre: String = conn
        .query_row(
            "SELECT g.name FROM track_genre tg JOIN genre g ON g.id = tg.genre_id WHERE tg.track_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(genre, "Hard Rock", "unlocked fields follow the file");
    assert_eq!(
        title_hits(&conn, "live"),
        ["Colorado (live)"],
        "index follows the catalog"
    );
}

#[test]
fn moved_file_keeps_its_track_and_deleted_file_becomes_missing() {
    let dir = TempDir::new("move");
    fixture(&dir);
    let mut conn = open_in_memory().unwrap();
    let root = add_root(&conn, &dir.0).unwrap();
    scan(&mut conn, root);
    let id: i64 = conn
        .query_row("SELECT id FROM track WHERE title = 'Žízeň'", [], |r| {
            r.get(0)
        })
        .unwrap();
    conn.execute("UPDATE track SET play_count = 42 WHERE id = ?1", [id])
        .unwrap();

    // Move: same content, new place. Two scans: one sees it gone, the next finds it again.
    std::fs::create_dir_all(dir.path("Archive")).unwrap();
    std::fs::rename(
        dir.path("Kabát/Colorado/02.wav"),
        dir.path("Archive/zizen.wav"),
    )
    .unwrap();
    // Delete: gone for good.
    std::fs::remove_file(dir.path("Singles/song.wav")).unwrap();

    let r = scan(&mut conn, root);
    assert_eq!((r.added, r.moved, r.missing), (0, 1, 1), "{r:?}");
    let (rel, plays): (String, i64) = conn
        .query_row(
            "SELECT f.rel_path, t.play_count FROM track t JOIN file f ON f.id = t.file_id WHERE t.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((rel.as_str(), plays), ("Archive/zizen.wav", 42));
    assert_eq!(
        count(&conn, "SELECT count(*) FROM file WHERE missing = 1"),
        1
    );
    assert!(
        title_hits(&conn, "dvorak").is_empty(),
        "missing files are not found"
    );

    // The deleted file comes back: same row, no longer missing.
    write_track(
        &dir.path("Singles/song.wav"),
        7,
        &Meta {
            title: "Příliš žluťoučký kůň",
            artist: "Dvořák",
            album: "Single",
            track: 1,
            genre: "Classical",
        },
    );
    let r = scan(&mut conn, root);
    assert_eq!(r.added + r.updated + r.moved, 1, "{r:?}");
    assert_eq!(
        count(&conn, "SELECT count(*) FROM file WHERE missing = 1"),
        0
    );
    assert_eq!(count(&conn, "SELECT count(*) FROM track"), 3);
}

#[test]
fn unreachable_root_marks_nothing_missing() {
    let dir = TempDir::new("offline");
    fixture(&dir);
    let mut conn = open_in_memory().unwrap();
    let root = add_root(&conn, &dir.0).unwrap();
    scan(&mut conn, root);

    let offline = dir.0.with_file_name(format!(
        "{}-offline",
        dir.0.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(&dir.0, &offline).unwrap();
    let result = scan_root(&mut conn, root, &AtomicBool::new(false), |_| {});
    std::fs::rename(&offline, &dir.0).unwrap();

    assert!(matches!(result, Err(DbError::RootUnavailable(_))));
    assert_eq!(
        count(&conn, "SELECT count(*) FROM file WHERE missing = 1"),
        0
    );
}

#[test]
fn corrupt_file_is_reported_and_cancel_stops_safely() {
    let dir = TempDir::new("corrupt");
    fixture(&dir);
    std::fs::write(dir.path("Singles/broken.mp3"), b"not an mp3 at all").unwrap();
    let mut conn = open_in_memory().unwrap();
    let root = add_root(&conn, &dir.0).unwrap();

    let r = scan_root(&mut conn, root, &AtomicBool::new(true), |_| {}).unwrap();
    assert!(r.cancelled);
    assert_eq!(
        count(&conn, "SELECT count(*) FROM file WHERE missing = 1"),
        0
    );

    let mut seen = 0;
    let r = scan_root(&mut conn, root, &AtomicBool::new(false), |p| {
        seen = p.processed.max(seen)
    })
    .unwrap();
    assert_eq!(r.added, 3);
    assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
    assert!(r.failed[0].0.ends_with("broken.mp3"));
    assert_eq!(seen, 4, "progress reports every file");
}
