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

/// The result for a request: a beat (seconds in the track) and the local beat period,
/// or `None` when the window has no steady beat to align on.
#[derive(Debug, Clone, Copy)]
pub struct AlignResult {
    pub track_id: i64,
    pub at_end: bool,
    pub beat: Option<(f64, f64)>,
}

/// A measurement counts with at least this many clear onsets…
const MIN_ONSETS: usize = 8;
/// …that stray from the grid by at most this much (median, seconds).
const MAX_SPREAD_S: f64 = 0.012;

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
                    let beat = measure(&req)
                        .filter(Measurement::reliable)
                        .map(|m| (m.beat_s, m.period_s));
                    let _ = done_tx.send(AlignResult {
                        track_id: req.track_id,
                        at_end: req.at_end,
                        beat,
                    });
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

/// What a measurement found.
#[derive(Debug, Clone)]
pub struct Measurement {
    /// A beat (seconds in the track), refined to the onsets.
    pub beat_s: f64,
    /// The same from the onset envelope alone.
    pub rough_s: f64,
    /// Each beat's onset minus the refined beat grid (seconds), for every beat that had
    /// a clear onset: how steady the beats are.
    pub offsets: Vec<f64>,
    /// The beat period the onsets follow (the requested one, corrected by a line fit).
    pub period_s: f64,
}

impl Measurement {
    /// Enough clear, steady beats to align on.
    pub fn reliable(&self) -> bool {
        let mut dev: Vec<f64> = self.offsets.iter().map(|o| o.abs()).collect();
        dev.sort_by(f64::total_cmp);
        self.offsets.len() >= MIN_ONSETS && dev[dev.len() / 2] <= MAX_SPREAD_S
    }
}

/// Decodes the window and finds the beat phase in it.
pub fn measure(req: &AlignRequest) -> Option<Measurement> {
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
    let onsets = onsets_near_beats(&mono, rate, rough, req.period_s);
    // A line through (beat number, onset offset): its slope corrects the period, its
    // value in the middle of the window places the beats there.
    let (mut period, mut phase) = (req.period_s, rough);
    if onsets.len() >= MIN_ONSETS {
        let n = onsets.len() as f64;
        let mk = onsets.iter().map(|o| o.0 as f64).sum::<f64>() / n;
        let mo = onsets.iter().map(|o| o.1).sum::<f64>() / n;
        let var = onsets.iter().map(|o| (o.0 as f64 - mk).powi(2)).sum::<f64>();
        if var > 0.0 {
            let slope = onsets.iter().map(|o| (o.0 as f64 - mk) * (o.1 - mo)).sum::<f64>() / var;
            // A real tempo error is tiny; a big slope means the fit caught noise.
            if slope.abs() < 0.005 * req.period_s {
                period += slope;
            }
        }
        // The beat nearest the middle, from the median onset around the corrected grid.
        let mut residual: Vec<f64> = onsets
            .iter()
            .map(|&(k, o)| o - (period - req.period_s) * (k as f64 - mk))
            .collect();
        residual.sort_by(f64::total_cmp);
        phase = rough + mk * req.period_s + residual[residual.len() / 2] - mk * period;
    }
    let offsets = onsets
        .iter()
        .map(|&(k, o)| rough + k as f64 * req.period_s + o - (phase + k as f64 * period))
        .collect();
    Some(Measurement {
        beat_s: start_s + phase,
        rough_s: start_s + rough,
        offsets,
        period_s: period,
    })
}

/// The onset envelope places a beat only to within a few milliseconds, and differently
/// for a slow kick than for a sharp click. Near each beat of the rough phase this finds
/// the exact onset in the audio itself (where the energy of the next 10 ms most exceeds
/// that of the 10 ms before, which works for low and high sounds alike). Returns (beat
/// number, onset minus the rough beat) for every beat with a clear onset.
fn onsets_near_beats(mono: &[f32], rate: f64, rough_s: f64, period_s: f64) -> Vec<(usize, f64)> {
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
    let mut onsets = Vec::new();
    let mut k = 0usize;
    loop {
        let beat = rough_s + k as f64 * period_s;
        if (beat + SEARCH_S) * rate + (w as f64) >= mono.len() as f64 {
            break;
        }
        let lo = (((beat - SEARCH_S) * rate).max(0.0) as usize).max(w);
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
            onsets.push((k, i as f64 / rate - beat));
        }
        k += 1;
    }
    onsets
}
