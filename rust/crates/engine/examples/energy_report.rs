//! Prints the energy of library tracks (from loudness and tempo, as the analysis does),
//! to check the energy scale by ear.
//!
//!   cargo run --release -p dap-engine --example energy_report -- <library.db> [max tracks] [every nth]
//!
//! Opens the database read-only; the app may keep running.

use std::path::PathBuf;

use dap_analysis::{LoudnessMeter, TempoDetector, track_energy};
use dap_audio_io::Decoder;
use rusqlite::{Connection, OpenFlags};

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args
        .next()
        .expect("usage: energy_report <library.db> [max tracks] [every nth]");
    let max: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(60);
    let step: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(1);
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
        .step_by(step.max(1))
        .take(max)
        .collect();
    let mut rows = Vec::new();
    for (path, rel) in files {
        let Ok(mut decoder) = Decoder::open(&path) else {
            continue;
        };
        let info = decoder.info().clone();
        let Ok(mut meter) = LoudnessMeter::new(info.sample_rate, info.channels) else {
            continue;
        };
        let mut tempo = TempoDetector::new(info.sample_rate, info.channels);
        while let Ok(Some(chunk)) = decoder.next_chunk() {
            let _ = meter.push(chunk);
            tempo.push(chunk);
        }
        let Some(lufs) = meter.finish().integrated_lufs else {
            continue;
        };
        let bpm = tempo.finish().map(|(b, _)| b);
        rows.push((track_energy(lufs, bpm), lufs, bpm.unwrap_or(0.0), rel));
    }
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (e, lufs, bpm, rel) in rows {
        println!("{e:.2}  {lufs:>6.1} LUFS  {bpm:>5.1} BPM  {rel}");
    }
}
