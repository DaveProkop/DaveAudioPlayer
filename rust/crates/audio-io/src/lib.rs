//! Decoding (symphonia), resampling (rubato), audio output (cpal) and ring buffers.

pub mod channels;
pub mod decoder;
pub mod output;
pub mod resample;
pub mod ring;

pub use channels::remap_channels;
pub use decoder::{DecodeError, Decoder, StreamInfo};
pub use output::{AudioOutput, AudioSource, OutputDevice, OutputError};
pub use resample::{ResampleError, StreamResampler};
pub use ring::{PcmConsumer, PcmProducer, pcm_ring};
