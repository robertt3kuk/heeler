# Quality gate and dev entry points. `just check` must be green before every merge.

default: check

# Full gate: format, lint, tests, licenses/advisories.
check: fmt-check clippy test deny

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

test:
    cargo build --workspace --bins --locked
    cargo nextest run --workspace --locked --no-tests=pass

deny:
    cargo deny --locked check

# Rewrite protocol golden fixtures after an intentional wire change (bump protocol version first).
update-golden:
    UPDATE_GOLDEN=1 cargo nextest run -p heeler-protocol --locked

# Measure the performance budgets B1-B6 (docs/perf.md); not part of the gate. Extra args: `just bench --only B1,B4`.
bench *args:
    cargo run -p xtask --release --locked -- bench {{args}}
