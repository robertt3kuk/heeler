//! File watching: `notify` events -> bounded channel -> debounce thread -> `reload_from_disk`.
//!
//! Directories are watched (not files) so atomic-rename saves and files created later are seen.
//! The reload re-reads all three files and compares contents, so event paths never need to be
//! matched (FSEvents reports canonical paths; temp dirs live behind symlinks).

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::SettingsError;
use crate::store::{Shared, create_private_dir};

/// Quiet period after the last event before reloading.
const DEBOUNCE: Duration = Duration::from_millis(100);
/// A steady stream of events (noisy workspace root) still reloads at least this often.
const MAX_DELAY: Duration = Duration::from_millis(500);
const QUEUE: usize = 64;

enum Msg {
    Changed,
    Stop,
}

pub(crate) struct WatchState {
    watcher: RecommendedWatcher,
    dirs: BTreeSet<PathBuf>,
}

/// Keeps the watcher and its debounce thread alive; dropping it stops hot reload.
pub struct WatchGuard {
    shared: Arc<Shared>,
    tx: SyncSender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for WatchGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WatchGuard").finish_non_exhaustive()
    }
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        *self.shared.watch.lock() = None;
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch_error(message: String) -> SettingsError {
    SettingsError::error(None, "", message)
}

pub(crate) fn start(shared: &Arc<Shared>) -> Result<WatchGuard, SettingsError> {
    if shared.watch.lock().is_some() {
        return Err(watch_error("settings are already being watched".to_owned()));
    }
    if let Some(dir) = shared.user_dir() {
        create_private_dir(&dir)
            .map_err(|e| watch_error(format!("cannot create {}: {e}", dir.display())))?;
    }

    let (tx, rx) = mpsc::sync_channel::<Msg>(QUEUE);
    let event_tx = tx.clone();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        match event {
            // A full queue already has a reload pending; dropping is correct.
            Ok(_) => {
                let _ = event_tx.try_send(Msg::Changed);
            }
            Err(e) => tracing::warn!("settings watcher error: {e}"),
        }
    })
    .map_err(|e| watch_error(format!("cannot create file watcher: {e}")))?;
    *shared.watch.lock() = Some(WatchState {
        watcher,
        dirs: BTreeSet::new(),
    });
    rewatch(shared);

    let weak = Arc::downgrade(shared);
    let thread = std::thread::Builder::new()
        .name("hutan-settings-watch".to_owned())
        .spawn(move || run(&rx, &weak))
        .map_err(|e| watch_error(format!("cannot spawn watcher thread: {e}")))?;

    // Anything edited between `SettingsStore::new` and now.
    shared.reload_from_disk();
    Ok(WatchGuard {
        shared: Arc::clone(shared),
        tx,
        thread: Some(thread),
    })
}

fn run(rx: &mpsc::Receiver<Msg>, store: &Weak<Shared>) {
    loop {
        match rx.recv() {
            Ok(Msg::Changed) => {}
            Ok(Msg::Stop) | Err(_) => return,
        }
        let started = Instant::now();
        loop {
            let left = MAX_DELAY.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(DEBOUNCE.min(left)) {
                Ok(Msg::Changed) => {}
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        let Some(shared) = store.upgrade() else {
            return;
        };
        shared.reload_from_disk();
        if rewatch(&shared) {
            // Edits made before the new directory was watched produced no events.
            shared.reload_from_disk();
        }
    }
}

/// Brings the watched directory set in line with the current paths (workspace root changes,
/// `.hutan` appearing). No-op when not watching. Returns whether a directory was newly watched.
pub(crate) fn rewatch(shared: &Shared) -> bool {
    let wanted = shared.watch_dirs();
    let mut guard = shared.watch.lock();
    let Some(state) = guard.as_mut() else {
        return false;
    };
    let mut added = false;
    for dir in state.dirs.difference(&wanted).cloned().collect::<Vec<_>>() {
        // The directory may already be gone; nothing to unwatch then.
        let _ = state.watcher.unwatch(&dir);
        state.dirs.remove(&dir);
    }
    for dir in wanted.difference(&state.dirs.clone()) {
        match state.watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                state.dirs.insert(dir.clone());
                added = true;
            }
            Err(e) => tracing::warn!("cannot watch {}: {e}", dir.display()),
        }
    }
    added
}
