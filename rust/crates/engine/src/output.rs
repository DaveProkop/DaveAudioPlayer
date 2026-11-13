//! Where the mixer's audio goes: the real device, or a null sink for tests and headless use.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use dap_audio_io::{AudioOutput, AudioSource, OutputDevice};
use dap_mixer::{Mixer, MixerControl, mixer};

#[derive(Clone)]
pub enum OutputConfig {
    /// A sound device: the one chosen in the settings, else the system default.
    Device,
    /// No device: a thread pulls audio at `speed` × real time, optionally recording it.
    Null {
        sample_rate: u32,
        channels: usize,
        speed: f64,
        capture: Option<Arc<Mutex<Vec<f32>>>>,
    },
}

struct MixerSource(Mixer);

impl AudioSource for MixerSource {
    fn render(&mut self, out: &mut [f32]) {
        self.0.render(out);
    }
}

pub(crate) enum Output {
    Device(#[allow(dead_code)] AudioOutput),
    Null {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    },
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Output::Null { stop, thread } = self {
            stop.store(true, Ordering::Relaxed);
            if let Some(t) = thread.take() {
                let _ = t.join();
            }
        }
    }
}

pub(crate) struct Started {
    pub output: Output,
    pub control: MixerControl,
    pub sample_rate: u32,
    pub channels: usize,
}

/// Names of the sound devices that can be chosen.
pub fn output_devices() -> Vec<String> {
    dap_audio_io::output_device_names()
}

/// Starts the output on `device` (by name; the default when `None` or not there). Must run
/// on the thread that keeps the result (cpal streams are not `Send`).
pub(crate) fn start(
    config: &OutputConfig,
    device: Option<&str>,
    on_error: impl FnMut(String) + Send + 'static,
) -> Result<Started, String> {
    match config {
        OutputConfig::Device => {
            let device = OutputDevice::named_or_default(device).map_err(|e| e.to_string())?;
            let (rate, ch) = (device.sample_rate(), device.channels());
            let (m, control) = mixer(rate, ch);
            let out = device
                .start(MixerSource(m), on_error)
                .map_err(|e| e.to_string())?;
            Ok(Started {
                output: Output::Device(out),
                control,
                sample_rate: rate,
                channels: ch,
            })
        }
        OutputConfig::Null {
            sample_rate,
            channels,
            speed,
            capture,
        } => {
            let (rate, ch) = (*sample_rate, *channels);
            let (mut m, control) = mixer(rate, ch);
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            let capture = capture.clone();
            let block_frames = 480usize;
            let pause =
                Duration::from_secs_f64(block_frames as f64 / f64::from(rate) / speed.max(0.01));
            let thread = std::thread::Builder::new()
                .name("null-output".into())
                .spawn(move || {
                    let mut buf = vec![0.0f32; block_frames * ch];
                    while !thread_stop.load(Ordering::Relaxed) {
                        m.render(&mut buf);
                        if let Some(c) = &capture {
                            c.lock().expect("capture lock").extend_from_slice(&buf);
                        }
                        std::thread::sleep(pause);
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(Started {
                output: Output::Null {
                    stop,
                    thread: Some(thread),
                },
                control,
                sample_rate: rate,
                channels: ch,
            })
        }
    }
}
