//! Parameter smoothing to avoid zipper noise and clicks when values change.

/// Moves linearly from the current value to a target over a fixed number of samples.
///
/// Allocation-free and cheap, safe to use on the audio thread.
#[derive(Debug, Clone, Copy)]
pub struct LinearSmoother {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
    ramp_len: u32,
}

impl LinearSmoother {
    /// Creates a smoother resting at `value`. `ramp_len` is the ramp length in samples
    /// (0 = changes apply immediately).
    pub fn new(value: f32, ramp_len: u32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
            ramp_len,
        }
    }

    /// Changes the ramp length used by subsequent [`set_target`](Self::set_target) calls.
    pub fn set_ramp_len(&mut self, ramp_len: u32) {
        self.ramp_len = ramp_len;
    }

    /// Starts a ramp from the current value to `target`.
    pub fn set_target(&mut self, target: f32) {
        if target == self.target {
            return;
        }
        self.target = target;
        if self.ramp_len == 0 {
            self.current = target;
            self.remaining = 0;
        } else {
            self.remaining = self.ramp_len;
            self.step = (target - self.current) / self.ramp_len as f32;
        }
    }

    /// Jumps to `value` without ramping.
    pub fn set_immediate(&mut self, value: f32) {
        self.current = value;
        self.target = value;
        self.remaining = 0;
    }

    /// Advances by one sample and returns the new value.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            // Land exactly on the target to avoid accumulated float error.
            self.current = if self.remaining == 0 {
                self.target
            } else {
                self.current + self.step
            };
        }
        self.current
    }

    /// Advances by `samples` at once and returns the new value.
    #[inline]
    pub fn advance(&mut self, samples: u32) -> f32 {
        if samples >= self.remaining {
            self.current = self.target;
            self.remaining = 0;
        } else {
            self.remaining -= samples;
            self.current += self.step * samples as f32;
        }
        self.current
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn is_settled(&self) -> bool {
        self.remaining == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_linearly_and_lands_on_target() {
        let mut s = LinearSmoother::new(0.0, 4);
        s.set_target(1.0);
        let values: Vec<f32> = (0..6).map(|_| s.next_value()).collect();
        assert_eq!(values, [0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
        assert!(s.is_settled());
    }

    #[test]
    fn advance_matches_per_sample_steps() {
        let mut a = LinearSmoother::new(-6.0, 100);
        let mut b = a;
        a.set_target(6.0);
        b.set_target(6.0);
        for _ in 0..30 {
            a.next_value();
        }
        assert!((a.current() - b.advance(30)).abs() < 1e-4);
        assert_eq!(b.advance(1000), 6.0);
    }

    #[test]
    fn zero_ramp_is_immediate() {
        let mut s = LinearSmoother::new(0.0, 0);
        s.set_target(0.5);
        assert_eq!(s.current(), 0.5);
        assert!(s.is_settled());
    }

    #[test]
    fn retarget_mid_ramp_starts_from_current_value() {
        let mut s = LinearSmoother::new(0.0, 10);
        s.set_target(10.0);
        s.advance(5);
        s.set_target(0.0);
        assert_eq!(s.next_value(), 4.5);
    }
}
