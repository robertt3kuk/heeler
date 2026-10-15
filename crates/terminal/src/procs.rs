//! Process inspection and signalling for the resource manager (`docs/resources.md`).
//!
//! On macOS the facts come from `libproc` (`proc_listchildpids`, `proc_pidinfo`) and the
//! arguments from `sysctl(KERN_PROCARGS2)`; other systems report no process at all (Hutan is
//! macOS-first). Everything returned is data: nothing here decides which process may be
//! signalled; the daemon verifies that against the session's process tree first.
//!
//! One of the two modules of the crate with `unsafe` (the other is `sys.rs`); every block
//! documents why it is sound.
#![allow(unsafe_code)]

use std::io;

/// Signals the resource manager may send to a single process (`process.signal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcSignal {
    Term,
    Kill,
    Int,
    Hup,
    Stop,
    Cont,
}

impl ProcSignal {
    fn raw(self) -> libc::c_int {
        match self {
            ProcSignal::Term => libc::SIGTERM,
            ProcSignal::Kill => libc::SIGKILL,
            ProcSignal::Int => libc::SIGINT,
            ProcSignal::Hup => libc::SIGHUP,
            ProcSignal::Stop => libc::SIGSTOP,
            ProcSignal::Cont => libc::SIGCONT,
        }
    }
}

/// What the kernel says a process is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    Running,
    Sleeping,
    Stopped,
    Zombie,
    /// Being created.
    Idle,
    Unknown,
}

/// One process as the kernel shows it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub ppid: u32,
    /// The kernel's name for it (the executable's name).
    pub name: String,
    pub state: ProcState,
    /// User plus system CPU time used so far, in nanoseconds.
    pub cpu_time_ns: u64,
    pub rss_bytes: u64,
    pub threads: u32,
    /// Start time, milliseconds since the Unix epoch; together with the pid it identifies the
    /// process (pids are reused).
    pub started_at_ms: u64,
}

