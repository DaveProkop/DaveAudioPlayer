//! Key detection from audio: a chromagram (energy per pitch class) over the whole track,
//! matched against major/minor key profiles.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};
use realfft::num_complex::Complex;

use crate::key::{Key, key_from_chroma};

/// FFT size: about 0.37 s at 44.1 kHz, fine enough to separate semitones from ~C2 up.
const FFT_SIZE: usize = 16_384;
const MIN_HZ: f64 = 65.4; // C2
const MAX_HZ: f64 = 2_093.0; // C7

/// Feeds interleaved audio, gives the most likely key of the whole track.
pub struct KeyDetector {
    channels: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Bin index, pitch class and weight for every bin in range.
    bins: Vec<(usize, usize, f32)>,
    filled: usize,
    chroma: [f64; 12],
}

impl KeyDetector {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|i| {
                let x = i as f64 / FFT_SIZE as f64;
                (0.5 - 0.5 * (2.0 * std::f64::consts::PI * x).cos()) as f32
            })
            .collect();
        let rate = f64::from(sample_rate);
        let bins = (1..FFT_SIZE / 2)
            .filter_map(|k| {
                let f = k as f64 * rate / FFT_SIZE as f64;
                if !(MIN_HZ..=MAX_HZ).contains(&f) {
                    return None;
                }
                let midi = 69.0 + 12.0 * (f / 440.0).log2();
                let nearest = midi.round();
                // Bins between two semitones count little: less smearing into neighbours.
                let weight = (1.0 - 2.0 * (midi - nearest).abs()) as f32;
                Some((k, (nearest as i64).rem_euclid(12) as usize, weight))
            })
            .collect();
        Self {
            channels: channels.max(1),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            input: vec![0.0; FFT_SIZE],
            fft,
            window,
            bins,
            filled: 0,
            chroma: [0.0; 12],
        }
    }

    /// Adds interleaved frames.
    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            self.input[self.filled] = frame.iter().sum::<f32>() / self.channels as f32;
            self.filled += 1;
            if self.filled == FFT_SIZE {
                self.analyze_block();
                self.filled = 0;
            }
        }
    }

    fn analyze_block(&mut self) {
        for (s, w) in self.input.iter_mut().zip(&self.window) {
            *s *= w;
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return;
        }
        let mut block = [0.0f64; 12];
        for &(k, pc, w) in &self.bins {
            block[pc] += f64::from(self.spectrum[k].norm() * w);
        }
        // Each block counts the same, so loud passages do not decide alone; near-silence
        // is skipped.
        let total: f64 = block.iter().sum();
        if total > 1e-3 {
            for (c, b) in self.chroma.iter_mut().zip(block) {
                *c += b / total;
            }
        }
    }

    /// The most likely key and a confidence 0–1; `None` for silence or very short audio.
    pub fn finish(&self) -> Option<(Key, f64)> {
        key_from_chroma(&self.chroma)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn midi_hz(m: i32) -> f64 {
        440.0 * 2f64.powf(f64::from(m - 69) / 12.0)
    }

    /// Chords (MIDI notes) one second each, with a couple of harmonics, stereo.
    fn progression(chords: &[&[i32]]) -> Vec<f32> {
        let mut out = Vec::new();
        for _ in 0..4 {
            for chord in chords {
                for i in 0..RATE {
                    let t = f64::from(i) / f64::from(RATE);
                    let mut s = 0.0;
                    for &n in *chord {
                        for (h, a) in [(1.0, 1.0), (2.0, 0.4), (3.0, 0.2)] {
                            s += a * (2.0 * std::f64::consts::PI * midi_hz(n) * h * t).sin();
                        }
                    }
                    let s = (s * 0.08) as f32;
                    out.extend([s, s]);
                }
            }
        }
        out
    }

    fn detect(samples: &[f32]) -> String {
        let mut d = KeyDetector::new(RATE, 2);
        for chunk in samples.chunks(4096) {
            d.push(chunk);
        }
        d.finish().map(|(k, _)| k.camelot().to_string()).unwrap_or_default()
    }

    #[test]
    fn detects_c_major_from_i_iv_v_i() {
        // C–E–G, F–A–C, G–B–D, C–E–G with the bass note an octave down.
        let audio = progression(&[&[48, 60, 64, 67], &[53, 60, 65, 69], &[55, 59, 62, 67], &[48, 60, 64, 67]]);
        assert_eq!(detect(&audio), "8B");
    }

    #[test]
    fn detects_a_minor_from_i_iv_v_i() {
        // Am, Dm, E (with G#), Am.
        let audio = progression(&[&[45, 57, 60, 64], &[50, 57, 62, 65], &[52, 56, 59, 64], &[45, 57, 60, 64]]);
        assert_eq!(detect(&audio), "8A");
    }

    #[test]
    fn silence_has_no_key() {
        assert_eq!(detect(&vec![0.0; RATE as usize * 4]), "");
    }
}
