//! Real-time mixer with a current and a preloaded next deck.
//!
//! Gapless: when the current deck is drained (its decoder finished and its ring is
//! empty), the mixer switches to the preloaded next deck within the same callback, so
//! not a single frame of silence is inserted between tracks.
//!
//! Crossfade: the control thread sends a [`TransitionPlan`] for the current deck. When the
//! deck reaches the plan's start frame, the next deck becomes current and the old one keeps
//! playing as the outgoing deck until the plan's end, with equal-power gain curves.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dap_dsp::eq::{BAND_COUNT, MAX_CHANNELS};
use dap_dsp::{BiquadCoeffs, BiquadState, Equalizer, LinearSmoother};

use crate::ring::PcmConsumer;

const COMMAND_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 256;
/// Pause, resume and volume changes ramp over this time to avoid clicks.
const FADE_MS: f64 = 10.0;
/// How often the playback position is reported.
const POSITION_REPORT_MS: f64 = 100.0;
const TRASH_SLOTS: usize = 4;
/// Per-deck gain changes (loudness normalization on/off) ramp over this time.
const DECK_GAIN_RAMP_MS: f64 = 50.0;
/// Outgoing deck audio is mixed in through a scratch buffer of this many frames.
const SCRATCH_FRAMES: usize = 1024;
/// The bass band for beatmatch transitions, and how fast the basses swap.
const BASS_HZ: f64 = 150.0;
const BASS_SWAP_MS: f64 = 30.0;

/// Audio for one track, filled by a deck decoder thread.
pub struct DeckInput {
    /// Identifies this load of a track (a track may be loaded several times).
    pub token: u64,
    pub rx: PcmConsumer,
    /// Set by the decoder thread after its last frame was pushed (also on failure).
    pub finished: Arc<AtomicBool>,
    /// Position of the first frame in the ring, in output frames (non-zero after a seek).
    pub start_frame: u64,
    /// Linear gain for this track (loudness normalization); 1.0 = unchanged.
    pub gain: f32,
}

/// Crossfade from the current deck `token` into the next deck: the next deck starts when
/// the current one reaches `start_frame`, and the current one fades out until `end_frame`
/// (its last frame). Positions are in output frames of the current deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionPlan {
    pub token: u64,
    pub start_frame: u64,
    pub end_frame: u64,
    pub shape: TransitionShape,
}

/// How the two decks are blended during a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransitionShape {
    /// Equal-power crossfade over the whole transition.
    #[default]
    Crossfade,
    /// DJ style, for beat-matched decks: the incoming deck fades in without bass, the
    /// basses swap at `swap` frames into the transition (a bar line), then the outgoing
    /// deck fades out.
    Beatmatch { swap: u64 },
}

/// A deck in use, with its smoothed gain and bass control.
struct Slot {
    deck: DeckInput,
    gain: LinearSmoother,
    /// 1 = full bass, 0 = bass removed (used during beatmatch transitions).
    bass: LinearSmoother,
    bass_state: [BiquadState; MAX_CHANNELS],
}

/// A transition in progress: the outgoing deck and how far the fade has got.
struct Fade {
    outgoing: Slot,
    shape: TransitionShape,
    pos: u64,
    len: u64,
    /// Frame in the current output buffer where the fade starts (only in its first buffer).
    offset: usize,
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
    /// Plans a crossfade out of the current deck (replaces any earlier plan).
    SetTransition(TransitionPlan),
    /// Back to gapless: the next deck starts when the current one ends.
    ClearTransition,
    /// Changes the gain of a loaded deck (current, next or outgoing), with a short ramp.
    SetDeckGain { token: u64, gain: f32 },
}

