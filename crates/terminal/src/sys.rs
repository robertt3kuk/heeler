//! The only module of the crate that needs `unsafe`: thin wrappers over `kill(2)`/`killpg(2)`,
//! `waitpid(2)`, and `getpwuid_r(3)`.
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

/// The current user's login shell from the account database (what `chsh` sets; on macOS the
/// directory service `UserShell`). `None` if the lookup fails or the entry has no shell.
pub(crate) fn login_shell() -> Option<String> {
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
        if rc != 0 || result.is_null() || entry.pw_shell.is_null() {
            return None;
        }
        // SAFETY: on success `pw_shell` is a NUL-terminated string inside `buffer`.
        let shell = unsafe { std::ffi::CStr::from_ptr(entry.pw_shell) };
        return shell
            .to_str()
            .ok()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
    }
}
