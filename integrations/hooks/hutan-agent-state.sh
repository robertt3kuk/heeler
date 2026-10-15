#!/bin/sh
# hutan-managed: agent-state hook for Codex and Claude Code.
# Installed by `hutan integration install`; reinstalling overwrites this file, so put custom
# hooks beside it instead of editing it.
#
# Usage: hutan-agent-state.sh <codex|claude>      (the hook's JSON arrives on stdin)
#
# Reports the agent's state to the Hutan session it runs in (`hutan agent hook`). Outside a
# Hutan session, or when the CLI is missing, it does nothing and says nothing: a hook must
# never disturb the agent that runs it.

agent="${1:-}"

drain() {
  cat >/dev/null 2>&1 || :
}

if [ "${HUTAN_ENV:-}" != "1" ] || [ -z "${HUTAN_SESSION_ID:-}" ]; then
  drain
  exit 0
fi

bin="${HUTAN_BIN:-hutan}"
if ! command -v "$bin" >/dev/null 2>&1; then
  drain
  exit 0
fi

"$bin" agent hook "$agent" 2>/dev/null || :
exit 0
