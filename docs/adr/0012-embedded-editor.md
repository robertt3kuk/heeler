# 0012. Embedded code editor (re-scopes the "no editor" non-goal)

- Status: accepted, option B (2026-10-17: the project is MIT OR Apache-2.0, so Zed's GPL-3.0 editor crates are out; Hutan builds its own editor on permissively licensed crates)
- Date: 2026-10-14
- Milestone: M12 (see roadmap)

## Context
The user wants Hutan to work as an editor too: open files next to terminals and agent sessions, LSP on demand, Vim mode, tree-sitter highlighting and structure, one theme across chrome, terminal, and code — "a mini Zed inside the ADE". `vision.md` previously listed "Code editor / LSP host" as a non-goal; this ADR re-scopes it.

Constraints:
- Zed's `editor`, `language`, `lsp`, `vim`, `project`, `workspace` crates are **GPL-3.0** (only `gpui`/`gpui_platform` are Apache-2.0) and depend on each other and on Zed's settings/project model; linking them makes Hutan GPL-3.0 and couples us to Zed's internals at the pinned rev.
- Warp's equivalents are AGPL-3.0 (ideas only).
- Permissive building blocks exist: `tree-sitter` + grammars (MIT), `ropey` (MIT), `lsp-types` (MIT), `async-lsp`/own JSON-RPC client.

## Options
| Option | Pros | Cons |
|---|---|---|
| A. Reuse Zed crates (`editor`, `language`, `lsp`, `vim`) | Fastest path to a very capable editor | Hutan becomes GPL-3.0; heavy dependency graph; tight coupling to Zed internals and its bump cadence |
| **B. Own editor on GPUI with permissive crates** (rope + tree-sitter + LSP client + own Vim layer) | License freedom; scoped to what an ADE needs; shares our theme/settings/panel model natively | More work; Vim/LSP completeness grows incrementally |
| C. External editor only (open in Zed/VS Code) | Zero cost | Not what the user wants |

## Decision: B (the license is MIT OR Apache-2.0)
- New UI crate `code_view` (GPUI) + non-UI crates `buffer` (rope, edits, undo, selections), `syntax` (tree-sitter: highlighting, folding, structural selection, indentation), `lsp_client` (process supervision, JSON-RPC, capabilities, diagnostics, completion, hover, go-to-definition, references, formatting, rename).
- LSP servers run **in the daemon** (`runtime` supervision, like agents) so agents and every client share diagnostics; started on demand per workspace + language, stopped when idle. Server config in settings (`languages.<lang>.lsp`).
- Editor items are workbench items (`ItemRef::File { path }`) — split next to terminals, movable like everything else; one `Theme` drives chrome, terminal palette, and syntax scopes.
- Vim mode is a setting (`editor.vim_mode`), implemented as a modal layer over editor actions (normal/insert/visual, motions, operators, text objects incl. tree-sitter ones, registers, `.` repeat, `:w`/`:q`).
- Files are read/written by the desktop client; agent edits arrive through the daemon and reload open buffers (conflict prompt when dirty).

## Consequences
- `vision.md` drops "Code editor / LSP host" from non-goals; the editor is a first-class surface.
- The license is decided (MIT OR Apache-2.0, 2026-10-17): no Zed editor crates; the V-1 vim layer (`crates/desktop/src/workbench/chat/vim.rs`) is the starting point for the editor's modal layer.