pub enum MixerEvent {
    /// A deck became current (by command, a gapless switch or the start of a crossfade).
    Started { token: u64 },
    /// A deck played to its end (the current deck, or the outgoing deck of a crossfade).
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
    current: Option<Slot>,
    next: Option<Slot>,
    fade: Option<Fade>,
    plan: Option<TransitionPlan>,
    scratch: Vec<f32>,
    deck_gain_ramp: u32,
    bass_ramp: u32,
    bass_lp: BiquadCoeffs,
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
        fade: None,
        plan: None,
        scratch: vec![0.0; SCRATCH_FRAMES * channels],
        deck_gain_ramp: (DECK_GAIN_RAMP_MS * rate / 1000.0) as u32,
        bass_ramp: (BASS_SWAP_MS * rate / 1000.0) as u32,
        bass_lp: BiquadCoeffs::low_pass(rate, BASS_HZ, std::f64::consts::FRAC_1_SQRT_2),
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
        if !silent {
            self.mix_fade(out);
        }

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
            && let Some(slot) = &self.current
        {
            self.since_report = 0;
            let event = MixerEvent::Position {
                token: slot.deck.token,
                frames: self.frames,
            };
            self.emit(event);
        }
    }

    /// Pulls audio from the decks, switching to the next deck when the current one ends
    /// or a planned transition starts.
    fn pull(&mut self, out: &mut [f32]) -> usize {
        let ch = self.channels;
        let mut filled = 0;
        while filled < out.len() {
            // A next deck that arrived after the current one ended (e.g. the current track
            // failed instantly) starts now instead of being stranded.
            if self.current.is_none()
                && let Some(next) = self.next.take()
            {
                self.make_current(next);
            }
            if self.current.is_none() {
                if !self.idle_reported {
                    self.idle_reported = true;
                    self.emit(MixerEvent::Idle);
                }
                break;
            }
            let mut want = out.len() - filled;
            if let Some(until) = self.frames_until_transition() {
                if until == 0 {
                    self.begin_transition(filled / ch);
                    continue;
                }
                want = want.min(until as usize * ch);
            }
            let slot = self.current.as_mut().expect("current deck");
            let n = slot.deck.rx.pop_into(&mut out[filled..filled + want]);
            apply_gain(&mut slot.gain, &mut out[filled..filled + n], ch);
            filled += n;
            self.frames += (n / ch) as u64;
            if n > 0 {
                self.in_underrun = false;
            }
            if n == want {
                continue;
            }
            if slot.deck.is_drained() {
                let ended = self.current.take().expect("current deck");
                self.emit(MixerEvent::Ended {
                    token: ended.deck.token,
                });
                self.discard(ended.deck);
                if let Some(next) = self.next.take() {
                    self.make_current(next);
                }
            } else {
                if !self.in_underrun {
                    self.in_underrun = true;
                    let token = slot.deck.token;
                    self.emit(MixerEvent::Underrun { token });
                }
                break;
            }
        }
        filled
    }

    /// Frames until the planned transition of the current deck starts (0 = now), or
    /// `None` when there is nothing to fade into.
    fn frames_until_transition(&self) -> Option<u64> {
        let plan = self.plan?;
        let current = self.current.as_ref()?;
        if current.deck.token != plan.token || self.next.is_none() || self.frames >= plan.end_frame
        {
            return None;
        }
        Some(plan.start_frame.saturating_sub(self.frames))
    }

    /// The next deck becomes current; the current one fades out until the plan's end.
    /// `offset` is where in the output buffer this happens.
    fn begin_transition(&mut self, offset: usize) {
        let plan = self.plan.take().expect("plan");
        let outgoing = self.current.take().expect("current deck");
        if let Some(old) = self.fade.take() {
            self.discard(old.outgoing.deck);
        }
        let mut outgoing = outgoing;
        let mut next = self.next.take().expect("next deck");
        if matches!(plan.shape, TransitionShape::Beatmatch { .. }) {
            // The incoming deck starts without bass; the basses swap mid-transition.
            outgoing.bass.set_immediate(1.0);
            outgoing.bass_state = [BiquadState::default(); MAX_CHANNELS];
            next.bass.set_immediate(0.0);
            next.bass_state = [BiquadState::default(); MAX_CHANNELS];
        }
        self.fade = Some(Fade {
            outgoing,
            shape: plan.shape,
            pos: 0,
            len: plan.end_frame - self.frames,
            offset,
        });
        self.make_current(next);
    }

    /// Applies the fade curves: the incoming deck (already in `out`) fades in, the
    /// outgoing deck fades out and is added.
    fn mix_fade(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let Some(fade) = self.fade.as_mut() else {
            return;
        };
        let frames = out.len() / ch;
        let mut i = std::mem::take(&mut fade.offset);
        let half_pi = std::f64::consts::FRAC_PI_2;
        let lp = self.bass_lp;
        let mut incoming = self.current.as_mut();
        while i < frames && fade.pos < fade.len {
            let chunk = (frames - i)
                .min(SCRATCH_FRAMES)
                .min((fade.len - fade.pos) as usize);
            let scratch = &mut self.scratch[..chunk * ch];
            let got = fade.outgoing.deck.rx.pop_into(scratch);
            scratch[got..].fill(0.0);
            apply_gain(&mut fade.outgoing.gain, &mut scratch[..got], ch);
            let dst = &mut out[i * ch..(i + chunk) * ch];
            for (j, (o, s)) in dst
                .chunks_exact_mut(ch)
                .zip(scratch.chunks_exact_mut(ch))
                .enumerate()
            {
                let pos = fade.pos + j as u64;
                let (fade_in, fade_out) = match fade.shape {
                    TransitionShape::Crossfade => {
                        let t = pos as f64 / fade.len as f64 * half_pi;
                        (t.sin(), t.cos())
                    }
                    TransitionShape::Beatmatch { swap } => {
                        let swap = swap.clamp(1, fade.len - 1);
                        if pos == swap {
                            fade.outgoing.bass.set_target(0.0);
                            if let Some(inc) = incoming.as_deref_mut() {
                                inc.bass.set_target(1.0);
                            }
                        }
                        if pos < swap {
                            ((pos as f64 / swap as f64 * half_pi).sin(), 1.0)
                        } else {
                            let t = (pos - swap) as f64 / (fade.len - swap) as f64 * half_pi;
                            (1.0, t.cos())
                        }
                    }
                };
                if matches!(fade.shape, TransitionShape::Beatmatch { .. }) {
                    apply_bass(&mut fade.outgoing, &lp, s);
                    if let Some(inc) = incoming.as_deref_mut() {
                        apply_bass(inc, &lp, o);
                    }
                }
                let (fade_in, fade_out) = (fade_in as f32, fade_out as f32);
                for (o, s) in o.iter_mut().zip(s.iter()) {
                    *o = *o * fade_in + *s * fade_out;
                }
            }
            fade.pos += chunk as u64;
            i += chunk;
        }
        if fade.pos >= fade.len {
            let done = self.fade.take().expect("fade");
            self.emit(MixerEvent::Ended {
                token: done.outgoing.deck.token,
            });
            self.discard(done.outgoing.deck);
        }
    }

    fn make_current(&mut self, slot: Slot) {
        self.frames = slot.deck.start_frame;
        self.since_report = self.report_every; // report the new position right away
        self.in_underrun = false;
        self.idle_reported = false;
        self.emit(MixerEvent::Started {
            token: slot.deck.token,
        });
        self.current = Some(slot);
    }

    fn slot(&self, deck: DeckInput) -> Slot {
        Slot {
            gain: LinearSmoother::new(sanitize_gain(deck.gain), self.deck_gain_ramp),
            bass: LinearSmoother::new(1.0, self.bass_ramp),
            bass_state: [BiquadState::default(); MAX_CHANNELS],
            deck,
        }
    }

    /// Drops the current deck and any fade (skip, seek, stop).
    fn clear_current(&mut self) {
        if let Some(old) = self.current.take() {
            self.discard(old.deck);
        }
        if let Some(fade) = self.fade.take() {
            self.discard(fade.outgoing.deck);
        }
        self.plan = None;
    }

    fn apply_commands(&mut self) {
        while let Ok(cmd) = self.cmd_rx.pop() {
            match cmd {
                MixerCommand::SetCurrent(deck) => {
                    self.clear_current();
                    let slot = self.slot(deck);
                    self.make_current(slot);
                }
                MixerCommand::ClearCurrent => {
                    self.clear_current();
                    self.idle_reported = true;
                }
                MixerCommand::SetNext(deck) => {
                    let slot = self.slot(deck);
                    if let Some(old) = self.next.replace(slot) {
                        self.discard(old.deck);
                    }
                }
                MixerCommand::ClearNext => {
                    if let Some(old) = self.next.take() {
                        self.discard(old.deck);
                    }
                }
                MixerCommand::SetTransition(plan) => self.plan = Some(plan),
                MixerCommand::ClearTransition => self.plan = None,
                MixerCommand::SetDeckGain { token, gain } => {
                    let gain = sanitize_gain(gain);
                    let fading = self.fade.as_mut().map(|f| &mut f.outgoing);
                    for slot in [self.current.as_mut(), self.next.as_mut(), fading]
                        .into_iter()
                        .flatten()
                    {
                        if slot.deck.token == token {
                            slot.gain.set_target(gain);
                        }
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

fn sanitize_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, 8.0)
    } else {
        1.0
    }
}

/// Lowers the bass of one frame by the slot's bass control: `x + (g - 1) * lowpass(x)`.
#[inline]
fn apply_bass(slot: &mut Slot, lp: &BiquadCoeffs, frame: &mut [f32]) {
    let g = f64::from(slot.bass.next_value());
    for (x, state) in frame.iter_mut().zip(slot.bass_state.iter_mut()) {
        let low = state.process(lp, f64::from(*x));
        *x += ((g - 1.0) * low) as f32;
    }
}

/// Multiplies interleaved frames by a smoothed gain (skipped at a steady 1.0).
#[inline]
fn apply_gain(gain: &mut LinearSmoother, buf: &mut [f32], channels: usize) {
    if gain.is_settled() {
        let g = gain.current();
        if g != 1.0 {
            for s in buf {
                *s *= g;
            }
        }
        return;
    }
    for frame in buf.chunks_exact_mut(channels) {
        let g = gain.next_value();
        for s in frame {
            *s *= g;
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
            gain: 1.0,
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
                gain: 1.0,
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

        let available = |m: &Mixer| m.current.as_ref().unwrap().deck.rx.available_frames();
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

    /// A fully decoded deck holding a constant value.
    fn const_deck(token: u64, value: f32, frames: usize) -> DeckInput {
        let (mut tx, rx) = pcm_ring(frames.max(1), 2);
        assert_eq!(tx.push(&vec![value; frames * 2]), frames * 2);
        DeckInput {
            token,
            rx,
            finished: Arc::new(AtomicBool::new(true)),
            start_frame: 0,
            gain: 1.0,
        }
    }

    #[test]
    fn crossfade_overlaps_decks_with_equal_power_curves() {
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetCurrent(const_deck(1, 1.0, 1000)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetNext(const_deck(2, 0.5, 2000)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetTransition(TransitionPlan {
            token: 1,
            start_frame: 600,
            end_frame: 1000,
            shape: TransitionShape::Crossfade,
        }))
        .ok()
        .unwrap();

        // Odd buffer size: the transition starts and ends in the middle of buffers.
        let mut out = Vec::new();
        let mut buf = vec![0.0; 2 * 250];
        for _ in 0..6 {
            m.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        let frame = |i: usize| out[i * 2];
        assert_eq!(frame(599), 1.0, "only the current deck before the start");
        for k in [0usize, 100, 200, 399] {
            let t = k as f64 / 400.0 * std::f64::consts::FRAC_PI_2;
            let want = (0.5 * t.sin() + t.cos()) as f32;
            assert!((frame(600 + k) - want).abs() < 1e-5, "frame {k}");
        }
        assert_eq!(frame(1000), 0.5, "only the next deck after the end");
        assert_eq!(frame(1499), 0.5);
        assert_eq!(events(&mut ctl), ["start 1", "start 2", "end 1", "trash 1"]);
        assert_eq!(m.frames, 900, "position counts from the start of the next track");
    }

    #[test]
    fn beatmatch_swaps_the_basses_mid_transition() {
        // DC is all bass: the bass control alone decides what is heard of it.
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetCurrent(const_deck(1, 1.0, 20_000)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetNext(const_deck(2, 0.5, 40_000)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetTransition(TransitionPlan {
            token: 1,
            start_frame: 1000,
            end_frame: 17_000,
            shape: TransitionShape::Beatmatch { swap: 8000 },
        }))
        .ok()
        .unwrap();
        let mut out = vec![0.0; 2 * 20_000];
        m.render(&mut out);
        let at = |frame: usize| out[frame * 2];
        assert_eq!(at(999), 1.0, "before the transition");
        // Before the swap: the outgoing deck at full level, the incoming one bass-less.
        assert!((at(1000 + 7000) - 1.0).abs() < 0.02, "{}", at(8000));
        // After the swap: the incoming deck with its bass, the outgoing one without.
        assert!((at(1000 + 12_000) - 0.5).abs() < 0.02, "{}", at(13_000));
        assert!((at(17_500) - 0.5).abs() < 1e-6, "after the transition: {}", at(17_500));
        assert_eq!(events(&mut ctl), ["start 1", "start 2", "end 1", "trash 1"]);
    }

    #[test]
    fn transition_is_ignored_without_a_next_deck_or_for_another_deck() {
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetCurrent(const_deck(1, 1.0, 100)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetTransition(TransitionPlan {
            token: 1,
            start_frame: 50,
            end_frame: 100,
            shape: TransitionShape::Crossfade,
        }))
        .ok()
        .unwrap();
        let mut out = vec![0.0; 2 * 100];
        m.render(&mut out);
        assert!(out.iter().all(|&s| s == 1.0), "no fade into nothing");

        ctl.send(MixerCommand::SetCurrent(const_deck(2, 1.0, 100)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetNext(const_deck(3, 0.0, 100)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetTransition(TransitionPlan {
            token: 1,
            start_frame: 50,
            end_frame: 100,
            shape: TransitionShape::Crossfade,
        }))
        .ok()
        .unwrap();
        m.render(&mut out);
        assert!(out.iter().all(|&s| s == 1.0), "plan for another deck: gapless");
    }

    #[test]
    fn seeking_during_a_crossfade_drops_the_outgoing_deck() {
        let (mut m, mut ctl) = playing_mixer();
        ctl.send(MixerCommand::SetCurrent(const_deck(1, 1.0, 1000)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetNext(const_deck(2, 0.5, 1000)))
            .ok()
            .unwrap();
        ctl.send(MixerCommand::SetTransition(TransitionPlan {
            token: 1,
            start_frame: 0,
            end_frame: 1000,
            shape: TransitionShape::Crossfade,
        }))
        .ok()
        .unwrap();
        let mut out = vec![0.0; 2 * 100];
        m.render(&mut out);
        ctl.send(MixerCommand::SetCurrent(const_deck(3, 0.25, 1000)))
            .ok()
            .unwrap();
        m.render(&mut out);
        assert!(out.iter().all(|&s| s == 0.25));
        assert_eq!(
            events(&mut ctl),
            ["start 1", "start 2", "trash 2", "trash 1", "start 3"]
        );
    }

    #[test]
    fn deck_gain_applies_and_ramps_on_change() {
        let (mut m, mut ctl) = playing_mixer();
        let mut d = const_deck(1, 1.0, 48_000);
        d.gain = 0.5;
        ctl.send(MixerCommand::SetCurrent(d)).ok().unwrap();
        let mut out = vec![0.0; 2 * 100];
        m.render(&mut out);
        assert!(out.iter().all(|&s| s == 0.5));

        ctl.send(MixerCommand::SetDeckGain {
            token: 1,
            gain: 1.0,
        })
        .ok()
        .unwrap();
        let mut ramp = vec![0.0; 2 * 2400]; // 50 ms
        m.render(&mut ramp);
        assert!(ramp[0] > 0.5 && ramp[0] < 0.51, "ramps, no jump: {}", ramp[0]);
        assert_eq!(ramp[ramp.len() - 1], 1.0);
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
