# Terminal engine + PTY spike

## Verdict
**Recommend `alacritty_terminal = "=0.26.0"` + `portable-pty = "=0.9.0"`.** Both worked headlessly, including real interactive zsh/vim, resize, replies, clean exit/reaping, and parent FD cleanup. Keep the daemon as the sole parser; send a versioned **render-state DTO snapshot + ordered cell/row updates**, not terminal-engine internals or a second client-side VT parser. That avoids requiring parser-state migration on client reattach.
Ghostty 0.2.2 builds only after pinning Zig **0.15.2**, plus an older compatible SDK; its pinned native API differs materially from today's snapshot documentation. Do not choose it on the assumption that crates.io 0.2.2 includes the advertised complete binary Snapshot API.

## Comparison
Numbers are median of three sequential fresh-process release runs; decimal MB/s; peak process RSS measured **before snapshot serialization** with `getrusage`, including baseline/runtime, not isolated terminal heap. Every run parsed 200,134,656 bytes at 80×24; chunk 182,272 bytes. No GUI or PTY I/O in throughput timing.

| Engine | Builds? | MB/s (range) | RSS MiB | Retained history | Replies API | Reattach/snapshot | Threading | License | Last crate release; latest repository commit observed |
|---|---|---:|---:|---|---|---|---|---|---|
| alacritty_terminal 0.26.0 | Yes, native Rust | 181.576 (179.612–188.430) | 23.469 | exactly 10,000 lines | `EventListener`, `Event::PtyWrite(String)` | Grid/Cell serde works; **not complete Term snapshot** | `Term<Events>: Send + Sync` compile-asserted | Apache-2.0 | 2026-04-06; 2026-10-05 |
| libghostty-vt 0.2.2 | Yes with Zig/SDK workaround; **no on installed Zig 0.16** | 68.305 (68.073–68.551) | 4.406 | **787 mixed / 1,065 plain lines**, despite option 10,000; native option is byte budget, see below | `Terminal::on_pty_write` callback | RenderState + VT Formatter; **no binary Snapshot API in this pin** | `Terminal: !Send + !Sync` compile-asserted | Bindings MIT OR Apache-2.0; native Ghostty MIT | 2026-09-28; bindings 2026-10-09; native pin 2026-07-11 |
| vt100 0.16.2 baseline | Yes, native Rust | 208.505 (207.371–208.682) | 28.969 | exactly 10,000 lines | **No generated DA/DSR replies**; only unhandled-CSI callbacks | `state_formatted`, `state_diff`; incomplete terminal/parser state | `Parser<VCallbacks>: Send + Sync` compile-asserted | MIT | 2025-07-12; 2025-07-12 |

Ghostty memory/performance is **not a like-for-like 10k retained-line comparison**: the requested 10k option yielded a much smaller bounded history. It stayed below 10k, but this API does not establish the desired exact line-cap contract. No rustix-only PTY or wezterm-term comparison was attempted; vt100 was the requested alternative baseline.

