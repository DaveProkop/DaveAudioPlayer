//! Real-time mixer with a current and a preloaded next deck.
//!
//! Gapless: when the current deck is drained (its decoder finished and its ring is
//! empty), the mixer switches to the preloaded next deck within the same callback, so
//! not a single frame of silence is inserted between tracks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dap_dsp::eq::BAND_COUNT;
use dap_dsp::{Equalizer, LinearSmoother};

use crate::ring::PcmConsumer;

const COMMAND_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 256;
/// Pause, resume and volume changes ramp over this time to avoid clicks.
const FADE_MS: f64 = 10.0;
/// How often the playback position is reported.
const POSITION_REPORT_MS: f64 = 100.0;
const TRASH_SLOTS: usize = 4;

/// Audio for one track, filled by a deck decoder thread.
pub struct DeckInput {
    /// Identifies this load of a track (a track may be loaded several times).
    pub token: u64,
    pub rx: PcmConsumer,
    /// Set by the decoder thread after its last frame was pushed (also on failure).
    pub finished: Arc<AtomicBool>,
    /// Position of the first frame in the ring, in output frames (non-zero after a seek).
    pub start_frame: u64,
}

impl DeckInput {
    fn is_drained(&self) -> bool {
        // Load the flag first: everything pushed before it was set is then visible.
        self.finished.load(Ordering::Acquire) && self.rx.available_frames() == 0
    }
}

pub enum MixerCommand {
    /// Replace the current deck immediately (play now, skip, seek).
    SetCurrent(DeckInput),
    ClearCurrent,
    /// Replace the preloaded next deck.
    SetNext(DeckInput),
    ClearNext,
    Play,
    Pause,
    /// Linear gain, 0.0–1.0.
    SetVolume(f32),
    SetEq {
        gains_db: [f32; BAND_COUNT],
        preamp_db: f32,
    },
    SetEqEnabled(bool),
}

pub enum MixerEvent {
    /// A deck became current (by command or by a gapless switch).
    Started { token: u64 },
    /// The current deck played to its end.
    Ended { token: u64 },
    /// Current playback position in output frames.
    Position { token: u64, frames: u64 },
    /// The current deck ran dry before its end (decoder too slow).
    Underrun { token: u64 },
    /// Nothing left to play.
    Idle,
    /// A deck the mixer no longer uses; drop it on the receiving thread.
    Trash(DeckInput),
}

/// Control-thread side of the mixer queues.
pub struct MixerControl {
    cmd_tx: rtrb::Producer<MixerCommand>,
    evt_rx: rtrb::Consumer<MixerEvent>,
}

impl MixerControl {
    /// Sends a command; gives it back if the queue is full.
    pub fn send(&mut self, cmd: MixerCommand) -> Result<(), MixerCommand> {
        self.cmd_tx.push(cmd).map_err(|rtrb::PushError::Full(c)| c)
    }

    pub fn try_event(&mut self) -> Option<MixerEvent> {
        self.evt_rx.pop().ok()
    }
}

pub struct Mixer {
    channels: usize,
    current: Option<DeckInput>,
    next: Option<DeckInput>,
    /// Position of the current deck in output frames.
    frames: u64,
    cmd_rx: rtrb::Consumer<MixerCommand>,
    evt_tx: rtrb::Producer<MixerEvent>,
    trash: [Option<DeckInput>; TRASH_SLOTS],
    eq: Equalizer,
    gain: LinearSmoother,
    volume: f32,
    playing: bool,
    in_underrun: bool,
    idle_reported: bool,
    report_every: u64,
    since_report: u64,
}

/// Creates a mixer for the audio thread and its control handle.
pub fn mixer(sample_rate: u32, channels: usize) -> (Mixer, MixerControl) {
    let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
    let (evt_tx, evt_rx) = rtrb::RingBuffer::new(EVENT_CAPACITY);
    let rate = f64::from(sample_rate);
    let mixer = Mixer {
        channels,
        current: None,
        next: None,
        frames: 0,
        cmd_rx,
        evt_tx,
        trash: Default::default(),
        eq: Equalizer::new(rate, channels),
        gain: LinearSmoother::new(0.0, (FADE_MS * rate / 1000.0) as u32),
        volume: 1.0,
        playing: false,
        in_underrun: false,
        idle_reported: true,
        report_every: (POSITION_REPORT_MS * rate / 1000.0) as u64,
        since_report: 0,
    };
    (mixer, MixerControl { cmd_tx, evt_rx })
}

