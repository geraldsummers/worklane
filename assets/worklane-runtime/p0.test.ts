import test from "node:test";
import assert from "node:assert/strict";
import p0, { helperCommand, redact, requireMutationApproval, runHelper, stackReadFields } from "./p0.ts";
import collab from "./collab.ts";

test("helper argv is typed, shell-free, and mail body never becomes argv", () => {
  const body = "$(unsafe); literal body";
  const command = helperCommand("mail.send", { to: "agent@example.test", subject: "a; $(x)", body });
  assert.equal(command.file, "mail.py"); assert.equal(command.input, body);
  assert.equal(command.argv.includes(body), false); assert.equal(command.mutation, true);
  assert.throws(() => helperCommand("mail.send", { to: "a\nBcc: b", subject: "x", body: "x" }), /header/);
  assert.equal(helperCommand("mail.list", {}).mutation, false);
  assert.deepEqual(helperCommand("mail.read", { uid: 42 }).argv, ["read", "42"]);
  assert.deepEqual(helperCommand("matrix.sync", { since: "cursor", timeout: 1000 }).argv, ["sync", "--timeout", "1000", "--since", "cursor"]);
  assert.deepEqual(helperCommand("matrix.whoami", {}).argv, ["whoami"]);
  assert.deepEqual(helperCommand("matrix.rooms", {}).argv, ["rooms"]);
  assert.deepEqual(helperCommand("matrix.send", { room_id: "!r", message: "hello" }).argv, ["send", "!r", "hello"]);
  assert.deepEqual(helperCommand("matrix.join", { room_id: "!r" }).argv, ["join", "!r"]);
  assert.deepEqual(helperCommand("matrix.create_room", { name: "test" }).argv, ["create-room", "test"]);
  assert.deepEqual(helperCommand("memory.add", { content: "fact" }).argv, ["add", "fact"]);
  assert.deepEqual(helperCommand("memory.search", { query: "fact", owner: "foreign", project: "other" }).argv, ["search", "fact"]);
  assert.equal(helperCommand("memory.search", { query: "fact" }).mutation, false);
  assert.throws(() => helperCommand("owner.delete" as any, {}), /Unsupported/);
});
test("recursive redaction removes credential fields and common credential text", () => {
  assert.deepEqual(redact({ public: 1, client_secret: "bad", nested: [{ access_token: "bad", lease: "bad", password: "bad", body: "Bearer SECRET" }] }), { public: 1, nested: [{ body: "Bearer [redacted]" }] });
  assert.equal(redact("sk-abcdefghijklmnop eyJabc.defghi.jklmnop"), "[redacted] [redacted]");
});
test("no broad owner API, break-glass, identity file, or untyped stack action registered", async () => {
  const tools = new Map<string, any>();
  p0({ registerTool: (tool: any) => tools.set(tool.name, tool) } as any);
  assert.equal(tools.size, 18);
  assert.equal(tools.has("p0_notify"), true);
  assert.equal(helperCommand("notify.send", {title: "Done"}).input, '{"title":"Done"}');
  assert.equal([...tools.keys()].some(name => /owner|goal|approve|token|identity|break.glass/.test(name)), false);
  const reads = tools.get("p0_stack_read").parameters.properties;
  assert.deepEqual(reads.action.anyOf.map((branch: any) => branch.const), ["status", "diagnostics", "verify", "logs", "operation-status"]);
  const pattern = reads.unit.pattern;
  assert.equal(new RegExp(pattern).test("webservices-test.service"), true);
  assert.equal(new RegExp(pattern).test("webservices-testXservice"), false);
  assert.deepEqual(Object.keys(tools.get("p0_stack_plan").parameters.properties), ["bundle", "sha256"]);
  assert.deepEqual(Object.keys(tools.get("p0_stack_rollback").parameters.properties), ["operation_id"]);
  await assert.rejects(tools.get("p0_memory_search").execute("test", {query: "fact"}, undefined, undefined, {}), /Lane-scoped.*not provisioned/);
  await assert.rejects(runHelper("missing-helper.py", [], undefined), /not provisioned/);
  assert.deepEqual(stackReadFields({ action: "status", approval_token: "never forwarded" }), { action: "status", args: {} });
  assert.deepEqual(stackReadFields({ action: "diagnostics" }), { action: "diagnostics", args: {} });
  assert.deepEqual(stackReadFields({ action: "verify", release: "hash" }), { action: "verify", args: { release: "hash" } });
  assert.deepEqual(stackReadFields({ action: "operation-status", operation_id: "id" }), { action: "operation-status", args: { operation_id: "id" } });
  assert.deepEqual(stackReadFields({ action: "logs", unit: "webservices-test.service" }), { action: "logs", args: { domain: "rootful", unit: "webservices-test.service" } });
  for (const params of [{ action: "verify" }, { action: "operation-status" }, { action: "logs" }, { action: "logs", domain: "apps", unit: "unit" }, { action: "apply" }]) assert.throws(() => stackReadFields(params));
});
test("external mutations require TUI human confirmation; RPC and denied confirmations fail closed", async () => {
  const saved = process.env.WORKLANE_RUN_ID; delete process.env.WORKLANE_RUN_ID;
  let confirmations = 0;
  const ui: any = { confirm: async () => { confirmations++; return true; } };
  try {
    await requireMutationApproval("p0.mail.send", { body: "hello" }, { mode: "tui", ui });
    assert.equal(confirmations, 1);
    await assert.rejects(requireMutationApproval("p0.mail.send", {}, { mode: "rpc", ui }), /TUI/);
    await assert.rejects(requireMutationApproval("p0.mail.send", {}, { mode: "tui", ui: { confirm: async () => false } as any }), /TUI/);
    assert.equal(confirmations, 1);
  } finally { if (saved === undefined) delete process.env.WORKLANE_RUN_ID; else process.env.WORKLANE_RUN_ID = saved; }
});
test("managed mutation must exactly authorize via lease; never falls back to TUI", async () => {
  const saved = { WORKLANE_SESSION: process.env.WORKLANE_SESSION, WORKLANE_RUN_ID: process.env.WORKLANE_RUN_ID, WORKLANE_RUN_KIND: process.env.WORKLANE_RUN_KIND, HERDR_PANE_ID: process.env.HERDR_PANE_ID };
  process.env.WORKLANE_SESSION = "test-session";
  process.env.WORKLANE_RUN_ID = "run-approved"; process.env.WORKLANE_RUN_KIND = "goal"; process.env.HERDR_PANE_ID = "w3:test";
  const handlers = new Map<string, Function>();
  let request: any;
  const pi: any = { on: (name: string, fn: Function) => handlers.set(name, fn), registerTool: () => {}, getAllTools: () => [], sendMessage: () => {} };
  const ctx: any = { hasUI: false, isIdle: () => true, sessionManager: { getSessionId: () => "session", getSessionFile: () => "/session.jsonl", getBranch: () => [] } };
  collab(pi, async (action, fields) => {
    if (action === "agent.register") return { lease: "private" };
    if (action === "run.authorize") { request = fields; return { allowed: JSON.stringify(fields?.arguments) === JSON.stringify({ plan_id: "approved" }) }; }
    return {};
  });
  const ui: any = { confirm: async () => { throw new Error("must not fallback"); } };
  try {
    await handlers.get("session_start")!({}, ctx);
    await requireMutationApproval("p0_stack_apply", { plan_id: "approved" }, { mode: "tui", ui });
    assert.equal(request.operation, "p0_stack_apply"); assert.equal(request.lease, "private"); assert.equal(request.run_id, "run-approved");
    await assert.rejects(requireMutationApproval("p0_stack_apply", { plan_id: "different" }, { mode: "tui", ui }), /exactly match/);
    await handlers.get("session_shutdown")!({}, ctx);
    await assert.rejects(requireMutationApproval("p0_stack_apply", { plan_id: "approved" }, { mode: "tui", ui }), /exactly match/);
  } finally {
    await handlers.get("session_shutdown")!({}, ctx);
    for (const [key, value] of Object.entries(saved)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
  }
});
