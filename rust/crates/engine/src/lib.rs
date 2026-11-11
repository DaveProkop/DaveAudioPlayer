//! Threads, commands, events and the transition planner.
//!
//! Threads:
//! - control (`engine-control`): queue, player state, commands and events
//! - deck decoders (`deck-N`): decode, resample and fill a ring per loaded track
//! - audio callback: the mixer, real-time safe
//! - database writer (`db-writer`): every write to SQLite
//! - loudness analysis (`analysis`): EBU R128 measurement of upcoming and other tracks

pub mod analyzer;
pub mod db_writer;
mod deck;
pub mod engine;
pub mod output;
pub mod queue;

pub use db_writer::DbWriter;
pub use engine::{
    Command, DEFAULT_CROSSFADE_MS, Engine, EngineConfig, Event, MAX_CROSSFADE_MS, PlayerSettings,
    PlayerState, ScanSummary, Status,
};
pub use output::OutputConfig;
pub use queue::{Queue, QueueEntry};
