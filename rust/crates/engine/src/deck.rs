//! Deck decoder threads: decode → resample to the output rate → remap channels → ring.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use dap_audio_io::{Decoder, StreamResampler, remap_channels};
use dap_mixer::{DeckInput, PcmProducer, pcm_ring};

/// Buffered audio per deck (docs: 0.5–1 s of reserve).
const RING_SECONDS: f64 = 1.0;

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

/// Starts decoding `path` from `start_ms` on its own thread; `gain` is the linear deck gain.
///
/// Returns immediately; a file that cannot be opened or decoded produces a deck that ends
/// at once with `failure` set, so the player skips it instead of stopping.
pub fn spawn_deck(
    token: u64,
    path: Option<PathBuf>,
    start_ms: u64,
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
    let start_frame = start_ms * u64::from(out_rate) / 1000;

    let thread_finished = finished.clone();
    let thread_info = info.clone();
    let spawned = thread::Builder::new()
        .name(format!("deck-{token}"))
        .spawn(move || {
            let result = match path {
                Some(p) => decode(&p, start_ms, start_frame, out_rate, out_channels, tx, &thread_info),
                None => Err("file is not available".into()),
            };
            if let Err(e) = result {
                let _ = thread_info.failure.set(e);
            }
            thread_finished.store(true, Ordering::Release);
        });
    if let Err(e) = spawned {
        let _ = info.failure.set(format!("cannot start decoder thread: {e}"));
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

fn decode(
    path: &std::path::Path,
    start_ms: u64,
    start_frame: u64,
    out_rate: u32,
    out_channels: usize,
    mut tx: PcmProducer,
    deck: &DeckInfo,
) -> Result<(), String> {
    let mut decoder = Decoder::open(path).map_err(|e| e.to_string())?;
    let info = decoder.info().clone();
    if start_ms > 0 {
        decoder
            .seek(start_ms * u64::from(info.sample_rate) / 1000)
            .map_err(|e| e.to_string())?;
    }
    if let Some(total) = info.total_frames {
        // The resampler turns n input frames into exactly round(n * ratio) output frames.
        let remaining = total.saturating_sub(decoder.position_frames());
        let ratio = f64::from(out_rate) / f64::from(info.sample_rate);
        let end = start_frame + (remaining as f64 * ratio).round() as u64;
        deck.end_frame.store(end, Ordering::Release);
    }
    let mut resampler = StreamResampler::new(info.sample_rate, out_rate, info.channels)
        .map_err(|e| e.to_string())?;
    let mut resampled = Vec::new();
    let mut remapped = Vec::new();

    loop {
        let end = match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                resampler
                    .process(chunk, &mut resampled)
                    .map_err(|e| e.to_string())?;
                false
            }
            Ok(None) => {
                resampler
                    .finish(&mut resampled)
                    .map_err(|e| e.to_string())?;
                true
            }
            // A damaged stream ends the track early; what was decoded still plays.
            Err(_) => {
                let _ = resampler.finish(&mut resampled);
                true
            }
        };
        remapped.clear();
        remap_channels(&resampled, info.channels, out_channels, &mut remapped);
        resampled.clear();
        if !push_all(&mut tx, &remapped) {
            return Ok(()); // deck was dropped (skip, seek, stop)
        }
        if end {
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