impl Mixer {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Fills `out` (interleaved). Real-time safe.
    pub fn render(&mut self, out: &mut [f32]) {
        self.apply_commands();
        self.flush_trash();

        let silent = !self.playing && self.gain.is_settled() && self.gain.current() == 0.0;
        let filled = if silent { 0 } else { self.pull(out) };
        out[filled..].fill(0.0);

        self.eq.process(out);
        if !(self.gain.is_settled() && self.gain.current() == 1.0) {
            for frame in out.chunks_exact_mut(self.channels) {
                let g = self.gain.next_value();
                for s in frame {
                    *s *= g;
                }
            }
        }

        let frames = (filled / self.channels) as u64;
        self.since_report += frames;
        if self.since_report >= self.report_every
            && let Some(d) = &self.current
        {
            self.since_report = 0;
            let event = MixerEvent::Position {
                token: d.token,
                frames: self.frames,
            };
            self.emit(event);
        }
    }

    /// Pulls audio from the decks, switching to the next deck when the current one ends.
    fn pull(&mut self, out: &mut [f32]) -> usize {
        let mut filled = 0;
        while filled < out.len() {
            // A next deck that arrived after the current one ended (e.g. the current track
            // failed instantly) starts now instead of being stranded.
            if self.current.is_none()
                && let Some(next) = self.next.take()
            {
                self.make_current(next);
            }
            let Some(deck) = self.current.as_mut() else {
                if !self.idle_reported {
                    self.idle_reported = true;
                    self.emit(MixerEvent::Idle);
                }
                break;
            };
            let n = deck.rx.pop_into(&mut out[filled..]);
            filled += n;
            self.frames += (n / self.channels) as u64;
            if n > 0 {
                self.in_underrun = false;
            }
            if filled == out.len() {
                break;
            }
            if deck.is_drained() {
                let ended = self.current.take().expect("current deck");
                self.emit(MixerEvent::Ended { token: ended.token });
                self.discard(ended);
                if let Some(next) = self.next.take() {
                    self.make_current(next);
                }
            } else {
                if !self.in_underrun {
                    self.in_underrun = true;
                    let token = deck.token;
                    self.emit(MixerEvent::Underrun { token });
                }
                break;
            }
        }
        filled
    }

    fn make_current(&mut self, deck: DeckInput) {
        self.frames = deck.start_frame;
        self.since_report = self.report_every; // report the new position right away
        self.in_underrun = false;
        self.idle_reported = false;
        self.emit(MixerEvent::Started { token: deck.token });
        self.current = Some(deck);
    }

    fn apply_commands(&mut self) {
        while let Ok(cmd) = self.cmd_rx.pop() {
            match cmd {
                MixerCommand::SetCurrent(deck) => {
                    if let Some(old) = self.current.take() {
                        self.discard(old);
                    }
                    self.make_current(deck);
                }
                MixerCommand::ClearCurrent => {
                    if let Some(old) = self.current.take() {
                        self.discard(old);
                    }
                    self.idle_reported = true;
                }
                MixerCommand::SetNext(deck) => {
                    if let Some(old) = self.next.replace(deck) {
                        self.discard(old);
                    }
                }
                MixerCommand::ClearNext => {
                    if let Some(old) = self.next.take() {
                        self.discard(old);
                    }
                }
                MixerCommand::Play => {
                    self.playing = true;
                    self.gain.set_target(self.volume);
                }
                MixerCommand::Pause => {
                    self.playing = false;
                    self.gain.set_target(0.0);
                }
                MixerCommand::SetVolume(v) => {
                    self.volume = if v.is_finite() {
                        v.clamp(0.0, 1.0)
                    } else {
                        1.0
                    };
                    if self.playing {
                        self.gain.set_target(self.volume);
                    }
                }
                MixerCommand::SetEq {
                    gains_db,
                    preamp_db,
                } => {
                    self.eq.set_gains_db(&gains_db);
                    self.eq.set_preamp_db(preamp_db);
                }
                MixerCommand::SetEqEnabled(on) => self.eq.set_enabled(on),
            }
        }
    }

