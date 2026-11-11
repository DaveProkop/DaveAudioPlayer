//! Decks, mixer and TransitionPlan automation evaluation.
//!
//! The [`Mixer`] runs on the real-time audio thread and talks to the control thread only
//! through lock-free queues. It never allocates, locks or frees memory: decks it no
//! longer needs are sent back to the control thread to be dropped there.

pub mod mixer;
pub mod ring;

pub use mixer::{DeckInput, Mixer, MixerCommand, MixerControl, MixerEvent, mixer};
pub use ring::{PcmConsumer, PcmProducer, pcm_ring};
