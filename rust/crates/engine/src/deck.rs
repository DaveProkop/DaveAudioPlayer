//! Deck decoder threads: decode → resample to the output rate → remap channels
//! (→ time-stretch for beatmatching) → ring.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use dap_audio_io::{Decoder, StreamResampler, remap_channels};
use dap_mixer::{DeckInput, PcmProducer, pcm_ring};
use dap_stretch::Stretch;

/// Buffered audio per deck (docs: 0.5–1 s of reserve).
const RING_SECONDS: f64 = 1.0;
/// Output frames per time-stretch step.
const STRETCH_BLOCK: usize = 512;

/// What a deck thread reports back to the control thread.
#[derive(Default)]
pub struct DeckInfo {
    /// Error text when the file could not be played.
    pub failure: OnceLock<String>,
    /// Position of the track's end in output frames, once the file is open
    /// ([`UNKNOWN_FRAMES`] until then, or when the format does not say).
    pub end_frame: AtomicU64,
}

pub const UNKNOWN_FRAMES: u64 = u64::MAX;

impl DeckInfo {
    pub fn end_frame(&self) -> Option<u64> {
        let v = self.end_frame.load(Ordering::Acquire);
        (v != UNKNOWN_FRAMES).then_some(v)
    }
}

/// A tempo schedule for a beatmatched deck, in output frames from its start: `rate` source
/// frames per output frame for `hold_frames`, then a linear ramp back to 1 over
/// `ramp_frames`. Source frames are counted at the output sample rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StretchPlan {
    pub rate: f64,
    pub hold_frames: u64,
    pub ramp_frames: u64,
}

impl StretchPlan {
    /// Playback rate (source frames per output frame) at output frame `out`.
    pub fn rate_at(&self, out: f64) -> f64 {
        let (h, r) = (self.hold_frames as f64, self.ramp_frames as f64);
        if out <= h {
            self.rate
        } else if out < h + r {
            self.rate + (1.0 - self.rate) * (out - h) / r
        } else {
            1.0
        }
    }

    /// Source frames played by output frame `out` (the integral of the rate).
    pub fn source_at(&self, out: f64) -> f64 {
        let (h, r, a) = (self.hold_frames as f64, self.ramp_frames as f64, self.rate);
        let out = out.max(0.0);
        if out <= h {
            a * out
        } else if out < h + r {
            let u = out - h;
            a * h + a * u + (1.0 - a) * u * u / (2.0 * r)
        } else {
            a * h + (a + 1.0) / 2.0 * r + (out - h - r)
        }
    }

    /// The output frame at which source frame `source` plays (inverse of [`source_at`]).
    ///
    /// [`source_at`]: Self::source_at
    pub fn output_at(&self, source: f64) -> f64 {
        if source <= 0.0 {
            return 0.0;
        }
        // Monotonic, so bisection is exact enough and cannot go wrong.
        let (mut lo, mut hi) = (0.0, source / self.rate.min(1.0) + 1.0);
        for _ in 0..64 {
            let mid = (lo + hi) / 2.0;
            if self.source_at(mid) < source {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }
}

/// Starts decoding `path` from `start_ms` on its own thread; `gain` is the linear deck gain.
///
/// With a `stretch` plan the deck starts exactly at `start_ms` (a beat) and follows the
/// plan's tempo; its output frames then count from 0 instead of from `start_ms`.
///
/// Returns immediately; a file that cannot be opened or decoded produces a deck that ends
/// at once with `failure` set, so the player skips it instead of stopping.
pub fn spawn_deck(
    token: u64,
    path: Option<PathBuf>,
    start_ms: u64,
    stretch: Option<StretchPlan>,
    gain: f32,
    out_rate: u32,
    out_channels: usize,
) -> (DeckInput, Arc<DeckInfo>) {
    let (tx, rx) = pcm_ring((f64::from(out_rate) * RING_SECONDS) as usize, out_channels);
    let finished = Arc::new(AtomicBool::new(false));
    let info = Arc::new(DeckInfo {
        failure: OnceLock::new(),
        end_frame: AtomicU64::new(UNKNOWN_FRAMES),
    });
    let start_frame = match stretch {
        Some(_) => 0,
        None => start_ms * u64::from(out_rate) / 1000,
    };

    let thread_finished = finished.clone();
    let thread_info = info.clone();
    let spawned = thread::Builder::new()
        .name(format!("deck-{token}"))
        .spawn(move || {
            let result = match (path, stretch) {
                (Some(p), None) => decode(&p, start_ms, out_rate, out_channels, tx, &thread_info),
                (Some(p), Some(plan)) => {
                    decode_stretched(&p, start_ms, plan, out_rate, out_channels, tx, &thread_info)
                }
                (None, _) => Err("file is not available".into()),
            };
            if let Err(e) = result {
                let _ = thread_info.failure.set(e);
            }
            thread_finished.store(true, Ordering::Release);
        });
    if let Err(e) = spawned {
        let _ = info
            .failure
            .set(format!("cannot start decoder thread: {e}"));
        finished.store(true, Ordering::Release);
    }

    (
        DeckInput {
            token,
            rx,
            finished,
            start_frame,
            gain,
        },
        info,
    )
}

/// A file as a stream at the output rate and channel count.
struct Source {
    decoder: Decoder,
    resampler: StreamResampler,
    in_channels: usize,
    out_channels: usize,
    resampled: Vec<f32>,
    done: bool,
    /// Position of the next frame [`Source::read`] returns, in output-rate frames.
    position: u64,
    /// Where the file ends, in output-rate frames, when the format says.
    total: Option<u64>,
}

impl Source {
    /// Opens `path` at output-rate frame `start`.
    fn open(path: &Path, start: u64, out_rate: u32, out_channels: usize) -> Result<Self, String> {
        let mut decoder = Decoder::open(path).map_err(|e| e.to_string())?;
        let info = decoder.info().clone();
        let ratio = f64::from(out_rate) / f64::from(info.sample_rate);
        if start > 0 {
            decoder
                .seek((start as f64 / ratio) as u64)
                .map_err(|e| e.to_string())?;
        }
        // The resampler turns n input frames into exactly round(n * ratio) output frames.
        let position = (decoder.position_frames() as f64 * ratio).round() as u64;
        let total = info.total_frames.map(|t| (t as f64 * ratio).round() as u64);
        let resampler = StreamResampler::new(info.sample_rate, out_rate, info.channels)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            decoder,
            resampler,
            in_channels: info.channels,
            out_channels,
            resampled: Vec::new(),
            done: false,
            position,
            total,
        })
    }

