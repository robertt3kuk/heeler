//! The only module of the crate that needs `unsafe`: thin wrappers over `kill(2)`/`killpg(2)`
//! and `waitpid(2)`.
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
