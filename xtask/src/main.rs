//! Developer tasks. `cargo run -p xtask --release -- bench` measures the performance budgets;
//! `cargo run -p xtask -- bundle` assembles and ad-hoc signs `Heeler.app` (`docs/packaging.md`).

mod bench;
mod bundle;
mod proc;
mod seed;
mod stats;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Heeler developer tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build release binaries and measure the budgets B1–B6 of docs/vision.md.
    Bench {
        /// Budgets to measure, comma separated (e.g. `B1,B4`); default all.
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Print the full report as JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Runs per scenario; the table shows the median.
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// Profile the desktop and daemon with `sample(1)` during the latency scenarios
        /// (output in target/bench/).
        #[arg(long)]
        sample: bool,
        /// Milliseconds between keystrokes of the typing and flood scenarios (B4: 100, B5: 50).
        #[arg(long)]
        key_interval_ms: Option<u64>,
    },
    /// Assemble Heeler.app (desktop, heelerd, heeler, LaunchAgent plist, shell integration
    /// scripts) and sign it ad hoc. Nothing is launched or registered.
    Bundle {
        /// Build (and bundle) the release binaries instead of the debug ones.
        #[arg(long)]
        release: bool,
        /// Folder that gets Heeler.app (default: target/bundle).
        #[arg(long, value_name = "DIR")]
        out: Option<std::path::PathBuf>,
        /// Bundle the binaries already built in DIR instead of running cargo build.
        #[arg(long, value_name = "DIR")]
        bin_dir: Option<std::path::PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Bench {
            only,
            json,
            runs,
            sample,
            key_interval_ms,
        } => {
            bench::run(bench::Options {
                only,
                json,
                runs: runs.max(1),
                sample,
                key_interval_ms,
            })
            .await
        }
        Command::Bundle {
            release,
            out,
            bin_dir,
        } => bundle::run(bundle::Options {
            release,
            out,
            bin_dir,
        }),
    }
}
