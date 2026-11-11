//! File decoding to interleaved `f32` PCM.
//!
//! Runs on a deck decoder thread, never on the audio callback: it allocates and does I/O.
//! Encoder delay/padding is trimmed by symphonia (gapless is on by default).

use std::fmt;
use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

/// After this many corrupt packets in a row the file is treated as undecodable.
const MAX_CONSECUTIVE_BAD_PACKETS: u32 = 50;

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub sample_rate: u32,
    pub channels: usize,
    /// Total length in frames, if the container declares it.
    pub total_frames: Option<u64>,
}

impl StreamInfo {
    pub fn duration_secs(&self) -> Option<f64> {
        self.total_frames
            .map(|f| f as f64 / f64::from(self.sample_rate))
    }
}

#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    /// Unknown container or codec, or a file that is not audio at all.
    Unsupported(String),
    NoAudioTrack,
    /// The stream is too damaged to continue.
    Corrupt(String),
    /// Sample rate or channel count changed mid-stream.
    FormatChanged,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Io(e) => write!(f, "I/O error: {e}"),
            DecodeError::Unsupported(what) => write!(f, "unsupported: {what}"),
            DecodeError::NoAudioTrack => write!(f, "no audio track"),
            DecodeError::Corrupt(what) => write!(f, "corrupt stream: {what}"),
            DecodeError::FormatChanged => write!(f, "stream format changed mid-stream"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<SymError> for DecodeError {
    fn from(e: SymError) -> Self {
        match e {
            SymError::IoError(e) => DecodeError::Io(e),
            SymError::Unsupported(what) => DecodeError::Unsupported(what.to_string()),
            other => DecodeError::Corrupt(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, DecodeError>;

/// Sequential decoder for one audio file.
pub struct Decoder {
    format: Box<dyn FormatReader + 'static>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    info: StreamInfo,
    buf: Vec<f32>,
    /// Frame index of the next frame returned by [`next_chunk`](Self::next_chunk).
    position: u64,
    /// Frames still to drop after an accurate seek landed before the target.
    skip_frames: u64,
    skipped_packets: u64,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(DecodeError::Io)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| match e {
                SymError::IoError(e) if e.kind() != std::io::ErrorKind::UnexpectedEof => {
                    DecodeError::Io(e)
                }
                _ => DecodeError::Unsupported("unrecognized format".into()),
            })?;

        let track = format
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::NoAudioTrack)?;
        let track_id = track.id;
        let time_base = track.time_base;
        let total_frames = track.num_frames;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(DecodeError::NoAudioTrack)?;
        let sample_rate = params
            .sample_rate
            .filter(|&r| r > 0)
            .ok_or_else(|| DecodeError::Unsupported("unknown sample rate".into()))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|&c| c > 0)
            .ok_or_else(|| DecodeError::Unsupported("unknown channel layout".into()))?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())?;

        Ok(Self {
            format,
            decoder,
            track_id,
            time_base,
            info: StreamInfo {
                sample_rate,
                channels,
                total_frames,
            },
            buf: Vec::new(),
            position: 0,
            skip_frames: 0,
            skipped_packets: 0,
        })
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Frame index of the next frame to be returned.
    pub fn position_frames(&self) -> u64 {
        self.position
    }

    /// Number of corrupt packets skipped so far.
    pub fn skipped_packets(&self) -> u64 {
        self.skipped_packets
    }

    /// Decodes the next chunk of interleaved samples. Returns `None` at the end of the stream.
    ///
    /// Isolated corrupt packets are skipped; only a badly damaged stream returns an error.
    pub fn next_chunk(&mut self) -> Result<Option<&[f32]>> {
        let mut bad_in_a_row = 0;
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(SymError::ResetRequired) => {
                    self.rebuild_decoder()?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    self.skipped_packets += 1;
                    bad_in_a_row += 1;
                    if bad_in_a_row >= MAX_CONSECUTIVE_BAD_PACKETS {
                        return Err(DecodeError::Corrupt("too many undecodable packets".into()));
                    }
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            bad_in_a_row = 0;

            let spec = decoded.spec();
            if spec.rate() != self.info.sample_rate || spec.channels().count() != self.info.channels
            {
                return Err(DecodeError::FormatChanged);
            }
            let frames = decoded.frames() as u64;
            if frames == 0 {
                continue;
            }
            self.buf.resize(decoded.samples_interleaved(), 0.0);
            decoded.copy_to_slice_interleaved(&mut self.buf);

            let skip = self.skip_frames.min(frames);
            self.skip_frames -= skip;
            if skip == frames {
                continue;
            }
            self.position += frames - skip;
            let start = skip as usize * self.info.channels;
            return Ok(Some(&self.buf[start..]));
        }
    }

    /// Seeks to `frame` (clamped to the stream length when known). Returns the new position.
    pub fn seek(&mut self, frame: u64) -> Result<u64> {
        let frame = self
            .info
            .total_frames
            .map_or(frame, |total| frame.min(total));
        let to = match self.time_base {
            Some(tb) => SeekTo::Timestamp {
                ts: Timestamp::new(frames_to_ts(frame, tb, self.info.sample_rate)),
                track_id: self.track_id,
            },
            None => {
                let secs = frame as f64 / f64::from(self.info.sample_rate);
                let time = Time::try_from_secs_f64(secs)
                    .ok_or_else(|| DecodeError::Corrupt("seek target out of range".into()))?;
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                }
            }
        };
        let seeked = self.format.seek(SeekMode::Accurate, to)?;
        self.decoder.reset();

        let required = self.ts_to_frames(seeked.required_ts);
        let actual = self.ts_to_frames(seeked.actual_ts);
        self.skip_frames = required.saturating_sub(actual);
        self.position = required;
        Ok(required)
    }

    fn ts_to_frames(&self, ts: Timestamp) -> u64 {
        let ts = ts.get().max(0) as u64;
        match self.time_base {
            Some(tb) => ts_to_frames(ts, tb, self.info.sample_rate),
            None => ts,
        }
    }

    fn rebuild_decoder(&mut self) -> Result<()> {
        let track = self
            .format
            .tracks()
            .iter()
            .find(|t| t.id == self.track_id)
            .ok_or(DecodeError::NoAudioTrack)?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(DecodeError::NoAudioTrack)?;
        self.decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())?;
        Ok(())
    }
}

