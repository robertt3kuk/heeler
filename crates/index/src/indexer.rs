//! The indexer: one low-priority worker thread that owns the write connection, an FSEvents
//! watcher that tells it which files changed, and the cloneable [`IndexHandle`] for everything
//! else.
//!
//! Nothing runs on a timer. The worker sleeps in `recv` until a file event, a Heeler record, or
//! a command arrives; at startup it does one `stat` pass over the source roots and reads only
//! the files whose stamp changed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use tracing::{debug, error, info, warn};

use crate::adapters::{Adapter, MAX_TOOL_CHUNKS, adapter_for};
use crate::error::{IndexError, Result};
use crate::redact::redact;
use crate::scan::{self, FileOutcome, Metrics, ScanContext, now_ms};
use crate::search;
use crate::store;
use crate::sys;
use crate::text::{chunks, idents};
use crate::types::{HeelerRecord, Hit, Role, SearchQuery, Source, Status};

/// Quiet time after the last file event before the changed files are read.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// A steady stream of events still gets read at least this often.
const MAX_DELAY: Duration = Duration::from_secs(2);
/// Messages waiting for the worker before producers drop (events) or wait (commands).
const QUEUE: usize = 4096;
const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone)]
pub struct IndexConfig {
    /// `index.sqlite`.
    pub db_path: PathBuf,
    /// Index only the last N days (files and rows); 0 keeps everything.
    pub range_days: u32,
    /// Transcript sources to read, with their roots. A source that is not listed is never read.
    pub roots: Vec<(Source, PathBuf)>,
    /// Accept Heeler records ([`IndexHandle::push_heeler`]).
    pub heeler: bool,
}

enum Msg {
    Fs(Vec<PathBuf>),
    Heeler(Box<HeelerRecord>),
    Flush(SyncSender<()>),
    Clear(SyncSender<Result<()>>),
    Rebuild,
    Stop,
}

#[derive(Debug)]
struct Shared {
    db_path: PathBuf,
    tx: SyncSender<Msg>,
    /// Events were dropped because the queue was full: the worker repeats the stat pass.
    overflow: AtomicBool,
    /// Heeler records dropped because the queue was full.
    dropped: AtomicU64,
    metrics: Arc<Metrics>,
    sources: Vec<Source>,
}

/// Cheap, cloneable access to a running [`Indexer`].
#[derive(Debug, Clone)]
pub struct IndexHandle {
    shared: Arc<Shared>,
}

impl IndexHandle {
    /// Best hit per session for `query`, best first.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<Hit>> {
        let conn = store::open_read_only(&self.shared.db_path)?;
        search::search(&conn, query)
    }

    pub fn status(&self) -> Result<Status> {
        let conn = store::open_read_only(&self.shared.db_path)?;
        Ok(Status {
            sources: store::source_status(&conn, &self.shared.sources)?,
            db_bytes: store::database_bytes(&self.shared.db_path),
        })
    }

    /// Queues one Heeler record. `false` when the queue is full (the record is dropped and
    /// counted) or the indexer has stopped.
    pub fn push_heeler(&self, record: HeelerRecord) -> bool {
        match self.shared.tx.try_send(Msg::Heeler(Box::new(record))) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Heeler records dropped so far because the worker could not keep up.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Reads the files with pending file events now (without the debounce), then returns once
    /// everything queued before this call has been applied.
    pub fn flush(&self) -> Result<()> {
        let (reply, done) = sync_channel(1);
        self.shared
            .tx
            .send(Msg::Flush(reply))
            .map_err(|_| IndexError::Stopped)?;
        done.recv().map_err(|_| IndexError::Stopped)
    }

    /// Deletes the index database and starts an empty one. Files already indexed are read again
    /// only when they change or on [`rebuild`](Self::rebuild).
    pub fn clear(&self) -> Result<()> {
        let (reply, done) = sync_channel(1);
        self.shared
            .tx
            .send(Msg::Clear(reply))
            .map_err(|_| IndexError::Stopped)?;
        done.recv().map_err(|_| IndexError::Stopped)?
    }

    /// Forgets everything read from transcript files and reads all of them again, in the
    /// background. Heeler rows are kept.
    pub fn rebuild(&self) -> Result<()> {
        self.shared
            .tx
            .send(Msg::Rebuild)
            .map_err(|_| IndexError::Stopped)
    }

    /// What the scanner has done to the file system.
    pub fn metrics(&self) -> &Metrics {
        &self.shared.metrics
    }

    pub fn db_path(&self) -> &Path {
        &self.shared.db_path
    }
}

