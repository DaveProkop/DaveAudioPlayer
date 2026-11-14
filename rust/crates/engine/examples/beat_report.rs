//! How well the stored beat grids hold where transitions happen: for library tracks with a
//! beat grid it measures the beats near the end (as the aligner does before a mix) and
//! compares them with the grid.
//!
//!   cargo run --release -p dap-engine --example beat_report -- <library.db> [max tracks] [every nth]
//!
//! Columns: BPM, grid error at the end (ms and beats), rough-to-refined shift, and the
//! spread of the single beats around the refined grid (median absolute deviation, ms, and
//! how many beats had a clear onset). Opens the database read-only.

use std::path::PathBuf;

use dap_engine::aligner::{AlignRequest, measure};
use rusqlite::{Connection, OpenFlags};

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args.next().expect("usage: beat_report <library.db> [max tracks] [every nth]");
    let max: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(40);
    let step: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open db");
    let mut st = conn
        .prepare(
            "SELECT t.id, r.path, f.rel_path, a.bpm, a.first_beat_ms, f.duration_ms,
                    coalesce(a.bpm_confidence, 0)
             FROM track t JOIN file f ON f.id = t.file_id
             JOIN library_root r ON r.id = f.root_id
             JOIN analysis a ON a.track_id = t.id
             WHERE f.missing = 0 AND a.bpm > 0 AND a.first_beat_ms IS NOT NULL
             ORDER BY t.id",
        )
        .unwrap();
    type Row = (i64, PathBuf, String, f64, i64, i64, f64);
    let rows: Vec<Row> = st
        .query_map([], |r| {
            let rel: String = r.get(2)?;
            Ok((
                r.get(0)?,
                PathBuf::from(r.get::<_, String>(1)?).join(&rel),
                rel,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .step_by(step.max(1))
        .take(max)
        .collect();
    println!("  BPM  conf | grid err ms (beats) | refine ms | spread ms (beats with onset) | track");
    let (mut ok, mut half, mut loose, mut drift, mut n) = (0, 0, 0, 0, 0);
    for (track_id, path, rel, bpm, first_ms, duration_ms, conf) in rows {
        let period = 60.0 / bpm;
        let end_s = duration_ms as f64 / 1000.0;
        let req = AlignRequest {
            track_id,
            path,
            from_s: (end_s - 26.0).max(0.0),
            to_s: (end_s - 2.0).max(4.0),
            period_s: period,
            at_end: true,
        };
        let Some(m) = measure(&req) else {
            println!("{bpm:>6.1} {conf:.2} | (no onsets) | {rel}");
            continue;
        };
        n += 1;
        // Grid error: measured beat minus the nearest grid beat.
        let first = first_ms as f64 / 1000.0;
        let k = ((m.beat_s - first) / period).round();
        let err = m.beat_s - (first + k * period);
        let frac = err / period;
        let mut dev: Vec<f64> = m.offsets.iter().map(|o| o.abs()).collect();
        dev.sort_by(f64::total_cmp);
        let mad = dev.get(dev.len() / 2).copied().unwrap_or(f64::NAN);
        let verdict = if (frac.abs() - 0.5).abs() < 0.08 {
            half += 1;
            "HALF"
        } else if !m.reliable() {
            loose += 1;
            "LOOSE"
        } else if err.abs() > 0.015 {
            drift += 1;
            "DRIFT"
        } else {
            ok += 1;
            "ok"
        };
        println!(
            "{bpm:>6.1} {conf:.2} | {:>7.1} ({:>+5.2}) | {:>6.1} | {:>5.1} ({:>2}) {verdict:5} | {rel}",
            err * 1000.0,
            frac,
            (m.beat_s - m.rough_s) * 1000.0,
            mad * 1000.0,
            m.offsets.len(),
        );
    }
    println!(
        "{n} tracks: {ok} ok, {drift} grid drifted (fixed by the aligner), {half} half a beat off, {loose} without a steady beat there (no beatmatch)"
    );
}
