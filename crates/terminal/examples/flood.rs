//! Throughput bench: feeds ~200 MB of mixed terminal output (plain text, SGR 16/256/truecolor,
//! cursor addressing, wide characters, hyperlinks, titles, erase, scroll regions, alternate
//! screen) through the engine path — sniffer + `alacritty_terminal`, no PTY — at 80×24 and
//! prints MB/s and peak RSS.
//!
//! `cargo run --release -p heeler-terminal --example flood [-- <megabytes>]`
#![allow(unsafe_code)]

use std::fmt::Write as _;
use std::time::Instant;

use heeler_protocol::messages::TerminalSize;
use heeler_terminal::Emulator;

/// Same chunk size as the engine spike (ADR 0004 evidence), so numbers are comparable.
const CHUNK: usize = 182_272;
const TARGET_MB: usize = 200;

/// xorshift64*: deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const WORDS: [&str; 12] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima",
];

fn words(rng: &mut Rng, out: &mut String, count: u64) {
    for _ in 0..count {
        out.push_str(WORDS[rng.below(12) as usize]);
        out.push(' ');
    }
}

fn mixed_block(rng: &mut Rng, target_len: usize) -> Vec<u8> {
    let mut out = String::with_capacity(target_len + 4096);
    while out.len() < target_len {
        match rng.below(20) {
            0..=7 => {
                let count = 4 + rng.below(12);
                words(rng, &mut out, count);
                out.push_str("\r\n");
            }
            8..=12 => {
                for _ in 0..(2 + rng.below(6)) {
                    let _ = write!(out, "\x1b[38;5;{}m", rng.below(256));
                    let count = 1 + rng.below(2);
                    words(rng, &mut out, count);
                    out.push_str("\x1b[0m");
                }
                out.push_str("\r\n");
            }
            13 | 14 => {
                let _ = write!(
                    out,
                    "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m",
                    rng.below(256),
                    rng.below(256),
                    rng.below(256),
                    rng.below(256),
                    rng.below(256),
                    rng.below(256)
                );
                words(rng, &mut out, 3);
                out.push_str("\x1b[0m\r\n");
            }
            15 => {
                let _ = write!(out, "\x1b[{};{}H", 1 + rng.below(24), 1 + rng.below(80));
                words(rng, &mut out, 2);
                out.push_str("\x1b[K\x1b[2K");
            }
            16 => out.push_str("界面😀 e\u{301}\u{301} 👩\u{200d}💻 wide text セル\r\n"),
            17 => {
                let _ = write!(
                    out,
                    "\x1b]0;title {}\x07\x1b]8;;https://example.com/{}\x07link\x1b]8;;\x07\r\n",
                    rng.below(1000),
                    rng.below(50)
                );
            }
            18 => {
                out.push_str("\x1b[3;20r\x1b[20;1H\n\n\x1b[r\x1b[1;1H");
            }
            _ => {
                out.push_str("\x1b[?1049h\x1b[2J\x1b[Halt screen ");
                words(rng, &mut out, 3);
                out.push_str("\x1b[?1049l");
            }
        }
    }
    out.into_bytes()
}

/// Peak resident set size of this process in bytes (`ru_maxrss` is bytes on macOS).
fn peak_rss_bytes() -> u64 {
    // SAFETY: `getrusage` fills the zero-initialised struct we pass; no other preconditions.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return 0;
        }
        let max = u64::try_from(usage.ru_maxrss).unwrap_or(0);
        if cfg!(target_os = "linux") {
            max * 1024
        } else {
            max
        }
    }
}

fn main() {
    let megabytes: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(TARGET_MB);
    let total = megabytes * 1_000_000;
    let block = mixed_block(&mut Rng(0x9e37_79b9_7f4a_7c15), 4 * 1024 * 1024);

    let size = TerminalSize {
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    let Ok(mut emulator) = Emulator::new(size, 10_000) else {
        eprintln!("cannot create the emulator");
        return;
    };

    let mut fed = 0usize;
    let start = Instant::now();
    // Chunks are cut at fixed offsets of the endlessly repeated block, so escape sequences
    // are split across reads like they are on a real PTY.
    'feed: loop {
        for chunk in block.chunks(CHUNK) {
            emulator.feed(chunk);
            // Replies/metadata are drained as the PTY reader would.
            drop(emulator.drain_replies());
            drop(emulator.drain_meta());
            fed += chunk.len();
            if fed >= total {
                break 'feed;
            }
        }
    }
    let elapsed = start.elapsed();

    let mb = fed as f64 / 1e6;
    let rss = peak_rss_bytes();
    println!(
        "fed {fed} bytes ({mb:.1} MB) in {:.3}s -> {:.1} MB/s, peak RSS {:.1} MiB, history {} lines",
        elapsed.as_secs_f64(),
        mb / elapsed.as_secs_f64(),
        rss as f64 / (1024.0 * 1024.0),
        emulator.history_len()
    );
}
