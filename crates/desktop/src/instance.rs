//! One Hutan desktop per data directory (`HUTAN_HOME`, else `~/Library/Application Support/Hutan`).
//!
//! The first desktop takes an exclusive `flock` on `<data dir>/desktop.lock` before any window
//! opens and listens on `<data dir>/desktop.sock` (mode `0600`; connections from another user
//! are dropped, as the daemon does). A second one finds the lock held, sends one JSON line,
//! `{"cmd":"activate"}`, to that socket, waits for `{"ok":true}` and exits 0: the first one
//! brings its window forward ([`ActivationSink`]). A lock and socket left by a crashed desktop
//! are harmless: the kernel drops the lock with the process, and the lock holder removes a stale
//! socket file before binding.
//!
//! No GPUI and no AppKit here: the part that touches the application is the [`ActivationSink`]
//! the caller supplies.

#![allow(unsafe_code)] // peer credentials through libc; each block says why it is sound.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const LOCK_FILE: &str = "desktop.lock";
pub const SOCKET_FILE: &str = "desktop.sock";

/// What a second desktop says.
const ACTIVATE_LINE: &str = r#"{"cmd":"activate"}"#;
const OK_LINE: &str = r#"{"ok":true}"#;
/// Longest request line accepted; anything longer is not ours.
const MAX_LINE: u64 = 256;
/// How long the first desktop waits for a client to send its line, and a second desktop for the
/// answer.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a second desktop retries a socket that is not there (yet).
pub const CONNECT_PATIENCE: Duration = Duration::from_secs(3);
const CONNECT_POLL: Duration = Duration::from_millis(50);

/// What the first desktop does when a second one asks it to come forward.
pub trait ActivationSink: Send + 'static {
    /// Called on the listener's thread, once per accepted request.
    fn activate(&self);
}

/// The outcome of [`claim`].
#[derive(Debug)]
pub enum Claim {
    /// This is the desktop; keep the guard for as long as it runs.
    Primary(Primary),
    /// Another desktop holds the lock.
    Running,
}

/// The first desktop's claim: the lock, the socket and the thread that serves it. Dropping it
/// stops serving, removes the socket and releases the lock.
#[derive(Debug)]
pub struct Primary {
    _lock: File,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Primary {
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for Primary {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the blocking `accept`; the thread sees the flag and ends.
        let _ = UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.socket);
    }
}

/// The data directory of this process: `settings.toml` and the daemon's files live there.
pub fn data_dir() -> io::Result<PathBuf> {
    hutan_settings::SettingsPaths::from_env()
        .ok()
        .and_then(|paths| paths.user.parent().map(Path::to_owned))
        .ok_or_else(|| io::Error::other("cannot determine the data directory; set HUTAN_HOME"))
}

/// Takes the lock of `dir`, or finds it held. On success the socket is bound and served:
/// `sink.activate()` runs for every request of a second desktop.
pub fn claim(dir: &Path, sink: impl ActivationSink) -> io::Result<Claim> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let lock_path = dir.join(LOCK_FILE);
    let mut lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(Claim::Running),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    // Informational: who holds the lock.
    lock.set_len(0)?;
    writeln!(lock, "{}", std::process::id())?;

    let socket = dir.join(SOCKET_FILE);
    remove_stale_socket(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name("hutan-instance".to_owned())
        .spawn({
            let stop = stop.clone();
            move || serve(&listener, &stop, &sink)
        })?;
    Ok(Claim::Primary(Primary {
        _lock: lock,
        socket,
        stop,
        thread: Some(thread),
    }))
}

/// Removes a socket file left by a dead desktop. Safe only while holding the lock; anything
/// that is not a socket is an error, never deleted.
fn remove_stale_socket(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is not a socket", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn serve(listener: &UnixListener, stop: &AtomicBool, sink: &impl ActivationSink) {
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else { continue };
        match handle(stream) {
            Ok(true) => sink.activate(),
            Ok(false) => {}
            Err(error) => tracing::debug!("a desktop instance request failed: {error}"),
        }
    }
}

/// One request: `Ok(true)` when it asked to activate (and was answered).
fn handle(stream: UnixStream) -> io::Result<bool> {
    if !is_same_user(&stream)? {
        tracing::warn!("rejecting a desktop instance request from a different user");
        return Ok(false);
    }
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_LINE)).read_line(&mut line)?;
    let activate = serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()
        .is_some_and(|value| value.get("cmd").and_then(|c| c.as_str()) == Some("activate"));
    let mut stream = stream;
    if activate {
        writeln!(stream, "{OK_LINE}")?;
    } else {
        writeln!(stream, r#"{{"ok":false,"error":"unknown request"}}"#)?;
    }
    Ok(activate)
}

