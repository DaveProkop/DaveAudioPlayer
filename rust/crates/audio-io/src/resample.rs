//! Streaming sample-rate conversion (rubato sinc) for interleaved `f32` audio.
//!
//! Runs on the deck decoder thread. Output is sample-accurate: the resampler delay is
//! trimmed at the start and [`StreamResampler::finish`] drains the tail, so a track of
//! `n` input frames yields exactly `round(n * out_rate / in_rate)` output frames (needed
//! for gapless playback).

use std::fmt;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters};

/// Input frames per resampler call.
const CHUNK_FRAMES: usize = 1024;
/// Headroom for adjusting the ratio later (clock drift compensation).
const MAX_RATIO_RELATIVE: f64 = 1.1;

#[derive(Debug)]
pub struct ResampleError(String);

impl fmt::Display for ResampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "resampler error: {}", self.0)
    }
}

impl std::error::Error for ResampleError {}

fn err(e: impl fmt::Display) -> ResampleError {
    ResampleError(e.to_string())
}

pub struct StreamResampler {
    /// `None` when input and output rates match (pass-through).
    inner: Option<Async<f32>>,
    channels: usize,
    ratio: f64,
    pending: Vec<f32>,
    scratch_in: Vec<f32>,
    scratch_out: Vec<f32>,
    delay_to_trim: usize,
    input_frames: u64,
    output_frames: u64,
}