/// A running indexer. Dropping it stops the worker and the watcher.
#[derive(Debug)]
pub struct Indexer {
    handle: IndexHandle,
    thread: Option<JoinHandle<()>>,
    watcher: Option<RecommendedWatcher>,
}

impl Indexer {
    /// Opens the database, starts the watcher on the existing source roots and the worker, which
    /// begins with the stat pass. Returns before that pass finishes.
    pub fn start(config: IndexConfig) -> Result<Self> {
        let conn = store::open(&config.db_path)?;
        let roots: Vec<Root> = config
            .roots
            .iter()
            .filter_map(|(source, root)| {
                let adapter: Arc<dyn Adapter> = Arc::from(adapter_for(*source)?);
                // FSEvents reports canonical paths; compare against the canonical root.
                let path = root.canonicalize().unwrap_or_else(|_| root.clone());
                Some(Root { path, adapter })
            })
            .collect();
        let mut sources: Vec<Source> = config.roots.iter().map(|(source, _)| *source).collect();
        if config.heeler {
            sources.push(Source::Heeler);
        }
        sources.sort();
        sources.dedup();

        let (tx, rx) = sync_channel(QUEUE);
        let shared = Arc::new(Shared {
            db_path: config.db_path.clone(),
            tx: tx.clone(),
            overflow: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            metrics: Arc::new(Metrics::default()),
            sources,
        });

        let watcher = watch_roots(&roots, &tx, &shared)?;
        let worker = Worker {
            conn: Some(conn),
            db_path: config.db_path,
            range_days: config.range_days,
            roots,
            heeler: config.heeler,
            shared: Arc::clone(&shared),
            pending: BTreeSet::new(),
            first_pending: None,
            last_event: None,
        };
        let thread = std::thread::Builder::new()
            .name("heeler-index".to_owned())
            .spawn(move || worker.run(&rx))
            .map_err(IndexError::io("starting the history indexer"))?;
        Ok(Self {
            handle: IndexHandle { shared },
            thread: Some(thread),
            watcher,
        })
    }

    pub fn handle(&self) -> IndexHandle {
        self.handle.clone()
    }

    /// Stops the watcher and the worker (after it finishes the file it is reading) and waits for
    /// it. Idempotent.
    pub fn stop(&mut self) {
        self.watcher = None;
        if let Some(thread) = self.thread.take() {
            // A failed send means the worker is already gone.
            let _ = self.handle.shared.tx.send(Msg::Stop);
            if thread.join().is_err() {
                error!("the history indexer panicked");
            }
        }
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn watch_roots(
    roots: &[Root],
    tx: &SyncSender<Msg>,
    shared: &Arc<Shared>,
) -> Result<Option<RecommendedWatcher>> {
    if roots.is_empty() {
        return Ok(None);
    }
    let event_tx = tx.clone();
    let shared = Arc::clone(shared);
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
            Ok(event) => {
                if let Err(TrySendError::Full(_)) = event_tx.try_send(Msg::Fs(event.paths)) {
                    shared.overflow.store(true, Ordering::Release);
                }
            }
            Err(error) => warn!(%error, "history watcher error"),
        })
        .map_err(|e| IndexError::Io {
            context: "creating the history file watcher".to_owned(),
            source: std::io::Error::other(e),
        })?;
    for root in roots {
        if !root.path.is_dir() {
            info!(root = %root.path.display(), "history source directory does not exist; not watching it");
            continue;
        }
        if let Err(error) = watcher.watch(&root.path, RecursiveMode::Recursive) {
            warn!(root = %root.path.display(), %error, "cannot watch a history source directory");
        }
    }
    Ok(Some(watcher))
}

struct Root {
    path: PathBuf,
    adapter: Arc<dyn Adapter>,
}

