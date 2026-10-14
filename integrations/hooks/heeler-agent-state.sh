#!/bin/sh
# heeler-managed: agent-state hook for Codex and Claude Code.
# Installed by `heeler integration install`; reinstalling overwrites this file, so put custom
# hooks beside it instead of editing it.
#
# Usage: heeler-agent-state.sh <codex|claude>      (the hook's JSON arrives on stdin)
#
# Reports the agent's state to the Heeler session it runs in (`heeler agent hook`). Outside a
# Heeler session, or when the CLI is missing, it does nothing and says nothing: a hook must
# never disturb the agent that runs it.

agent="${1:-}"

drain() {
  cat >/dev/null 2>&1 || :
}

if [ "${HEELER_ENV:-}" != "1" ] || [ -z "${HEELER_SESSION_ID:-}" ]; then
  drain
  exit 0
fi

bin="${HEELER_BIN:-heeler}"
if ! command -v "$bin" >/dev/null 2>&1; then
  drain
  exit 0
fi

"$bin" agent hook "$agent" 2>/dev/null || :
exit 0
