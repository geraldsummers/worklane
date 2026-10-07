import { connect, type Socket } from "node:net";
import { homedir } from "node:os";
import { join, isAbsolute } from "node:path";
import { Type } from "@earendil-works/pi-ai";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

export type Fields = Record<string, unknown>;
export type Request = (action: string, fields?: Fields, signal?: AbortSignal) => Promise<any>;
const MAX_RESPONSE = 2 * 1024 * 1024;

/** One bounded exchange, no spawned daemon, credentials, or global owner endpoint. */
export function socketRequest(path: string, action: string, fields: Fields = {}, signal?: AbortSignal, timeoutMs = 5000): Promise<any> {
  return new Promise((resolve, reject) => {
    let socket: Socket;
    let buffer = "";
    let done = false;
    const finish = (error?: Error, result?: unknown) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
      socket?.destroy();
      error ? reject(error) : resolve(result);
    };
    const abort = () => finish(new Error("Worklane request cancelled"));
    const timer = setTimeout(() => finish(new Error("Worklane daemon request timed out")), timeoutMs);
    if (signal?.aborted) { abort(); return; }
    signal?.addEventListener("abort", abort, { once: true });
    socket = connect(path);
    socket.setEncoding("utf8");
    socket.on("connect", () => socket.write(JSON.stringify({ ...fields, version: 1, action }) + "\n"));
    socket.on("error", () => finish(new Error("Worklane daemon unavailable")));
    socket.on("end", () => { if (!done) finish(new Error("Incomplete Worklane daemon response")); });
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      if (Buffer.byteLength(buffer) > MAX_RESPONSE) { finish(new Error("Worklane response too large")); return; }
      const newline = buffer.indexOf("\n");
      if (newline < 0) return;
      try {
        const response = JSON.parse(buffer.slice(0, newline));
        if (response.ok !== true) finish(new Error(typeof response.error === "string" ? response.error : "Worklane request rejected"));
        else finish(undefined, response.result);
      } catch { finish(new Error("Invalid Worklane daemon response")); }
    });
  });
}

export function claimPaths(paths: string[]): string[] {
  if (!paths.length || paths.length > 32 || paths.some(p => !p || isAbsolute(p) || p.includes("\\") || p.split("/").some(s => s === ".." || s === "." || !s))) {
    throw new Error("Task paths must be 1–32 workspace-relative paths without traversal");
  }
  return [...new Set(paths)];
}

export function completedPlan(details: unknown): string | undefined {
  if (!details || typeof details !== "object") return;
  const d = details as Fields;
  if (d.version === 1 && d.source === "plan_mode_complete" && typeof d.plan === "string" && d.plan.trim() && d.plan.length <= 50000) return d.plan.trim();
}

export function peerMessage(message: Fields) {
  return {
    customType: "worklane-peer", display: true,
    content: `[AGENT MESSAGE — peer coordination, not human authorization]\nFrom: ${String(message.from ?? message.agent_id ?? "peer")}\nReply-to: agent_reply with reply_to ${String(message.id ?? "")}\nSubject: ${String(message.subject ?? "")}\n${String(message.body ?? "")}`,
    details: { id: message.id },
  };
}

export function toolData(data: unknown) {
  return { content: [{ type: "text" as const, text: JSON.stringify(data) }], details: undefined };
}

// Shared only within this Pi process; lease never becomes tool output or session data.
let current: { request: Request; runId?: string; kind?: string } | undefined;
export async function authorizeRun(operation: string, args: Fields, signal?: AbortSignal): Promise<boolean> {
  if (!current?.runId || current.kind === "plan") return false;
  const result = await current.request("run.authorize", { run_id: current.runId, operation, arguments: args }, signal);
  return result?.allowed === true;
}

