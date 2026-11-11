//! Minimal end-to-end player for testing the audio chain:
//! decoder thread → resampler → channel remap → ring buffer → EQ (audio thread) → device.
//!
//! Usage:
//!   cargo run -p dap-audio-io --example play -- <file> [--eq g0,g1,..,g9] [--preamp dB] [--seconds N]
//!
//! Example (bass boost): --eq 6,5,3,0,0,0,0,0,0,0 --preamp -4

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use dap_audio_io::{
    AudioSource, Decoder, OutputDevice, PcmConsumer, PcmProducer, StreamResampler, pcm_ring,
    remap_channels,
};
use dap_dsp::Equalizer;
use dap_dsp::eq::BAND_COUNT;

const RING_SECONDS: f64 = 1.0;
const PREFILL_SECONDS: f64 = 0.25;

struct Args {
    path: PathBuf,
    eq: Option<[f32; BAND_COUNT]>,
    preamp_db: f32,
    max_seconds: Option<f64>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut path = None;
    let mut eq = None;
    let mut preamp_db = 0.0;
    let mut max_seconds = None;
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--eq" => {
                let gains: Vec<f32> = value("--eq")?
                    .split(',')
                    .map(|g| g.trim().parse().map_err(|_| format!("bad gain '{g}'")))
                    .collect::<Result<_, _>>()?;
                eq = Some(
                    gains
                        .try_into()
                        .map_err(|_| format!("--eq needs {BAND_COUNT} values"))?,
                );
            }
            "--preamp" => preamp_db = value("--preamp")?.parse().map_err(|_| "bad --preamp")?,
            "--seconds" => {
                max_seconds = Some(value("--seconds")?.parse().map_err(|_| "bad --seconds")?)
            }
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }
    let path = path.ok_or("usage: play <file> [--eq g0,..,g9] [--preamp dB] [--seconds N]")?;
    Ok(Args {
        path,
        eq,
        preamp_db,
        max_seconds,
    })
}

/// Shared counters between the audio thread and the UI (main) thread.
#[derive(Default)]
struct Stats {
    played_frames: AtomicU64,
    underruns: AtomicU64,
    decoder_done: AtomicBool,
    finished: AtomicBool,
}

/// Runs on the real-time audio thread.
struct Player {
    rx: PcmConsumer,
    eq: Equalizer,
    channels: usize,
    stats: Arc<Stats>,
}

impl AudioSource for Player {
    fn render(&mut self, out: &mut [f32]) {
        let n = self.rx.pop_into(out);
        out[n..].fill(0.0);
        self.eq.process(&mut out[..n]);
        self.stats
            .played_frames
            .fetch_add((n / self.channels) as u64, Ordering::Relaxed);
        if n < out.len() {
            if self.stats.decoder_done.load(Ordering::Acquire) {
                self.stats.finished.store(true, Ordering::Release);
            } else {
                self.stats.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Decoder thread: decode, resample to the device rate, remap channels, push into the ring.
fn decode_into_ring(
    mut decoder: Decoder,
    out_rate: u32,
    out_channels: usize,
    mut tx: PcmProducer,
    stats: Arc<Stats>,
) {
    let info = decoder.info().clone();
    let mut resampler = match StreamResampler::new(info.sample_rate, out_rate, info.channels) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            stats.decoder_done.store(true, Ordering::Release);
            return;
        }
    };
    let mut resampled = Vec::new();
    let mut remapped = Vec::new();

    let mut push_all = |tx: &mut PcmProducer, resampled: &mut Vec<f32>| {
        remapped.clear();
        remap_channels(resampled, info.channels, out_channels, &mut remapped);
        resampled.clear();
        let mut off = 0;
        while off < remapped.len() {
            if tx.is_abandoned() {
                return false;
            }
            let n = tx.push(&remapped[off..]);
            off += n;
            if n == 0 {
                thread::sleep(Duration::from_millis(5));
            }
        }
        true
    };

    loop {
        match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                if let Err(e) = resampler.process(chunk, &mut resampled) {
                    eprintln!("{e}");
                    break;
                }
                if !push_all(&mut tx, &mut resampled) {
                    break;
                }
            }
            Ok(None) => {
                if resampler.finish(&mut resampled).is_ok() {
                    push_all(&mut tx, &mut resampled);
                }
                break;
            }
            Err(e) => {
                // A broken file skips the track; it never takes the app down.
                eprintln!("decode error, skipping rest of track: {e}");
                break;
            }
        }
    }
    if decoder.skipped_packets() > 0 {
        eprintln!("skipped {} corrupt packets", decoder.skipped_packets());
    }
    stats.decoder_done.store(true, Ordering::Release);
}

fn fmt_time(secs: f64) -> String {
    format!("{}:{:04.1}", (secs / 60.0) as u64, secs % 60.0)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let device = OutputDevice::default_device().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let (rate, channels) = (device.sample_rate(), device.channels());

    let decoder = Decoder::open(&args.path).unwrap_or_else(|e| {
        eprintln!("cannot open {}: {e}", args.path.display());
        std::process::exit(1);
    });
    let info = decoder.info().clone();
    println!(
        "{}: {} Hz, {} ch, {}",
        args.path.display(),
        info.sample_rate,
        info.channels,
        info.duration_secs()
            .map_or("unknown length".into(), fmt_time)
    );
    println!("output: {} ({} Hz, {} ch)", device.name(), rate, channels);

    let mut eq = Equalizer::new(f64::from(rate), channels);
    if let Some(gains) = args.eq {
        eq.set_gains_db(&gains);
    }
    eq.set_preamp_db(args.preamp_db);

    let stats = Arc::new(Stats::default());
    let (tx, rx) = pcm_ring((f64::from(rate) * RING_SECONDS) as usize, channels);
    let decoder_thread = {
        let stats = stats.clone();
        thread::Builder::new()
            .name("deck-decoder".into())
            .spawn(move || decode_into_ring(decoder, rate, channels, tx, stats))
            .expect("spawn decoder thread")
    };

    // Prefill so playback does not start with an underrun.
    let prefill = (f64::from(rate) * PREFILL_SECONDS) as usize;
    while rx.available_frames() < prefill && !stats.decoder_done.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(5));
    }

    let player = Player {
        rx,
        eq,
        channels,
        stats: stats.clone(),
    };
    let output = device
        .start(player, |e| eprintln!("\nstream error: {e}"))
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });

    let started = Instant::now();
    while !stats.finished.load(Ordering::Acquire) {
        if args
            .max_seconds
            .is_some_and(|max| started.elapsed().as_secs_f64() >= max)
        {
            break;
        }
        let pos = stats.played_frames.load(Ordering::Relaxed) as f64 / f64::from(rate);
        print!(
            "\r{}  underruns: {}   ",
            fmt_time(pos),
            stats.underruns.load(Ordering::Relaxed)
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();
        thread::sleep(Duration::from_millis(200));
    }
    drop(output); // stops the stream and drops the ring consumer, ending the decoder
    let _ = decoder_thread.join();

    let played = stats.played_frames.load(Ordering::Relaxed);
    println!(
        "\nplayed {} ({} frames), underruns: {}",
        fmt_time(played as f64 / f64::from(rate)),
        played,
        stats.underruns.load(Ordering::Relaxed)
    );
}
