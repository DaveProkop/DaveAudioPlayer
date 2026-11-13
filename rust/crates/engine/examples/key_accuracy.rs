//! Measures the key detection against keys found in tags: for every track whose key came
//! from its tags, decodes the file once, computes the chroma in several variants and
//! compares the key each variant (and each profile set) finds with the tag.
//!
//!   cargo run --release -p dap-engine --example key_accuracy -- <library.db>
//!
//! Opens the database read-only; the app may keep running.

use std::path::{Path, PathBuf};

use dap_analysis::chroma::ChromaOptions;
use dap_analysis::key::{ALL_PROFILES, DEFAULT_PROFILES, key_from_chroma_with};
use dap_analysis::{Camelot, KeyDetector};
use dap_audio_io::Decoder;
use rusqlite::{Connection, OpenFlags};

fn variants() -> Vec<(&'static str, ChromaOptions)> {
    let d = ChromaOptions::default();
    vec![
        ("default", d),
        ("peaks", ChromaOptions { peaks_only: true, ..d }),
        ("peaks+no-norm", ChromaOptions { peaks_only: true, normalize_blocks: false, ..d }),
        ("peaks 55-2093", ChromaOptions { peaks_only: true, min_hz: 55.0, ..d }),
    ]
}

/// One chroma per variant, from a single decoding pass.
fn chromas_of(path: &Path) -> Option<Vec<[f64; 12]>> {
    let mut decoder = Decoder::open(path).ok()?;
    let info = decoder.info().clone();
    let mut keys: Vec<KeyDetector> = variants()
        .into_iter()
        .map(|(_, o)| KeyDetector::with_options(info.sample_rate, info.channels, o))
        .collect();
    while let Ok(Some(chunk)) = decoder.next_chunk() {
        for k in &mut keys {
            k.push(chunk);
        }
    }
    Some(keys.iter().map(KeyDetector::chroma).collect())
}

#[derive(Default)]
struct Score {
    exact: usize,
    relative: usize,
    neighbour: usize,
    near: usize,
    wrong: usize,
}

impl Score {
    fn add(&mut self, tag: Camelot, found: Camelot) {
        let step = (i32::from(found.number) - i32::from(tag.number)).rem_euclid(12);
        if found == tag {
            self.exact += 1;
        } else if found.number == tag.number {
            self.relative += 1;
        } else if found.minor == tag.minor && (step == 1 || step == 11) {
            self.neighbour += 1;
        } else if step == 1 || step == 11 {
            self.near += 1;
        } else {
            self.wrong += 1;
        }
    }

    fn print(&self, name: &str) {
        let n = (self.exact + self.relative + self.neighbour + self.near + self.wrong).max(1) as f64;
        let p = |x: usize| 100.0 * x as f64 / n;
        println!(
            "{name:24} exact {:5.1} %  relative {:4.1} %  neighbour {:4.1} %  wrong {:4.1} %  mixable {:5.1} %",
            p(self.exact),
            p(self.relative),
            p(self.neighbour),
            p(self.wrong),
            p(self.exact + self.relative + self.neighbour)
        );
    }
}

fn main() {
    let db = std::env::args().nth(1).expect("usage: key_accuracy <library.db>");
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open db");
    let mut st = conn
        .prepare(
            "SELECT r.path, f.rel_path, a.key_camelot FROM analysis a
             JOIN track t ON t.id = a.track_id JOIN file f ON f.id = t.file_id
             JOIN library_root r ON r.id = f.root_id
             WHERE a.key_confidence >= 1.0 AND f.missing = 0 ORDER BY t.id",
        )
        .unwrap();
    let rows: Vec<(PathBuf, Camelot)> = st
        .query_map([], |r| {
            let root: String = r.get(0)?;
            let rel: String = r.get(1)?;
            let key: String = r.get(2)?;
            Ok((PathBuf::from(root).join(rel), key))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .filter_map(|(p, k)| Some((p, Camelot::parse(&k)?)))
        .collect();

    let mut data: Vec<(Camelot, Vec<[f64; 12]>)> = Vec::new();
    for (i, (path, tag)) in rows.iter().enumerate() {
        if let Some(c) = chromas_of(path) {
            eprintln!("{:4}/{} {}", i + 1, rows.len(), path.display());
            data.push((*tag, c));
        }
    }
    println!("tracks with a tag key: {}", data.len());

    let names: Vec<&str> = variants().into_iter().map(|(n, _)| n).collect();
    println!("-- chroma variants, {} profiles --", DEFAULT_PROFILES.name);
    for (v, name) in names.iter().enumerate() {
        let mut s = Score::default();
        for (tag, chromas) in &data {
            if let Some((k, _)) = key_from_chroma_with(&chromas[v], &DEFAULT_PROFILES) {
                s.add(*tag, k.camelot());
            }
        }
        s.print(name);
    }
    println!("-- every variant x profile set: exact / mixable --");
    for (v, name) in names.iter().enumerate() {
        for p in &ALL_PROFILES {
            let mut s = Score::default();
            for (tag, chromas) in &data {
                if let Some((k, _)) = key_from_chroma_with(&chromas[v], p) {
                    s.add(*tag, k.camelot());
                }
            }
            s.print(&format!("{name} / {}", p.name));
        }
    }
}