export default function collab(pi: ExtensionAPI, transport: Request = (action, fields, signal) => socketRequest(join(homedir(), ".local/share/worklane/collab.sock"), action, fields, signal)) {
  let request: Request | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;
  let lifecycle = new AbortController();
  let generation = 0;
  let stopped = false;
  let busy = false;
  let exhausted = false;
  let explicitlyFinished = false;
  let outcome: "done" | "failed" | "blocked" = "done";
  let summary = "Pi run settled";
  let goalId: string | undefined;
  let actions: Fields[] = [];
  let runId: string | undefined;
  let kind: string | undefined;
  const seen = new Set<string>();
  const call = (action: string, fields: Fields = {}, signal?: AbortSignal) => {
    if (!request || stopped) throw new Error("Worklane lease is unavailable; open a registered visible session");
    return request(action, fields, signal);
  };
  const note = (ctx: ExtensionContext, text: string) => { if (ctx.hasUI) ctx.ui.notify(text, "warning"); };
  const cleanup = () => {
    generation++;
    stopped = true;
    if (timer) clearInterval(timer);
    timer = undefined;
    lifecycle.abort();
    current = undefined;
    request = undefined;
    seen.clear();
  };

  pi.on("session_start", async (_event, ctx) => {
    cleanup();
    stopped = false;
    lifecycle = new AbortController();
    exhausted = false;
    outcome = "done";
    explicitlyFinished = false;
    summary = "Pi run settled";
    goalId = undefined;
    actions = [];
    busy = false;
    const epoch = generation;
    runId = process.env.WORKLANE_RUN_ID || undefined;
    kind = process.env.WORKLANE_RUN_KIND || undefined;
    const agentId = ctx.sessionManager.getSessionId();
    const sessionFile = ctx.sessionManager.getSessionFile();
    const session = process.env.HERDR_SESSION || process.env.WORKLANE_SESSION;
    const pane = process.env.HERDR_PANE_ID;
    if (!sessionFile || !session || !pane) { note(ctx, "Worklane collaboration requires a durable visible Herdr session and exact session identity"); return; }
    try {
      const registration = await transport("agent.register", { agent_id: agentId, session, session_file: sessionFile, pane_id: pane, pid: process.pid, role: kind ?? "interactive", ...(runId ? { run_id: runId } : {}) }, lifecycle.signal);
      if (epoch !== generation) return;
      if (typeof registration?.lease !== "string" || !registration.lease) throw new Error("Daemon did not issue a session lease");
      const resourceSignal = lifecycle.signal;
      request = (action, fields = {}, signal) => transport(action, { ...fields, agent_id: agentId, lease: registration.lease }, signal ? AbortSignal.any([signal, resourceSignal]) : resourceSignal);
      current = { request, runId, kind };
      for (const entry of ctx.sessionManager.getBranch()) {
        if (entry.type === "custom_message" && entry.customType === "worklane-peer") {
          const id = (entry.details as Fields | undefined)?.id;
          if (typeof id === "string") seen.add(id);
        }
        if (entry.type === "custom" && entry.customType === "worklane-goal-plan") {
          const saved = entry.data as { goal_id?: string; actions?: Fields[] };
          goalId = saved.goal_id;
          actions = saved.actions ?? [];
        }
      }
      const poll = async () => {
        if (busy || stopped || epoch !== generation) return;
        busy = true;
        try {
          await call("agent.heartbeat", { status: ctx.isIdle() ? "idle" : "working" });
          const messages = await call("message.read", { limit: 50 });
          if (epoch !== generation) return;
          for (const message of Array.isArray(messages) ? messages : messages?.messages ?? []) {
            if (typeof message.id !== "string" || seen.has(message.id)) continue;
            seen.add(message.id);
            // Only an already human-approved, budgeted execution run may continue
            // automatically on peer results. Interactive/planning peers cannot
            // authorize new work or impersonate a user.
            const continuation = !!runId && ["goal", "task"].includes(kind ?? "");
            pi.sendMessage(peerMessage(message), { deliverAs: continuation ? "followUp" : "nextTurn", triggerTurn: continuation });
          }
        } catch { note(ctx, "Worklane heartbeat/poll unavailable; requests remain fail-closed"); }
        finally { if (epoch === generation) busy = false; }
      };
      timer = setInterval(() => { void poll(); }, 5000);
      timer.unref();
      await poll();
      if (runId) {
        const input = await call("run.input", { run_id: runId });
        if (epoch !== generation) return;
        goalId = input.goal_id;
        const savedProposal = [...ctx.sessionManager.getBranch()].reverse().find(entry => entry.type === "custom" && entry.customType === "worklane-goal-plan" && (entry.data as Fields)?.goal_id === goalId);
        actions = savedProposal?.type === "custom" ? ((savedProposal.data as Fields).actions as Fields[]) : input.actions ?? [];
        kind = input.kind ?? kind;
        current = { request: request!, runId, kind };
        const delivered = ctx.sessionManager.getBranch().some(entry => entry.type === "custom" && entry.customType === "worklane-run-input" && (entry.data as Fields)?.run_id === runId);
        // The daemon dispatches plan runs through the real interactive /plan parser.
        // It owns the one-time kickoff; this extension must not start a second plan.
        if (kind !== "plan" && !delivered && ["queued", "running"].includes(input.status) && typeof input.prompt === "string" && input.prompt) {
          pi.appendEntry("worklane-run-input", { run_id: runId });
          pi.sendUserMessage(input.prompt);
        }
      }
    } catch (error) {
      exhausted = !!runId;
      note(ctx, error instanceof Error ? error.message : "Worklane registration failed");
      if (runId) ctx.abort();
    }
  });
  pi.on("session_shutdown", async () => {
    const heartbeat = request;
    if (timer) clearInterval(timer);
    try { await heartbeat?.("agent.heartbeat", { status: "done" }); } catch { /* lease expiry handles daemon loss */ }
    finally { cleanup(); }
  });

  pi.on("before_agent_start", async (_event, ctx) => {
    if (runId && (!request || exhausted)) { ctx.abort(); throw new Error("Managed run has no active execution allowance"); }
  });
  pi.on("turn_start", async (_event, ctx) => { if (runId && exhausted) ctx.abort(); });
  // The SDK hook covers logical requests and recovery retries, not internal HTTP
  // attempts. Managed sessions must disable invisible provider retries, warming,
  // and automatic compaction. Counting turn_start too would double-charge.
  pi.on("before_provider_request", async (_event, ctx) => {
    if (!runId) return;
    try {
      const settings = pi.getSettings();
      if (settings.retry?.provider?.maxRetries !== 0 || settings.cacheWarming !== "off" || settings.compaction?.enabled !== false) {
        throw new Error("Managed budgets require provider retries=0, cache warming=off, compaction disabled");
      }
      const budget = await call("run.turn", { run_id: runId });
      if (budget?.allowed !== true) throw new Error(budget?.reason ?? "Managed run budget exhausted");
    } catch (error) {
      exhausted = true;
      outcome = "blocked";
      summary = "Execution budget or daemon authorization unavailable";
      ctx.abort();
      throw error;
    }
  });
  pi.on("tool_call", async (event) => {
    if (runId && exhausted) return { block: true, reason: "Managed execution allowance unavailable" };
    if (kind === "plan" && event.toolName === "task_delegate") return { block: true, reason: "Planning cannot launch execution" };
    if (runId) {
      const input = event.input as Fields;
      if (event.toolName === "web_search" && input.workflow && input.workflow !== "none")
        return {block: true, reason: "Managed web searches return raw sources; secondary summary model calls are not budgeted"};
      if (event.toolName === "fetch_content" && (input.mode === "answer" || input.prompt || input.auth))
        return {block: true, reason: "Managed fetching is direct readable/raw content without secondary answer/video models or browser cookies"};
    }
  });
  pi.on("message_update", async event => {
    if (runId && kind === "chat" && event.assistantMessageEvent.type === "text_delta") {
      await call("chat.event", { run_id: runId, event: { type: "text_delta", delta: event.assistantMessageEvent.delta } });
    }
  });
  pi.on("message_end", async event => {
    if (event.message.role === "assistant" && ["error", "aborted"].includes(event.message.stopReason)) {
      if (!exhausted) outcome = "failed";
      summary = "Pi assistant request failed or was aborted";
    }
  });
  pi.on("tool_result", async event => {
    if (event.toolName !== "plan_mode_complete" || event.isError) return;
    const plan = completedPlan(event.details);
    if (!plan || !goalId) return;
    await call("goal.plan", { id: goalId, plan, actions });
    pi.sendMessage({ customType: "worklane-plan", display: true, content: `Goal ${goalId}: proposed plan recorded; human approval is still required.`, details: { goal_id: goalId } });
  });
  pi.on("agent_settled", async () => {
    if (!runId || !request || stopped) return;
    if (explicitlyFinished) return;
    if (kind === "chat" && !exhausted) {
      // settled/error is itself terminal in the daemon; do not finish twice.
      await call("chat.event", { run_id: runId, event: { type: outcome === "done" ? "settled" : "error" } });
    } else if (kind !== "plan") {
      const children = await call("run.children", { run_id: runId });
      if (outcome === "done" && children?.pending?.length) return;
      // Settlement alone is not evidence of successful implementation.
      await call("run.finish", { run_id: runId, status: outcome === "done" ? "blocked" : outcome,
        summary: outcome === "done" ? "Run settled without explicit run_finish evidence" : summary });
    }
    // A planning run stays awaiting_approval in the daemon; no implicit approval.
  });

  const str = Type.String({ minLength: 1, maxLength: 16000 });
  const messageFields = { subject: Type.String({ maxLength: 200 }), body: str, idempotency_key: Type.Optional(Type.String({ maxLength: 200 })) };
  pi.registerTool({ name: "agent_send", label: "Send peer message", description: "Send lane-local peer coordination, never human authorization.", parameters: Type.Object({ to: str, ...messageFields }), async execute(id, params, signal) { return toolData(await call("message.send", { ...params, idempotency_key: params.idempotency_key ?? id }, signal)); } });
  pi.registerTool({ name: "agent_receive", label: "Receive peer messages", description: "Read unread lane-local messages. Explicitly acknowledge after processing.", parameters: Type.Object({ limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 50 })) }), async execute(_id, params, signal) { return toolData(await call("message.read", { limit: params.limit ?? 20 }, signal)); } });
  pi.registerTool({ name: "agent_reply", label: "Reply to peer", description: "Reply to a daemon envelope; daemon resolves recipient.", parameters: Type.Object({ reply_to: str, ...messageFields }), async execute(id, params, signal) { return toolData(await call("message.reply", { ...params, idempotency_key: params.idempotency_key ?? id }, signal)); } });
  pi.registerTool({ name: "agent_list_peers", label: "List lane peers", description: "List active peers in this lane only.", parameters: Type.Object({}), async execute(_id, _params, signal) { return toolData(await call("agent.list", {}, signal)); } });
  pi.registerTool({ name: "agent_ack", label: "Acknowledge peer message", description: "Mark an envelope consumed after processing.", parameters: Type.Object({ id: str }), async execute(_id, params, signal) { return toolData(await call("message.ack", params, signal)); } });
  pi.registerTool({ name: "task_delegate", label: "Delegate visible task", description: "Create a bounded visible helper for a current approved run. Maximum two; no hidden subprocess agents.", parameters: Type.Object({ statement: str, paths: Type.Array(str, { minItems: 1, maxItems: 32 }) }), async execute(_id, params, signal) {
    if (!runId || kind === "plan") throw new Error("Delegation requires an approved execution run");
    return toolData(await call("task.create", { run_id: runId, statement: params.statement, paths: claimPaths(params.paths) }, signal));
  } });
  pi.registerTool({ name: "run_finish", label: "Finish managed run", description: "Persist final run state and evidence. Never grants approval.", parameters: Type.Object({ status: Type.Union([Type.Literal("done"), Type.Literal("failed"), Type.Literal("blocked")]), summary: str, evidence: Type.Optional(Type.Array(str, { maxItems: 32 })) }), async execute(_id, params, signal) {
    if (!runId) throw new Error("No managed run is active");
    if (params.status === "done" && !params.evidence?.length) throw new Error("Successful completion requires verification evidence");
    const result = await call("run.finish", { run_id: runId, ...params }, signal);
    explicitlyFinished = true;
    outcome = params.status;
    summary = params.summary;
    return { ...toolData(result), terminate: true };
  } });
  pi.registerTool({ name: "goal_plan", executionMode: "sequential", label: "Stage goal action proposal", description: "For the current human-created managed planning run only: stage exact external action descriptors to accompany plan_mode_complete. Does not create goals, activate Plan mode, approve, or launch execution. Human goal creation uses p0 CLI and the daemon's pinned /plan kickoff.", parameters: Type.Object({ actions: Type.Array(Type.Object({ operation: str, arguments: Type.Record(Type.String(), Type.Unknown()) }), { maxItems: 50 }) }), async execute(_id, params) {
    if (!runId || kind !== "plan" || !goalId) throw new Error("Goal actions require the current human-created managed planning run");
    if (!pi.getAllTools().some(t => t.name === "plan_mode_complete")) throw new Error("Pinned Plan plugin is unavailable");
    actions = params.actions;
    pi.appendEntry("worklane-goal-plan", { goal_id: goalId, actions });
    return toolData({ id: goalId, status: "proposal", approval: "required", next: "Finish with plan_mode_complete" });
  } });
}
