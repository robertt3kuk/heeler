//! `hutand`: the Hutan daemon.
//!
//! Environment: `HUTAN_HOME` (base directory), `HUTAN_SOCKET` (socket path only),
//! `HUTAN_LOG` or `RUST_LOG` (log filter, default `info`). Exit codes: 0 clean stop,
//! 1 failure, 2 another daemon already holds the lock, 64 unknown argument.
//!
//! Arguments: none to run; `--version` / `-V` and `--help` / `-h` print and exit without
//! touching any directory (anything else is refused, so a mistyped flag never starts a daemon
//! on the real home).
//!
//! Threads: tokio runs on a thread of its own. When the daemon runs from `Hutan.app` the main
//! thread serves the Cocoa run loop of the native notifications (`notify_native`), because the
//! system's notification centre delivers its callbacks there; otherwise the main thread just
//! waits for the runtime thread.

use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use hutan_daemon::notify_native::{self, NativePoster, Stopper};
use hutan_daemon::{DaemonConfig, DaemonError, Paths};
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const EXIT_ALREADY_RUNNING: u8 = 2;
const EXIT_USAGE: u8 = 64;

const HELP: &str = "hutand: the Hutan daemon (normally started by `hutan` or the app)

Usage: hutand [--version | --help]

Environment: HUTAN_HOME (base directory), HUTAN_SOCKET (socket path), HUTAN_LOG (log filter)";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--version" | "-V") => {
            println!("hutand {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("hutand: unknown argument `{other}`\n\n{HELP}");
            return ExitCode::from(EXIT_USAGE);
        }
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hutand: {error:#}");
            match error.downcast_ref::<DaemonError>() {
                Some(DaemonError::AlreadyRunning { .. }) => ExitCode::from(EXIT_ALREADY_RUNNING),
                _ => ExitCode::FAILURE,
            }
        }
    }
}

/// The runtime thread's stack: what the main thread has, where the daemon used to run.
const RUNTIME_STACK_BYTES: usize = 8 * 1024 * 1024;

fn run() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    paths.prepare().context("preparing directories")?;
    init_logging(&paths)?;

    // `Some` only inside Hutan.app: nothing is asked of the system before the first post.
    let host = notify_native::system_host();
    let poster = host.as_ref().map(notify_native::MainThreadHost::poster);
    let stopper = host.as_ref().map(notify_native::MainThreadHost::stopper);
    let runtime = std::thread::Builder::new()
        .name("hutand-runtime".to_owned())
        .stack_size(RUNTIME_STACK_BYTES)
        .spawn(move || {
            // However the daemon ends (a panic included), the main thread's loop must too.
            let _stop = stopper.map(StopOnDrop);
            run_daemon(paths, poster)
        })
        .context("starting the runtime thread")?;
    if let Some(host) = host {
        host.run();
    }
    runtime
        .join()
        .map_err(|_| anyhow::anyhow!("the runtime thread panicked"))?
}

/// Ends the main thread's run loop when dropped.
struct StopOnDrop(Stopper);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

fn run_daemon(paths: Paths, native: Option<Arc<dyn NativePoster>>) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(async {
        let mut config = DaemonConfig::new(paths);
        config.native = native;
        let daemon = hutan_daemon::start(config).await?;

        let token = daemon.shutdown_token();
        let mut sigterm = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
        let mut sigint = signal(SignalKind::interrupt()).context("installing SIGINT handler")?;
        tokio::spawn(async move {
            let name = tokio::select! {
                _ = sigterm.recv() => "SIGTERM",
                _ = sigint.recv() => "SIGINT",
                _ = token.cancelled() => return,
            };
            info!(signal = name, "signal received");
            token.cancel();
        });

        daemon.wait().await?;
        Ok(())
    })
}

/// Logs to stderr and to `hutand.log`.
fn init_logging(paths: &Paths) -> anyhow::Result<()> {
    let log_path = paths.log_file();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let filter = EnvFilter::try_from_env("HUTAN_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr),
        )
        .with(fmt::layer().with_ansi(false).with_writer(Mutex::new(file)))
        .init();
    Ok(())
}