impl StreamResampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Result<Self, ResampleError> {
        if in_rate == 0 || out_rate == 0 || channels == 0 {
            return Err(ResampleError("invalid rate or channel count".into()));
        }
        let ratio = f64::from(out_rate) / f64::from(in_rate);
        let inner = if in_rate == out_rate {
            None
        } else {
            Some(
                Async::<f32>::new_sinc(
                    ratio,
                    MAX_RATIO_RELATIVE,
                    &SincInterpolationParameters::default(),
                    CHUNK_FRAMES,
                    channels,
                    FixedAsync::Input,
                )
                .map_err(err)?,
            )
        };
        let delay_to_trim = inner.as_ref().map_or(0, |r| r.output_delay());
        Ok(Self {
            inner,
            channels,
            ratio,
            pending: Vec::new(),
            scratch_in: Vec::new(),
            scratch_out: Vec::new(),
            delay_to_trim,
            input_frames: 0,
            output_frames: 0,
        })
    }

    pub fn is_passthrough(&self) -> bool {
        self.inner.is_none()
    }

    /// Resamples `input` (any length, whole frames) and appends available output to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), ResampleError> {
        let ch = self.channels;
        self.input_frames += (input.len() / ch) as u64;
        if self.inner.is_none() {
            out.extend_from_slice(input);
            self.output_frames += (input.len() / ch) as u64;
            return Ok(());
        }
        self.pending.extend_from_slice(input);
        loop {
            let need = self.inner.as_ref().map_or(0, |r| r.input_frames_next());
            if self.pending.len() / ch < need {
                break;
            }
            let chunk: Vec<f32> = self.pending.drain(..need * ch).collect();
            self.run(&chunk, None, out, None)?;
        }
        Ok(())
    }

    /// Flushes buffered input and the resampler tail. Call once at end of stream.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), ResampleError> {
        if self.inner.is_none() {
            return Ok(());
        }
        let ch = self.channels;
        let expected = (self.input_frames as f64 * self.ratio).round() as u64;
        let pending = std::mem::take(&mut self.pending);
        let mut partial = pending.len() / ch;
        let mut chunk = pending;
        // Each round feeds a zero-padded chunk; bounded in case of a resampler bug.
        for _ in 0..64 {
            if self.output_frames >= expected {
                break;
            }
            self.run(&chunk, Some(partial), out, Some(expected))?;
            chunk.clear();
            partial = 0;
        }
        Ok(())
    }

    /// Discards all state (after a seek).
    pub fn reset(&mut self) {
        if let Some(r) = self.inner.as_mut() {
            r.reset();
            self.delay_to_trim = r.output_delay();
        }
        self.pending.clear();
        self.input_frames = 0;
        self.output_frames = 0;
    }

    fn run(
        &mut self,
        input: &[f32],
        partial_len: Option<usize>,
        out: &mut Vec<f32>,
        limit: Option<u64>,
    ) -> Result<(), ResampleError> {
        let ch = self.channels;
        let Some(r) = self.inner.as_mut() else {
            return Ok(());
        };
        let need = r.input_frames_next();
        // The input adapter must always cover a full chunk; pad with silence.
        self.scratch_in.clear();
        self.scratch_in.extend_from_slice(input);
        self.scratch_in.resize(need * ch, 0.0);
        let cap = r.output_frames_max();
        self.scratch_out.resize(cap * ch, 0.0);

        let in_adapter = InterleavedSlice::new(&self.scratch_in[..], ch, need).map_err(err)?;
        let mut out_adapter =
            InterleavedSlice::new_mut(&mut self.scratch_out[..], ch, cap).map_err(err)?;
        let indexing = partial_len.map(|p| Indexing::new().partial_len(p));
        let (_, written) = r
            .process_into_buffer(&in_adapter, &mut out_adapter, indexing.as_ref())
            .map_err(err)?;

        let skip = self.delay_to_trim.min(written);
        self.delay_to_trim -= skip;
        let mut n = (written - skip) as u64;
        if let Some(limit) = limit {
            n = n.min(limit.saturating_sub(self.output_frames));
        }
        let start = skip * ch;
        out.extend_from_slice(&self.scratch_out[start..start + n as usize * ch]);
        self.output_frames += n;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn stereo_sine(rate: u32, freq: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * PI * freq * i as f64 / f64::from(rate)).sin() as f32 * 0.5;
                [v, -v]
            })
            .collect()
    }

    fn resample_in_pieces(input: &[f32], from: u32, to: u32, piece_frames: usize) -> Vec<f32> {
        let mut r = StreamResampler::new(from, to, 2).unwrap();
        let mut out = Vec::new();
        for piece in input.chunks(piece_frames * 2) {
            r.process(piece, &mut out).unwrap();
        }
        r.finish(&mut out).unwrap();
        out
    }

    #[test]
    fn passthrough_is_identical() {
        let input = stereo_sine(48_000, 440.0, 5000);
        assert_eq!(resample_in_pieces(&input, 48_000, 48_000, 333), input);
    }

    #[test]
    fn output_length_is_exact() {
        for (from, to, frames) in [
            (44_100, 48_000, 44_100),
            (48_000, 44_100, 12_345),
            (96_000, 48_000, 1_000),
        ] {
            let input = stereo_sine(from, 1000.0, frames);
            let out = resample_in_pieces(&input, from, to, 777);
            let expected = (frames as f64 * f64::from(to) / f64::from(from)).round() as usize;
            assert_eq!(out.len(), expected * 2, "{from} -> {to}");
        }
    }

    #[test]
    fn signal_is_clean_and_aligned_within_one_sample() {
        let input = stereo_sine(44_100, 1000.0, 44_100);
        let out = resample_in_pieces(&input, 44_100, 48_000, 500);
        // rubato reports its delay in whole frames, so up to one output frame of
        // (inaudible) offset remains; find it and check the signal is otherwise clean.
        let best = (-10..=10)
            .map(|tenth| {
                let shift = f64::from(tenth) / 10.0;
                (1000..47_000)
                    .map(|i| {
                        let t = (i as f64 + shift) / 48_000.0;
                        let ideal = (2.0 * PI * 1000.0 * t).sin() as f32 * 0.5;
                        (out[i * 2] - ideal)
                            .abs()
                            .max((out[i * 2 + 1] + ideal).abs())
                    })
                    .fold(0.0f32, f32::max)
            })
            .fold(f32::MAX, f32::min);
        assert!(best < 2e-3, "max error {best}");
    }

    #[test]
    fn reset_allows_reuse() {
        let input = stereo_sine(44_100, 1000.0, 4410);
        let mut r = StreamResampler::new(44_100, 48_000, 2).unwrap();
        let mut first = Vec::new();
        r.process(&input, &mut first).unwrap();
        r.finish(&mut first).unwrap();
        r.reset();
        let mut second = Vec::new();
        r.process(&input, &mut second).unwrap();
        r.finish(&mut second).unwrap();
        assert_eq!(first, second);
    }
}
