//! Time-stretching: changes tempo without changing pitch (beatmatching), using the vendored
//! Signalsmith Stretch library (MIT, see `vendor/*/LICENSE.txt`).
//!
//! Runs in the deck decoder threads, never in the audio callback.

use std::ffi::c_int;

#[repr(C)]
struct RawStretch {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn dap_stretch_new(channels: c_int, sample_rate: f32) -> *mut RawStretch;
    fn dap_stretch_free(s: *mut RawStretch);
    fn dap_stretch_reset(s: *mut RawStretch);
    fn dap_stretch_input_latency(s: *const RawStretch) -> c_int;
    fn dap_stretch_output_latency(s: *const RawStretch) -> c_int;
    fn dap_stretch_seek(s: *mut RawStretch, input: *const f32, in_frames: c_int, rate: f64);
    fn dap_stretch_process(
        s: *mut RawStretch,
        input: *const f32,
        in_frames: c_int,
        output: *mut f32,
        out_frames: c_int,
    );
    fn dap_stretch_flush(s: *mut RawStretch, output: *mut f32, out_frames: c_int);
}

/// A time-stretcher for interleaved `f32` audio with a fixed channel count.
pub struct Stretch {
    raw: *mut RawStretch,
    channels: usize,
}

// SAFETY: the C++ object has no thread affinity; `&mut self` keeps it to one thread at a time.
unsafe impl Send for Stretch {}

impl Stretch {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        assert!(channels > 0, "at least one channel");
        // SAFETY: plain constructor; the pointer is owned by this value.
        let raw = unsafe { dap_stretch_new(channels as c_int, sample_rate as f32) };
        assert!(!raw.is_null(), "dap_stretch_new failed");
        Self { raw, channels }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn reset(&mut self) {
        // SAFETY: `raw` is valid for the lifetime of `self`.
        unsafe { dap_stretch_reset(self.raw) }
    }

    /// Input frames the stretcher needs ahead of the output position.
    pub fn input_latency(&self) -> usize {
        // SAFETY: `raw` is valid.
        unsafe { dap_stretch_input_latency(self.raw) as usize }
    }

    /// Output frames by which the output lags the input.
    pub fn output_latency(&self) -> usize {
        // SAFETY: `raw` is valid.
        unsafe { dap_stretch_output_latency(self.raw) as usize }
    }

    /// Primes the stretcher with input before the playback point (avoids a fade-in),
    /// at `rate` input frames per output frame.
    pub fn seek(&mut self, input: &[f32], rate: f64) {
        let frames = input.len() / self.channels;
        // SAFETY: the buffer holds `frames` interleaved frames of `channels` samples.
        unsafe { dap_stretch_seek(self.raw, input.as_ptr(), frames as c_int, rate) }
    }

    /// Consumes all of `input` and writes all of `output` (both interleaved). The ratio of
    /// their lengths is the playback rate: more input than output speeds up.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let (inf, outf) = (input.len() / self.channels, output.len() / self.channels);
        // SAFETY: both buffers hold whole interleaved frames of `channels` samples.
        unsafe {
            dap_stretch_process(
                self.raw,
                input.as_ptr(),
                inf as c_int,
                output.as_mut_ptr(),
                outf as c_int,
            )
        }
    }

    /// Writes out what is still buffered (at the end of a stream).
    pub fn flush(&mut self, output: &mut [f32]) {
        let frames = output.len() / self.channels;
        // SAFETY: `output` holds whole interleaved frames.
        unsafe { dap_stretch_flush(self.raw, output.as_mut_ptr(), frames as c_int) }
    }
}

impl Drop for Stretch {
    fn drop(&mut self) {
        // SAFETY: created by dap_stretch_new, freed exactly once.
        unsafe { dap_stretch_free(self.raw) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn sine(freq: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / f64::from(RATE);
                let s = (0.5 * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32;
                [s, s]
            })
            .collect()
    }

    /// Frequency estimated from rising zero crossings of the left channel.
    fn frequency(samples: &[f32]) -> f64 {
        let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
        let crossings = left.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f64 * f64::from(RATE) / left.len() as f64
    }

    #[test]
    fn faster_tempo_keeps_the_pitch() {
        let mut s = Stretch::new(2, RATE);
        let input = sine(440.0, RATE as usize * 4);
        // 4 s of input into 3.2 s of output: 1.25x faster.
        let (block_in, block_out) = (1250, 1000);
        let mut out = Vec::new();
        for chunk in input.chunks_exact(block_in * 2) {
            let mut o = vec![0.0f32; block_out * 2];
            s.process(chunk, &mut o);
            out.extend_from_slice(&o);
        }
        assert_eq!(out.len() / 2, (RATE as usize * 4 / block_in) * block_out);
        // Skip the first second (latency, fade-in); the pitch must stay 440 Hz.
        let f = frequency(&out[RATE as usize * 2..]);
        assert!((f - 440.0).abs() < 5.0, "{f} Hz");
    }

    #[test]
    fn latencies_are_sane() {
        let s = Stretch::new(2, 48_000);
        assert!(s.input_latency() > 0 && s.input_latency() < 48_000);
        assert!(s.output_latency() > 0 && s.output_latency() < 48_000);
    }
}
