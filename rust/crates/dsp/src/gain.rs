//! Decibel helpers.

/// Converts decibels to a linear amplitude factor.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Converts a linear amplitude factor to decibels. Returns `-inf` for zero.
#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for db in [-24.0, -6.0, 0.0, 3.0, 12.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-4);
        }
        assert!((db_to_gain(6.0206) - 2.0).abs() < 1e-4);
        assert_eq!(db_to_gain(0.0), 1.0);
    }
}
