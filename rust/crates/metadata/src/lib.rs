//! Online metadata: MusicBrainz, AcoustID, Cover Art Archive, LRCLIB.
//!
//! Plain blocking calls: they run on the API's worker threads or a background thread,
//! never on the audio or control threads.

pub mod http;
pub mod lrc;
pub mod lrclib;
pub mod musicbrainz;

pub use http::{MetadataError, USER_AGENT, client};
pub use lrc::{LrcLine, parse_lrc};
