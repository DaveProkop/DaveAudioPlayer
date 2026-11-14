//! Local beat alignment (thread `aligner`): measures where the beats really fall around a
//! mix point.
//!
//! A track's beat grid is its first beat plus a steady period. Far from the first beat a
//! tiny tempo error adds up (0.1 BPM is a third of a beat after 400 beats), so before a
//! beat-based transition the control thread asks for the beat phase near the end of the
//! outgoing track and near the first beat of the incoming one, measured from those few
//! seconds of audio alone.

use std::path::PathBuf;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, unbounded};
use dap_analysis::TempoDetector;
use dap_audio_io::Decoder;

/// Where to measure: seconds `from`..`to` of a track whose beats are `period_s` apart.
#[derive(Debug, Clone)]
pub struct AlignRequest {
    pub track_id: i64,
    pub path: PathBuf,
    pub from_s: f64,
    pub to_s: f64,
    pub period_s: f64,
    /// The outgoing side (near the track's end), else the incoming side (its start).
    pub at_end: bool,
}

/// A measured beat: the time (seconds in the track) of one beat in the window.
#[derive(Debug, Clone, Copy)]
pub struct AlignResult {
    pub track_id: i64,
    pub at_end: bool,
    pub beat_s: f64,
}

pub struct Aligner {
    tx: Option<Sender<AlignRequest>>,
    thread: Option<JoinHandle<()>>,
    pub done: Receiver<AlignResult>,
}

impl Aligner {
    pub fn start() -> Self {
        let (tx, rx) = unbounded::<AlignRequest>();
        let (done_tx, done) = unbounded();
        let thread = std::thread::Builder::new()
            .name("aligner".into())
            .spawn(move || {
                for req in rx {
                    if let Some(beat_s) = measure(&req) {
                        let _ = done_tx.send(AlignResult {
                            track_id: req.track_id,
                            at_end: req.at_end,
                            beat_s,
                        });
                    }
                }
            })
            .ok();
        Self {
            tx: thread.as_ref().map(|_| tx),
            thread,
            done,
        }
    }

    pub fn request(&self, req: AlignRequest) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(req);
        }
    }
}

impl Drop for Aligner {
    fn drop(&mut self) {
        self.tx = None; // ends the thread's loop
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Decodes the window and finds the beat phase in it.
fn measure(req: &AlignRequest) -> Option<f64> {
    let mut decoder = Decoder::open(&req.path).ok()?;
    let info = decoder.info().clone();
    let rate = f64::from(info.sample_rate);
    decoder.seek((req.from_s.max(0.0) * rate) as u64).ok()?;
    let start_s = decoder.position_frames() as f64 / rate;
    let wanted = ((req.to_s - start_s).max(0.0) * rate) as usize;
    let ch = info.channels.max(1);
    let mut tempo = TempoDetector::new(info.sample_rate, info.channels);
    let mut mono: Vec<f32> = Vec::with_capacity(wanted);
    while mono.len() < wanted {
        match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                tempo.push(chunk);
                mono.extend(chunk.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32));
            }
            _ => break,
        }
    }
    let rough = tempo.beat_phase(req.period_s)?;
    Some(start_s + refine_onsets(&mono, rate, rough, req.period_s))
}

/// The onset envelope places a beat only to within a few milliseconds, and differently
/// for a slow kick than for a sharp click. Near each beat of the rough phase this finds
/// the exact onset in the audio itself (where the energy of the next 10 ms most exceeds
/// that of the 10 ms before, which works for low and high sounds alike) and moves the
/// phase by the median difference.
fn refine_onsets(mono: &[f32], rate: f64, rough_s: f64, period_s: f64) -> f64 {
    const WINDOW_S: f64 = 0.01;
    const STEP_S: f64 = 0.0005;
    const SEARCH_S: f64 = 0.02;
    let w = ((rate * WINDOW_S) as usize).max(1);
    let step = ((rate * STEP_S) as usize).max(1);
    let mut prefix = Vec::with_capacity(mono.len() + 1);
    prefix.push(0.0f64);
    for &x in mono {
        let last = *prefix.last().expect("prefix");
        prefix.push(last + f64::from(x) * f64::from(x));
    }
    let energy = |a: usize, b: usize| (prefix[b] - prefix[a]) / (b - a) as f64;
    let mut offsets = Vec::new();
    let mut beat = rough_s;
    while (beat + SEARCH_S) * rate + (w as f64) < mono.len() as f64 {
        let lo = (((beat - SEARCH_S) * rate) as usize).max(w);
        let hi = ((beat + SEARCH_S) * rate) as usize;
        let best = (lo..hi)
            .step_by(step)
            .map(|i| {
                let ratio = (energy(i, i + w) + 1e-10) / (energy(i - w, i) + 1e-10);
                (i, 10.0 * ratio.log10())
            })
            .max_by(|a, b| a.1.total_cmp(&b.1));
        // Only clear onsets count (the energy at least 4x what came before).
        if let Some((i, _)) = best.filter(|b| b.1 > 6.0) {
            offsets.push(i as f64 / rate - beat);
        }
        beat += period_s;
    }
    if offsets.len() < 4 {
        return rough_s;
    }
    offsets.sort_by(f64::total_cmp);
    rough_s + offsets[offsets.len() / 2]
}
