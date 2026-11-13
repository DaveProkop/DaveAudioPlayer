//! Energy of a track (0–1).
//!
//! Track energy combines what is measured reliably anyway: integrated loudness (loud,
//! dense masters vs. quiet, dynamic ones) and tempo (ballads are slow). Features such as
//! brightness or transient density were tried on a real library and did not separate
//! ballads from dance tracks.
//!
//! Both are stored by the analysis already, so the energy is derived when read.

/// Loudness and tempo ranges mapped to 0–1, and their weights.
const LUFS: (f64, f64) = (-16.0, -7.0);
const BPM: (f64, f64) = (80.0, 140.0);
const WEIGHTS: (f64, f64) = (0.6, 0.4);

fn norm(v: f64, (lo, hi): (f64, f64)) -> f64 {
    ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// Energy 0–1 from integrated loudness (LUFS) and tempo (BPM); an unknown tempo counts as
/// mid-tempo.
pub fn track_energy(lufs: f64, bpm: Option<f64>) -> f64 {
    let tempo = bpm.map_or(0.5, |b| norm(b, BPM));
    WEIGHTS.0 * norm(lufs, LUFS) + WEIGHTS.1 * tempo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loud_fast_tracks_have_more_energy_than_quiet_slow_ones() {
        let edm = track_energy(-8.0, Some(128.0));
        let ballad = track_energy(-14.0, Some(70.0));
        let old_rock = track_energy(-17.0, Some(130.0));
        assert!(edm > 0.8, "{edm}");
        assert!(ballad < 0.25, "{ballad}");
        assert!(old_rock > ballad && old_rock < edm, "{old_rock}");
        assert_eq!(track_energy(-30.0, None), 0.2);
    }
}
