// heeler-managed: OMP agent-state extension.
// Installed by `heeler integration install omp`; reinstalling overwrites this file, so put
// custom extensions beside it instead of editing it.
//
// Reports OMP's state (turn running, waiting for an approval or an answer, idle) and how to
// resume the conversation (`omp --resume=<id>`) to the Heeler session OMP runs in, by calling
// `heeler agent report`. Every agent is a node of the session's agent tree: OMP binds this
// extension to the main session and to every subagent session, and `ctx.agent` (`kind`, `id`,
// `parentId`, `name`) says which one an instance serves. Does nothing outside a Heeler session.
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
  agent?: { kind?: string; id?: string; parentId?: string; name?: string };
  model?: { id?: string; name?: string };
}

interface Identity {
  kind: "main" | "sub";
  id: string;
  parentId?: string;
  name?: string;
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

function report(
  who: Identity,
  state: State,
  message?: string,
  model?: string,
  agentSessionId?: string,
  outcome?: string,
): void {
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
    `--node=${who.id}`,
    `--kind=${who.kind}`,
  ];
  if (who.parentId) args.push(`--parent=${who.parentId}`);
  if (who.name) args.push(`--name=${who.name}`);
  if (model) args.push(`--model=${model}`);
  if (outcome) args.push(`--outcome=${outcome}`);
  if (message) args.push(`--message=${message.slice(0, 200)}`);
  // Only the main agent's conversation can be resumed.
  if (who.kind === "main" && agentSessionId) {
    args.push(`--agent-session-id=${agentSessionId}`, "--", "omp", `--resume=${agentSessionId}`);
  }
  heeler(args);
}

export default function (pi) {
  if (!ENABLED) return;

  let identity: Identity | undefined;
  let model: string | undefined;
  let agentSessionId: string | undefined;
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
    // A subagent whose agent loop is over has finished its task.
    const outcome = identity?.kind === "sub" && state === "idle" ? "completed" : undefined;
    const key = `${state}|${message ?? ""}|${model ?? ""}|${outcome ?? ""}`;
    if (!force && key === lastPublished) return;
    lastPublished = key;
    if (identity) report(identity, state, message, model, agentSessionId, outcome);
  }

  // This instance serves either the interactive (main) session, which the user sees, or a
  // subagent session (`ctx.agent.kind === "sub"`). Print mode has no UI and is neither.
  function adopt(ctx: HostContext | undefined): boolean {
    const agent = ctx?.agent;
    const safe = (value: unknown): value is string =>
      typeof value === "string" && SAFE_ID.test(value);
    const label = (value: unknown) =>
      typeof value === "string" && value.length > 0 ? value.slice(0, 100) : undefined;
    const known = ctx?.model?.id ?? ctx?.model?.name;
    model = typeof known === "string" && known.length <= 128 ? known : model;
    if (agent?.kind === "sub" && safe(agent.id)) {
      identity = {
        kind: "sub",
        id: agent.id,
        parentId: safe(agent.parentId) ? agent.parentId : undefined,
        name: label(agent.name),
      };
      rootSession = true;
      return true;
    }
    if (ctx?.hasUI !== true) return false;
    identity = { kind: "main", id: safe(agent?.id) ? agent.id : "Main", name: label(agent?.name) };
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

  pi.on("agent_end", (event: { willContinue?: boolean }) => {
    // The loop goes on (a retry, queued follow-up work): still working.
    if (event?.willContinue === true) return;
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
    // A subagent's session closing ends its node, whatever state it was in.
    if (identity?.kind === "sub") {
      lastPublished = undefined;
      turnActive = false;
      blockedCount = 0;
      publish(true);
    }
  });
}