    /// Appends the next piece of audio to `out`; false once the stream has ended.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        if self.done {
            return Ok(false);
        }
        match self.decoder.next_chunk() {
            Ok(Some(chunk)) => self
                .resampler
                .process(chunk, &mut self.resampled)
                .map_err(|e| e.to_string())?,
            Ok(None) => {
                self.resampler
                    .finish(&mut self.resampled)
                    .map_err(|e| e.to_string())?;
                self.done = true;
            }
            // A damaged stream ends the track early; what was decoded still plays.
            Err(_) => {
                let _ = self.resampler.finish(&mut self.resampled);
                self.done = true;
            }
        }
        let before = out.len();
        remap_channels(&self.resampled, self.in_channels, self.out_channels, out);
        self.resampled.clear();
        self.position += ((out.len() - before) / self.out_channels) as u64;
        Ok(true)
    }
}

fn decode(
    path: &Path,
    start_ms: u64,
    out_rate: u32,
    out_channels: usize,
    mut tx: PcmProducer,
    deck: &DeckInfo,
) -> Result<(), String> {
    let start_frame = start_ms * u64::from(out_rate) / 1000;
    let mut source = Source::open(path, start_frame, out_rate, out_channels)?;
    if let Some(total) = source.total {
        let remaining = total.saturating_sub(source.position);
        deck.end_frame
            .store(start_frame + remaining, Ordering::Release);
    }
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let more = source.read(&mut buf)?;
        if !push_all(&mut tx, &buf) {
            return Ok(()); // deck was dropped (skip, seek, stop)
        }
        if !more {
            return Ok(());
        }
    }
}

