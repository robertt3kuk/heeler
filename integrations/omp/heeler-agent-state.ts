// heeler-managed: OMP agent-state extension.
// Installed by `heeler integration install omp`; reinstalling overwrites this file, so put
// custom extensions beside it instead of editing it.
//
// Reports OMP's state (turn running, waiting for an approval or an answer, idle) and how to
// resume the conversation (`omp --resume=<id>`) to the Heeler session OMP runs in, by calling
// `heeler agent report`. Does nothing outside a Heeler session.
// @ts-nocheck

import { spawn } from "node:child_process";

const SOURCE = "omp-extension";
// A turn that ends and restarts within this time (an automatic retry) stays "working".
const IDLE_DEBOUNCE_MS = 400;
// Ids that are safe inside the resume command the daemon will run unattended.
const SAFE_ID = /^[A-Za-z0-9._:-]{1,200}$/;

type State = "idle" | "working" | "blocked";

interface HostContext {
  hasUI?: boolean;
  isIdle?: () => boolean;
  sessionManager?: { getSessionId?: () => unknown };
}

interface ApprovalEvent {
  reason?: string;
  toolName?: string;
}

interface ToolEvent {
  toolName?: string;
  args?: { questions?: { question?: unknown }[] };
}

const ENABLED = process.env.HEELER_ENV === "1" && !!process.env.HEELER_SESSION_ID;

// Increases per report; microseconds keep it above what an earlier instance of this
// extension (a reload) used and fit a double exactly.
let lastSeq = Date.now() * 1000;
function nextSeq(): number {
  lastSeq += 1;
  return lastSeq;
}

// One `heeler` process at a time, in the order of the events, each bounded in time.
let queue: Promise<void> = Promise.resolve();
function heeler(args: string[]): void {
  queue = queue.then(
    () =>
      new Promise<void>((resolve) => {
        let done = false;
        const finish = () => {
          if (done) return;
          done = true;
          resolve();
        };
        try {
          const child = spawn(process.env.HEELER_BIN || "heeler", args, {
            stdio: "ignore",
            windowsHide: true,
          });
          child.on("error", finish);
          child.on("exit", finish);
          setTimeout(finish, 1500).unref?.();
        } catch {
          finish();
        }
      }),
  );
}

let agentSessionId: string | undefined;

function report(state: State, message?: string): void {
  // `--name=value` so a value that starts with `-` is not taken for an option.
  const args = [
    "agent",
    "report",
    "--source",
    SOURCE,
    "--agent",
    "omp",
    "--state",
    state,
    `--seq=${nextSeq()}`,
  ];
  if (message) args.push(`--message=${message.slice(0, 200)}`);
  if (agentSessionId) {
    args.push(`--agent-session-id=${agentSessionId}`, "--", "omp", `--resume=${agentSessionId}`);
  }
  heeler(args);
}

export default function (pi) {
  if (!ENABLED) return;

  let rootSession = false;
  let turnActive = false;
  let blockedCount = 0;
  let blockedLabel: string | undefined;
  let lastPublished: string | undefined;
  let idleTimer: NodeJS.Timeout | undefined;

  function clearIdleTimer() {
    clearTimeout(idleTimer);
    idleTimer = undefined;
  }

  function publish(force = false) {
    let state: State = "idle";
    let message: string | undefined;
    if (blockedCount > 0) {
      state = "blocked";
      message = blockedLabel;
    } else if (turnActive) {
      state = "working";
    }
    const key = `${state}|${message ?? ""}`;
    if (!force && key === lastPublished) return;
    lastPublished = key;
    report(state, message);
  }

  // Only the interactive (root) session is the agent the user sees; subagents and print
  // mode have no UI and must not overwrite its state.
  function adopt(ctx: HostContext | undefined): boolean {
    if (ctx?.hasUI !== true) return false;
    rootSession = true;
    try {
      const id = ctx.sessionManager?.getSessionId?.();
      agentSessionId = typeof id === "string" && SAFE_ID.test(id) ? id : undefined;
    } catch {
      agentSessionId = undefined;
    }
    return true;
  }

  function block(label: string | undefined) {
    clearIdleTimer();
    blockedCount += 1;
    blockedLabel = label;
    publish();
  }

  function unblock() {
    blockedCount = Math.max(0, blockedCount - 1);
    if (blockedCount === 0) blockedLabel = undefined;
    publish();
  }

  pi.on("session_start", (_event: unknown, ctx: HostContext) => {
    if (!adopt(ctx)) return;
    // A reload can replace this extension in the middle of a turn.
    turnActive = ctx.isIdle?.() === false;
    publish(true);
  });

  pi.on("session_switch", (_event: unknown, ctx: HostContext) => {
    if (!adopt(ctx)) return;
    clearIdleTimer();
    turnActive = false;
    blockedCount = 0;
    blockedLabel = undefined;
    publish(true);
  });

  pi.on("agent_start", (_event: unknown, ctx: HostContext) => {
    if (!adopt(ctx)) return;
    clearIdleTimer();
    turnActive = true;
    publish();
  });

  pi.on("agent_end", () => {
    if (!rootSession || !turnActive) return;
    turnActive = false;
    clearIdleTimer();
    idleTimer = setTimeout(() => {
      idleTimer = undefined;
      publish();
    }, IDLE_DEBOUNCE_MS);
    idleTimer.unref?.();
  });

  pi.on("tool_approval_requested", (event: ApprovalEvent, ctx: HostContext) => {
    if (!rootSession && !adopt(ctx)) return;
    block(event?.reason || `${event?.toolName || "tool"} approval`);
  });

  pi.on("tool_approval_resolved", () => {
    if (!rootSession) return;
    unblock();
  });

  // The `ask` tool waits for the user's answer.
  pi.on("tool_execution_start", (event: ToolEvent, ctx: HostContext) => {
    if (event?.toolName !== "ask") return;
    if (!rootSession && !adopt(ctx)) return;
    const questions = Array.isArray(event.args?.questions) ? event.args.questions : [];
    const first = questions.find((q) => typeof q?.question === "string");
    block(typeof first?.question === "string" ? first.question : "waiting for an answer");
  });

  pi.on("tool_execution_end", (event: ToolEvent) => {
    if (event?.toolName !== "ask" || !rootSession) return;
    unblock();
  });

  pi.on("session_shutdown", () => {
    clearIdleTimer();
  });
}
