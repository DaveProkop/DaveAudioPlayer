//! Folder scanner, SQLite library, full-text search and playlists.

pub mod catalog;
pub mod covers;
pub mod db;
pub mod scanner;
pub mod search;

pub use db::{DbError, SCHEMA_VERSION};
pub use scanner::{ScanJob, ScanProgress, ScanReport, add_root, root_path, scan_root};
pub use search::search_tracks;
