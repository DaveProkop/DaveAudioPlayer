//! Measures the tempo detection against BPM tags: reads the tags of the library's files,
//! detects the tempo of those that have one and compares.
//!
//!   cargo run --release -p dap-engine --example bpm_accuracy -- <library.db> [max tracks]
//!
//! Opens the database read-only; the app may keep running.

use std::path::PathBuf;

use dap_analysis::{TempoDetector, parse_bpm_tag};
use dap_audio_io::Decoder;
use rusqlite::{Connection, OpenFlags};

fn detect(path: &std::path::Path) -> Option<(f64, f64)> {
    let mut decoder = Decoder::open(path).ok()?;
    let info = decoder.info().clone();
    let mut t = TempoDetector::new(info.sample_rate, info.channels);
    while let Ok(Some(chunk)) = decoder.next_chunk() {
        t.push(chunk);
    }
    t.finish()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args.next().expect("usage: bpm_accuracy <library.db> [max tracks]");
    let max: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(300);
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open db");
    let mut st = conn
        .prepare(
            "SELECT r.path, f.rel_path FROM file f JOIN library_root r ON r.id = f.root_id
             WHERE f.missing = 0 ORDER BY f.id",
        )
        .unwrap();
    let files: Vec<PathBuf> = st
        .query_map([], |r| {
            Ok(PathBuf::from(r.get::<_, String>(0)?).join(r.get::<_, String>(1)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    let (mut exact, mut close, mut octave, mut wrong, mut none, mut n) = (0, 0, 0, 0, 0, 0);
    for path in files {
        if n >= max {
            break;
        }
        let Some(tag) = dap_tags::read(&path)
            .ok()
            .and_then(|t| t.tags.bpm)
            .and_then(|b| parse_bpm_tag(&b))
        else {
            continue;
        };
        n += 1;
        let Some((found, conf)) = detect(&path) else {
            none += 1;
            eprintln!("{n:4} tag {tag:6.1} found  none       {}", path.display());
            continue;
        };
        let ratio = found / tag;
        let near = |r: f64| (ratio / r - 1.0).abs() <= 0.04;
        let kind = if (found - tag).abs() <= 1.0 {
            exact += 1;
            "exact"
        } else if near(1.0) {
            close += 1;
            "close"
        } else if near(2.0) || near(0.5) || near(1.5) || near(2.0 / 3.0) {
            octave += 1;
            "octave"
        } else {
            wrong += 1;
            "wrong"
        };
        eprintln!("{n:4} tag {tag:6.1} found {found:6.1} {conf:4.2} {kind:6} {}", path.display());
    }
    let p = |x: usize| 100.0 * x as f64 / n.max(1) as f64;
    println!("tracks with a BPM tag: {n}");
    println!("exact (±1 BPM):        {exact:4}  {:5.1} %", p(exact));
    println!("close (±4 %):          {close:4}  {:5.1} %", p(close));
    println!("half/double/3:2:       {octave:4}  {:5.1} %", p(octave));
    println!("wrong:                 {wrong:4}  {:5.1} %", p(wrong));
    println!("no tempo found:        {none:4}  {:5.1} %", p(none));
}
