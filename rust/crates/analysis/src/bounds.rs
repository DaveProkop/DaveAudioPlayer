//! Where a track really starts and ends: leading and trailing silence, and fade-outs.
//!
//! Levels are relative to the track's own loud level (95th percentile of 50 ms blocks), so
//! quiet masters and loud ones are treated alike.

/// Block length for the level envelope.
const BLOCK_S: f64 = 0.05;
/// Blocks per smoothed step of the fade detection (0.5 s).
const STEP_BLOCKS: usize = 10;
/// Below this (relative to the loud level) a block counts as silence.
const SILENCE_DB: f64 = -40.0;
/// A fade-out is mixed out where it has dropped this far.
const FADE_MIX_OUT_DB: f64 = -20.0;
/// "Still loud" before a fade-out begins.
const LOUD_DB: f64 = -6.0;
/// Shorter decays (a final chord ringing out) are not fade-outs.
const MIN_FADE_S: f64 = 6.0;
/// A fade-out drops at least this far…
const MIN_FADE_DROP_DB: f64 = 20.0;
/// …and keeps falling: this share of its 0.5 s steps do not rise noticeably.
const FADE_SMOOTHNESS: f64 = 0.85;
/// Tracks quieter than this throughout are treated as silent.
const SILENT_TRACK_DBFS: f64 = -70.0;

/// The audible span of a track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    /// First audible sound (leading silence skipped).
    pub start_s: f64,
    /// Where a transition out of the track should end: the end of the sound, or for a
    /// fade-out the point where it has faded by 20 dB.
    pub end_s: f64,
    /// The track ends with a fade-out.
    pub fade_out: bool,
}

/// Collects a level envelope from interleaved PCM.
pub struct BoundsDetector {
    channels: usize,
    block_frames: usize,
    acc: f64,
    count: usize,
    /// Mean square per block.
    blocks: Vec<f64>,
}

impl BoundsDetector {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            channels: channels.max(1),
            block_frames: ((f64::from(sample_rate) * BLOCK_S) as usize).max(1),
            acc: 0.0,
            count: 0,
            blocks: Vec::new(),
        }
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            self.acc += frame
                .iter()
                .map(|&s| f64::from(s) * f64::from(s))
                .sum::<f64>()
                / self.channels as f64;
            self.count += 1;
            if self.count == self.block_frames {
                self.blocks.push(self.acc / self.count as f64);
                self.acc = 0.0;
                self.count = 0;
            }
        }
    }

    /// `None` for a silent (or empty) track.
    pub fn finish(mut self) -> Option<Bounds> {
        if self.count > 0 {
            self.blocks.push(self.acc / self.count as f64);
        }
        let db: Vec<f64> = self
            .blocks
            .iter()
            .map(|&p| 10.0 * (p + 1e-15).log10())
            .collect();
        let mut sorted: Vec<f64> = db.iter().copied().filter(|&d| d > -100.0).collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(f64::total_cmp);
        let loud = sorted[((sorted.len() - 1) as f64 * 0.95) as usize];
        if loud < SILENT_TRACK_DBFS {
            return None;
        }
        let audible = |d: f64| d > loud + SILENCE_DB;
        let first = db.iter().position(|&d| audible(d))?;
        let last = db.iter().rposition(|&d| audible(d))?;
        let start_s = first as f64 * BLOCK_S;
        let sound_end_s = (last + 1) as f64 * BLOCK_S;

        // Smoothed steps up to the end of the sound.
        let steps: Vec<f64> = self.blocks[..=last]
            .chunks(STEP_BLOCKS)
            .map(|c| 10.0 * (c.iter().sum::<f64>() / c.len() as f64 + 1e-15).log10())
            .collect();
        let step_s = BLOCK_S * STEP_BLOCKS as f64;
        let fade = steps
            .iter()
            .rposition(|&d| d >= loud + LOUD_DB)
            .and_then(|last_loud| {
                let tail = &steps[last_loud..];
                let long = (tail.len() - 1) as f64 * step_s >= MIN_FADE_S;
                let drop = tail[0] - tail[tail.len() - 1] >= MIN_FADE_DROP_DB;
                let falling = tail.windows(2).filter(|w| w[1] <= w[0] + 1.5).count() as f64
                    >= FADE_SMOOTHNESS * (tail.len() - 1) as f64;
                (long && drop && falling).then_some(last_loud)
            });
        let end_s = match fade {
            Some(last_loud) => steps[last_loud..]
                .iter()
                .position(|&d| d < loud + FADE_MIX_OUT_DB)
                .map_or(sound_end_s, |k| (last_loud + k) as f64 * step_s),
            None => sound_end_s,
        };
        Some(Bounds {
            start_s,
            end_s: end_s.max(start_s),
            fade_out: fade.is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 8000;

    /// Mono noise-like signal whose level follows `level_db(t)` (dBFS, None = silence).
    fn render(seconds: f64, level_db: impl Fn(f64) -> Option<f64>) -> Vec<f32> {
        let mut seed = 12345u32;
        (0..(seconds * f64::from(RATE)) as usize)
            .map(|i| {
                let t = i as f64 / f64::from(RATE);
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f64 / f64::from(1u32 << 24) * 2.0 - 1.0;
                // Uniform noise has RMS 1/sqrt(3).
                level_db(t).map_or(0.0, |db| {
                    (noise * 3f64.sqrt() * 10f64.powf(db / 20.0)) as f32
                })
            })
            .collect()
    }

    fn bounds(pcm: &[f32]) -> Bounds {
        let mut d = BoundsDetector::new(RATE, 1);
        for chunk in pcm.chunks(1000) {
            d.push(chunk);
        }
        d.finish().expect("bounds")
    }

    #[test]
    fn fade_out_is_mixed_out_when_it_has_dropped_20_db() {
        // 1 s silence, 20 s at -12 dBFS, a 10 s fade by 48 dB, 2 s silence.
        let pcm = render(33.0, |t| match t {
            t if t < 1.0 => None,
            t if t < 21.0 => Some(-12.0),
            t if t < 31.0 => Some(-12.0 - 4.8 * (t - 21.0)),
            _ => None,
        });
        let b = bounds(&pcm);
        assert!((b.start_s - 1.0).abs() < 0.06, "{b:?}");
        assert!(b.fade_out, "{b:?}");
        // -20 dB after 20/4.8 = 4.2 s of fading.
        assert!((b.end_s - 25.2).abs() < 0.6, "{b:?}");
    }

    #[test]
    fn a_hard_ending_is_not_a_fade() {
        // Music to 20 s, a 1.5 s ringing tail, 3 s of silence.
        let pcm = render(24.5, |t| match t {
            t if t < 20.0 => Some(-10.0 + 4.0 * (t * 3.0).sin()),
            t if t < 21.5 => Some(-10.0 - 30.0 * (t - 20.0)),
            _ => None,
        });
        let b = bounds(&pcm);
        assert_eq!(b.start_s, 0.0);
        assert!(!b.fade_out, "{b:?}");
        // The tail drops below -40 dB relative after about a second.
        assert!(b.end_s > 20.5 && b.end_s < 21.6, "{b:?}");
    }

    #[test]
    fn silence_has_no_bounds() {
        let mut d = BoundsDetector::new(RATE, 2);
        d.push(&vec![0.0; 16_000]);
        assert_eq!(d.finish(), None);
    }
}
