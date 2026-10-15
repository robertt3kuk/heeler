//! The only module of the crate that needs `unsafe`: thin wrappers over `kill(2)`/`killpg(2)`,
//! `waitpid(2)`, `getpwuid_r(3)`, and `confstr(3)`.
#![allow(unsafe_code)]

use std::io;

/// Signals sent to the child's process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    Hangup,
    Kill,
}

impl Signal {
    fn raw(self) -> libc::c_int {
        match self {
            Signal::Hangup => libc::SIGHUP,
            Signal::Kill => libc::SIGKILL,
        }
    }
}

/// Sends `signal` to the process group led by `pid` (the child is a session and group
/// leader, see `portable-pty`'s `setsid`), falling back to the single process when the group
/// is already gone.
pub(crate) fn signal_group(pid: u32, signal: Signal) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
    // SAFETY: `killpg` only takes plain integers and has no memory-safety preconditions; a stale
    // pid at worst returns ESRCH (callers check that the child has not been reaped first).
    if unsafe { libc::killpg(pid, signal.raw()) } == 0 {
        return Ok(());
    }
    // SAFETY: as above, `kill` takes two integers.
    if unsafe { libc::kill(pid, signal.raw()) } == 0 {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}

/// How a reaped child ended, from the raw wait status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reaped {
    Exited(i32),
    Signaled(i32),
}

/// Blocks until the child `pid` exits or is killed and reaps it. Must be the only reaper of
/// `pid` (the caller owns the child and never calls the child's own `wait`).
pub(crate) fn wait_child(pid: u32) -> io::Result<Reaped> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: `status` is a valid, exclusively borrowed c_int for the duration of the call;
        // `waitpid` has no other memory-safety preconditions.
        let result = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if libc::WIFEXITED(status) {
            return Ok(Reaped::Exited(libc::WEXITSTATUS(status)));
        }
        if libc::WIFSIGNALED(status) {
            return Ok(Reaped::Signaled(libc::WTERMSIG(status)));
        }
        // Stopped/continued reports are not requested (no WUNTRACED); keep waiting.
    }
}

/// The current user's account entry, from the account database (what `chsh` edits; on macOS the
/// directory service).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Account {
    pub(crate) name: String,
    pub(crate) home: String,
    /// `None` when the entry has no shell.
    pub(crate) shell: Option<String>,
}

fn field(pointer: *const libc::c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the caller passes a field of a successful `getpwuid_r` result: a NUL-terminated
    // string inside the lookup buffer, which is still alive.
    let text = unsafe { std::ffi::CStr::from_ptr(pointer) };
    text.to_str()
        .ok()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The current user's account entry; `None` if the lookup fails or the entry has no name or
/// home directory.
pub(crate) fn account() -> Option<Account> {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let mut buffer = vec![0 as libc::c_char; 4096];
    loop {
        // SAFETY: an all-zero `passwd` is a valid value for this plain C struct of integers
        // and pointers; it is only an output parameter.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call: `entry` and `result` are exclusively
        // borrowed locals, `buffer` is writable for `buffer.len()` bytes. On success the string
        // fields of `entry` point into `buffer`, which outlives their use below.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &raw mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &raw mut result,
            )
        };
        if rc == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        return Some(Account {
            name: field(entry.pw_name)?,
            home: field(entry.pw_dir)?,
            shell: field(entry.pw_shell),
        });
    }
}

/// The current user's login shell from the account database. `None` if the lookup fails or the
/// entry has no shell.
pub(crate) fn login_shell() -> Option<String> {
    account()?.shell
}

/// The per-user temporary directory (`confstr(_CS_DARWIN_USER_TEMP_DIR)`, what launchd exports
/// as `TMPDIR`, with a trailing slash). `None` where the system has no such directory.
#[cfg(target_os = "macos")]
pub(crate) fn user_temp_dir() -> Option<String> {
    let mut buffer = vec![0 as libc::c_char; libc::PATH_MAX as usize + 1];
    // SAFETY: `buffer` is writable for `buffer.len()` bytes; `confstr` writes at most that many,
    // NUL-terminated, and returns the length it needs (0 on failure).
    let needed = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr(),
            buffer.len(),
        )
    };
    if needed == 0 || needed > buffer.len() {
        return None;
    }
    field(buffer.as_ptr())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn user_temp_dir() -> Option<String> {
    None
}
