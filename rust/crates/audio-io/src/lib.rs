//! Decoding (symphonia), resampling (rubato), channel remapping and audio output (cpal).

pub mod channels;
pub mod decoder;
pub mod output;
pub mod resample;

pub use channels::remap_channels;
pub use decoder::{DecodeError, Decoder, StreamInfo};
pub use output::{AudioOutput, AudioSource, OutputDevice, OutputError, output_device_names};
pub use resample::{ResampleError, StreamResampler};