## Evidence: versions, revisions, commands, timings
- Observed environment: macOS **26.6.2**, build **25G83**, arm64; `rustc 1.98.0 (88d9e12ae 2026-08-18)`; `cargo 1.98.0 (797e8a9bc 2026-08-05)`; installed `zig version` → `0.16.0`.
- `cargo search alacritty_terminal --limit 1`, corresponding searches for libghostty-vt/vt100/portable-pty, then `cargo info` for all four and libghostty-vt-sys. Resolved latest versions above; rust-version: Alacritty 1.85, Ghostty bindings 1.90, vt100 1.70.
- Published source revisions (`.cargo_vcs_info.json`): Alacritty `94e7c8874e526b1e67b349d9ba30ddf81669119e`; Ghostty bindings `c9876b2902d5bb3e1f3d61b6395059f1e5e7b721`; vt100 `eb66ffaf7d771c13303ef73b29f6f2a56fdacecf`; portable-pty `f8921727a11b9f8b073e8c24821d72fd41283500`.
- libghostty-vt-sys fetches Ghostty **`a887df42c56f6de86c0fe6da9c4eeca37931e083`**, dated 2026-07-11. Cargo.lock is retained; native build defaults to ReleaseFast, baseline CPU, default kitty-graphics enabled.
- `/usr/bin/time -l cargo build --release --features ghost`: **22.74s**, exit 101 on installed Zig. Exact diagnostic: `src/build/zig.zig:13:9: error: Your Zig version v0.16.0 does not meet the required build version of v0.15.2`; also `build.zig:27:62: error: member function expected 4 argument(s), found 3`.
- `curl -fL https://ziglang.org/download/0.15.2/zig-aarch64-macos-0.15.2.tar.xz -o zig-0.15.2.tar.xz && tar -xf zig-0.15.2.tar.xz && ./zig-aarch64-macos-0.15.2/zig version`: **12.53s**, `0.15.2`; archive SHA256 `3cc2bab367e185cdfb27501c4b30b1b0653c28d9f73df8dc91488e66ece5fa6b`.
- Zig 0.15.2 with Xcode's macOS 27 SDK: **16.56s**, exit 101. Exact errors include `error: undefined symbol: __availability_version_check`, `error: undefined symbol: _abort`, `error: undefined symbol: _arc4random_buf`, `error: undefined symbol: _bzero`; native build panics `zig build failed with status exit status: 2`.
- Adding `DEVELOPER_DIR=/Library/Developer/CommandLineTools` still failed, **5.42s**, same undefined symbols with current CLT SDK. A first local xcrun wrapper matched the wrong argument shape and likewise failed, **13.60s**; Zig actually invokes `xcrun --sdk macosx --show-sdk-path`.
- Working workaround: `tools/xcrun` returns `/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk` for `--show-sdk-path` (first or third argument), otherwise execs `/usr/bin/xcrun "$@"`; no global SDK/toolchain changes. Older SDK's `libSystem.tbd` explicitly includes `arm64-macos`.
- Working native command: `env PATH=/tmp/heeler-spikes/term/tools:/tmp/heeler-spikes/term/zig-aarch64-macos-0.15.2:/opt/homebrew/bin:/usr/bin:/bin:/Users/workie/.cargo/bin /usr/bin/time -l cargo build --release --features ghost`. Native libraries compiled in **147.26s**; harness initially failed `error[E0308]: mismatched types` / `expected u32, found u16` for PointCoordinate.y; fixed via `u32::from(r)`. Next complete build **1.43s**, exit 0. Final render-probe rebuild **1.412s**, exit 0.
- Rust-only `cargo build --release`: **1.94s** incremental, exit 0. Bench commands, each repeated three times: `target/release/heeler-term-spike alacritty bench`, `... vt100 bench`, `... ghost bench`.
- Parse seconds: Alacritty **1.102208 / 1.114259 / 1.062115**; vt100 **0.959042 / 0.959854 / 0.965106**; Ghostty **2.930000 / 2.939988 / 2.919481**. Exact logs: `*-bench-{1,2,3}.log`.
- RSS bytes: Alacritty **24,608,768 / 24,608,768 / 24,625,152**; vt100 **30,375,936 / 30,392,320 / 30,375,936**; Ghostty **4,603,904 / 4,620,288 / 4,620,288**. Initial vt100 diagnostic callback accumulated unhandled strings, inflating RSS to 393,248,768; disabled collection for performance runs above, retaining collection only for the small corpus.

### Corpus and replies
Commands: `target/release/heeler-term-spike {alacritty,vt100,ghost} corpus`; all exit 0. Logs `*-corpus.log` contain every case's text, selected cells, mode/cursor, history, replies, snapshot size and restore results.
- Corpus: 20,000-line plain flood; SGR indexed 196, RGB foreground (1,2,3), background (4,5,6); 1049 alternate enter/exit; CUP row 5/col 10; rows 2–4 scroll region with TOP/BOTTOM outside; `界😀é 👩‍💻`; OSC8 URL; OSC133 A/B/C/D; 2026 open and closed; bracketed-paste mode query, kitty keyboard set/query, primary/secondary DA, DSR 5/6, DECRQM 2026.
- All three kept text, colors, alternate enter/exit, cursor addressing and scroll-region boundaries in the recorded cases. Wide cells/continuations and combining text were retained; all reported cursor col **10** for the Unicode example. Emoji grapheme width/rendering correctness beyond this corpus is **[UNVERIFIED]**.
- Alacritty + Ghostty retain OSC8 hyperlinks in cells. Ghostty reports OSC133 row `Prompt`; Alacritty has no observed prompt metadata/API (source contains no OSC133 handling). vt100 reports OSC8/133 as unhandled.
- Alacritty 2026-open keeps `SYNC` out of the canonical grid with `sync_pending=4`; closed commits it. Its parser exposes `sync_timeout()` / `stop_sync()`; stock event loop uses a **150ms** deadline. A custom daemon must preserve that timeout integration.
- Ghostty parses `SYNC` into terminal state immediately; frontend/render scheduling semantics during an open synchronized batch were **[UNVERIFIED]**. vt100 treats 2026 as unhandled and shows text immediately.
- Alacritty requires **`Config { kitty_keyboard: true, ... }`** (default false). Verified replies: `ESC[?2004;1$y`, `ESC[?1u`, `ESC[?6c`, `ESC[>0;2600;1c`, `ESC[0n`, `ESC[1;1R`, `ESC[?2026;2$y`.
- Ghostty callback verified: `ESC[?2004;1$y`, `ESC[?1u`, `ESC[?62;22c`, `ESC[>1;0;0c`, `ESC[0n`, `ESC[1;1R`, `ESC[?2026;2$y`. vt100 generates none; DA/DSR/kitty queries are unhandled callbacks, not a writeback implementation.