/// Whether the peer of `stream` runs as the user this process runs as.
pub fn is_same_user(stream: &UnixStream) -> io::Result<bool> {
    // SAFETY: `geteuid` has no preconditions.
    let own = unsafe { libc::geteuid() };
    Ok(peer_uid(stream)? == own)
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the descriptor is open for the call and both out-pointers are valid.
    let status = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if status == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    // SAFETY: the descriptor is open; `cred` and `len` are valid for `SO_PEERCRED`.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    if status == 0 {
        Ok(cred.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// What a second desktop does: asks the running one to come forward. Retries for `patience`
/// while the socket is not there yet (the first desktop takes the lock a moment before it binds).
pub fn notify_running(dir: &Path, patience: Duration) -> io::Result<()> {
    let socket = dir.join(SOCKET_FILE);
    let deadline = Instant::now() + patience;
    let stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(CONNECT_POLL);
            }
            Err(error) => return Err(error),
        }
    };
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut writer = &stream;
    writeln!(writer, "{ACTIVATE_LINE}")?;
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer)?;
    let ok = serde_json::from_str::<serde_json::Value>(answer.trim())
        .ok()
        .is_some_and(|value| value.get("ok").and_then(serde_json::Value::as_bool) == Some(true));
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "the running Hutan did not accept the request: {}",
            answer.trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    struct Counter(mpsc::Sender<()>);

    impl ActivationSink for Counter {
        fn activate(&self) {
            let _ = self.0.send(());
        }
    }

    fn primary(dir: &Path) -> (Primary, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        match claim(dir, Counter(tx)).unwrap() {
            Claim::Primary(primary) => (primary, rx),
            Claim::Running => panic!("the lock of {} is held", dir.display()),
        }
    }

    fn quiet(dir: &Path) -> Claim {
        let (tx, _rx) = mpsc::channel();
        claim(dir, Counter(tx)).unwrap()
    }

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn the_second_desktop_finds_the_lock_held_and_the_first_is_told_to_come_forward() {
        let dir = tempfile::tempdir().unwrap();
        let (first, activations) = primary(dir.path());
        assert!(matches!(quiet(dir.path()), Claim::Running));
        assert!(activations.try_recv().is_err(), "nothing yet");

        notify_running(dir.path(), CONNECT_PATIENCE).unwrap();
        activations
            .recv_timeout(WAIT)
            .expect("the first desktop was told");
        notify_running(dir.path(), CONNECT_PATIENCE).unwrap();
        activations.recv_timeout(WAIT).expect("and again");
        assert_eq!(
            fs::metadata(first.socket()).unwrap().permissions().mode() & 0o777,
            0o600,
            "only the owner may connect"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(LOCK_FILE))
                .unwrap()
                .trim(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn dropping_the_first_desktop_frees_the_lock_and_removes_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _activations) = primary(dir.path());
        let socket = first.socket().to_owned();
        drop(first);
        assert!(!socket.exists());
        let (_second, activations) = primary(dir.path());
        notify_running(dir.path(), CONNECT_PATIENCE).unwrap();
        activations.recv_timeout(WAIT).unwrap();
    }

    #[test]
    fn a_socket_left_by_a_crashed_desktop_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        // What a crash leaves: the socket file without a listener (and no lock held).
        drop(UnixListener::bind(dir.path().join(SOCKET_FILE)).unwrap());
        assert!(dir.path().join(SOCKET_FILE).exists());
        assert!(
            notify_running(dir.path(), Duration::from_millis(100)).is_err(),
            "nobody answers a stale socket"
        );
        let (_first, activations) = primary(dir.path());
        notify_running(dir.path(), CONNECT_PATIENCE).unwrap();
        activations.recv_timeout(WAIT).unwrap();
    }

    #[test]
    fn something_else_at_the_socket_path_is_an_error_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SOCKET_FILE), "precious").unwrap();
        let (tx, _rx) = mpsc::channel();
        let error = claim(dir.path(), Counter(tx)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(dir.path().join(SOCKET_FILE)).unwrap(),
            "precious"
        );
    }

    #[test]
    fn a_held_lock_without_a_socket_is_reported_after_the_patience_runs_out() {
        let dir = tempfile::tempdir().unwrap();
        let held = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.path().join(LOCK_FILE))
            .unwrap();
        held.try_lock().unwrap();
        assert!(matches!(quiet(dir.path()), Claim::Running));
        let started = Instant::now();
        let error = notify_running(dir.path(), Duration::from_millis(200)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn anything_but_the_activate_line_is_refused_and_activates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (first, activations) = primary(dir.path());
        for request in ["hello", r#"{"cmd":"quit"}"#, "{", ""] {
            let stream = UnixStream::connect(first.socket()).unwrap();
            stream.set_read_timeout(Some(WAIT)).unwrap();
            let mut writer = &stream;
            writeln!(writer, "{request}").unwrap();
            let mut answer = String::new();
            BufReader::new(&stream).read_line(&mut answer).unwrap();
            assert!(answer.contains(r#""ok":false"#), "{request:?} -> {answer}");
        }
        // A line that never ends is cut off, not buffered forever.
        let stream = UnixStream::connect(first.socket()).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut writer = &stream;
        writer.write_all(&vec![b'x'; 4096]).unwrap();
        writer.write_all(b"\n").unwrap();
        let mut answer = String::new();
        BufReader::new(&stream).read_line(&mut answer).unwrap();
        assert!(answer.contains(r#""ok":false"#));
        assert!(activations.try_recv().is_err());
        // The listener is still fine afterwards.
        notify_running(dir.path(), CONNECT_PATIENCE).unwrap();
        activations.recv_timeout(WAIT).unwrap();
    }

    #[test]
    fn the_peer_check_knows_this_user() {
        let (a, b) = UnixStream::pair().unwrap();
        assert!(is_same_user(&a).unwrap());
        assert!(is_same_user(&b).unwrap());
    }

    #[test]
    fn different_data_directories_do_not_share_an_instance() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let (_a, _rx_a) = primary(one.path());
        let (_b, _rx_b) = primary(two.path());
    }

    #[test]
    fn the_data_directory_follows_hutan_home_like_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        // Same rule as `SettingsPaths::resolve`, without touching the environment.
        let paths =
            hutan_settings::SettingsPaths::resolve(Some(dir.path().as_os_str().to_owned()), None)
                .unwrap();
        assert_eq!(paths.user.parent(), Some(dir.path()));
    }
}
