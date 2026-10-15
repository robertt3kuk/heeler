//! `hutan-codex-fixture`: record, scrub and replay Codex app-server transcripts.
//!
//! ```text
//! hutan-codex-fixture replay [--received FILE] TRANSCRIPT_OR_DIR [ignored args...]
//! hutan-codex-fixture proxy OUT.jsonl -- PROGRAM [ARGS...]
//! hutan-codex-fixture scrub RAW.jsonl OUT.jsonl
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use hutan_agents::fixture::{proxy, replay, scrub};

const USAGE: &str = "usage:
  hutan-codex-fixture replay [--received FILE] TRANSCRIPT_OR_DIR [ignored args...]
  hutan-codex-fixture proxy OUT.jsonl -- PROGRAM [ARGS...]
  hutan-codex-fixture scrub RAW.jsonl OUT.jsonl";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let outcome = match args.next().as_deref() {
        Some("replay") => replay_command(args.collect()),
        Some("proxy") => proxy_command(args.collect()),
        Some("scrub") => scrub_command(args.collect()),
        _ => Err(USAGE.to_owned()),
    };
    match outcome {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(message) => {
            eprintln!("hutan-codex-fixture: {message}");
            ExitCode::from(2)
        }
    }
}

fn replay_command(args: Vec<String>) -> Result<i32, String> {
    let mut args = args.into_iter();
    let mut received = None;
    let mut transcript = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--received" => received = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            _ if transcript.is_none() => transcript = Some(PathBuf::from(arg)),
            // `app-server` and friends: the real binary's arguments.
            _ => {}
        }
    }
    let transcript = transcript.ok_or(USAGE)?;
    replay::run(&transcript, received.as_deref()).map_err(|e| e.to_string())
}

fn proxy_command(args: Vec<String>) -> Result<i32, String> {
    let mut args = args.into_iter();
    let out = PathBuf::from(args.next().ok_or(USAGE)?);
    if args.next().as_deref() != Some("--") {
        return Err(USAGE.to_owned());
    }
    let program = args.next().ok_or(USAGE)?;
    let rest: Vec<String> = args.collect();
    proxy::run(&out, &program, &rest).map_err(|e| e.to_string())
}

fn scrub_command(args: Vec<String>) -> Result<i32, String> {
    let [raw, out] = args.as_slice() else {
        return Err(USAGE.to_owned());
    };
    let text = std::fs::read_to_string(raw).map_err(|e| format!("reading {raw}: {e}"))?;
    let scrubbed = scrub::scrub(&text).map_err(|e| e.to_string())?;
    std::fs::write(out, scrubbed).map_err(|e| format!("writing {out}: {e}"))?;
    Ok(0)
}