struct Worker {
    conn: Option<Connection>,
    db_path: PathBuf,
    range_days: u32,
    roots: Vec<Root>,
    heeler: bool,
    shared: Arc<Shared>,
    pending: BTreeSet<PathBuf>,
    first_pending: Option<Instant>,
    last_event: Option<Instant>,
}

impl Worker {
    fn run(mut self, rx: &Receiver<Msg>) {
        sys::lower_current_thread_priority();
        self.startup();
        loop {
            let message = match self.next_deadline() {
                None => match rx.recv() {
                    Ok(message) => message,
                    Err(_) => break,
                },
                Some(wait) => match rx.recv_timeout(wait) {
                    Ok(message) => message,
                    Err(RecvTimeoutError::Timeout) => {
                        self.read_pending();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                },
            };
            match message {
                Msg::Fs(paths) => {
                    let now = Instant::now();
                    self.first_pending.get_or_insert(now);
                    self.last_event = Some(now);
                    self.pending.extend(paths);
                }
                Msg::Heeler(record) => self.index_heeler(&record),
                Msg::Flush(reply) => {
                    self.read_pending();
                    let _ = reply.send(());
                }
                Msg::Clear(reply) => {
                    let _ = reply.send(self.clear());
                }
                Msg::Rebuild => self.rebuild(),
                Msg::Stop => break,
            }
            if self.shared.overflow.swap(false, Ordering::AcqRel) {
                debug!("file events were dropped; running the stat pass again");
                self.stat_pass();
            }
        }
        debug!("history indexer stopped");
    }

    /// How long to wait for more file events before reading the pending files; `None` when
    /// nothing is pending (sleep until something arrives).
    fn next_deadline(&self) -> Option<Duration> {
        let (first, last) = (self.first_pending?, self.last_event?);
        let due = (last + DEBOUNCE).min(first + MAX_DELAY);
        Some(due.saturating_duration_since(Instant::now()))
    }

    fn cutoff_ms(&self) -> u64 {
        if self.range_days == 0 {
            0
        } else {
            now_ms().saturating_sub(u64::from(self.range_days) * DAY_MS)
        }
    }

    fn conn(&mut self) -> Result<&mut Connection> {
        if self.conn.is_none() {
            self.conn = Some(store::open(&self.db_path)?);
        }
        self.conn.as_mut().ok_or(IndexError::Stopped)
    }

    fn startup(&mut self) {
        let cutoff = self.cutoff_ms();
        if cutoff > 0 {
            match self
                .conn()
                .and_then(|conn| store::prune_before(conn, cutoff))
            {
                Ok(0) => {}
                Ok(removed) => info!(
                    removed,
                    "dropped history rows older than the configured range"
                ),
                Err(error) => warn!(%error, "pruning the history index failed"),
            }
        }
        self.stat_pass();
    }

    /// One `stat` pass over every root; reads only the files whose stamp changed.
    fn stat_pass(&mut self) {
        let started = Instant::now();
        let cutoff_ms = self.cutoff_ms();
        let mut changed: Vec<(usize, PathBuf)> = Vec::new();
        for (index, root) in self.roots.iter().enumerate() {
            let Some(conn) = self.conn.as_ref() else {
                break;
            };
            match scan::changed_files(conn, root.adapter.as_ref(), &root.path, cutoff_ms) {
                Ok(files) => changed.extend(files.into_iter().map(|file| (index, file))),
                Err(error) => {
                    warn!(root = %root.path.display(), %error, "scanning a history source failed")
                }
            }
        }
        let total = changed.len();
        let mut rows = 0;
        for (index, path) in changed {
            rows += self.process(index, &path);
            std::thread::yield_now();
        }
        if total > 0 {
            info!(
                files = total,
                rows,
                elapsed_ms = started.elapsed().as_millis(),
                "history stat pass finished"
            );
        }
    }

    fn read_pending(&mut self) {
        self.first_pending = None;
        self.last_event = None;
        let pending = std::mem::take(&mut self.pending);
        for path in pending {
            let Some(index) = self.root_of(&path) else {
                continue;
            };
            let is_dir = std::fs::metadata(&path).is_ok_and(|meta| meta.is_dir());
            if is_dir {
                let mut files = Vec::new();
                scan::for_each_file(&path, &mut |file, _| files.push(file.to_owned()));
                for file in files {
                    self.process(index, &file);
                    std::thread::yield_now();
                }
            } else {
                self.process(index, &path);
            }
        }
    }

    fn root_of(&self, path: &Path) -> Option<usize> {
        self.roots
            .iter()
            .position(|root| path.starts_with(&root.path))
    }

    /// Indexes one file; returns the rows added. Failures are logged, not fatal.
    fn process(&mut self, root: usize, path: &Path) -> u64 {
        let adapter = Arc::clone(&self.roots[root].adapter);
        if !adapter.accepts(path) {
            return 0;
        }
        let metrics = Arc::clone(&self.shared.metrics);
        let ctx = ScanContext {
            cutoff_ms: self.cutoff_ms(),
            metrics: &metrics,
        };
        let conn = match self.conn() {
            Ok(conn) => conn,
            Err(error) => {
                error!(%error, "the history database is not available");
                return 0;
            }
        };
        // A bug on one transcript must not end indexing for all of them.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scan::process_file(conn, adapter.as_ref(), path, ctx)
        }))
        .unwrap_or_else(|_| {
            error!(path = %path.display(), "the history indexer panicked on a transcript; skipping it");
            Ok(FileOutcome::Skipped)
        });
        match outcome {
            Ok(FileOutcome::Indexed {
                rows,
                errors,
                reindexed,
            }) => {
                debug!(path = %path.display(), rows, errors, reindexed, "indexed a transcript");
                rows
            }
            Ok(FileOutcome::Skipped | FileOutcome::Unchanged) => 0,
            Err(error) => {
                warn!(path = %path.display(), %error, "indexing a transcript failed");
                0
            }
        }
    }

    fn index_heeler(&mut self, record: &HeelerRecord) {
        if !self.heeler {
            return;
        }
        let cutoff = self.cutoff_ms();
        if cutoff > 0 && record.ts_ms < cutoff {
            return;
        }
        let result = (|| -> Result<()> {
            let conn = self.conn()?;
            let tx = conn
                .transaction()
                .map_err(IndexError::sqlite("starting a Heeler write"))?;
            let session = store::ensure_session(
                &tx,
                Source::Heeler,
                &record.session_key,
                None,
                record.title.as_deref(),
                record.cwd.as_deref(),
            )?;
            store::delete_ext(&tx, session, &record.ext_key)?;
            let text = redact(&record.text);
            for (index, piece) in chunks(&text).into_iter().take(MAX_TOOL_CHUNKS).enumerate() {
                let key = if index == 0 {
                    record.ext_key.clone()
                } else {
                    format!("{}#{index}", record.ext_key)
                };
                // The command line is the identifier of a command row.
                let idents = if record.role == Role::Command {
                    crate::adapters::truncate(piece, 1024).to_owned()
                } else {
                    idents(piece)
                };
                store::insert_entry(
                    &tx,
                    session,
                    record.role,
                    Some(record.ts_ms),
                    Some(&key),
                    piece,
                    &idents,
                )?;
            }
            store::set_meta(&tx, "heeler_last_indexed_ms", &now_ms().to_string())?;
            tx.commit()
                .map_err(IndexError::sqlite("committing a Heeler write"))
        })();
        if let Err(error) = result {
            warn!(%error, "indexing a Heeler record failed");
        }
    }

    fn clear(&mut self) -> Result<()> {
        self.pending.clear();
        self.first_pending = None;
        self.last_event = None;
        // Close first: the file is deleted below.
        self.conn = None;
        store::remove_database(&self.db_path)?;
        self.conn = Some(store::open(&self.db_path)?);
        info!("history index cleared");
        Ok(())
    }

    fn rebuild(&mut self) {
        let result = self.conn().and_then(|conn| {
            let tx = conn
                .transaction()
                .map_err(IndexError::sqlite("starting a rebuild"))?;
            store::delete_external(&tx)?;
            tx.commit()
                .map_err(IndexError::sqlite("committing a rebuild"))
        });
        match result {
            Ok(()) => {
                info!("rebuilding the history index");
                self.stat_pass();
            }
            Err(error) => warn!(%error, "rebuilding the history index failed"),
        }
    }
}
