-- Schema v5: model providers for direct chat (roadmap M5, protocol 0.11, ADR 0008).
--
-- Configuration only. The API key lives in the macOS Keychain (service `dev.hutan.provider`,
-- account = `id`) and never in this database: `keychain_ref` says where it is, nothing more.
CREATE TABLE providers (
    id            TEXT PRIMARY KEY NOT NULL,
    kind          TEXT NOT NULL,
    name          TEXT NOT NULL,
    base_url      TEXT NOT NULL,
    default_model TEXT,
    keychain_ref  TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT;
