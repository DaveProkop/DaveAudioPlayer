//! Track analysis: BPM/beatgrid, key, EBU R128 loudness, energy, intro/outro.
//!
//! Analysis works on decoded PCM handed in by the caller, so this crate stays free of
//! file formats and platform code.

pub mod chroma;
pub mod key;
pub mod loudness;

pub use chroma::KeyDetector;
pub use key::{Camelot, Key, key_from_chroma, parse_key_tag};
pub use loudness::{Loudness, LoudnessMeter, normalization_gain_db};
