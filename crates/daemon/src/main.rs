//! `staffyd`: the Staffy daemon.
//!
//! Environment: `STAFFY_HOME` (base directory), `STAFFY_SOCKET` (socket path only),
//! `STAFFY_LOG` or `RUST_LOG` (log filter, default `info`). Exit codes: 0 clean stop,
//! 1 failure, 2 another daemon already holds the lock.

use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::Context;
use staffy_daemon::{DaemonConfig, DaemonError, Paths};
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const EXIT_ALREADY_RUNNING: u8 = 2;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("staffyd: {error:#}");
            match error.downcast_ref::<DaemonError>() {
                Some(DaemonError::AlreadyRunning { .. }) => ExitCode::from(EXIT_ALREADY_RUNNING),
                _ => ExitCode::FAILURE,
            }
        }
    }
}

fn run() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    paths.prepare().context("preparing directories")?;
    init_logging(&paths)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(async {
        let daemon = staffy_daemon::start(DaemonConfig::new(paths)).await?;

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

/// Logs to stderr and to `staffyd.log`.
fn init_logging(paths: &Paths) -> anyhow::Result<()> {
    let log_path = paths.log_file();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let filter = EnvFilter::try_from_env("STAFFY_LOG")
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
