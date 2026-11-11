//! 10-band graphic equalizer with preamp.

use crate::biquad::{BiquadCoeffs, BiquadState};
use crate::gain::db_to_gain;
use crate::smooth::LinearSmoother;

pub const BAND_COUNT: usize = 10;
/// ISO octave band centers.
pub const BAND_FREQUENCIES_HZ: [f64; BAND_COUNT] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
/// Band and preamp gains are clamped to `±MAX_GAIN_DB`.
pub const MAX_GAIN_DB: f32 = 12.0;
pub const MAX_CHANNELS: usize = 8;

/// One-octave bandwidth.
const BAND_Q: f64 = std::f64::consts::SQRT_2;
/// Coefficients are recomputed at most once per this many frames while a gain ramps.
const CONTROL_BLOCK_FRAMES: usize = 64;
const RAMP_MS: f64 = 50.0;
/// Bands at or above this fraction of the sample rate are skipped.
const NYQUIST_GUARD: f64 = 0.45;

/// Real-time safe 10-band EQ for interleaved `f32` audio.
///
/// Setters may be called from the audio thread (they never allocate); gain changes ramp
/// over ~50 ms to avoid clicks.
#[derive(Debug, Clone)]
pub struct Equalizer {
    sample_rate: f64,
    channels: usize,
    enabled: bool,
    gains_db: [f32; BAND_COUNT],
    preamp_db: f32,
    band_gain_db: [LinearSmoother; BAND_COUNT],
    preamp_gain: LinearSmoother,
    coeffs: [BiquadCoeffs; BAND_COUNT],
    band_usable: [bool; BAND_COUNT],
    state: [[BiquadState; MAX_CHANNELS]; BAND_COUNT],
}

impl Equalizer {
    /// Creates a flat, enabled equalizer.
    ///
    /// # Panics
    /// If `channels` is 0 or greater than [`MAX_CHANNELS`], or `sample_rate` is not positive.
    pub fn new(sample_rate: f64, channels: usize) -> Self {
        assert!(
            (1..=MAX_CHANNELS).contains(&channels),
            "unsupported channel count {channels}"
        );
        assert!(sample_rate > 0.0, "invalid sample rate {sample_rate}");
        let ramp = Self::ramp_len(sample_rate);
        let mut eq = Self {
            sample_rate,
            channels,
            enabled: true,
            gains_db: [0.0; BAND_COUNT],
            preamp_db: 0.0,
            band_gain_db: [LinearSmoother::new(0.0, ramp); BAND_COUNT],
            preamp_gain: LinearSmoother::new(1.0, ramp),
            coeffs: [BiquadCoeffs::IDENTITY; BAND_COUNT],
            band_usable: [false; BAND_COUNT],
            state: [[BiquadState::default(); MAX_CHANNELS]; BAND_COUNT],
        };
        for (b, usable) in eq.band_usable.iter_mut().enumerate() {
            *usable = BAND_FREQUENCIES_HZ[b] < sample_rate * NYQUIST_GUARD;
        }
        eq
    }

    fn ramp_len(sample_rate: f64) -> u32 {
        (RAMP_MS * sample_rate / 1000.0) as u32
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The user-set gain of `band` (independent of the enabled state).
    pub fn band_gain_db(&self, band: usize) -> f32 {
        self.gains_db[band]
    }

    pub fn preamp_db(&self) -> f32 {
        self.preamp_db
    }

    /// Sets the gain of one band; out-of-range bands are ignored.
    pub fn set_band_gain_db(&mut self, band: usize, gain_db: f32) {
        if band < BAND_COUNT {
            self.gains_db[band] = clamp_gain(gain_db);
            self.retarget();
        }
    }

    pub fn set_gains_db(&mut self, gains_db: &[f32; BAND_COUNT]) {
        for (dst, src) in self.gains_db.iter_mut().zip(gains_db) {
            *dst = clamp_gain(*src);
        }
        self.retarget();
    }

    pub fn set_preamp_db(&mut self, gain_db: f32) {
        self.preamp_db = clamp_gain(gain_db);
        self.retarget();
    }

    /// Disabling ramps everything to flat and then bypasses processing entirely.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.retarget();
    }

    /// Clears filter memory (call after a seek or track change without a crossfade).
    pub fn reset(&mut self) {
        for band in &mut self.state {
            for s in band.iter_mut() {
                s.reset();
            }
        }
    }

