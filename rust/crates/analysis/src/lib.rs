//! Track analysis: BPM/beatgrid, key, EBU R128 loudness, energy, intro/outro.
//!
//! Analysis works on decoded PCM handed in by the caller, so this crate stays free of
//! file formats and platform code.

pub mod bounds;
pub mod chroma;
pub mod energy;
pub mod key;
pub mod loudness;
pub mod mix;
pub mod tempo;

pub use bounds::{Bounds, BoundsDetector};
pub use chroma::KeyDetector;
pub use energy::track_energy;
pub use key::{Camelot, Key, key_from_chroma, parse_key_tag};
pub use tempo::{Beatgrid, TempoDetector, parse_bpm_tag};
pub use mix::{MixTrack, mix_order};
pub use loudness::{Loudness, LoudnessMeter, normalization_gain_db};
