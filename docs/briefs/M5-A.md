# M5-A: model gateway in the daemon — providers, Keychain, direct chat sessions, CLI (roadmap M5.1–M5.3)

Agent: implementer   Depends on: M13-A merged (protocol 0.10; this slice takes **0.11**)   Branch: slice/M5-A

## Context
Read `AGENTS.md`, ADR 0008 (credentials — normative: secrets only in Keychain via the daemon, never echoed/logged; subscription tokens are never reused), ADR 0005 (`AgentEvent`), ADR 0006, `docs/agents.md` (M4-A chat sessions, journal, `chat.*` protocol), `docs/roadmap.md` § M5, `crates/agents`, `crates/daemon` chat manager.

## Scope (contract changes authorized)
New crate `crates/gateway` (`staffy-gateway`), `crates/daemon`, `crates/storage` (migration for providers), `crates/protocol` (0.11, capability `chat.direct`, `provider.*`), `crates/client`, `crates/cli`, `docs/protocol.md`, `docs/gateway.md` (new). No desktop UI (M5-B reuses the M4-B chat view).

## Contract
- **Provider trait** in `staffy-gateway`: `list_models()`, `stream_chat(request) -> Stream<GatewayEvent>` with messages (system/user/assistant), model, temperature/max_tokens optional, cancellation; events map onto the existing `AgentEvent` variants (message start/delta/end, usage with optional fields, turn completed/failed/cancelled) so direct chats reuse the M4 journal, protocol events and the upcoming chat view.
- **Adapters**: OpenAI-compatible (`/v1/chat/completions` SSE + `/v1/models`; covers OpenAI, OpenRouter, Groq, LM Studio, vLLM…), Anthropic Messages (`/v1/messages` SSE + `/v1/models`), Ollama (native `/api/chat` NDJSON + `/api/tags`, or its OpenAI-compatible endpoint — pick one, document why). HTTP via `reqwest` (rustls) — check cargo-deny; timeouts, retry only on connect errors before any byte streamed, clear error mapping (401/403/404 model/429 with retry-after/5xx).
- **Local discovery**: probe `127.0.0.1:11434` (Ollama) and `127.0.0.1:1234` (LM Studio) on request (`provider.discover`), never in the background on a timer.
- **Credentials (ADR 0008)**: Keychain via `security-framework` (generic password, service `dev.staffy.provider`, account = provider id); SQLite `providers` table stores `{id, kind, name, base_url, default_model, keychain_ref, created_at}` only. `provider.set_credential { provider_id, secret }` writes Keychain, never logs/echoes; `provider.list` returns `has_credential: bool`. **Tests never touch the real login keychain**: put the Keychain access behind a trait; the default gate uses an in-memory store; one `#[ignore]` test exercises a temporary keychain file created with `security create-keychain` in a temp dir (deleted after) — run it once manually and report.
- **Direct chat sessions**: `chat.create { agent: "direct", provider_id, model, system_prompt? }` → `SessionKind::Harness` (as M4-A) with `SessionInfo.chat.agent = "direct"`; `chat.send` streams; `chat.set_model { session_id, provider_id?, model }` switches mid-conversation (history kept; next turn uses the new model; journal records the switch); `chat.interrupt` cancels the HTTP stream. Conversation context = the session's journal (user/assistant text only; truncate oldest to a setting `gateway.max_context_chars`, default 200 000, noted in the turn).
- **Protocol 0.11**: `provider.list`, `provider.add { kind, name, base_url?, default_model? }`, `provider.update`, `provider.remove`, `provider.set_credential`, `provider.clear_credential`, `provider.models { provider_id }`, `provider.discover`, `chat.set_model`; golden fixtures (no secret ever appears in any response fixture).
- **CLI**: `staffy provider add openai|anthropic|ollama|openai-compatible [--name --base-url --model]`, `staffy provider key <id>` (reads the secret from stdin without echo / `--from-env VAR`), `staffy provider ls|models|rm|discover`, `staffy chat new --direct --provider <id> --model <m>`, `staffy chat model <session> <model> [--provider]`.
- Redaction: request/response logging never includes `Authorization`/`x-api-key` headers or bodies at info level.

## Acceptance
- Adapter tests against local mock HTTP servers (SSE/NDJSON recorded shapes, written by hand from the providers' public docs): streaming deltas, usage, error mapping, cancellation mid-stream, connect-retry only before first byte.
- Daemon tests: provider CRUD persisted across restart, credential stored via the in-memory keychain and never present in DB/logs/responses (assert by scanning), direct chat create → send → stream → journal → restart → history; model switch mid-conversation; interrupt.
- Manual: if Ollama or LM Studio is running locally use it for one real chat; otherwise say none was available (do not install anything). No real paid API calls.
- `just check` green (cargo-deny passes with any new deps); committed on `slice/M5-A`.

## Rules
Inline `STAFFY_HOME=<temp dir>`; never touch `/tmp/hh-*`, the real Staffy home, or the user's login keychain. No push, no hard reset; `git grep -n '^<<<<<<<'` empty after any rebase.
