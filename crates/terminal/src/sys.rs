//! The only module of the crate that needs `unsafe`: thin wrappers over `kill(2)`/`killpg(2)`.
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
