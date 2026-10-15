# M2-C: `terminal_view` — GPUI terminal element, input encoding, selection, scrollback (roadmap M2.3)

Agent: implementer   Depends on: master   Branch: slice/M2-C

## Context
Read `AGENTS.md`, ADR 0003 (GPUI sourcing: pinned zed rev, `runtime_shaders` — the Metal compiler is not installed), ADR 0004 (daemon is the only VT parser; clients draw `TerminalSnapshot`/`TerminalFrame`), ADR 0010, `docs/design.md` (§ Visual system, § Settings `[terminal]`, § Keyboard defaults), `docs/protocol.md` (§ Terminal render types and all semantics notes: trimmed rows, wide chars, link table, credit-1 acks, history paging), `crates/protocol/src/render.rs`, `crates/cli/src/render.rs` (host-terminal renderer: shows how DTOs map to cells), `docs/spikes/2026-10-13-gpui.md`.

## Scope
New crate `crates/terminal_view` (package `staffy-terminal-view`, library + one example). Depends on `staffy-protocol`, `gpui`, `gpui_platform`; `staffy-client` only as a **dev-dependency** for the example. Do not edit other crates. You may extend `deny.toml` license allow-list with permissive licenses or `MPL-2.0` needed by GPUI; anything GPL/LGPL/AGPL → stop and report.

## Contract
The view is passive: an owner feeds it updates and implements a backend.

```rust
pub trait TerminalBackend: 'static {
    fn input(&self, bytes: Vec<u8>);
    fn resize(&self, size: TerminalSize);
    fn ack(&self, frame: u64);
    fn request_history(&self, from_line: u32, count: u32);   // answer arrives via TerminalView::apply_history
    fn set_default_colors(&self, colors: DefaultColors);      // fg/bg/cursor; wired to the protocol later
}

pub struct TerminalView { /* gpui Entity, Focusable, implements Render */ }
impl TerminalView {
    pub fn new(backend: Rc<dyn TerminalBackend>, settings: TerminalViewSettings, window: &mut Window, cx: &mut Context<Self>) -> Self;
    pub fn apply_render(&mut self, render: TerminalRender, cx: &mut Context<Self>);
    pub fn apply_history(&mut self, from_line: u32, rows: Vec<Row>, cx: &mut Context<Self>);
    pub fn set_status(&mut self, status: ViewStatus, cx: &mut Context<Self>); // Live | Reconnecting | Ended(String)
    pub fn update_settings(&mut self, settings: TerminalViewSettings, cx: &mut Context<Self>);
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalViewSettings {   // plain config; wave 2 maps the `terminal` settings namespace (settings crate) onto it
    pub font_family: String, pub font_size: f32, pub line_height: f32,
    pub palette: TerminalPalette,      // resolved colors: 16 ANSI + fg/bg/cursor/selection; Default = staffy-dark (bg #1e1e1e fg #d4d4d4 cursor #d4d4d4)
    pub cursor: CursorSettings, pub option_as_meta: OptionAsMeta, pub copy_on_select: bool,
    pub scroll_to_bottom_on_input: bool,
}
impl Default for TerminalViewSettings { /* Menlo 13, line height 1.2, staffy-dark, block blinking cursor, option_as_meta = Left */ }
```
Requirements:
- **Model** (GPUI-free module): applies snapshots and frames (incl. `scrolled`, `modes`, `title: Option<Option<_>>`, links) to a local grid; holds scrollback cache filled from `apply_history`.
- **Ack pacing**: ack frame N only after it has been painted (next-frame callback), so floods are paced by the display.
- **Rendering**: custom element; backgrounds merged per run; text shaped per row and cached (key: row content + font + theme); glyphs aligned to the cell grid (column x = col × cell width; wide chars occupy 2 cells; font fallback must not shift later columns); bold/italic/dim/inverse/hidden/strikethrough, underline styles incl. undercurl and underline color; cursor block/bar/underline/hollow (hollow when unfocused), blink per settings; OSC 8 links underlined on hover, ⌘-click opens via the OS.
- **Size**: cols/rows from bounds and font metrics (cell width = advance of `M`, height = font size × line height); report via `backend.resize` (debounced ~50 ms) with pixel size.
- **Input encoding** (GPUI-free module, own `KeyInput` type so it is unit-testable): legacy xterm encoding (app cursor/keypad modes, modifiers `CSI 1;m X`, ctrl-chars, alt/option-as-meta = ESC prefix) and kitty keyboard protocol when `modes.kitty_keyboard` flags are set (implement flags 1 disambiguate and 8 report-all-keys; others encode as if unset). Text input goes through GPUI's IME input handler (`EntityInputHandler`) so composition and non-Latin layouts (Russian) work; marked text drawn inline at the cursor.
- **Mouse**: SGR (1006) and legacy encodings for click/drag/motion modes; wheel → arrow keys in alt screen with `alternate_scroll`, else scrollback viewing (pages of 200 via `request_history`), shift bypasses app mouse capture for selection.
- **Selection & clipboard**: drag / double-click word / triple-click line, across scrollback; ⌘C copies (never sent to PTY); ⌘V pastes with bracketed-paste wrapping when the mode is on (strip embedded `ESC[201~`); `copy_on_select`.
- **Theme**: map `Color::Default/Indexed/Rgb` with palette overrides from the snapshot; call `set_default_colors` on creation and theme change.
- **Example** `cargo run -p staffy-terminal-view --example attach -- [--socket PATH] <terminal_id>`: opens one window, connects with `staffy_client::Client` on a Tokio thread, attaches, renders, sends input/resize/acks. Used for manual verification.

## Acceptance
- Unit tests: model (snapshot, frames, scroll, title clear, links, history merge), key encoding (xterm vectors for arrows/F-keys/ctrl/alt/app-cursor; kitty flag 1/8 vectors), mouse encoding (SGR press/release/drag/wheel, legacy), bracketed paste sanitizing, selection extraction incl. wide chars.
- GPUI test (`TestAppContext`/`VisualTestContext`): view lays out, computes cols/rows for a given bounds/font, applies a snapshot, and acks after a frame.
- Manual: with `staffyd` running and a session, the example shows `vim` and `htop`-like TUIs correctly and accepts typing. **Do not take screenshots or trigger macOS permission prompts**; describe what you observed and the commands; the orchestrator verifies visually.
- `just check` green.

## Out of scope
Workbench, panes, docks, settings files (only the serde struct), reconnect handling (owner's job), chat.

## Report
Summary, files changed, commands run with results, deviations, open questions (especially GPUI API gaps). Commit on `slice/M2-C` (scope `terminal_view`).
