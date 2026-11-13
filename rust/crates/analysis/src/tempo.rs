//! Tempo (BPM) detection: an onset strength envelope (spectral flux) over the whole track,
//! its autocorrelation, and the best beat period with a preference for common dance tempos.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

const FRAME: usize = 1024;
const HOP: usize = 256;
/// Reported tempos are folded into this range (half / double time).
pub const MIN_BPM: f64 = 70.0;
pub const MAX_BPM: f64 = 180.0;
/// Tempo prior: log-normal around this, so 120 wins over 60 or 240 for the same pulse.
const PRIOR_BPM: f64 = 128.0;
const PRIOR_OCTAVES: f64 = 0.7;

/// Feeds interleaved audio, gives the tempo of the whole track.
pub struct TempoDetector {
    channels: usize,
    frame_rate: f64,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    ring: Vec<f32>,
    ring_pos: usize,
    since_hop: usize,
    filled: usize,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    prev: Vec<f32>,
    /// Onset strength per hop.
    envelope: Vec<f32>,
    /// Onset strength of the bass bins only (kick drums), for the beat phase.
    low_envelope: Vec<f32>,
    low_bins: usize,
    /// Previous linear magnitudes of the bass bins.
    prev_low: Vec<f32>,
}

impl TempoDetector {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FRAME);
        let window = (0..FRAME)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / FRAME as f64).cos()) as f32)
            .collect();
        Self {
            channels: channels.max(1),
            frame_rate: f64::from(sample_rate) / HOP as f64,
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            input: vec![0.0; FRAME],
            fft,
            window,
            ring: vec![0.0; FRAME],
            ring_pos: 0,
            since_hop: 0,
            filled: 0,
            prev: vec![0.0; FRAME / 2 + 1],
            envelope: Vec::new(),
            low_envelope: Vec::new(),
            // Bins up to ~170 Hz.
            low_bins: ((170.0 * FRAME as f64 / f64::from(sample_rate)).ceil() as usize).max(2),
            prev_low: vec![0.0; FRAME / 2 + 1],
        }
    }

    /// Adds interleaved frames.
    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            self.ring[self.ring_pos] = frame.iter().sum::<f32>() / self.channels as f32;
            self.ring_pos = (self.ring_pos + 1) % FRAME;
            self.filled = (self.filled + 1).min(FRAME);
            self.since_hop += 1;
            if self.since_hop == HOP && self.filled == FRAME {
                self.since_hop = 0;
                self.onset();
            } else if self.since_hop == HOP {
                self.since_hop = 0;
            }
        }
    }

    /// Spectral flux of the newest frame: how much louder each bin got (log magnitude).
    fn onset(&mut self) {
        for i in 0..FRAME {
            self.input[i] = self.ring[(self.ring_pos + i) % FRAME] * self.window[i];
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return;
        }
        let mut flux = 0.0f32;
        let mut low = 0.0f32;
        for (k, (c, p)) in self.spectrum.iter().zip(self.prev.iter_mut()).enumerate() {
            let m = (1.0 + 100.0 * c.norm()).ln();
            let rise = (m - *p).max(0.0);
            flux += rise;
            if (1..=self.low_bins).contains(&k) {
                // Linear here: a kick is loud in these bins, broadband noise is not.
                let lin = c.norm();
                low += (lin - self.prev_low[k]).max(0.0);
                self.prev_low[k] = lin;
            }
            *p = m;
        }
        self.envelope.push(flux);
        self.low_envelope.push(low);
    }

    /// Tempo in BPM (within [MIN_BPM, MAX_BPM], 0.1 BPM steps) and a confidence 0–1;
    /// `None` for audio without a pulse or shorter than a few seconds.
    pub fn finish(&self) -> Option<(f64, f64)> {
        self.finish_grid()
            .map(|g| ((g.bpm * 10.0).round() / 10.0, g.confidence))
    }

    /// The beat grid: precise tempo and where the first beat falls. Assumes a steady
    /// tempo (true for most dance and pop music).
    pub fn finish_grid(&self) -> Option<Beatgrid> {
        let fr = self.frame_rate;
        let n = self.envelope.len();
        if n < (fr * 8.0) as usize {
            return None;
        }
        let env = rhythmic(&self.envelope, fr);
        let energy: f64 = env.iter().map(|v| v * v).sum();
        if energy <= 0.0 {
            return None;
        }

        // Autocorrelation over lags from 40 to 240 BPM (also the multiples used below).
        let lag_of = |bpm: f64| 60.0 * fr / bpm;
        let min_lag = lag_of(240.0).floor() as usize;
        let max_lag = (lag_of(40.0).ceil() as usize * 2).min(n - 1);
        let mut acf = vec![0.0f64; max_lag + 2];
        for (lag, a) in acf.iter_mut().enumerate().take(max_lag + 1).skip(min_lag / 2) {
            let mut s = 0.0;
            for i in lag..n {
                s += env[i] * env[i - lag];
            }
            *a = s / (n - lag) as f64;
        }
        let at = |x: f64| -> f64 {
            // Linear interpolation between integer lags.
            let i = x.floor() as usize;
            if i + 1 >= acf.len() {
                return 0.0;
            }
            let f = x - i as f64;
            acf[i] * (1.0 - f) + acf[i + 1] * f
        };

        // A beat period also lines up at 2x, 3x and 4x the lag; weight the candidates
        // by the tempo prior.
        let mut best = (0.0f64, 0.0f64); // (lag, score)
        let mut second = 0.0f64;
        let lo = lag_of(MAX_BPM * 1.05).floor() as usize;
        let hi = lag_of(MIN_BPM * 0.95).ceil() as usize;
        let mut steps = Vec::new();
        let mut lag = lo as f64;
        while lag <= hi as f64 {
            let comb = at(lag) + 0.5 * at(2.0 * lag) + 0.33 * at(3.0 * lag) + 0.25 * at(4.0 * lag);
            let bpm = 60.0 * fr / lag;
            let prior = (-0.5 * ((bpm / PRIOR_BPM).log2() / PRIOR_OCTAVES).powi(2)).exp();
            steps.push((lag, comb * prior));
            lag += 0.1;
        }
        for &(lag, score) in &steps {
            if score > best.1 {
                best = (lag, score);
            }
        }
        // Runner-up: the best score more than 4 % in tempo away from the winner.
        for &(lag, score) in &steps {
            if (lag / best.0 - 1.0).abs() > 0.04 && score > second {
                second = score;
            }
        }
        if best.1 <= 0.0 {
            return None;
        }
        let mut bpm = 60.0 * fr / best.0;
        while bpm < MIN_BPM {
            bpm *= 2.0;
        }
        while bpm >= MAX_BPM {
            bpm /= 2.0;
        }
        let confidence = ((best.1 - second) / best.1 * 3.0).clamp(0.0, 1.0);

        // Beat phase near the start and near the end; the beats between them give the
        // tempo far more precisely than the autocorrelation peak. The phase follows the
        // kick drum (bass onsets), not hi-hats on the off-beats; without bass, everything.
        let low = rhythmic(&self.low_envelope, fr);
        let env = if low.iter().map(|v| v * v).sum::<f64>() > energy * 0.01 { low } else { env };
        let mut period = 60.0 * fr / bpm;
        let window = (fr * 30.0).min(n as f64 / 2.0);
        let start = phase(&env, 0.0, window, period);
        if n as f64 > fr * 60.0 {
            let end = phase(&env, n as f64 - window, n as f64, period);
            let beats = ((end - start) / period).round();
            if beats >= 1.0 {
                let refined = (end - start) / beats;
                if (refined / period - 1.0).abs() < 0.02 {
                    period = refined;
                }
            }
        }
        let first = start.rem_euclid(period);
        // An onset shows when the transient sits in the middle of the analysis frame.
        let first_beat_s = (first * HOP as f64 + FRAME as f64 / 2.0) / (fr * HOP as f64);
        Some(Beatgrid {
            bpm: 60.0 * fr / period,
            confidence,
            first_beat_s: first_beat_s.rem_euclid(60.0 / (60.0 * fr / period)),
        })
    }
}

