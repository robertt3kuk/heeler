# 0010. Desktop workbench: docks, pane tree, panels, layered settings

- Status: accepted
- Date: 2026-10-14
- Milestone: M2

## Context
The user wants full control of the layout: any component movable anywhere, every component configurable, layouts switchable (default / focus / overview). Design: `docs/design.md`. Constraints: GPUI only in UI crates (ADR 0003), daemon owns sessions (ADR 0001), presentation state belongs to the desktop.

## Options
| Option | Pros | Cons |
|---|---|---|
| Fixed layout with toggles | Fast to build | Contradicts the requirement |
| Free-floating windows/tiles | Maximum freedom | Hard to make keyboard-first and restorable; un-native |
| **Workbench: 3 docks + center split tree + panel registry + presets** (Zed/VS Code model) | Proven, keyboard-friendly, serializable, every piece swappable | More upfront work in M2 |

## Decision

### Model
- `Workbench` per window = left/right/bottom `Dock`s + center `PaneTree` + `StatusBar` + overlays.
- `Dock { panels: Vec<PanelId>, active, size, visible }`.
- `PaneTree = Split { axis, ratios, children } | Pane { items: Vec<ItemId>, active }`; arbitrary depth.
- Items: `Terminal(SessionId)`, `Chat(SessionId)` (M4), `Diff(..)` (M7), `Panel(PanelId)` (any panel opened in the center).
- Panels implement one trait and register in a `PanelRegistry`: id, title, icon, default dock, settings namespace (`panels.<id>`), render. Moving a panel = changing data, never code.
- Status bar = ordered item ids (left/right) from a `StatusItemRegistry`.
- Layout state (`desktop-state.json`, per window): docks, pane tree, open items, sizes. Presets live in settings (`layout.presets.<name>`); applying a preset rewrites the state; sessions are never closed by layout changes.

### Settings
- TOML files: user `settings.toml`, `keymap.toml` (app support dir), workspace `<repo>/.staffy/settings.toml`.
- Layers: defaults < user < workspace < runtime overrides; merged as values, then each namespace is deserialized into its owner's typed struct with `#[serde(default)]`; schema per namespace via `schemars` drives validation, docs, and the generated settings UI.
- Hot reload with a file watcher; invalid files keep the last good values and surface errors.
- Desktop-only. Settings that change daemon behavior (default shell, scrollback, env) become a separate daemon config + protocol later; the desktop never edits daemon files.

### Crates (amends ADR 0003 "only `desktop` links GPUI")
| Crate | GPUI | Role |
|---|---|---|
| `settings` | no | layered TOML store, schema registry, watcher, keymap parsing |
| `terminal_view` | yes | GPUI terminal element/view over render DTOs + input encoding |
| `desktop` | yes | app, workbench (docks, pane tree, registry, presets), panels, wiring |

Rule: GPUI only in UI crates (`desktop`, `terminal_view`, future `chat_view`); service crates, `client`, `settings`, `protocol`, `core` never link GPUI.

## Consequences
- M2 grows: workbench core, settings, panels are first-class slices (see roadmap).
- Every new UI feature ships as a panel/item/status item with a settings namespace.
- Keymap is data; actions are named (`workbench::SplitRight`, …) so the palette, menus, and keymap share one action list.
