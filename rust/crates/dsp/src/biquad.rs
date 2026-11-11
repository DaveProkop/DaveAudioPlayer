//! Second-order IIR filters after the RBJ "Audio EQ Cookbook", in transposed direct form II.
//!
//! Coefficients and state are `f64`: low-frequency bands (31 Hz at 48 kHz) lose too much
//! precision in `f32`.

use std::f64::consts::PI;

/// Normalized biquad coefficients (`a0 == 1`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl BiquadCoeffs {
    /// Passes the signal through unchanged.
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        let inv = 1.0 / a0;
        Self {
            b0: b0 * inv,
            b1: b1 * inv,
            b2: b2 * inv,
            a1: a1 * inv,
            a2: a2 * inv,
        }
    }

    /// Returns `(cos w0, sin w0)`; the frequency is clamped below Nyquist to stay stable.
    fn omega(sample_rate: f64, freq: f64) -> (f64, f64) {
        let freq = freq.clamp(1.0, sample_rate * 0.49);
        let w0 = 2.0 * PI * freq / sample_rate;
        (w0.cos(), w0.sin())
    }

    /// Peaking (bell) filter.
    pub fn peaking(sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let (cos, sin) = Self::omega(sample_rate, freq);
        let alpha = sin / (2.0 * q);
        Self::normalized(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        )
    }

    /// Low shelf filter.
    pub fn low_shelf(sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let (cos, sin) = Self::omega(sample_rate, freq);
        let k = 2.0 * a.sqrt() * sin / (2.0 * q);
        Self::normalized(
            a * ((a + 1.0) - (a - 1.0) * cos + k),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - k),
            (a + 1.0) + (a - 1.0) * cos + k,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - k,
        )
    }

    /// High shelf filter.
    pub fn high_shelf(sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let (cos, sin) = Self::omega(sample_rate, freq);
        let k = 2.0 * a.sqrt() * sin / (2.0 * q);
        Self::normalized(
            a * ((a + 1.0) + (a - 1.0) * cos + k),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - k),
            (a + 1.0) - (a - 1.0) * cos + k,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - k,
        )
    }

    /// 12 dB/octave low-pass filter.
    pub fn low_pass(sample_rate: f64, freq: f64, q: f64) -> Self {
        let (cos, sin) = Self::omega(sample_rate, freq);
        let alpha = sin / (2.0 * q);
        Self::normalized(
            (1.0 - cos) / 2.0,
            1.0 - cos,
            (1.0 - cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    /// 12 dB/octave high-pass filter.
    pub fn high_pass(sample_rate: f64, freq: f64, q: f64) -> Self {
        let (cos, sin) = Self::omega(sample_rate, freq);
        let alpha = sin / (2.0 * q);
        Self::normalized(
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    /// Magnitude response in dB at `freq` (for UI curves and tests).
    pub fn magnitude_db(&self, sample_rate: f64, freq: f64) -> f64 {
        let w = 2.0 * PI * freq / sample_rate;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -(self.b1 * s1 + self.b2 * s2);
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -(self.a1 * s1 + self.a2 * s2);
        let num = (num_re * num_re + num_im * num_im).sqrt();
        let den = (den_re * den_re + den_im * den_im).sqrt();
        20.0 * (num / den).log10()
    }
}

/// Per-channel filter memory.
#[derive(Debug, Clone, Copy, Default)]
pub struct BiquadState {
    z1: f64,
    z2: f64,
}

impl BiquadState {
    #[inline]
    pub fn process(&mut self, c: &BiquadCoeffs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Zeroes values that decayed into the denormal range, which is very slow on x86.
    #[inline]
    pub fn flush_denormals(&mut self) {
        const TINY: f64 = 1e-30;
        if self.z1.abs() < TINY {
            self.z1 = 0.0;
        }
        if self.z2.abs() < TINY {
            self.z2 = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn peaking_hits_gain_at_center_and_flat_far_away() {
        let c = BiquadCoeffs::peaking(SR, 1000.0, 1.0, 6.0);
        assert!(close(c.magnitude_db(SR, 1000.0), 6.0, 0.01));
        assert!(close(c.magnitude_db(SR, 20.0), 0.0, 0.05));
        assert!(close(c.magnitude_db(SR, 20_000.0), 0.0, 0.2));
    }

    #[test]
    fn zero_gain_peaking_is_flat() {
        let c = BiquadCoeffs::peaking(SR, 250.0, 1.41, 0.0);
        for f in [30.0, 250.0, 5000.0] {
            assert!(close(c.magnitude_db(SR, f), 0.0, 1e-9));
        }
    }

    #[test]
    fn shelves_reach_full_gain_on_their_side() {
        let low = BiquadCoeffs::low_shelf(SR, 200.0, 0.707, -9.0);
        assert!(close(low.magnitude_db(SR, 20.0), -9.0, 0.1));
        assert!(close(low.magnitude_db(SR, 10_000.0), 0.0, 0.1));
        let high = BiquadCoeffs::high_shelf(SR, 5000.0, 0.707, 9.0);
        assert!(close(high.magnitude_db(SR, 20_000.0), 9.0, 0.3));
        assert!(close(high.magnitude_db(SR, 50.0), 0.0, 0.1));
    }

    #[test]
    fn pass_filters_attenuate_the_other_side() {
        let lp = BiquadCoeffs::low_pass(SR, 1000.0, 0.707);
        assert!(close(lp.magnitude_db(SR, 1000.0), -3.0, 0.1));
        assert!(lp.magnitude_db(SR, 10_000.0) < -35.0);
        let hp = BiquadCoeffs::high_pass(SR, 1000.0, 0.707);
        assert!(hp.magnitude_db(SR, 100.0) < -35.0);
    }

    #[test]
    fn processing_matches_designed_gain() {
        let c = BiquadCoeffs::peaking(SR, 1000.0, 1.0, 6.0);
        let mut s = BiquadState::default();
        let n = SR as usize;
        let mut peak = 0.0f64;
        for i in 0..n {
            let x = (2.0 * PI * 1000.0 * i as f64 / SR).sin();
            let y = s.process(&c, x);
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        assert!(close(20.0 * peak.log10(), 6.0, 0.05));
    }

    #[test]
    fn frequency_above_nyquist_stays_stable() {
        let c = BiquadCoeffs::peaking(22_050.0, 16_000.0, 1.41, 12.0);
        let mut s = BiquadState::default();
        for i in 0..10_000 {
            let y = s.process(&c, if i == 0 { 1.0 } else { 0.0 });
            assert!(y.is_finite());
        }
    }
}
