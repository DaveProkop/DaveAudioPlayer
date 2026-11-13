//! Folder scanner, SQLite library, full-text search and playlists.

pub mod catalog;
pub mod covers;
pub mod db;
pub mod playlists;
pub mod roots;
pub mod scanner;
pub mod search;

pub use db::{DbError, SCHEMA_VERSION};
pub use roots::{add_root, remove_root, repair_overlapping_roots};
pub use scanner::{ScanJob, ScanProgress, ScanReport, root_path, scan_root};
pub use search::search_tracks;