/// Plays from output-rate source frame X = `start_ms` following `plan`, so that output
/// frame 0 is exactly X.
///
/// The stretcher's output lags its input by `input_latency + output_latency * rate` source
/// frames, so it is primed (`seek`) with input ending that far past X and then fed from
/// there.
fn decode_stretched(
    path: &Path,
    start_ms: u64,
    plan: StretchPlan,
    out_rate: u32,
    out_channels: usize,
    mut tx: PcmProducer,
    deck: &DeckInfo,
) -> Result<(), String> {
    let ch = out_channels;
    let mut stretch = Stretch::new(ch, out_rate);
    let x = (start_ms * u64::from(out_rate) / 1000) as i64;
    let (in_lat, out_lat) = (
        stretch.input_latency() as i64,
        stretch.output_latency() as i64,
    );
    let p0 = x + in_lat + (out_lat as f64 * plan.rate).round() as i64;
    let preroll_start = p0 - 2 * in_lat;

    let mut source = Source::open(path, preroll_start.max(0) as u64, out_rate, ch)?;
    if let Some(total) = source.total {
        let end = plan.output_at((total as i64 - x).max(0) as f64).round() as u64;
        deck.end_frame.store(end, Ordering::Release);
    }
    let end_frame = deck.end_frame();

    // `fifo` holds source frames from `fifo_pos` on.
    let mut fifo: Vec<f32> = Vec::new();
    let mut fifo_pos = source.position as i64;
    if fifo_pos > preroll_start {
        // Before the start of the file (or the seek landed late): silence.
        let pad = (fifo_pos - preroll_start) as usize;
        fifo.resize(pad * ch, 0.0);
        fifo_pos = preroll_start;
    }
    // Fills the fifo until it reaches source frame `until`; false at the end of the file.
    let mut fill = |fifo: &mut Vec<f32>, fifo_pos: i64, until: i64| -> Result<bool, String> {
        while fifo_pos + (fifo.len() / ch) as i64 <= until {
            if !source.read(fifo)? {
                return Ok(false);
            }
        }
        Ok(true)
    };
    fill(&mut fifo, fifo_pos, p0)?;
    // Drop anything before the preroll (a seek that landed early).
    if fifo_pos < preroll_start {
        let skip = ((preroll_start - fifo_pos) as usize * ch).min(fifo.len());
        fifo.drain(..skip);
        fifo_pos = preroll_start;
    }
    let primed = (((p0 - fifo_pos) as usize) * ch).min(fifo.len());
    stretch.seek(&fifo[..primed], plan.rate);
    fifo.drain(..primed);
    fifo_pos += (primed / ch) as i64;

    let mut out = vec![0.0f32; STRETCH_BLOCK * ch];
    let mut produced: u64 = 0;
    let push = |tx: &mut PcmProducer, samples: &[f32], produced: &mut u64| -> bool {
        // Never past the announced end, so the transition plan out of this deck holds.
        let frames = (samples.len() / ch) as u64;
        let keep = end_frame.map_or(frames, |e| e.saturating_sub(*produced).min(frames));
        *produced += keep;
        push_all(tx, &samples[..keep as usize * ch])
    };
    loop {
        let target = p0
            + plan
                .source_at((produced + STRETCH_BLOCK as u64) as f64)
                .floor() as i64;
        let more = fill(&mut fifo, fifo_pos, target)?;
        let available = fifo_pos + (fifo.len() / ch) as i64;
        if !more && available < target {
            // End of the file: play out what is left, then the stretcher's tail.
            let left = fifo.len() / ch;
            let rate = plan.rate_at(produced as f64);
            let frames = (left as f64 / rate).round() as usize;
            let mut tail = vec![0.0f32; frames * ch];
            stretch.process(&fifo, &mut tail);
            let mut flushed = vec![0.0f32; out_lat as usize * ch];
            stretch.flush(&mut flushed);
            tail.extend_from_slice(&flushed);
            if !push(&mut tx, &tail, &mut produced) {
                return Ok(());
            }
            // Exactly as long as announced.
            if let Some(end) = end_frame {
                let silence = vec![0.0f32; (end.saturating_sub(produced) as usize) * ch];
                push(&mut tx, &silence, &mut produced);
            }
            return Ok(());
        }
        let n = ((target - fifo_pos) as usize).min(fifo.len() / ch);
        stretch.process(&fifo[..n * ch], &mut out);
        fifo.drain(..n * ch);
        fifo_pos += n as i64;
        if !push(&mut tx, &out, &mut produced) {
            return Ok(());
        }
        if end_frame.is_some_and(|e| produced >= e) {
            return Ok(());
        }
    }
}

/// Pushes everything, waiting while the ring is full. Returns false if the deck is gone.
fn push_all(tx: &mut PcmProducer, mut samples: &[f32]) -> bool {
    while !samples.is_empty() {
        if tx.is_abandoned() {
            return false;
        }
        let n = tx.push(samples);
        samples = &samples[n..];
        if n == 0 {
            thread::sleep(Duration::from_millis(5));
        }
    }
    !tx.is_abandoned()
}

#[cfg(test)]
mod tests {
    use super::StretchPlan;

    #[test]
    fn stretch_plan_maps_both_ways() {
        let p = StretchPlan {
            rate: 1.05,
            hold_frames: 1000,
            ramp_frames: 2000,
        };
        assert_eq!(p.source_at(500.0), 525.0);
        // Ramp: the average rate is halfway.
        assert!((p.source_at(3000.0) - (1050.0 + 1.025 * 2000.0)).abs() < 1e-9);
        assert!((p.source_at(4000.0) - (1050.0 + 2050.0 + 1000.0)).abs() < 1e-9);
        for s in [0.0, 10.0, 1050.0, 2000.0, 3100.0, 10_000.0] {
            assert!((p.source_at(p.output_at(s)) - s).abs() < 1e-6, "{s}");
        }
        assert_eq!(p.rate_at(2000.0), 1.025);
    }
}
