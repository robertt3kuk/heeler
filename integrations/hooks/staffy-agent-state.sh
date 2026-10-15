#!/bin/sh
# staffy-managed: agent-state hook for Codex and Claude Code.
# Installed by `staffy integration install`; reinstalling overwrites this file, so put custom
# hooks beside it instead of editing it.
#
# Usage: staffy-agent-state.sh <codex|claude>      (the hook's JSON arrives on stdin)
#
# Reports the agent's state to the Staffy session it runs in (`staffy agent hook`). Outside a
# Staffy session, or when the CLI is missing, it does nothing and says nothing: a hook must
# never disturb the agent that runs it.

agent="${1:-}"

drain() {
  cat >/dev/null 2>&1 || :
}

if [ "${STAFFY_ENV:-}" != "1" ] || [ -z "${STAFFY_SESSION_ID:-}" ]; then
  drain
  exit 0
fi

bin="${STAFFY_BIN:-staffy}"
if ! command -v "$bin" >/dev/null 2>&1; then
  drain
  exit 0
fi

"$bin" agent hook "$agent" 2>/dev/null || :
exit 0
