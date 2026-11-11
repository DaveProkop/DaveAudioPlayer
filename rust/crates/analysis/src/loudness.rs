//! EBU R128 integrated loudness and true peak, and the playback gain derived from them.

use ebur128::{EbuR128, Mode};

/// Target loudness for volume normalization (ReplayGain 2.0 reference level).
pub const TARGET_LUFS: f64 = -18.0;
/// Highest true peak a normalized track may reach.
pub const PEAK_CEILING_DB: f64 = -1.0;
/// Assumed loudness of a track that has not been analyzed yet (typical modern master).
pub const UNKNOWN_LUFS: f64 = -14.0;
const MAX_GAIN_DB: f64 = 12.0;
const MIN_GAIN_DB: f64 = -24.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// Integrated loudness in LUFS; `None` for silence (nothing above the gate).
    pub integrated_lufs: Option<f64>,
    /// Highest true peak over all channels in dBTP; `None` for digital silence.
    pub true_peak_db: Option<f64>,
}

/// Measures one track, fed in interleaved chunks.
pub struct LoudnessMeter {
    inner: EbuR128,
    channels: u32,
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32, channels: usize) -> Result<Self, String> {
        let channels = channels as u32;
        let inner = EbuR128::new(channels, sample_rate, Mode::I | Mode::TRUE_PEAK)
            .map_err(|e| format!("loudness meter: {e:?}"))?;
        Ok(Self { inner, channels })
    }

    /// Adds interleaved frames.
    pub fn push(&mut self, interleaved: &[f32]) -> Result<(), String> {
        self.inner
            .add_frames_f32(interleaved)
            .map_err(|e| format!("loudness meter: {e:?}"))
    }

    pub fn finish(&self) -> Loudness {
        let integrated = self
            .inner
            .loudness_global()
            .ok()
            .filter(|l| l.is_finite());
        let peak = (0..self.channels)
            .filter_map(|c| self.inner.true_peak(c).ok())
            .fold(0.0f64, f64::max);
        Loudness {
            integrated_lufs: integrated,
            true_peak_db: (peak > 0.0).then(|| 20.0 * peak.log10()),
        }
    }
}

/// Gain in dB that brings a track to [`TARGET_LUFS`] without pushing its true peak over
/// [`PEAK_CEILING_DB`]. Unknown loudness assumes [`UNKNOWN_LUFS`].
pub fn normalization_gain_db(integrated_lufs: Option<f64>, true_peak_db: Option<f64>) -> f64 {
    let mut gain = TARGET_LUFS - integrated_lufs.unwrap_or(UNKNOWN_LUFS);
    if let Some(peak) = true_peak_db {
        gain = gain.min(PEAK_CEILING_DB - peak);
    }
    gain.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, seconds: f64, amplitude: f32) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds) as usize;
        (0..frames)
            .flat_map(|i| {
                let s = amplitude
                    * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / f64::from(rate)).sin()
                        as f32;
                [s, s]
            })
            .collect()
    }

    fn measure(samples: &[f32]) -> Loudness {
        let mut m = LoudnessMeter::new(48_000, 2).unwrap();
        for chunk in samples.chunks(4096) {
            m.push(chunk).unwrap();
        }
        m.finish()
    }

    #[test]
    fn stereo_1khz_sine_matches_the_reference() {
        // EBU Tech 3341: a 1 kHz sine at 0 dBFS in both channels reads 0 LUFS, so an
        // amplitude of 0.1 (-20 dBFS) reads -20 LUFS.
        let l = measure(&sine(48_000, 5.0, 0.1));
        let lufs = l.integrated_lufs.unwrap();
        assert!((lufs + 20.0).abs() < 0.2, "{lufs}");
        let peak = l.true_peak_db.unwrap();
        assert!((peak + 20.0).abs() < 0.2, "{peak}");
    }

    #[test]
    fn silence_has_no_loudness() {
        let l = measure(&vec![0.0; 48_000 * 2 * 3]);
        assert_eq!(l.integrated_lufs, None);
        assert_eq!(l.true_peak_db, None);
    }

    #[test]
    fn gain_reaches_target_but_respects_the_peak_ceiling() {
        // Loud master: just turned down.
        assert_eq!(normalization_gain_db(Some(-8.0), Some(0.5)), -10.0);
        // Quiet track with headroom: turned up to the target.
        assert_eq!(normalization_gain_db(Some(-24.0), Some(-12.0)), 6.0);
        // Quiet but peaky (classical): limited by the true peak.
        assert_eq!(normalization_gain_db(Some(-24.0), Some(-3.0)), 2.0);
        // Not analyzed yet.
        assert_eq!(normalization_gain_db(None, None), -4.0);
    }
}
