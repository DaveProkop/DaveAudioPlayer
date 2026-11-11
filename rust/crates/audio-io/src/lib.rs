//! Decoding (symphonia), resampling (rubato), audio output (cpal) and ring buffers.

pub mod decoder;

pub use decoder::{DecodeError, Decoder, StreamInfo};
