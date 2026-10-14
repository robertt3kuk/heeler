# M5-B: direct model chat in the desktop — providers UI, model picker, chat view reuse (roadmap M5.4)

Agent: implementer   Depends on: M5-A merged (protocol 0.11 `provider.*`, `chat.create {agent: "direct"}`, `chat.set_model`), M4-B merged (chat view), M13-B merged   Branch: slice/M5-B

## Context
Read `AGENTS.md`, ADR 0008 (credentials — the desktop never stores or logs a secret; it sends it once via `provider.set_credential`), `docs/gateway.md`, `docs/agents.md` § chat view, `docs/design.md` (settings sheet, palette, chat surfaces), `crates/desktop/src/workbench/{agents.rs,chat/}`, settings sheet code.

## Scope
`crates/desktop`, small additive `crates/client` helpers. No daemon/protocol changes (report gaps).

## Behavior
- **Providers settings page** (settings sheet section "Models"): list from `provider.list` (name, kind, base URL, default model, key status ✓/—), Add (kind picker: OpenAI, Anthropic, Ollama, OpenAI-compatible; name; base URL prefilled per kind; default model), Edit, Remove (confirm), **Set key…** (secure text field — masked, never rendered back, cleared from memory after send; paste allowed; sends `provider.set_credential` then shows ✓), Clear key, **Discover local** (`provider.discover` → offer to add Ollama/LM Studio found), per-provider **Models** list (`provider.models`) with refresh.
- **New chat picker** (⌘⇧T, from M4-B): agent list gains "Direct model" (enabled when at least one provider exists) → provider + model (searchable, from `provider.models`, default model preselected), optional system prompt; creates via `chat.create { agent: "direct", … }`.
- **Chat view reuse**: direct sessions render in the same chat item; header shows provider · model with a **model switcher** dropdown (`chat.set_model`; disabled while a turn runs; switch recorded inline as a small divider "Switched to <model>"); no approval UI (direct chats have no tools); usage footer shows the provider's usage fields when present.
- Errors from the gateway (401 → "Key rejected — Set key…" action; 404 model → "Model not found — pick another"; 429 → retry-after text; network) shown inline on the failed turn with the right action.
- ⌘K: "Models: Manage Providers…", "Chat: New Direct Chat…", "Chat: Switch Model…".

## Acceptance
- GPUI tests with an in-process daemon using the in-memory credential store (never the login keychain — check how M5-A's daemon tests inject `MemoryStore` and use the same) and a local mock OpenAI-compatible HTTP server (reuse M5-A's mock): add provider → set key (assert the secret never appears in desktop state files, logs, or the view tree after submit) → models listed → new direct chat → streamed reply renders → switch model → next turn uses it (mock asserts the model) → 401 path shows the Set key action.
- Manual: desktop on a temp `HEELER_HOME` against the mock server; no screenshots (orchestrator verifies visually). A real `heelerd` reads the login keychain — do not run one with real providers.
- `just check` green; committed on `slice/M5-B`.

## Rules
Inline `HEELER_HOME=<temp dir>`; never touch `/tmp/hh-*`, the real Heeler home, or the login keychain. No push, no hard reset; `git grep -n '^<<<<<<<'` empty after any rebase; stop every daemon you start (`pgrep -x heelerd` clean before finishing).
