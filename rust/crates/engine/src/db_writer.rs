//! The single database writer thread. Every write in the app goes through it.

use std::path::Path;
use std::thread::JoinHandle;

use crossbeam_channel::{Sender, bounded, unbounded};
use dap_library::DbError;
use dap_library::db::open_writer;
use rusqlite::Connection;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Handle to the writer thread. Cheap to clone; the thread ends when all handles are gone.
#[derive(Clone)]
pub struct DbWriter {
    tx: Sender<Job>,
}

impl DbWriter {
    /// Opens (and migrates) the database on a new writer thread.
    pub fn open(path: &Path) -> Result<(Self, JoinHandle<()>), DbError> {
        let path = path.to_path_buf();
        let (ready_tx, ready_rx) = bounded(1);
        let (tx, rx) = unbounded::<Job>();
        let thread = std::thread::Builder::new()
            .name("db-writer".into())
            .spawn(move || match open_writer(&path) {
                Ok(mut conn) => {
                    let _ = ready_tx.send(Ok(()));
                    for job in rx {
                        job(&mut conn);
                    }
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(DbError::Io)?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok((Self { tx }, thread)),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(DbError::File("database writer thread died".into())),
        }
    }

    /// A handle with no thread behind it (all jobs are dropped). Used during shutdown to
    /// release the real handle.
    pub fn closed() -> Self {
        let (tx, _) = unbounded();
        Self { tx }
    }

    /// Queues a write without waiting for it.
    pub fn run(&self, job: impl FnOnce(&mut Connection) + Send + 'static) {
        let _ = self.tx.send(Box::new(job));
    }

    /// Runs `job` on the writer thread and waits for its result.
    /// Returns `None` if the writer thread is gone.
    pub fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> T + Send + 'static,
    ) -> Option<T> {
        let (tx, rx) = bounded(1);
        self.tx
            .send(Box::new(move |conn| {
                let _ = tx.send(job(conn));
            }))
            .ok()?;
        rx.recv().ok()
    }
}