// Exact integer conversions (float seconds lose single frames).
// seconds = ts * numer / denom, frames = seconds * rate.

fn ts_to_frames(ts: u64, tb: TimeBase, sample_rate: u32) -> u64 {
    let num = u128::from(ts) * u128::from(tb.numer.get()) * u128::from(sample_rate);
    let den = u128::from(tb.denom.get());
    ((num + den / 2) / den) as u64
}

fn frames_to_ts(frames: u64, tb: TimeBase, sample_rate: u32) -> i64 {
    let num = u128::from(frames) * u128::from(tb.denom.get());
    let den = u128::from(tb.numer.get()) * u128::from(sample_rate);
    ((num + den / 2) / den).min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::num::NonZero;

    #[test]
    fn timestamp_conversions_are_exact() {
        let tb = |n, d| TimeBase {
            numer: NonZero::new(n).unwrap(),
            denom: NonZero::new(d).unwrap(),
        };
        assert_eq!(frames_to_ts(100, tb(1, 48_000), 48_000), 100);
        assert_eq!(ts_to_frames(100, tb(1, 48_000), 48_000), 100);
        // Millisecond time base at 44.1 kHz.
        assert_eq!(ts_to_frames(1_000, tb(1, 1_000), 44_100), 44_100);
        assert_eq!(frames_to_ts(44_100, tb(1, 1_000), 44_100), 1_000);
    }
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("dap-audio-io-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes a 16-bit PCM WAV where sample `i` of channel `c` is `(i * (c + 1)) % 32768`
    /// (scaled), so positions can be verified exactly after decoding.
    fn write_ramp_wav(path: &Path, sample_rate: u32, channels: u16, frames: u32) {
        let data_len = frames * u32::from(channels) * 2;
        let mut f = File::create(path).unwrap();
        let mut w = |b: &[u8]| f.write_all(b).unwrap();
        w(b"RIFF");
        w(&(36 + data_len).to_le_bytes());
        w(b"WAVEfmt ");
        w(&16u32.to_le_bytes());
        w(&1u16.to_le_bytes());
        w(&channels.to_le_bytes());
        w(&sample_rate.to_le_bytes());
        w(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
        w(&(channels * 2).to_le_bytes());
        w(&16u16.to_le_bytes());
        w(b"data");
        w(&data_len.to_le_bytes());
        let mut data = Vec::with_capacity(data_len as usize);
        for i in 0..frames {
            for c in 0..u32::from(channels) {
                data.extend_from_slice(&(ramp_value(i, c) as i16).to_le_bytes());
            }
        }
        w(&data);
    }

    fn ramp_value(frame: u32, channel: u32) -> i32 {
        ((frame * (channel + 1)) % 32768) as i32
    }

    fn expected_sample(frame: u64, channel: u32) -> f32 {
        ramp_value(frame as u32, channel) as f32 / 32768.0
    }

    fn decode_all(dec: &mut Decoder) -> Vec<f32> {
        let mut out = Vec::new();
        while let Some(chunk) = dec.next_chunk().unwrap() {
            out.extend_from_slice(chunk);
        }
        out
    }

    #[test]
    fn decodes_wav_completely_and_exactly() {
        let dir = TempDir::new("full");
        let path = dir.0.join("ramp.wav");
        write_ramp_wav(&path, 44_100, 2, 10_000);

        let mut dec = Decoder::open(&path).unwrap();
        assert_eq!(
            dec.info(),
            &StreamInfo {
                sample_rate: 44_100,
                channels: 2,
                total_frames: Some(10_000)
            }
        );
        let samples = decode_all(&mut dec);
        assert_eq!(samples.len(), 20_000);
        for (i, frame) in samples.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(frame[0], expected_sample(i as u64, 0));
            assert_eq!(frame[1], expected_sample(i as u64, 1));
        }
        assert_eq!(dec.position_frames(), 10_000);
        assert!(dec.next_chunk().unwrap().is_none(), "stays at end");
    }

    #[test]
    fn seek_lands_on_exact_frame() {
        let dir = TempDir::new("seek");
        let path = dir.0.join("ramp.wav");
        write_ramp_wav(&path, 48_000, 2, 48_000);

        let mut dec = Decoder::open(&path).unwrap();
        for target in [12_345u64, 100, 40_000] {
            assert_eq!(dec.seek(target).unwrap(), target);
            let chunk = dec.next_chunk().unwrap().unwrap();
            assert_eq!(chunk[0], expected_sample(target, 0), "target {target}");
            assert_eq!(chunk[1], expected_sample(target, 1), "target {target}");
        }
        let rest = {
            dec.seek(47_000).unwrap();
            decode_all(&mut dec)
        };
        assert_eq!(rest.len(), 1_000 * 2);
    }

    #[test]
    fn missing_file_is_io_error() {
        let err = Decoder::open(Path::new("definitely/not/here.mp3"))
            .err()
            .unwrap();
        assert!(matches!(err, DecodeError::Io(_)), "{err}");
    }

    #[test]
    fn garbage_file_is_unsupported_not_a_panic() {
        let dir = TempDir::new("garbage");
        let path = dir.0.join("noise.mp3");
        std::fs::write(
            &path,
            (0..4096u32)
                .map(|i| (i * 7919 % 251) as u8)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let result = Decoder::open(&path).and_then(|mut d| {
            while d.next_chunk()?.is_some() {}
            Ok(())
        });
        // Either rejected up front or ends cleanly / with an error; never a panic.
        if let Err(e) = result {
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn truncated_wav_ends_cleanly() {
        let dir = TempDir::new("trunc");
        let path = dir.0.join("ramp.wav");
        write_ramp_wav(&path, 44_100, 1, 5_000);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 3_001]).unwrap();

        let mut dec = Decoder::open(&path).unwrap();
        let samples = decode_all(&mut dec);
        assert!(
            samples.len() < 5_000 && samples.len() > 3_000,
            "{}",
            samples.len()
        );
    }
}