/// The rhythmic part of an onset envelope: minus its moving average (~0.5 s), no negatives.
fn rhythmic(envelope: &[f32], fr: f64) -> Vec<f64> {
    let n = envelope.len();
    let half = (fr * 0.25) as usize;
    let mut prefix = vec![0.0f64; n + 1];
    for (i, &v) in envelope.iter().enumerate() {
        prefix[i + 1] = prefix[i] + f64::from(v);
    }
    (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
            let mean = (prefix[b] - prefix[a]) / (b - a) as f64;
            (f64::from(envelope[i]) - mean).max(0.0)
        })
        .collect()
}

/// Precise tempo and beat phase of a track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beatgrid {
    /// Beats per minute, not rounded.
    pub bpm: f64,
    pub confidence: f64,
    /// Time of the first beat in seconds (0 ≤ x < one beat); later beats follow every
    /// `60 / bpm` seconds.
    pub first_beat_s: f64,
}

/// The envelope frame (fractional) in `[from, from + period)` where beats spaced by
/// `period` line up best with the onsets in `[from, to)`.
fn phase(env: &[f64], from: f64, to: f64, period: f64) -> f64 {
    let at = |x: f64| -> f64 {
        let i = x.floor() as usize;
        if i + 1 >= env.len() {
            return 0.0;
        }
        let f = x - i as f64;
        env[i] * (1.0 - f) + env[i + 1] * f
    };
    let mut best = (from, f64::MIN);
    let mut p = from;
    while p < from + period {
        let mut score = 0.0;
        let mut x = p;
        while x < to {
            score += at(x);
            x += period;
        }
        if score > best.1 {
            best = (p, score);
        }
        p += 0.25;
    }
    best.0
}