    /// Combined magnitude response of the target settings at `freq`, in dB (for UI curves).
    pub fn response_db(&self, freq: f64) -> f64 {
        if !self.enabled {
            return 0.0;
        }
        let bands: f64 = (0..BAND_COUNT)
            .filter(|&b| self.band_usable[b] && self.gains_db[b] != 0.0)
            .map(|b| {
                BiquadCoeffs::peaking(
                    self.sample_rate,
                    BAND_FREQUENCIES_HZ[b],
                    BAND_Q,
                    f64::from(self.gains_db[b]),
                )
                .magnitude_db(self.sample_rate, freq)
            })
            .sum();
        bands + f64::from(self.preamp_db)
    }

    fn retarget(&mut self) {
        for b in 0..BAND_COUNT {
            let target = if self.enabled { self.gains_db[b] } else { 0.0 };
            self.band_gain_db[b].set_target(target);
        }
        let preamp = if self.enabled {
            db_to_gain(self.preamp_db)
        } else {
            1.0
        };
        self.preamp_gain.set_target(preamp);
    }

    /// True when the output would equal the input.
    fn is_transparent(&self) -> bool {
        self.preamp_gain.is_settled()
            && self.preamp_gain.current() == 1.0
            && self
                .band_gain_db
                .iter()
                .all(|g| g.is_settled() && g.current() == 0.0)
    }

    fn band_active(&self, b: usize) -> bool {
        let g = &self.band_gain_db[b];
        self.band_usable[b] && !(g.is_settled() && g.current() == 0.0)
    }

    /// Processes interleaved audio in place. `buf.len()` must be a multiple of the channel count.
    pub fn process(&mut self, buf: &mut [f32]) {
        debug_assert_eq!(buf.len() % self.channels, 0);
        if self.is_transparent() {
            return;
        }
        let ch = self.channels;
        for block in buf.chunks_mut(CONTROL_BLOCK_FRAMES * ch) {
            let frames = block.len() / ch;

            // Collect active bands for this block into a fixed array (no allocation).
            let mut active = [0usize; BAND_COUNT];
            let mut active_len = 0;
            #[allow(clippy::needless_range_loop)] // b indexes several parallel arrays
            for b in 0..BAND_COUNT {
                if self.band_active(b) {
                    let gain = &mut self.band_gain_db[b];
                    if !gain.is_settled() || self.coeffs[b] == BiquadCoeffs::IDENTITY {
                        let db = gain.advance(frames as u32);
                        self.coeffs[b] = BiquadCoeffs::peaking(
                            self.sample_rate,
                            BAND_FREQUENCIES_HZ[b],
                            BAND_Q,
                            f64::from(db),
                        );
                    }
                    active[active_len] = b;
                    active_len += 1;
                } else if self.coeffs[b] != BiquadCoeffs::IDENTITY {
                    // Band just went flat: drop its filter and memory.
                    self.coeffs[b] = BiquadCoeffs::IDENTITY;
                    for s in &mut self.state[b] {
                        s.reset();
                    }
                }
            }
            let active = &active[..active_len];

            for frame in block.chunks_exact_mut(ch) {
                let pre = self.preamp_gain.next_value();
                for (c, sample) in frame.iter_mut().enumerate() {
                    let mut x = f64::from(*sample * pre);
                    for &b in active {
                        x = self.state[b][c].process(&self.coeffs[b], x);
                    }
                    *sample = x as f32;
                }
            }

            for &b in active {
                for s in &mut self.state[b][..ch] {
                    s.flush_denormals();
                }
            }
        }
    }
}

