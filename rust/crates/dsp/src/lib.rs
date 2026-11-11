//! Platform-independent DSP: biquad EQ, filters, delay, limiter, parameter smoothing.
//!
//! Everything in this crate is real-time safe: no allocation, locking or I/O after
//! construction, so processors can live on the audio thread.

pub mod biquad;
pub mod eq;
pub mod gain;
pub mod smooth;

pub use biquad::{BiquadCoeffs, BiquadState};
pub use eq::Equalizer;
pub use smooth::LinearSmoother;