/// Sends `signal` to the single process `pid`. `ESRCH` (already gone) is an error like any
/// other; callers decide whether that matters.
pub fn signal(pid: u32, signal: ProcSignal) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
    // SAFETY: `kill` takes two integers and has no memory-safety preconditions; `pid > 1`
    // excludes the special values 0 and -1 (whole groups and everything) and init.
    if unsafe { libc::kill(pid, signal.raw()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The process's argument vector (`argv`, without the environment), as it was started.
/// `None` when the process is gone, belongs to someone else, or the kernel refuses.
pub fn arguments(pid: u32) -> Option<Vec<String>> {
    imp::arguments(pid)
}

/// The direct children of `pid`; empty when it has none or is gone.
pub fn children(pid: u32) -> Vec<u32> {
    imp::children(pid)
}

/// Every process below `root` (children, their children, …) breadth first, parents before
/// children, at most `limit` of them; `root` itself is not listed.
pub fn descendants(root: u32, limit: usize) -> Vec<u32> {
    let mut found = Vec::new();
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::from([root]);
    let mut next = 0;
    let mut frontier = vec![root];
    while next < frontier.len() && found.len() < limit {
        let parent = frontier[next];
        next += 1;
        for child in imp::children(parent) {
            if found.len() >= limit {
                break;
            }
            if seen.insert(child) {
                found.push(child);
                frontier.push(child);
            }
        }
    }
    found
}

/// The process `pid`; `None` when it is gone or cannot be inspected.
pub fn inspect(pid: u32) -> Option<ProcessSnapshot> {
    imp::inspect(pid)
}

/// Splits a `KERN_PROCARGS2` buffer into the process's arguments: `argc` (native-endian
/// `i32`), the executable path, NUL padding, then `argc` NUL-terminated strings (the
/// environment that follows is never read).
pub fn parse_procargs(buffer: &[u8]) -> Option<Vec<String>> {
    let (head, rest) = buffer.split_first_chunk::<4>()?;
    let argc = usize::try_from(i32::from_ne_bytes(*head)).ok()?;
    // Executable path, then the padding NULs before argv[0].
    let path_end = rest.iter().position(|b| *b == 0)?;
    let mut rest = &rest[path_end..];
    while let [0, tail @ ..] = rest {
        rest = tail;
    }
    let mut args = Vec::new();
    while args.len() < argc && !rest.is_empty() {
        let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
        args.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = rest.get(end + 1..).unwrap_or(&[]);
    }
    Some(args)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::LazyLock;

    use super::{ProcState, ProcessSnapshot, parse_procargs};

    /// Most direct children listed at once; a process with more is listed in part.
    const MAX_CHILDREN: usize = 16 * 1024;

    pub(super) fn children(pid: u32) -> Vec<u32> {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return Vec::new();
        };
        let mut capacity = 256usize;
        loop {
            let mut buffer = vec![0 as libc::pid_t; capacity];
            let bytes = libc::c_int::try_from(capacity * size_of::<libc::pid_t>())
                .unwrap_or(libc::c_int::MAX);
            // SAFETY: `buffer` is writable for `bytes` bytes, which is what is passed; the call
            // writes at most that many.
            let count = unsafe { libc::proc_listchildpids(pid, buffer.as_mut_ptr().cast(), bytes) };
            let Ok(count) = usize::try_from(count) else {
                return Vec::new();
            };
            // The count is of pids; a full buffer may have cut the list off.
            if count >= capacity && capacity < MAX_CHILDREN {
                capacity *= 4;
                continue;
            }
            return buffer[..count.min(capacity)]
                .iter()
                .filter_map(|pid| u32::try_from(*pid).ok())
                .filter(|pid| *pid != 0)
                .collect();
        }
    }

    /// Mach absolute time units per nanosecond, as `numer / denom`.
    // `libc` points to the `mach2` crate for this; one call is not worth a dependency.
    #[allow(deprecated)]
    static TIMEBASE: LazyLock<(u64, u64)> = LazyLock::new(|| {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `info` is a valid, exclusively borrowed struct for the call.
        let status = unsafe { libc::mach_timebase_info(&raw mut info) };
        if status == 0 && info.numer != 0 && info.denom != 0 {
            (u64::from(info.numer), u64::from(info.denom))
        } else {
            (1, 1)
        }
    });

    fn mach_to_ns(ticks: u64) -> u64 {
        let (numer, denom) = *TIMEBASE;
        u64::try_from(u128::from(ticks) * u128::from(numer) / u128::from(denom)).unwrap_or(u64::MAX)
    }

    fn text(field: &[libc::c_char]) -> String {
        let bytes: Vec<u8> = field
            .iter()
            .take_while(|c| **c != 0)
            // Reinterpreting `c_char` as a byte: the value is not used as a number.
            .map(|c| *c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn state_of(status: u32) -> ProcState {
        match status {
            libc::SIDL => ProcState::Idle,
            libc::SRUN => ProcState::Running,
            libc::SSLEEP => ProcState::Sleeping,
            libc::SSTOP => ProcState::Stopped,
            libc::SZOMB => ProcState::Zombie,
            _ => ProcState::Unknown,
        }
    }

    fn bsd_name(info: &libc::proc_bsdinfo) -> String {
        let name = text(&info.pbi_name);
        if name.is_empty() {
            text(&info.pbi_comm)
        } else {
            name
        }
    }

    fn started_ms(info: &libc::proc_bsdinfo) -> u64 {
        info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec / 1000
    }

    pub(super) fn inspect(pid: u32) -> Option<ProcessSnapshot> {
        let raw_pid = libc::c_int::try_from(pid).ok()?;
        // SAFETY: an all-zero `proc_taskallinfo` is a valid value for this plain struct of
        // integers and byte arrays; it is only an output parameter.
        let mut all: libc::proc_taskallinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(size_of::<libc::proc_taskallinfo>()).ok()?;
        // SAFETY: `all` is writable for `size` bytes (its own size) and exclusively borrowed.
        let got = unsafe {
            libc::proc_pidinfo(
                raw_pid,
                libc::PROC_PIDTASKALLINFO,
                0,
                (&raw mut all).cast(),
                size,
            )
        };
        if got == size {
            let bsd = &all.pbsd;
            let task = &all.ptinfo;
            return Some(ProcessSnapshot {
                pid,
                ppid: bsd.pbi_ppid,
                name: bsd_name(bsd),
                state: state_of(bsd.pbi_status),
                cpu_time_ns: mach_to_ns(task.pti_total_user.saturating_add(task.pti_total_system)),
                rss_bytes: task.pti_resident_size,
                threads: u32::try_from(task.pti_threadnum).unwrap_or(0),
                started_at_ms: started_ms(bsd),
            });
        }
        // No task info (a zombie has no task): the BSD part still names it and its parent.
        // SAFETY: as above for `proc_bsdinfo`.
        let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `bsd` is writable for `size` bytes (its own size) and exclusively borrowed.
        let got = unsafe {
            libc::proc_pidinfo(
                raw_pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut bsd).cast(),
                size,
            )
        };
        if got != size || bsd.pbi_pid != pid {
            return None;
        }
        Some(ProcessSnapshot {
            pid,
            ppid: bsd.pbi_ppid,
            name: bsd_name(&bsd),
            state: state_of(bsd.pbi_status),
            cpu_time_ns: 0,
            rss_bytes: 0,
            threads: 0,
            started_at_ms: started_ms(&bsd),
        })
    }

    pub(super) fn arguments(pid: u32) -> Option<Vec<String>> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size: libc::size_t = 0;
        // SAFETY: `name` has 3 entries as stated; with a null output buffer the call only
        // writes the needed size into `size`, an exclusively borrowed local.
        let status = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 || size == 0 {
            return None;
        }
        // The arguments can grow between the two calls; ask for some slack.
        let mut buffer = vec![0u8; size.saturating_add(1024)];
        let mut size = buffer.len();
        // SAFETY: `buffer` is writable for `size` bytes, and `size` is updated to what was
        // written; `name` as above.
        let status = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return None;
        }
        buffer.truncate(size);
        parse_procargs(&buffer)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::ProcessSnapshot;

    pub(super) fn children(_pid: u32) -> Vec<u32> {
        Vec::new()
    }

    pub(super) fn inspect(_pid: u32) -> Option<ProcessSnapshot> {
        None
    }

    pub(super) fn arguments(_pid: u32) -> Option<Vec<String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn procargs(argc: i32, path: &str, padding: usize, args: &[&str], env: &[&str]) -> Vec<u8> {
        let mut buffer = argc.to_ne_bytes().to_vec();
        buffer.extend_from_slice(path.as_bytes());
        buffer.extend(std::iter::repeat_n(0, 1 + padding));
        for text in args.iter().chain(env) {
            buffer.extend_from_slice(text.as_bytes());
            buffer.push(0);
        }
        buffer
    }

    #[test]
    fn procargs_yield_argv_without_the_environment() {
        let buffer = procargs(
            3,
            "/bin/sh",
            3,
            &["sh", "-c", "sleep 60"],
            &["HOME=/x", "TOKEN=secret"],
        );
        assert_eq!(
            parse_procargs(&buffer).unwrap(),
            ["sh", "-c", "sleep 60"].map(String::from)
        );
    }

    #[test]
    fn procargs_stop_at_argc_and_survive_truncation() {
        let buffer = procargs(1, "/bin/ls", 0, &["ls"], &["A=1"]);
        assert_eq!(parse_procargs(&buffer).unwrap(), vec!["ls".to_owned()]);
        // The buffer ended before argc strings were read: what is there is returned.
        let cut = procargs(5, "/bin/ls", 0, &["ls", "-l"], &[]);
        assert_eq!(parse_procargs(&cut).unwrap().len(), 2);
        assert!(parse_procargs(&[1, 0]).is_none());
        assert!(parse_procargs(&(-1i32).to_ne_bytes()).is_none());
    }

    #[test]
    fn signalling_refuses_the_special_pids() {
        for pid in [0, 1, u32::MAX] {
            assert!(signal(pid, ProcSignal::Term).is_err(), "pid {pid}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_process_and_its_children_are_visible() {
        let me = std::process::id();
        let snapshot = inspect(me).unwrap();
        assert_eq!(snapshot.pid, me);
        assert!(snapshot.rss_bytes > 0 && snapshot.threads > 0 && snapshot.cpu_time_ns > 0);
        assert!(snapshot.started_at_ms > 1_500_000_000_000);
        let args = arguments(me).unwrap();
        assert!(!args.is_empty());

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(children(me).contains(&pid));
        let seen = inspect(pid).unwrap();
        assert_eq!(seen.ppid, me);
        assert_eq!(seen.name, "sleep");
        assert_eq!(arguments(pid).unwrap(), ["sleep", "30"].map(String::from));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(inspect(pid).is_none());
    }
}
