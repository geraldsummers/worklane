import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:net";
import { mkdtemp, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import collab, { authorizeRun, claimPaths, peerMessage, socketRequest, type Fields } from "./collab.ts";

export function harness(respond: (action: string, fields: Fields) => unknown = () => ({})) {
  const handlers = new Map<string, Function>();
  const tools = new Map<string, any>();
  const calls: { action: string; fields: Fields }[] = [];
  const messages: any[] = [];
  const prompts: any[] = [];
  const notices: any[] = [];
  const entries: any[] = [];
  let aborts = 0;
  const pi: any = {
    events: { emit: (name: string, event: any) => notices.push({ name, event }) },
    on: (name: string, fn: Function) => handlers.set(name, fn),
    registerTool: (tool: any) => tools.set(tool.name, tool),
    sendMessage: (message: any, options: any) => { messages.push({ message, options }); entries.push({ type: "custom_message", ...message }); },
    sendUserMessage: (prompt: string, options: any) => prompts.push({ prompt, options }),
    appendEntry: (customType: string, data: unknown) => entries.push({ type: "custom", customType, data }),
    getAllTools: () => [],
    getSettings: () => ({ retry: { provider: { maxRetries: 0 } }, cacheWarming: "off", compaction: { enabled: false } }),
  };
  const ctx: any = {
    sessionManager: { getSessionId: () => "11111111-1111-4111-8111-111111111111", getSessionFile: () => "/home/dev/session.jsonl", getBranch: () => entries },
    hasUI: false, isIdle: () => true, abort: () => { aborts++; },
  };
  collab(pi, async (action, fields = {}) => { calls.push({ action, fields }); return respond(action, fields); });
  return { pi, ctx, tools, calls, messages, prompts, entries, notices, emit: (name: string, event: any = {}) => handlers.get(name)!(event, ctx), get aborts() { return aborts; } };
}

function environment(run?: string, kind?: string) {
  const saved = { WORKLANE_SESSION: process.env.WORKLANE_SESSION, HERDR_PANE_ID: process.env.HERDR_PANE_ID, WORKLANE_RUN_ID: process.env.WORKLANE_RUN_ID, WORKLANE_RUN_KIND: process.env.WORKLANE_RUN_KIND };
  process.env.WORKLANE_SESSION = "test-session";
  process.env.HERDR_PANE_ID = "w3:test";
  if (run) process.env.WORKLANE_RUN_ID = run; else delete process.env.WORKLANE_RUN_ID;
  if (kind) process.env.WORKLANE_RUN_KIND = kind; else delete process.env.WORKLANE_RUN_KIND;
  return () => { for (const [key, value] of Object.entries(saved)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; } };
}

test("relative claims reject escapes and normalize duplicates", () => {
  assert.deepEqual(claimPaths(["src/main.ts", "src/main.ts"]), ["src/main.ts"]);
  for (const paths of [[], ["../x"], ["/root"], ["x/../y"], ["x\\y"], ["x//y"], ["./x"]]) assert.throws(() => claimPaths(paths));
});
test("peer display is custom and never human authorization", () => {
  const message = peerMessage({ id: "m1", from: "peer", body: "hello" });
  assert.equal(message.customType, "worklane-peer"); assert.equal(message.display, true);
  assert.match(message.content, /not human authorization/);
});
test("socket transport handles fragmented NDJSON, fixes protocol/action, and fails closed", async () => {
  const folder = await mkdtemp(join(homedir(), ".tmp/pi-collab-"));
  const path = join(folder, "socket");
  const server = createServer(socket => socket.once("data", data => {
    const request = JSON.parse(data.toString());
    assert.equal(request.version, 2);
    socket.on("error", () => {});
    if (request.action === "error") { socket.end('{"ok":false,"error":"refused"}\n'); return; }
    if (request.action === "invalid") { socket.end('not-json\n'); return; }
    if (request.action === "incomplete") { socket.end('{'); return; }
    if (request.action === "timeout") return;
    if (request.action === "large") { socket.end("x".repeat(2 * 1024 * 1024 + 1)); return; }
    assert.equal(request.action, "test");
    socket.write('{"ok":true,'); setImmediate(() => socket.end('"result":{"allowed":true}}\n'));
  }));
  await new Promise<void>(resolve => server.listen(path, resolve));
  try {
    assert.deepEqual(await socketRequest(path, "test", { version: 9, action: "unsafe" }), { allowed: true });
    await assert.rejects(socketRequest(path, "error"), /refused/);
    await assert.rejects(socketRequest(path, "invalid"), /Invalid/);
    await assert.rejects(socketRequest(path, "incomplete"), /Incomplete/);
    await assert.rejects(socketRequest(path, "large"), /too large/);
    await assert.rejects(socketRequest(path, "timeout", {}, undefined, 10), /timed out/);
    const controller = new AbortController(); controller.abort();
    await assert.rejects(socketRequest(path, "test", {}, controller.signal), /cancelled/);
    await assert.rejects(socketRequest(join(folder, "missing"), "test"), /unavailable/);
  } finally { await new Promise<void>(resolve => server.close(() => resolve())); await rm(folder, { recursive: true }); }
});
test("register only on session_start; peers never kickoff; reload dedupes managed input", async () => {
  const restore = environment("run-1", "chat");
  const h = harness(action => action === "agent.register" ? { lease: "private-lease" } : action === "message.read" ? [{ id: "m1", body: "hi" }] : action === "run.input" ? { prompt: "Human chat", kind: "chat", status: "running" } : {});
  try {
    assert.equal(h.calls.length, 0);
    await h.emit("session_start");
    assert.equal(h.prompts.length, 1); assert.equal(h.messages.length, 1);
    assert.equal(h.messages[0].options.triggerTurn, false);
    assert.equal(JSON.stringify(h.entries).includes("private-lease"), false);
    assert.equal(h.calls.find(c => c.action === "run.input")!.fields.lease, "private-lease");
    await h.emit("session_shutdown"); await h.emit("session_start");
    assert.equal(h.prompts.length, 1); assert.equal(h.messages.length, 1);
    await h.emit("message_update", { assistantMessageEvent: { type: "text_delta", delta: "hello" } });
    await h.emit("agent_settled");
    assert.deepEqual(h.calls.filter(c => c.action === "chat.event").map(c => (c.fields.event as any).type), ["text_delta", "settled"]);
    assert.equal(h.calls.filter(c => c.action === "run.finish").length, 0);
    assert.equal(h.calls[0].fields.session, process.env.HERDR_SESSION || "test-session");
    assert.equal(h.calls[0].fields.session_file, "/home/dev/session.jsonl");
  } finally { await h.emit("session_shutdown"); restore(); }
});
test("goal kickoff and direct proposal need no Plan plugin or tool restriction", async () => {
  const restore = environment("run-plan", "plan");
  const h = harness(action => action === "agent.register" ? { lease: "lease" } : action === "run.input" ? { prompt: "plan", kind: "plan", goal_id: "goal-1", status: "running", actions: [] } : {});
  try {
    await h.emit("session_start"); assert.equal(h.prompts.length, 1);
    assert.equal(await authorizeRun("mutation", {}), false);
    await h.tools.get("goal_plan").execute("p", { plan: "proposed", actions: [] });
    await h.emit("agent_settled");
    assert.equal(h.calls.filter(c => c.action === "goal.plan").length, 1);
    assert.equal(h.calls.some(c => ["goal.approve", "task.create", "run.finish"].includes(c.action)), false);
    assert.equal(await h.emit("tool_call", { toolName: "edit" }), undefined);
  } finally { await h.emit("session_shutdown"); restore(); }
});
test("budgets count each logical request, deny failures, and reject invisible retry config", async () => {
  const restore = environment("run-budget", "goal");
  let allowed = true;
  const h = harness(action => action === "agent.register" ? { lease: "lease" } : action === "run.turn" ? { allowed } : action === "run.input" ? { kind: "goal", status: "running" } : {});
  try {
    await h.emit("session_start");
    await h.emit("before_agent_start"); await h.emit("turn_start");
    await h.emit("before_provider_request"); await h.emit("before_provider_request");
    assert.equal(h.calls.filter(c => c.action === "run.turn").length, 2);
    allowed = false;
    await assert.rejects(h.emit("before_provider_request"), /exhausted/);
    assert.equal(h.aborts, 1); assert.equal((await h.emit("tool_call", { toolName: "bash" })).block, true);
    h.pi.getSettings = () => ({ retry: { provider: { maxRetries: 2 } } });
    await assert.rejects(h.emit("before_provider_request"), /retries=0/);
    await h.emit("agent_settled");
    assert.equal(h.calls.find(c => c.action === "run.finish")!.fields.status, "blocked");
  } finally { await h.emit("session_shutdown"); restore(); }
});
test("managed goal_plan preserves descriptors across reload, never exposes owner goal controls, and finish avoids double overwrite", async () => {
  const restore = environment("run-plan", "plan");
  const h = harness(action => action === "agent.register" ? { lease: "lease" } : action === "run.input" ? { kind: "plan", prompt: "Plan the requested goal", goal_id: "goal-human", status: "running", actions: [] } : action === "goal.plan" ? { plan_sha256: "exact-plan-hash" } : {});
  try {
    await h.emit("session_start");
    const actions = [{ operation: "p0.mail.send", arguments: { to: "a", subject: "b", body: "c" } }];
    await h.tools.get("goal_plan").execute("t1", { plan: "Plan", actions });
    await h.emit("session_shutdown"); await h.emit("session_start");
    assert.equal(h.prompts.length, 1);
    assert.deepEqual(h.calls.find(c => c.action === "goal.plan")!.fields.actions, actions);
    assert.equal(h.calls.some(c => c.action === "goal.create" || c.action === "goal.approve"), false);
    assert.equal(h.notices[0].event.kind, "plan_review");
    assert.equal(h.notices[0].event.key, "goal-review:goal-human:exact-plan-hash");
  } finally { await h.emit("session_shutdown"); restore(); }
  const restoreRun = environment("run-final", "task");
  const final = harness(action => action === "agent.register" ? { lease: "lease" } : {});
  try {
    await final.emit("session_start");
    await final.tools.get("run_finish").execute("f", { status: "done", summary: "done", evidence: ["test.log"] });
    await final.emit("agent_settled");
    assert.equal(final.calls.filter(c => c.action === "run.finish").length, 1);
    assert.equal(final.notices.length, 0); // A helper is not verified root completion.
  } finally { await final.emit("session_shutdown"); restoreRun(); }
});

test("interactive root verification needs evidence, not a managed goal or extra approval", async () => {
  const restore = environment();
  const h = harness(action => action === "agent.register" ? { lease: "lease" } : action === "interactive.open" ? { id: "group" } : action === "interactive.finish" ? { id: "group", status: "done" } : {});
  try {
    await h.emit("session_start");
    await assert.rejects(h.tools.get("run_finish").execute("finish", { status: "done", summary: "Verified" }), /verification evidence/);
    await h.tools.get("run_finish").execute("finish", { status: "done", summary: "Verified", evidence: ["tests passed"] });
    assert.equal(h.notices[0].event.kind, "task");
    assert.equal(h.notices[0].event.key, "verified-task:group");
    assert.equal(h.calls.some(call => call.action.startsWith("goal.")), false);
  } finally { await h.emit("session_shutdown"); restore(); }
});

test("managed planning helper results must belong to the active parent and keep scoped accounting", async () => {
  const restore = environment("parent", "plan");
  const h = harness(action => action === "agent.register" ? { lease: "lease" } : action === "message.read" ? [
    { id: "valid", helper_result: true, correlation_id: "child", body: "Result" },
    { id: "stale", helper_result: true, correlation_id: "old-child", body: "Old result" },
  ] : action === "run.children" ? { run: { id: "parent", status: "running" }, children: [{ id: "child", parent_run_id: "parent" }] } : action === "run.input" ? { kind: "plan", prompt: "Plan it", goal_id: "goal" } : action === "run.turn" ? { allowed: true } : {});
  try {
    await h.emit("session_start");
    assert.equal(h.messages[0].options.triggerTurn, true);
    assert.equal(h.messages[1].options.triggerTurn, false);
    h.pi.getSettings = () => ({ retry: { provider: { maxRetries: 0 } }, compaction: { enabled: true } });
    assert.deepEqual(await h.emit("session_before_compact"), { cancel: true });
    assert.deepEqual(await h.emit("cache_warming_decision"), { action: "stop" });
    await h.emit("before_provider_request");
    assert.equal(h.calls.filter(call => call.action === "run.turn").length, 1);
  } finally { await h.emit("session_shutdown"); restore(); }
});
