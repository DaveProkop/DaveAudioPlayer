//! Folder watching (`folder-watch`): notices files added, changed, moved or deleted in the
//! library folders and asks for a rescan of the affected roots once things calm down.
//!
//! A rescan is incremental (unchanged files are skipped), so watching only has to say
//! *which root* changed, not which file. Changes are collected until no new event came
//! for [`QUIET`] (copying an album produces a burst), but never longer than [`MAX_DELAY`].
//! Roots that cannot be watched (unplugged drive) are retried every [`RESYNC`].

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use dap_library::db::open_reader;
use dap_library::scanner::AUDIO_EXTENSIONS;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;

pub const QUIET: Duration = Duration::from_secs(3);
pub const MAX_DELAY: Duration = Duration::from_secs(30);
const RESYNC: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_millis(250);
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp"];

pub struct FolderWatcher {
    refresh: Sender<()>,
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
    /// Roots that changed and should be rescanned.
    pub changed: Receiver<BTreeSet<i64>>,
}

impl FolderWatcher {
    /// `ignore` is a folder whose changes never count (the app's own cover cache).
    pub fn start(db_path: &Path, ignore: PathBuf) -> Result<Self, String> {
        let reader = open_reader(db_path).map_err(|e| e.to_string())?;
        let (raw_tx, raw_rx) = unbounded::<Vec<PathBuf>>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && !matches!(event.kind, EventKind::Access(_))
            {
                let _ = raw_tx.send(event.paths);
            }
        })
        .map_err(|e| format!("folder watching: {e}"))?;
        let (refresh, refresh_rx) = bounded(1);
        let (stop, stop_rx) = bounded(1);
        let (changed_tx, changed) = unbounded();
        let worker = Worker {
            reader,
            watcher,
            watched: HashMap::new(),
            ignore,
            changed: changed_tx,
        };
        let thread = std::thread::Builder::new()
            .name("folder-watch".into())
            .spawn(move || worker.run(raw_rx, refresh_rx, stop_rx))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            refresh,
            stop,
            thread: Some(thread),
            changed,
        })
    }

    /// Re-reads the list of library folders (after one was added).
    pub fn refresh(&self) {
        let _ = self.refresh.try_send(());
    }
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Worker {
    reader: Connection,
    watcher: RecommendedWatcher,
    /// Roots being watched successfully.
    watched: HashMap<i64, PathBuf>,
    ignore: PathBuf,
    changed: Sender<BTreeSet<i64>>,
}

impl Worker {
    fn run(mut self, raw: Receiver<Vec<PathBuf>>, refresh: Receiver<()>, stop: Receiver<()>) {
        self.sync_roots();
        let mut pending = BTreeSet::new();
        let mut first: Option<Instant> = None;
        let mut last = Instant::now();
        let mut synced = Instant::now();
        loop {
            select! {
                recv(raw) -> paths => {
                    let Ok(paths) = paths else { return };
                    for path in paths {
                        if let Some(root) = self.root_of(&path) {
                            pending.insert(root);
                            first.get_or_insert_with(Instant::now);
                            last = Instant::now();
                        }
                    }
                }
                recv(refresh) -> _ => {
                    self.sync_roots();
                    synced = Instant::now();
                }
                recv(stop) -> _ => return,
                default(TICK) => {}
            }
            let now = Instant::now();
            if let Some(f) = first
                && (now - last >= QUIET || now - f >= MAX_DELAY)
            {
                let _ = self.changed.send(std::mem::take(&mut pending));
                first = None;
            }
            if now - synced >= RESYNC {
                self.sync_roots();
                synced = now;
            }
        }
    }

    /// Watches every enabled root not watched yet; stops watching removed ones.
    fn sync_roots(&mut self) {
        let roots: HashMap<i64, PathBuf> = self
            .reader
            .prepare("SELECT id, path FROM library_root WHERE enabled = 1")
            .and_then(|mut st| {
                st.query_map([], |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?))))?
                    .collect()
            })
            .unwrap_or_default();
        let gone: Vec<i64> = self
            .watched
            .iter()
            .filter(|(id, path)| roots.get(id) != Some(path))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            if let Some(path) = self.watched.remove(&id) {
                let _ = self.watcher.unwatch(&path);
            }
        }
        for (id, path) in roots {
            if !self.watched.contains_key(&id)
                && self.watcher.watch(&path, RecursiveMode::Recursive).is_ok()
            {
                self.watched.insert(id, path);
            }
        }
    }

    /// The watched root a changed path belongs to, if the change can matter to the
    /// library: audio files, cover images, and folders (renamed or deleted as a whole).
    fn root_of(&self, path: &Path) -> Option<i64> {
        if path.starts_with(&self.ignore) || !is_relevant(path) {
            return None;
        }
        self.watched
            .iter()
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.as_os_str().len())
            .map(|(id, _)| *id)
    }
}

fn is_relevant(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext = ext.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&ext.as_str())
                || IMAGE_EXTENSIONS.contains(&ext.as_str())
                || path.is_dir()
        }
        // No extension: most likely a folder (also when it no longer exists).
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_audio_images_and_folders_matter() {
        for p in ["a/song.MP3", "a/x.flac", "a/cover.jpg", "a/Album", "a/b/c"] {
            assert!(is_relevant(Path::new(p)), "{p}");
        }
        for p in ["a/song.mp3.part", "a/notes.txt", "a/library.db-wal", "a/x.tmp"] {
            assert!(!is_relevant(Path::new(p)), "{p}");
        }
    }
}
