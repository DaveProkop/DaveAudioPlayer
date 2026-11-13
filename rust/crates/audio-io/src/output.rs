//! Audio output via cpal (WASAPI on Windows, AAudio on Android).
//!
//! The device callback runs on a real-time thread: [`AudioSource::render`] must not
//! allocate, lock or do I/O. Non-`f32` devices are served through a scratch buffer
//! allocated before the stream starts.

use std::fmt;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

/// Produces interleaved `f32` audio for the device. Called on the real-time audio thread.
pub trait AudioSource: Send + 'static {
    /// Fills all of `out` (interleaved, device channel count). Write silence when idle.
    fn render(&mut self, out: &mut [f32]);
}

/// Largest block converted at once for non-`f32` devices.
const SCRATCH_FRAMES: usize = 8192;

#[derive(Debug)]
pub enum OutputError {
    NoDevice,
    UnsupportedFormat(String),
    Backend(String),
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutputError::NoDevice => write!(f, "no audio output device"),
            OutputError::UnsupportedFormat(fmt_) => write!(f, "unsupported sample format {fmt_}"),
            OutputError::Backend(e) => write!(f, "audio backend error: {e}"),
        }
    }
}

impl std::error::Error for OutputError {}

fn backend(e: impl fmt::Display) -> OutputError {
    OutputError::Backend(e.to_string())
}

/// An output device with its preferred (shared mode) configuration.
pub struct OutputDevice {
    device: cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    name: String,
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "unknown device".into())
}

/// Names of the output devices the system offers (for choosing one).
pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    let mut names: Vec<String> = host
        .output_devices()
        .map(|devices| devices.map(|d| device_name(&d)).collect())
        .unwrap_or_default();
    names.dedup();
    names
}

impl OutputDevice {
    /// The system default output device.
    pub fn default_device() -> Result<Self, OutputError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(OutputError::NoDevice)?;
        Self::from_cpal(device)
    }

    /// The output device called `name`, or the default one when it is not there
    /// (unplugged) or `name` is `None`.
    pub fn named_or_default(name: Option<&str>) -> Result<Self, OutputError> {
        let host = cpal::default_host();
        let found = name.and_then(|name| {
            host.output_devices()
                .ok()?
                .find(|d| device_name(d) == name)
        });
        match found {
            Some(device) => Self::from_cpal(device),
            None => Self::default_device(),
        }
    }

    fn from_cpal(device: cpal::Device) -> Result<Self, OutputError> {
        let supported = device.default_output_config().map_err(backend)?;
        let name = device_name(&device);
        Ok(Self {
            device,
            sample_format: supported.sample_format(),
            config: supported.config(),
            name,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    pub fn channels(&self) -> usize {
        usize::from(self.config.channels)
    }

    /// Starts playback, pulling audio from `source`. `on_error` receives stream errors
    /// (device lost, xruns) on a non-real-time thread.
    pub fn start<S, E>(self, source: S, on_error: E) -> Result<AudioOutput, OutputError>
    where
        S: AudioSource,
        E: FnMut(String) + Send + 'static,
    {
        let stream = match self.sample_format {
            SampleFormat::F32 => self.build_f32(source, on_error)?,
            SampleFormat::F64 => self.build_converting::<f64, _, _>(source, on_error)?,
            SampleFormat::I16 => self.build_converting::<i16, _, _>(source, on_error)?,
            SampleFormat::I32 => self.build_converting::<i32, _, _>(source, on_error)?,
            SampleFormat::U16 => self.build_converting::<u16, _, _>(source, on_error)?,
            SampleFormat::I24 => self.build_converting::<cpal::I24, _, _>(source, on_error)?,
            other => return Err(OutputError::UnsupportedFormat(other.to_string())),
        };
        stream.play().map_err(backend)?;
        Ok(AudioOutput {
            _stream: stream,
            sample_rate: self.sample_rate(),
            channels: self.channels(),
            device_name: self.name,
        })
    }

    fn build_f32<S, E>(&self, mut source: S, mut on_error: E) -> Result<cpal::Stream, OutputError>
    where
        S: AudioSource,
        E: FnMut(String) + Send + 'static,
    {
        self.device
            .build_output_stream(
                self.config,
                move |data: &mut [f32], _| source.render(data),
                move |e| on_error(e.to_string()),
                None,
            )
            .map_err(backend)
    }

    fn build_converting<T, S, E>(
        &self,
        mut source: S,
        mut on_error: E,
    ) -> Result<cpal::Stream, OutputError>
    where
        T: SizedSample + FromSample<f32>,
        S: AudioSource,
        E: FnMut(String) + Send + 'static,
    {
        let ch = self.channels();
        let mut scratch = vec![0.0f32; SCRATCH_FRAMES * ch];
        self.device
            .build_output_stream(
                self.config,
                move |data: &mut [T], _| {
                    for block in data.chunks_mut(scratch.len()) {
                        let tmp = &mut scratch[..block.len()];
                        source.render(tmp);
                        for (dst, &src) in block.iter_mut().zip(tmp.iter()) {
                            *dst = T::from_sample(src);
                        }
                    }
                },
                move |e| on_error(e.to_string()),
                None,
            )
            .map_err(backend)
    }
}

/// A running output stream. Dropping it stops playback.
pub struct AudioOutput {
    _stream: cpal::Stream,
    sample_rate: u32,
    channels: usize,
    device_name: String,
}

impl AudioOutput {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "lists the machine's devices"]
    fn print_devices() {
        for name in super::output_device_names() {
            println!("device: {name}");
        }
        let d = super::OutputDevice::named_or_default(Some("no such device")).unwrap();
        println!("fallback: {} {} Hz", d.name(), d.sample_rate());
    }
}
