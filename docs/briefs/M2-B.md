# M2-B: `desktop` workbench core — native window, docks, pane tree, panels, palette, presets (roadmap M2.1)

Agent: implementer   Depends on: master   Branch: slice/M2-B

## Context
Read `AGENTS.md`, ADR 0010 (binding model), `docs/design.md` (layout model, default layout mockup, presets, moving things, keyboard defaults, density tokens, visual system), ADR 0003 + `docs/spikes/2026-10-13-gpui.md` (pinned GPUI rev, `runtime_shaders`, bootstrap `gpui_platform::application().run(...)`).

## Scope
New crate `crates/desktop` (package `heeler-desktop`, binary `heeler-desktop`). Depends on `gpui`, `gpui_platform`, serde. **No** dependency on `client`, `settings`, or `terminal_view` in this slice (they are built concurrently; wave 2 wires them). You may extend `deny.toml` with permissive licenses or `MPL-2.0` needed by GPUI; GPL/LGPL/AGPL → stop and report.

## Contract

### Layout model (GPUI-free module `workbench::model`, fully unit-tested)
```rust
pub struct WorkbenchState { pub docks: Docks /* left, right, bottom */, pub center: PaneTree, pub zoomed: Option<PaneId>, pub focus: FocusTarget }
pub struct Dock { pub panels: Vec<PanelId>, pub active: Option<PanelId>, pub size: f32, pub visible: bool }
pub enum PaneTree { Split { axis: Axis, ratios: Vec<f32>, children: Vec<PaneTree> }, Pane(Pane) }
pub struct Pane { pub id: PaneId, pub items: Vec<ItemRef>, pub active: usize }
pub enum ItemRef { Terminal { session: String }, Chat { session: String }, Panel { panel: PanelId }, Placeholder { title: String } }
pub struct LayoutPreset { /* docks + optional center override, e.g. overview = Panel(overview) only */ }
```
Operations (pure, return new state or mutate with invariants): split pane (axis, side), move item (to pane index / to new split at edge / to dock as panel when it is a panel), move panel between docks or into the center, close item (empty panes collapse; single-child splits flatten; ratios renormalize), resize split / dock, zoom/unzoom, focus neighbor (geometric), apply preset (keeps all open items: items not placed by the preset go to the first pane), serialize/deserialize (serde JSON, versioned `{ "version": 1, ... }`; unknown version → default layout + warning).

### GPUI shell
- Window: transparent titlebar with inset traffic lights, title shows the active item; blurred/translucent background behind the left dock if GPUI supports it (`WindowBackgroundAppearance::Blurred` or equivalent) — verify and report; opaque fallback otherwise. App menu (About, Settings… ⌘, (no-op placeholder), Quit), View menu (dock toggles, presets, Overview), Window menu.
- Docks render panel stacks with a tab per panel, drag-resizable edge, collapse; panel tab context menu: Move to Left/Right/Bottom dock, Open as tab, Close.
- Center renders the pane tree: splitters (drag to resize), per-pane tab strip (close ×, middle-click close, reorder by drag), drop zones when dragging a tab or panel (edges = split, tab strip = insert), zoom (double-click tab), focus ring on the active pane.
- `PanelRegistry` (id → factory, title, icon, default dock) and `StatusItemRegistry`; built-in **placeholder** panels for this slice: `sessions`, `inspector`, `overview`, `notifications` (each renders its name + a short description); status items: `connection` ("—"), `density`, `palette_hint` ("⌘K").
- Actions: named GPUI actions for everything in design.md § Moving things and § Keyboard defaults (`workbench::SplitRight`, `ToggleLeftDock`, `ApplyPreset(name)`, `ToggleOverview`, `ClosePaneItem`, `FocusNeighbor(dir)`, …); a default keymap table in code (wave 2 replaces it with `keymap.toml`). ⌘K palette: fuzzy filter over all actions + their key hints; enter runs.
- Presets built-in: `default`, `focus`, `overview` (design.md table); ⌘⇧O toggles overview and back to the previous preset.
- Density tokens from design.md applied via a `Density` value (compact default) that can be switched at runtime (View menu) — wave 2 binds it to settings.
- Layout state persisted to `$HEELER_HOME/desktop-state.json` (else `~/Library/Application Support/Heeler/desktop-state.json`) on change (debounced) and restored on launch; missing/corrupt → default preset with a log line.

## Acceptance
- Unit tests for every model operation incl. invariants (no empty panes, ratios sum to 1, item conservation across preset switches, serialization roundtrip, unknown version fallback).
- GPUI tests (`TestAppContext`) for: dispatching actions changes state (split, toggle dock, apply preset, close last item), palette filtering, state save/restore through a temp `HEELER_HOME`.
- Manual run `HEELER_HOME=/tmp/hh-m2b cargo run -p heeler-desktop`: window opens without panics, actions via keyboard work. **Do not take screenshots or trigger macOS permission prompts**; describe observations; the orchestrator verifies visually.
- `just check` green.

## Out of scope
Real terminals/chat, daemon connection, settings files, settings UI, overview content.

## Report
Summary, files changed, commands run with results, GPUI API findings (transparent titlebar, blur, drag & drop, menus), deviations, open questions. Commit on `slice/M2-B` (scope `desktop`).
