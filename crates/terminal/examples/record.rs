//! Corpus recorder: runs a command in a PTY (80×24 unless `--size` says otherwise), answers the
//! terminal's queries through a live engine (so programs such as vim see a real terminal), and
//! stores `tests/corpus/recorded/<name>.raw` (the exact bytes the program wrote) plus
//! `<name>.grid` (the final screen as text, from a fresh replay of the raw bytes). A size other
//! than 80×24 is stored as `<name>.size` (`COLSxROWS`) so the replay uses it.
//!
//! ```text
//! cargo run -p hutan-terminal --example record -- <name> [options] -- <argv…>
//!   --send <delay_ms>:<bytes>   after <delay_ms>, type <bytes> (escapes: \e \a \r \n \t \\ \xHH);
//!                               repeatable, executed in order
//!   --timeout-ms <ms>           kill the program if it runs longer (default 10000)
//!   --env KEY=VALUE             extra environment, repeatable
//!   --size COLSxROWS            terminal size (default 80x24)
//! ```

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use hutan_protocol::messages::TerminalSize;
use hutan_terminal::Emulator;
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// A boxed error is enough for a dev tool.
type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

struct Args {
    name: String,
    sends: Vec<(u64, Vec<u8>)>,
    timeout: Duration,
    env: Vec<(String, String)>,
    cols: u16,
    rows: u16,
    argv: Vec<String>,
}

fn unescape(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 2;
        match bytes[i - 1] {
            b'e' => out.push(0x1b),
            b'a' => out.push(0x07),
            b'r' => out.push(b'\r'),
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'x' if i + 2 <= bytes.len() => {
                if let Some(byte) = std::str::from_utf8(&bytes[i..i + 2])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(byte);
                    i += 2;
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let name = args
        .next()
        .ok_or("usage: record <name> [options] -- <argv…>")?;
    let mut parsed = Args {
        name,
        sends: Vec::new(),
        timeout: Duration::from_secs(10),
        env: Vec::new(),
        cols: 80,
        rows: 24,
        argv: Vec::new(),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                parsed.argv = args.collect();
                break;
            }
            "--send" => {
                let spec = args.next().ok_or("--send needs <delay_ms>:<bytes>")?;
                let (delay, bytes) = spec
                    .split_once(':')
                    .ok_or("--send needs <delay_ms>:<bytes>")?;
                parsed.sends.push((delay.parse()?, unescape(bytes)));
            }
            "--timeout-ms" => {
                parsed.timeout =
                    Duration::from_millis(args.next().ok_or("--timeout-ms needs a value")?.parse()?)
            }
            "--env" => {
                let pair = args.next().ok_or("--env needs KEY=VALUE")?;
                let (key, value) = pair.split_once('=').ok_or("--env needs KEY=VALUE")?;
                parsed.env.push((key.to_owned(), value.to_owned()));
            }
            "--size" => {
                let spec = args.next().ok_or("--size needs COLSxROWS")?;
                let (cols, rows) = spec.split_once('x').ok_or("--size needs COLSxROWS")?;
                parsed.cols = cols.parse()?;
                parsed.rows = rows.parse()?;
            }
            other => return Err(format!("unknown option {other}").into()),
        }
    }
    if parsed.argv.is_empty() {
        return Err("missing `-- <argv…>`".into());
    }
    Ok(parsed)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let size = TerminalSize {
        cols: args.cols,
        rows: args.rows,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pair = native_pty_system().openpty(PtySize {
        rows: args.rows,
        cols: args.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::from_argv(args.argv.iter().map(Into::into).collect());
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "hutan");
    for (key, value) in &args.env {
        cmd.env(key, value);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));

    // Reader: capture raw bytes and keep a live engine to answer queries.
    let raw = Arc::new(Mutex::new(Vec::<u8>::new()));
    let done = Arc::new(AtomicBool::new(false));
    let reader_thread = {
        let (raw, writer, done) = (Arc::clone(&raw), Arc::clone(&writer), Arc::clone(&done));
        thread::spawn(move || {
            let mut live = match Emulator::new(size, 1000) {
                Ok(live) => live,
                Err(_) => return,
            };
            let mut buf = [0u8; 16 * 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                raw.lock().extend_from_slice(&buf[..n]);
                live.feed(&buf[..n]);
                let replies = live.drain_replies();
                if !replies.is_empty() {
                    let _ = writer.lock().write_all(&replies);
                }
            }
            done.store(true, Ordering::Release);
        })
    };

    let started = Instant::now();
    'script: {
        for (delay, bytes) in &args.sends {
            let wake = Instant::now() + Duration::from_millis(*delay);
            while Instant::now() < wake {
                if child.try_wait()?.is_some() {
                    break 'script;
                }
                thread::sleep(Duration::from_millis(10));
            }
            writer.lock().write_all(bytes)?;
        }
    }
    // On timeout the capture is frozen first: whatever the program prints while being hung
    // up (vim restores the screen and says goodbye) is not part of the recording.
    let mut frozen_at = None;
    while child.try_wait()?.is_none() {
        if started.elapsed() > args.timeout {
            frozen_at = Some(raw.lock().len());
            child.kill()?;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait()?;
    let drain_until = Instant::now() + Duration::from_secs(2);
    while !done.load(Ordering::Acquire) && Instant::now() < drain_until {
        thread::sleep(Duration::from_millis(10));
    }
    drop(writer);
    drop(pair.master);
    let _ = reader_thread.join();

    let mut raw = raw.lock().clone();
    let killed = frozen_at.is_some();
    if let Some(len) = frozen_at {
        raw.truncate(len);
    }
    let mut replay = Emulator::new(size, 10_000)?;
    replay.feed(&raw);
    let grid = replay.grid_text();

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/recorded");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{}.raw", args.name)), &raw)?;
    std::fs::write(
        dir.join(format!("{}.grid", args.name)),
        format!("{}\n", grid.trim_end()),
    )?;
    let size_file = dir.join(format!("{}.size", args.name));
    if (args.cols, args.rows) == (80, 24) {
        let _ = std::fs::remove_file(&size_file);
    } else {
        std::fs::write(&size_file, format!("{}x{}\n", args.cols, args.rows))?;
    }
    eprintln!(
        "recorded {}: {} bytes, exit {status}{} -> {}",
        args.name,
        raw.len(),
        if killed { " (killed on timeout)" } else { "" },
        dir.display()
    );
    println!("{}", grid.trim_end());
    Ok(())
}
