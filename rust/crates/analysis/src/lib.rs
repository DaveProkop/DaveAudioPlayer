//! Track analysis: BPM/beatgrid, key, EBU R128 loudness, energy, intro/outro.
//!
//! Analysis works on decoded PCM handed in by the caller, so this crate stays free of
//! file formats and platform code.

pub mod loudness;

pub use loudness::{Loudness, LoudnessMeter, normalization_gain_db};
