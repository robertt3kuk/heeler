# UX-C: fixes from user test round 2 — Resources menus, narrow Changes, Start choices

Agent: implementer   Depends on: master `8c8ead1`+ (protocol 0.27)   Branch: slice/UX-C

## Context
Read AGENTS.md, `docs/spikes/2026-10-17-user-test-2.md` (**N5, N6, N7, N8** and the recovery-badge note in its top 10; screenshots in `/Users/workie/proj/hutan-usertest/scratch/` named in the report), `docs/design.md` (§ Resources, § Changes, § Start sheet), `docs/briefs/M13-D.md`, `UX-B.md`. Code: `workbench/panels/resources.rs` (+ `resources_model.rs`, context menus via `open_menu`), `workbench/changes*.rs`, `workbench/launcher.rs`/`launcher_ui.rs`, sidebar recovery chips (`panels/sessions.rs`). TI-1 (text input) runs at the same time in the shared text-field code — do not touch `chat/input.rs` or field key handling.

## Scope
`crates/desktop`, docs. No protocol change.

## Contract
- **N5**: the Resources process row's ⋯ button and right-click open the menu (Open Session, Interrupt/Terminate/Kill signals per M13-D) in the bottom dock and as a center tab; Kill (SIGKILL)… shows its confirmation; find why the menu does not show (event handling, menu anchor, popover dismissed by the row's own mouse-down) and fix it at the cause.
- **N6**: the Changes view adapts to narrow centers: below ~720 px the file list collapses into a dropdown/top strip, the diff gets the full width with horizontal scroll for long lines, and the scope card's Commit all… stays visible (wrap or move into the toolbar). Test at 900 px window with both docks visible.
- **N7**: the first-send confirmation's "Don't ask again for this model" is **unchecked** by default.
- **N8**: the Start sheet's Agent row keeps a fixed order and position for OMP / Claude Code / Codex / Direct; readiness actions (Install…, Log in…) appear **below** the row for the selected agent only, never between choices.
- **Recovery chips**: in large sidebars (30+ sessions) the "respawned/resumed/interrupted" chips collapse to a small icon with a tooltip, and the summary is one row ("12 sessions recovered — review"), so rows stay readable.

## Acceptance
- GPUI tests: Resources menu opens from ⋯ and right-click in both placements and Kill asks for confirmation (fake process tree/signal seam, no real kill); Changes at 900 px with docks shows the diff text and Commit all…; confirmation checkbox unchecked; agent choices keep their bounds when Claude is not installed; recovery chips compact above the threshold.
- `just check` green; committed on `slice/UX-C`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run `hutand`/`hutan-desktop` without it. No paid turns. No push, no hard reset. Do not edit docs/status.md or docs/roadmap.md. Never edit the main checkout (absolute worktree paths only). The user's `/Applications/Hutan.app`: never touch it.
