//! Scans a music folder into a library database and optionally searches it.
//!
//! Usage:
//!   cargo run --release -p dap-library --example scan -- <library.db> <folder> [search words...]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use dap_library::db::open_writer;
use dap_library::{add_root, scan_root, search_tracks};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: scan <library.db> <folder> [search words...]");
        std::process::exit(2);
    }
    let db = PathBuf::from(&args[0]);
    let folder = std::fs::canonicalize(&args[1]).unwrap_or_else(|e| {
        eprintln!("{}: {e}", args[1]);
        std::process::exit(1);
    });

    let mut conn = open_writer(&db).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let root = add_root(&conn, &folder).expect("register folder");

    let started = Instant::now();
    let mut last_print = Instant::now();
    let report = scan_root(&mut conn, root, &AtomicBool::new(false), |p| {
        if last_print.elapsed().as_millis() > 250 {
            eprint!("\r{}/{} files", p.processed, p.total);
            last_print = Instant::now();
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    eprintln!(
        "\rdone in {:.1} s: {} added, {} updated, {} unchanged, {} moved, {} missing, {} failed",
        started.elapsed().as_secs_f64(),
        report.added,
        report.updated,
        report.unchanged,
        report.moved,
        report.missing,
        report.failed.len()
    );
    for (path, err) in report.failed.iter().take(10) {
        eprintln!("  failed: {}: {err}", path.display());
    }

    if args.len() > 2 {
        let query = args[2..].join(" ");
        let started = Instant::now();
        let hits = search_tracks(&conn, &query, 20).expect("search");
        println!(
            "\"{query}\": {} hits in {:.1} ms",
            hits.len(),
            started.elapsed().as_secs_f64() * 1000.0
        );
        for id in hits {
            let (title, artist, album): (String, Option<String>, Option<String>) = conn
                .query_row(
                    "SELECT t.title,
                            (SELECT group_concat(a.name, ', ') FROM track_artist ta
                             JOIN artist a ON a.id = ta.artist_id WHERE ta.track_id = t.id),
                            al.title
                     FROM track t LEFT JOIN album al ON al.id = t.album_id WHERE t.id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .expect("track");
            println!(
                "  {title} — {} [{}]",
                artist.unwrap_or_default(),
                album.unwrap_or_default()
            );
        }
    }
}