### Snapshot / reattach findings
- Alacritty Grid/Cell serialize + deserialize succeeded. **Both current and saved cursor are `serde(skip)`**, Term itself is not serializable. Snapshot must explicitly carry render cursor/shape/visibility, palette/default colors, viewport/history, cell flags, wide/combining content, hyperlinks, dimensions, modes as needed; grid serde alone is insufficient. Full mixed-output grid JSON was **76,085,261 bytes** including history/storage; do not use that as an IPC wire format.
- vt100 `state_formatted()` restored visible text and paste/cursor state in every ordinary corpus case; mixed snapshot **991 bytes** excludes history. It loses the primary screen when reattaching in active alternate-screen mode, and unfinished `ESC[31` parser state: after appending `mX`, source prints red X but restored parser prints mX. Both probes record `after_equal=false`.
- Ghostty `RenderState::update` worked: `cols=Ok(80) rows=Ok(24) dirty=Ok(Full)`; row/cell iteration **24 rows / 1,920 cells**, including graphemes. These borrowed/opaque !Send snapshots still need conversion into owned IPC DTOs.
- Ghostty VT Formatter with all exposed extras restored text and keyboard flags in corpus; full restored style/hyperlink equality is **[UNVERIFIED]**. **Cursor incorrectly ends at column 72** with `with_tabstops(true)`; zmx explicitly disables that option because it moves the cursor after CUP. Mixed VT snapshot **42,587 bytes** (only smaller native history). Active-alt and unfinished-CSI restoration both fail after further updates. VT formatting is not complete state serialization.
- Today's [Terminal Snapshot docs](https://libghostty.tip.ghostty.org/group__snapshot.html) describe binary CRC-protected READY/FINISH records, unfinished parser continuation, and incremental history restore; **no `ghostty_snapshot_*` declarations exist in the 0.2.2 sys bindings or its pinned native headers**. Availability in a later compatible released Rust binding is **[UNVERIFIED]**.
- Inspected zmx commit **`2d23c0d440583736f365a55aa09cb2c82b0fe7fa`**, 2026-10-03; native Ghostty pin `8af6897c0afc63037a8a3efee4162a380e3a4572`. [zmx util.zig](https://github.com/neurosnap/zmx/blob/2d23c0d440583736f365a55aa09cb2c82b0fe7fa/src/util.zig#L790-L906) uses the **Zig VT formatter**, two-phase history + viewport, suppresses synchronized-output restoration, disables tabstop restore, emits title separately. It is reattach precedent, **not evidence that crates.io Rust 0.2.2 has the new binary API**. zmx itself was not executed.

## PTY findings
- Command: `target/release/heeler-term-spike alacritty pty`, final exit 0; `alacritty-pty.log` + `zsh-pty.raw` + `vim-pty.raw` retained. `/bin/zsh -i`: zsh **5.9**; `/usr/bin/vim -u NONE -n`: vim **9.1**, patches **1–1752**, compiled 2026-07-31. TERM=xterm-256color, initial 80×24, then 120×40.
- Shell received commands, displayed `SHELL-READY`, `24 80`, `/dev/ttys...`; resize caused actual trap output **`WINCH-SEEN` + `40 120`**, proving SIGWINCH delivery. Vim accepted `iHEELER-VIM-GRID ESC`; engine grid contained inserted text, tilde rows and ALT_SCREEN, then redrew after resize (**4,884 output bytes**). Sent `:q! CR`; shell received `exit CR`.
- Final zsh PID **57475**, vim PID **57529**; each had SID=PGID=foreground tty PGID=PID, consistent with real session/controlling tty. Each exited code **0**, signal None; `try_wait` then `wait` succeeded, subsequent libc `waitpid` returned **-1 / ECHILD (os error 10)**. Timings **1.758s / 1.735s** include harness drain waits.
- Master fd **3**, `FD_CLOEXEC=true`; parent open-fd count **3 before / 3 after** each complete teardown (fcntl scan 0–1023). Source `portable-pty/src/unix.rs` uses `openpty`, then **fcntl FD_CLOEXEC on master + slave**, `setsid`, `TIOCSCTTY`, closes child fds ≥3 via `/dev/fd`, and `TIOCSWINSZ` resize. This is **not atomic `O_CLOEXEC` acquisition**: concurrent unrelated fork/exec between openpty and fcntl is a risk. Slave flag/child extraneous-fd absence was source-reviewed, not separately runtime enumerated.
- Drop the daemon's slave handle immediately after spawn; retain master/read/write handles until reader shutdown and reap explicitly. Crate APIs are blocking; Tokio bridge needs nonblocking raw-fd/AsyncFd or dedicated blocking readers, with replies routed back to the same PTY. Async cancellation/backpressure behavior was **[UNVERIFIED]**.

## Exact dependency lines that worked
```toml
alacritty_terminal = "=0.26.0"
vt100 = "=0.16.2"
portable-pty = "=0.9.0"
libghostty-vt = { version = "=0.2.2", optional = true }
libc = "0.2"
serde_json = "1"
static_assertions = "1"
parking_lot = "0.12"
# [features]
# ghost = ["dep:libghostty-vt"]
```
Cargo.lock records transitive versions; harness `src/main.rs` contains no GUI dependency. Source revisions, metadata JSON, corpus/throughput/PTY logs and SDK wrapper are all in this spike directory.

## Pitfalls / main risks
- **Alacritty:** own compact render snapshot/delta protocol required; OSC133 prompt tracking requires an explicit integration decision; kitty keyboard opt-in; synchronized-output deadlines must be serviced; Unicode grapheme-width expectations need conformance tests. Grid serde is a test representation, not a production reattach contract.
- **Ghostty:** Zig 0.15.2/SDK packaging; pinned native ABI/API drift; !Send/!Sync implies thread-affine owner (OS thread or Tokio LocalSet, never freely migrating tokio::spawn state); C/Rust docs misleadingly call max_scrollback “lines” while it passes through to PageList **maximum bytes**. Source chain: c/terminal.zig → Terminal.zig → Screen.zig → PageList.init; page-level minimums also affect retention. Binary snapshot doc/crate mismatch and VT restore bugs dominate adoption risk.
- **vt100:** smallest integration surface, but no engine-generated replies, no OSC8/133/2026/kitty support; partial snapshots/primary-screen loss make it an unsuitable canonical engine for the specified feature set despite benchmark speed.
- **portable-pty:** blocking API and non-atomic CLOEXEC need deliberate daemon integration. Latest repo activity does not prove active maintenance of the PTY subcrate specifically.

## Open questions
- Which owned snapshot/cell-delta schema and ordering epoch will Heeler use; do clients need only viewport plus lazy history, or immediate full 10k history? Transport/render costs are **[UNVERIFIED]** here.
- Can a newer released Ghostty binding expose binary READY snapshots and correct line-bound retention without custom FFI/fork? Current pin cannot establish that contract.
- Multi-session steady RSS, fair same-history Ghostty benchmark, snapshot encoding latency, grapheme-mode 2027, resize races, crash restart, shell prompt metadata policy and hostile/malformed-output fuzzing remain **[UNVERIFIED]**.
- Release/maintenance provenance: [Alacritty crate](https://crates.io/crates/alacritty_terminal), [Ghostty bindings](https://crates.io/crates/libghostty-vt), [vt100](https://crates.io/crates/vt100), [portable-pty](https://crates.io/crates/portable-pty); crates.io API release timestamps and GitHub latest-commit responses retained locally.