    fn emit(&mut self, event: MixerEvent) {
        // A full queue drops the event; only trash must never be lost (see `discard`).
        let _ = self.evt_tx.push(event);
    }

    /// Hands a deck back to the control thread instead of freeing it here.
    fn discard(&mut self, deck: DeckInput) {
        match self.evt_tx.push(MixerEvent::Trash(deck)) {
            Ok(()) => {}
            Err(rtrb::PushError::Full(MixerEvent::Trash(deck))) => {
                if let Some(slot) = self.trash.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(deck);
                } else {
                    // Every slot is taken: freeing here is the lesser evil than leaking.
                    drop(deck);
                }
            }
            Err(_) => unreachable!("pushed a Trash event"),
        }
    }

    fn flush_trash(&mut self) {
        for i in 0..TRASH_SLOTS {
            if let Some(deck) = self.trash[i].take()
                && let Err(rtrb::PushError::Full(MixerEvent::Trash(deck))) =
                    self.evt_tx.push(MixerEvent::Trash(deck))
            {
                self.trash[i] = Some(deck);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::{PcmProducer, pcm_ring};

    const RATE: u32 = 48_000;

    /// A deck whose samples count up from `base`, fully decoded.
    fn deck(token: u64, base: f32, frames: usize) -> DeckInput {
        let (mut tx, rx) = pcm_ring(frames.max(1), 2);
        let samples: Vec<f32> = (0..frames * 2).map(|i| base + i as f32).collect();
        assert_eq!(tx.push(&samples), samples.len());
        DeckInput {
            token,
            rx,
            finished: Arc::new(AtomicBool::new(true)),
            start_frame: 0,
        }
    }

    fn streaming_deck(token: u64, frames: usize) -> (DeckInput, PcmProducer, Arc<AtomicBool>) {
        let (tx, rx) = pcm_ring(frames, 2);
        let finished = Arc::new(AtomicBool::new(false));
        (
            DeckInput {
                token,
                rx,
                finished: finished.clone(),
                start_frame: 0,
            },
            tx,
            finished,
        )
    }

    fn events(ctl: &mut MixerControl) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(e) = ctl.try_event() {
            out.push(match e {
                MixerEvent::Started { token } => format!("start {token}"),
                MixerEvent::Ended { token } => format!("end {token}"),
                MixerEvent::Position { .. } => continue,
                MixerEvent::Underrun { token } => format!("underrun {token}"),
                MixerEvent::Idle => "idle".into(),
                MixerEvent::Trash(d) => format!("trash {}", d.token),
            });
        }
        out
    }

    fn playing_mixer() -> (Mixer, MixerControl) {
        let (mut m, mut ctl) = mixer(RATE, 2);
        ctl.send(MixerCommand::Play).ok().unwrap();
        // Run through the fade-in so tests see unity gain.
        let mut warmup = vec![0.0; 2 * 1024];
        m.render(&mut warmup);
        (m, ctl)
    }

    #[test]
    fn switches_to_next_deck_without_a_gap() {
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetCurrent(deck(1, 0.0, 100)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetNext(deck(2, 1000.0, 100)))
            .ok()
            .unwrap();

        let mut out = vec![-1.0; 2 * 150];
        m.render(&mut out);
        // 100 frames of deck 1, then deck 2 continues in the same buffer.
        assert_eq!(out[199], 199.0);
        assert_eq!(out[200], 1000.0);
        assert_eq!(out[299], 1099.0);
        assert_eq!(events(&mut ctl), ["start 1", "end 1", "trash 1", "start 2"]);

        let mut out = vec![-1.0; 2 * 100];
        m.render(&mut out);
        assert_eq!(out[0], 1100.0);
        assert_eq!(out[100], 0.0, "silence after the last deck");
        assert_eq!(events(&mut ctl), ["end 2", "trash 2", "idle"]);
    }

    #[test]
    fn next_deck_arriving_after_the_end_still_plays() {
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetCurrent(deck(1, 0.0, 0)))
            .ok()
            .unwrap(); // fails instantly
        let mut out = vec![0.0; 2 * 10];
        m.render(&mut out);
        assert_eq!(events(&mut ctl), ["start 1", "end 1", "trash 1", "idle"]);

        ctl.send(MixerCommand::SetNext(deck(2, 5.0, 10)))
            .ok()
            .unwrap();
        m.render(&mut out);
        assert_eq!(out[0], 5.0);
        assert_eq!(events(&mut ctl), ["start 2"]);
    }

    #[test]
    fn slow_decoder_is_an_underrun_not_an_end() {
        let (mut m, mut ctl) = playing_mixer();
        let (d, mut tx, finished) = streaming_deck(7, 1000);
        ctl.send(MixerCommand::SetCurrent(d)).ok().unwrap();
        tx.push(&[0.5; 20]);

        let mut out = vec![0.0; 2 * 64];
        m.render(&mut out);
        m.render(&mut out);
        assert_eq!(
            events(&mut ctl),
            ["start 7", "underrun 7"],
            "reported once per episode"
        );

        tx.push(&[0.5; 20]);
        finished.store(true, Ordering::Release);
        m.render(&mut out);
        assert_eq!(&out[..20], &[0.5; 20]);
        assert_eq!(events(&mut ctl), ["end 7", "trash 7", "idle"]);
    }

    #[test]
    fn pause_fades_out_and_stops_consuming() {
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetCurrent(deck(1, 1.0, 48_000)))
            .ok()
            .unwrap();
        let mut out = vec![0.0; 2 * 480];
        m.render(&mut out);
        ctl.send(MixerCommand::Pause).ok().unwrap();
        m.render(&mut out); // 10 ms fade = 480 frames
        assert!(out[0].abs() > 0.0 && out[out.len() - 1].abs() < out[0].abs() * 0.01 + 1.0);

        let available = |m: &Mixer| m.current.as_ref().unwrap().rx.available_frames();
        let before = available(&m);
        m.render(&mut out);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(available(&m), before, "paused mixer keeps the audio");
        assert_eq!(m.frames, 960);
    }

    #[test]
    fn replacing_decks_returns_old_ones_for_dropping() {
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetCurrent(deck(1, 0.0, 10)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetNext(deck(2, 0.0, 10)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetNext(deck(3, 0.0, 10)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetCurrent(deck(4, 0.0, 10)))
            .ok()
            .unwrap();
        let mut out = vec![0.0; 2];
        m.render(&mut out);
        assert_eq!(
            events(&mut ctl),
            ["start 1", "trash 2", "trash 1", "start 4"]
        );
    }

    #[test]
    fn seek_start_frame_sets_position() {
        let (mut m, mut ctl) = playing_mixer();
        let mut d = deck(1, 0.0, 10_000);
        d.start_frame = 96_000;
        ctl.send(MixerCommand::SetCurrent(d)).ok().unwrap();
        let mut out = vec![0.0; 2 * 100];
        m.render(&mut out);
        let mut pos = None;
        while let Some(e) = ctl.try_event() {
            if let MixerEvent::Position { frames, .. } = e {
                pos = Some(frames);
            }
        }
        assert_eq!(pos, Some(96_100));
    }

    #[test]
    fn volume_scales_output() {
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetVolume(0.5)).ok().unwrap();
        ctl.send(MixerCommand::SetCurrent(deck(1, 2.0, 4800)))
            .ok()
            .unwrap();
        let mut out = vec![0.0; 2 * 2400];
        m.render(&mut out);
        let last = out.len() - 1;
        assert_eq!(out[last], (2.0 + last as f32) * 0.5);
    }
}
