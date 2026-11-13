//! Interactive console player on top of the engine.
//!
//! Usage:
//!   cargo run --release -p dap-engine --example player -- <library.db> [folder to scan] [search words...]
//!
//! Scans the folder (if given), queues the search results (or the whole library) in album
//! order and plays them gaplessly. Commands: Enter = play/pause, n = next, p = previous,
//! s <seconds> = seek, v <0-100> = volume, l = list queue, q = quit.

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use dap_engine::{Command, Engine, EngineConfig, Event, OutputConfig, Status};
use dap_library::db::open_writer;
use dap_library::{add_root, scan_root, search_tracks};
use rusqlite::Connection;

fn track_label(conn: &Connection, track_id: i64) -> String {
    conn.query_row(
        "SELECT t.title,
                coalesce((SELECT group_concat(a.name, ', ') FROM track_artist ta
                          JOIN artist a ON a.id = ta.artist_id WHERE ta.track_id = t.id), '?')
         FROM track t WHERE t.id = ?1",
        [track_id],
        |r| {
            Ok(format!(
                "{} — {}",
                r.get::<_, String>(1)?,
                r.get::<_, String>(0)?
            ))
        },
    )
    .unwrap_or_else(|_| format!("track {track_id}"))
}

fn fmt_ms(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(db_path) = args.first().map(PathBuf::from) else {
        eprintln!("usage: player <library.db> [folder] [search words...]");
        std::process::exit(2);
    };

    let mut conn = open_writer(&db_path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    if let Some(folder) = args.get(1) {
        let root = add_root(&conn, &PathBuf::from(folder)).expect("register folder");
        eprint!("scanning {folder}… ");
        match scan_root(&mut conn, root, &AtomicBool::new(false), |_| {}) {
            Ok(r) => eprintln!(
                "{} added, {} updated, {} unchanged",
                r.added, r.updated, r.unchanged
            ),
            Err(e) => eprintln!("{e}"),
        }
    }

    let track_ids: Vec<i64> = if args.len() > 2 {
        search_tracks(&conn, &args[2..].join(" "), 500).expect("search")
    } else {
        conn.prepare(
            "SELECT t.id FROM track t JOIN file f ON f.id = t.file_id AND f.missing = 0
             LEFT JOIN album al ON al.id = t.album_id
             ORDER BY al.title_sort, t.disc_no, t.track_no, t.title_sort",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    drop(conn); // the engine owns the database writer from here on

    let engine = Engine::start(EngineConfig {
        db_path: db_path.clone(),
        output: OutputConfig::Device,
        watch_folders: true,
    })
    .unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let labels = dap_library::db::open_reader(&db_path).expect("reader");

    if !track_ids.is_empty() {
        engine.send(Command::ClearQueue);
        println!("queued {} tracks", track_ids.len());
        engine.send(Command::Enqueue {
            track_ids,
            at: None,
        });
    }
    engine.send(Command::Play);

    // Commands from stdin on their own thread.
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if cmd_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut queue = Vec::new();
    loop {
        crossbeam_channel::select! {
            recv(cmd_rx) -> line => {
                let Ok(line) = line else { break };
                let mut parts = line.split_whitespace();
                match (parts.next(), parts.next()) {
                    (None, _) => engine.send(Command::TogglePlayPause),
                    (Some("n"), _) => engine.send(Command::Next),
                    (Some("p"), _) => engine.send(Command::Previous),
                    (Some("s"), Some(sec)) => {
                        if let Ok(s) = sec.parse::<f64>() {
                            engine.send(Command::Seek { position_ms: (s * 1000.0) as u64 });
                        }
                    }
                    (Some("v"), Some(v)) => {
                        if let Ok(v) = v.parse::<f32>() {
                            engine.send(Command::SetVolume(v / 100.0));
                        }
                    }
                    (Some("l"), _) => {
                        for (i, e) in queue.iter().enumerate() {
                            println!("{i:4}  {}", track_label(&labels, *e));
                        }
                    }
                    (Some("x"), Some(v)) => {
                        if let Ok(sec) = v.parse::<f64>() {
                            engine.send(Command::SetCrossfade { ms: (sec * 1000.0) as u32 });
                        }
                    }
                    (Some("r"), Some(v)) => engine.send(Command::SetNormalization(v == "1")),
                    (Some("q"), _) => break,
                    _ => println!(
                        "Enter = play/pause, n, p, s <sec>, v <0-100>, x <crossfade sec>, r <0|1 normalize>, l, q"
                    ),
                }
            }
            recv(engine.events()) -> event => {
                match event {
                    Ok(Event::State(s)) => {
                        let what = s.entry.map_or("—".into(), |e| track_label(&labels, e.track_id));
                        let icon = match s.status { Status::Playing => "▶", Status::Paused => "⏸", Status::Stopped => "⏹" };
                        println!(
                            "{icon} [{}/{}] {what}  {} / {}",
                            s.index.map_or(0, |i| i + 1),
                            queue.len(),
                            fmt_ms(s.position_ms),
                            s.duration_ms.map_or("?".into(), fmt_ms)
                        );
                    }
                    Ok(Event::Queue(q)) => queue = q.iter().map(|e| e.track_id).collect(),
                    Ok(Event::Settings(s)) => println!(
                        "crossfade {} s, normalization {}",
                        f64::from(s.crossfade_ms) / 1000.0,
                        if s.normalize { "on" } else { "off" }
                    ),
                    Ok(Event::TrackFailed { track_id, error }) => {
                        println!("skipped {}: {error}", track_label(&labels, track_id));
                    }
                    Ok(Event::Error(e)) => println!("error: {e}"),
                    Ok(Event::ScanProgress { .. } | Event::LibraryChanged | Event::AnalysisProgress { .. }) => {}
                    Ok(Event::ScanFinished(s)) => println!("scan finished: {s:?}"),
                    Err(_) => break,
                }
            }
        }
    }
    engine.shutdown();
}