/// Parses a BPM tag ("128", "128.00", "127,5"); `None` outside 30–300.
pub fn parse_bpm_tag(tag: &str) -> Option<f64> {
    let v: f64 = tag.trim().replace(',', ".").parse().ok()?;
    (30.0..=300.0).contains(&v).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// A drum pattern: kick on every beat, hi-hat on the off-beats, some noise, stereo.
    fn pattern(bpm: f64, seconds: f64) -> Vec<f32> {
        let n = (f64::from(RATE) * seconds) as usize;
        let beat = 60.0 / bpm * f64::from(RATE);
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (f64::from(seed >> 16) / 32768.0 - 1.0) as f32
        };
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let pos = i as f64 % beat;
            let half = (i as f64 + beat / 2.0) % beat;
            let t = pos / f64::from(RATE);
            let kick = (2.0 * std::f64::consts::PI * 55.0 * t).sin() * (-t * 25.0).exp();
            let th = half / f64::from(RATE);
            let hat = f64::from(noise()) * (-th * 60.0).exp() * 0.3;
            let s = (kick * 0.6 + hat) as f32 + noise() * 0.01;
            out.extend([s, s]);
        }
        out
    }

    fn detect(samples: &[f32]) -> Option<f64> {
        let mut d = TempoDetector::new(RATE, 2);
        for chunk in samples.chunks(4096) {
            d.push(chunk);
        }
        d.finish().map(|(b, _)| b)
    }

    #[test]
    fn finds_common_dance_tempos() {
        for bpm in [92.0, 120.0, 128.0, 140.0, 174.0] {
            let found = detect(&pattern(bpm, 30.0)).expect("a tempo");
            assert!((found - bpm).abs() < 1.0, "{bpm} BPM: found {found}");
        }
    }

    fn grid(samples: &[f32]) -> Beatgrid {
        let mut d = TempoDetector::new(RATE, 2);
        for chunk in samples.chunks(4096) {
            d.push(chunk);
        }
        d.finish_grid().expect("a beat grid")
    }

    #[test]
    fn beat_grid_is_precise_and_in_phase() {
        // 127.5 BPM for 90 s, starting after 0.3 s of silence.
        let mut audio = vec![0.0f32; (f64::from(RATE) * 0.3) as usize * 2];
        audio.extend(pattern(127.5, 90.0));
        let g = grid(&audio);
        assert!((g.bpm - 127.5).abs() < 0.02, "{} BPM", g.bpm);
        assert!((g.first_beat_s - 0.3).abs() < 0.015, "first beat at {} s", g.first_beat_s);
        // The grid still matches at the end: beat 190 (≈ 89.7 s) within 15 ms.
        let beat = 60.0 / g.bpm;
        let predicted = g.first_beat_s + 190.0 * beat;
        let actual = 0.3 + 190.0 * 60.0 / 127.5;
        assert!((predicted - actual).abs() < 0.015, "drift {} s", predicted - actual);
    }

    #[test]
    fn silence_and_short_audio_have_no_tempo() {
        assert_eq!(detect(&vec![0.0; RATE as usize * 2 * 20]), None);
        assert_eq!(detect(&pattern(120.0, 3.0)), None);
    }

    #[test]
    fn bpm_tags() {
        assert_eq!(parse_bpm_tag("128"), Some(128.0));
        assert_eq!(parse_bpm_tag(" 127,5 "), Some(127.5));
        assert_eq!(parse_bpm_tag("0"), None);
        assert_eq!(parse_bpm_tag("fast"), None);
    }
}
