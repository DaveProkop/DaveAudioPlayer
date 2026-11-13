//! Prints where the analysis finds each track's audible start and end and whether it
//! ends with a fade-out, to check the detection by ear.
//!
//!   cargo run --release -p dap-engine --example bounds_report -- <library.db> [max tracks]
//!   cargo run --release -p dap-engine --example bounds_report -- <audio file>
//!
//! With an audio file it prints the file's level every 5 s instead.
//!
//! Opens the database read-only; the app may keep running.

use std::path::PathBuf;

use dap_analysis::{Bounds, BoundsDetector};
use dap_audio_io::Decoder;
use rusqlite::{Connection, OpenFlags};

fn detect(path: &std::path::Path) -> Option<(Bounds, f64)> {
    let mut decoder = Decoder::open(path).ok()?;
    let info = decoder.info().clone();
    let mut d = BoundsDetector::new(info.sample_rate, info.channels);
    let mut frames = 0usize;
    while let Ok(Some(chunk)) = decoder.next_chunk() {
        frames += chunk.len() / info.channels;
        d.push(chunk);
    }
    Some((d.finish()?, frames as f64 / f64::from(info.sample_rate)))
}

/// Level (dBFS) of every 5 s of a file.
fn print_levels(path: &std::path::Path) {
    let mut decoder = Decoder::open(path).expect("open");
    let info = decoder.info().clone();
    let block = info.sample_rate as usize * 5;
    let (mut acc, mut n, mut t) = (0.0f64, 0usize, 0usize);
    while let Ok(Some(chunk)) = decoder.next_chunk() {
        for frame in chunk.chunks_exact(info.channels) {
            acc += frame.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / info.channels as f64;
            n += 1;
            if n == block {
                println!(
                    "{:>5} s {:>7.1} dB",
                    t * 5,
                    10.0 * (acc / n as f64 + 1e-15).log10()
                );
                (acc, n, t) = (0.0, 0, t + 1);
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args
        .next()
        .expect("usage: bounds_report <library.db> [max tracks]");
    if !db.ends_with(".db") {
        print_levels(std::path::Path::new(&db));
        return;
    }
    let max: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(100);
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open db");
    let mut st = conn
        .prepare(
            "SELECT r.path, f.rel_path FROM file f JOIN library_root r ON r.id = f.root_id
             WHERE f.missing = 0 ORDER BY f.id",
        )
        .unwrap();
    let files: Vec<(PathBuf, String)> = st
        .query_map([], |r| {
            let rel: String = r.get(1)?;
            Ok((PathBuf::from(r.get::<_, String>(0)?).join(&rel), rel))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .take(max)
        .collect();
    let (mut fades, mut n) = (0, 0);
    for (path, rel) in files {
        let Some((b, length)) = detect(&path) else {
            continue;
        };
        n += 1;
        fades += usize::from(b.fade_out);
        println!(
            "{:>6.2} {:>7.2} / {:>7.2} cut {:>5.1} s {} {rel}",
            b.start_s,
            b.end_s,
            length,
            length - b.end_s,
            if b.fade_out { "FADE" } else { "    " },
        );
    }
    println!("{n} tracks, {fades} fade-outs");
}
