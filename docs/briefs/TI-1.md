# TI-1: real text input in every Hutan text field (IME, dictation, accents, emoji, paste)

Agent: implementer   Depends on: master (protocol 0.27)   Branch: slice/TI-1

## Context
Read AGENTS.md, `docs/spikes/2026-10-17-user-test.md` and `-2.md` (when present), `docs/design.md`, the V-1 vim layer (`crates/desktop/src/workbench/chat/vim.rs`, `vim_ui.rs`). Code: `crates/desktop/src/workbench/chat/input.rs` (`TextInput`), `chat/view.rs` (`on_key` → `type_text(keystroke.key_char)`), every other text field (palette/Jump query, launcher, prompt/modal inputs, settings sheet, sidebar filter/rename, history/resources filters, tasks/merge/attach forms — find `TextInput` and `on_key_down`), and `crates/terminal_view/src/element.rs` which already implements GPUI's `EntityInputHandler` + `window.handle_input(…, ElementInputHandler::new(…))`.

Findings: Hutan's own fields read `KeyDownEvent.keystroke.key_char` only. Text that does not come from a plain key press never arrives correctly: synthesized Unicode text (accessibility tools, automation, Cua Driver) degrades to the layout's letter for the key code ("ффф" on the Russian layout), and by the same path IME composition (Japanese/Chinese/Korean), macOS dictation, press-and-hold accents, the emoji & symbols picker and text services cannot work. ⌘A/⌘V do nothing in the Jump query. The terminal works because it uses the platform input handler.

## Scope
`crates/desktop` (a shared text-field element/handler), docs. No daemon/protocol change.

## Contract
- One reusable text-field implementation used by **every** Hutan text field: implements GPUI `EntityInputHandler` over `TextInput` (`text_for_range`, `selected_text_range`, `marked_text_range`, `unmark_text`, `replace_text_in_range`, `replace_and_mark_text_in_range`, `bounds_for_range`, `character_index_for_point`) and registers it with `window.handle_input` while the field is focused; UTF-16 ranges converted correctly (emoji, Cyrillic, CJK).
- Text insertion comes **only** from the input handler (`replace_text_in_range`); `on_key_down` keeps editing keys (arrows, backspace, enter, escape, tab, shortcuts) and vim Normal/Visual keys. No double insertion.
- Marked (composing) text is drawn underlined and replaced on commit; `bounds_for_range` returns the caret rectangle so the IME candidate window and the press-and-hold accent popup appear at the caret.
- Every field supports ⌘A select all, ⌘C/⌘X/⌘V (paste multi-line into multi-line fields, first line into single-line fields), ⌘Z/⌘⇧Z (V-1 undo), ⌥←/→ word moves, ⌘←/→ line ends, shift-selection, mouse click to place the caret and drag to select, double-click word.
- Vim (V-1): Insert mode goes through the input handler; Normal/Visual keys stay in `on_key_down` and the handler ignores text while not in Insert (no stray characters).
- R-1 composer paste (images/long text chips) keeps working: ⌘V still checks the clipboard for images/file URLs before inserting text.

## Acceptance
- GPUI tests: `replace_text_in_range` inserts Unicode (Cyrillic, CJK, emoji) at the caret in the composer, palette, launcher prompt and sidebar rename; marked text (`replace_and_mark_text_in_range` then `unmark_text`/commit) ends as committed text; UTF-16 ranges round-trip; ⌘A/⌘V in the Jump query; vim Normal ignores handler text; no double insertion on a plain key press.
- Manual (the integrator, with Cua Driver): `type_text` of Cyrillic into the palette and composer shows the text, not "ффф".
- `just check` green; committed on `slice/TI-1`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run `hutand`/`hutan-desktop` without it. No paid turns. No push, no hard reset. Do not edit docs/status.md or docs/roadmap.md. The user's `/Applications/Hutan.app`: never touch it.
