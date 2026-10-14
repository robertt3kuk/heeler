# 0003. GPUI sourcing and UI component strategy

- Status: accepted
- Date: 2026-10-13
- Milestone: M0/M2

## Context
- crates.io `gpui` 0.2.2 (Oct 2025) predates the `gpui` / `gpui_platform` split; current upstream `gpui` cannot be published (unpublishable path deps), so standalone apps depend on zed git ([zed#55271](https://github.com/zed-industries/zed/discussions/55271)).
- Spike `docs/spikes/2026-10-13-gpui.md`: upstream zed rev `ef376006f32728b15e6a67706a04849cb781c812` builds and runs here (release build 62 s cold, stripped binary 6.3 MiB, idle RSS 75 MiB, 80×24 colored grid via 24 row text elements, key input works).
- Xcode here lacks the separately installed Metal compiler; `gpui_platform` feature `runtime_shaders` avoids it.
- `gpui-component` (now `longbridge/gpui-kit`, 0.7.3) pins `gpui-pre = "=0.3.9"` (zed `93d7456` snapshot) → **type identity conflict** with upstream git gpui (E0308).
- `gpui-standalone` 1.4.4 README bootstrap does not compile (E0599).
- `gpui-terminal` 0.1.0 targets old `gpui 0.2.2` + `alacritty_terminal 0.25.1` → not reusable directly.

## Options
| Option | Pros | Cons |
|---|---|---|
| **Upstream zed git, exact rev, own widgets** | Same code as Zed; one crate identity; full control | Manual bumps; no ready component kit |
| gpui-pre snapshot + gpui-component/kit | Rich widgets (tabs, dock, inputs) | Lags upstream; ties us to a third party's snapshot cadence |
| gpui-standalone / gpui-unofficial mirrors | crates.io-style deps | Third-party, already diverged API |

## Decision
- Depend on upstream zed git for `gpui` and `gpui_platform` at **one exact rev** in `[workspace.dependencies]`, `Cargo.lock` committed:

```toml
gpui = { git = "https://github.com/zed-industries/zed", rev = "ef376006f32728b15e6a67706a04849cb781c812", default-features = false }
gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "ef376006f32728b15e6a67706a04849cb781c812", default-features = false, features = ["font-kit", "runtime_shaders"] }
```

- Only UI crates may depend on GPUI: `desktop`, `terminal_view`, later `chat_view` and `notifier` (amended by ADR 0010).
- Build our own small widget set (tabs, splits, list, input, chat bubbles) on GPUI primitives; port specific components from Zed's `ui` crate where licensing (GPL/Apache per crate) allows — check per crate before copying.
- GPUI bumps are explicit slices (`chore(desktop): bump gpui to <rev>`) with the M2 demo re-run.

## Consequences
- No crates.io publish of `desktop` (fine: distributed as an app bundle).
- Terminal rendering is ours: custom GPUI element painting runs of cells (ADR 0004 supplies the grid).
- Revisit if Zed resumes publishing gpui or gpui-kit tracks upstream git.

## Evidence
`docs/spikes/2026-10-13-gpui.md`