fn clamp_gain(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: f64 = 48_000.0;

    /// Stereo sine, interleaved, with the right channel at half amplitude.
    fn stereo_sine(freq: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * PI * freq * i as f32 / SR as f32).sin() * 0.25;
                [v, v * 0.5]
            })
            .collect()
    }

    fn rms(samples: impl Iterator<Item = f32>) -> f32 {
        let (sum, n) = samples.fold((0.0f64, 0usize), |(s, n), x| (s + f64::from(x * x), n + 1));
        (sum / n as f64).sqrt() as f32
    }

    fn channel(buf: &[f32], c: usize, skip_frames: usize) -> impl Iterator<Item = f32> + '_ {
        buf.as_chunks::<2>()
            .0
            .iter()
            .skip(skip_frames)
            .map(move |f| f[c])
    }

    fn process_in_chunks(eq: &mut Equalizer, buf: &mut [f32], chunk_frames: usize) {
        for chunk in buf.chunks_mut(chunk_frames * 2) {
            eq.process(chunk);
        }
    }

    #[test]
    fn flat_eq_is_bit_transparent() {
        let input = stereo_sine(440.0, 4800);
        let mut buf = input.clone();
        Equalizer::new(SR, 2).process(&mut buf);
        assert_eq!(buf, input);
    }

    #[test]
    fn boosted_band_raises_level_at_its_frequency() {
        let mut eq = Equalizer::new(SR, 2);
        eq.set_band_gain_db(5, 6.0); // 1 kHz
        let input = stereo_sine(1000.0, SR as usize);
        let mut buf = input.clone();
        process_in_chunks(&mut eq, &mut buf, 441);
        let half = SR as usize / 2;
        for c in 0..2 {
            let ratio = rms(channel(&buf, c, half)) / rms(channel(&input, c, half));
            assert!(
                (20.0 * ratio.log10() - 6.0).abs() < 0.15,
                "channel {c}: {ratio}"
            );
        }
    }

    #[test]
    fn boosted_band_leaves_distant_frequencies_alone() {
        let mut eq = Equalizer::new(SR, 2);
        eq.set_band_gain_db(0, 12.0); // 31 Hz
        let input = stereo_sine(8000.0, SR as usize / 2);
        let mut buf = input.clone();
        eq.process(&mut buf);
        let skip = SR as usize / 4;
        let ratio = rms(channel(&buf, 0, skip)) / rms(channel(&input, 0, skip));
        assert!((20.0 * ratio.log10()).abs() < 0.1);
    }

    #[test]
    fn preamp_scales_output() {
        let mut eq = Equalizer::new(SR, 2);
        eq.set_preamp_db(-6.0206);
        let input = stereo_sine(440.0, 9600);
        let mut buf = input.clone();
        eq.process(&mut buf);
        let last_in = input[input.len() - 2];
        let last_out = buf[buf.len() - 2];
        assert!((last_out - last_in * 0.5).abs() < 1e-5);
    }

    #[test]
    fn disabling_returns_to_bit_transparent_after_ramp() {
        let mut eq = Equalizer::new(SR, 2);
        eq.set_band_gain_db(3, 9.0);
        eq.set_preamp_db(-3.0);
        let mut warmup = stereo_sine(250.0, 4800);
        eq.process(&mut warmup);

        eq.set_enabled(false);
        let mut ramp = stereo_sine(250.0, 4800); // 100 ms > 50 ms ramp
        eq.process(&mut ramp);

        let input = stereo_sine(250.0, 1000);
        let mut buf = input.clone();
        eq.process(&mut buf);
        assert_eq!(buf, input);
        assert_eq!(
            eq.band_gain_db(3),
            9.0,
            "user setting is kept while disabled"
        );
    }

    #[test]
    fn gain_change_does_not_jump() {
        let mut eq = Equalizer::new(SR, 2);
        let mut buf = stereo_sine(1000.0, 4800);
        eq.process(&mut buf[..2400]);
        eq.set_band_gain_db(5, 12.0);
        eq.process(&mut buf[2400..]);
        let max_step = buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        // A 1 kHz sine at 0.25 amplitude moves at most ~0.033 per sample; +12 dB gives ~4x.
        assert!(max_step < 0.14, "max step {max_step}");
    }

    #[test]
    fn gains_are_clamped_and_nan_is_rejected() {
        let mut eq = Equalizer::new(SR, 2);
        eq.set_band_gain_db(1, 40.0);
        eq.set_band_gain_db(2, f32::NAN);
        eq.set_band_gain_db(99, 3.0);
        assert_eq!(eq.band_gain_db(1), MAX_GAIN_DB);
        assert_eq!(eq.band_gain_db(2), 0.0);
    }

    #[test]
    fn bands_above_nyquist_are_skipped_at_low_sample_rates() {
        let mut eq = Equalizer::new(22_050.0, 1);
        eq.set_gains_db(&[12.0; BAND_COUNT]);
        let mut buf: Vec<f32> = (0..22_050)
            .map(|i| if i % 100 == 0 { 0.5 } else { 0.0 })
            .collect();
        eq.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn response_curve_matches_settings() {
        let mut eq = Equalizer::new(SR, 2);
        eq.set_band_gain_db(5, 6.0);
        eq.set_preamp_db(-2.0);
        assert!((eq.response_db(1000.0) - 4.0).abs() < 0.2);
        eq.set_enabled(false);
        assert_eq!(eq.response_db(1000.0), 0.0);
    }
}
