//! The only module of the crate that needs `unsafe`: lowering the indexer thread's priority and
//! reading the process's CPU time (tests and the perf example).
#![allow(unsafe_code)]

/// Runs the calling thread at background priority: QoS "utility" on macOS (the scheduler keeps
/// it on efficiency cores and throttles its I/O), `nice 10` for the thread elsewhere.
pub(crate) fn lower_current_thread_priority() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: plain integers; the call only changes the calling thread's QoS class.
        let rc =
            unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0) };
        if rc != 0 {
            tracing::debug!(rc, "could not set the indexer thread QoS");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // SAFETY: plain integers. On Linux `who = 0` addresses the calling thread.
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
        if rc != 0 {
            tracing::debug!("could not lower the indexer thread priority");
        }
    }
}

/// User + system CPU time this process has used so far, in microseconds.
pub fn process_cpu_micros() -> u64 {
    // SAFETY: `rusage` is plain data that `getrusage` fully initialises on success.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) != 0 {
            return 0;
        }
        usage
    };
    let micros = |t: libc::timeval| {
        u64::try_from(t.tv_sec).unwrap_or(0) * 1_000_000 + u64::try_from(t.tv_usec).unwrap_or(0)
    };
    micros(usage.ru_utime) + micros(usage.ru_stime)
}
