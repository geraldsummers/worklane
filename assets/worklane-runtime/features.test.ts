import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat, symlink } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { EventEmitter } from "node:events";
import complain, { writeComplaint } from "./complain.ts";
import questionnaire, { askQuestions } from "./questionnaire.ts";
import plans from "./plans.ts";
import subscription from "./subscription.ts";
import notifications, { terminalTitle } from "./notifications.ts";

function harness() {
  const handlers = new Map<string, Function>();
  const tools = new Map<string, any>();
  const commands = new Map<string, any>();
  const entries: any[] = [];
  const messages: any[] = [];
  const events = new EventEmitter();
  const pi: any = { on: (name: string, fn: Function) => handlers.set(name, fn), registerTool: (tool: any) => tools.set(tool.name, tool),
    registerCommand: (name: string, command: any) => commands.set(name, command), events: { emit: (...args: any[]) => events.emit(args[0], ...args.slice(1)), on: (name: string, fn: any) => { events.on(name, fn); return () => events.off(name, fn); } },
    appendEntry: (customType: string, data: any) => entries.push({ type: "custom", customType, data }),
    sendUserMessage: (message: string) => messages.push(message), sendMessage: (message: any) => messages.push(message),
    setModel: async (model: any) => { ctx.model = model; return true; }, setThinkingLevel: () => {}, getThinkingLevel: () => "medium",
  };
  const ctx: any = { cwd: "/home/dev/project", mode: "tui", hasUI: true, model: { provider: "openai-codex", id: "gpt-6-sol" },
    ui: { notify: () => {}, select: async () => undefined, editor: async () => undefined, confirm: async () => true }, abort: () => {},
    modelRegistry: { find: (provider: string, id: string) => ({ provider, id }), isUsingOAuth: () => true,
      getProvider: () => ({ auth: { oauth: { isSubscription: true } } }), getProviderAuthStatus: () => ({ configured: true, source: "stored" }) },
    sessionManager: { getSessionId: () => "session", getBranch: () => entries },
  };
  return { pi, ctx, handlers, tools, commands, entries, messages, emit: (name: string, event: any = {}) => handlers.get(name)?.(event, ctx) };
}

test("local complaints are private, redacted, bounded and never require approval", async () => {
  const home = await mkdtemp(join(homedir(), ".tmp/worklane-complaint-"));
  try {
    const row = await writeComplaint(home, "Broken tool; Bearer secret-value sk-abcdefghijklmnop");
    assert.equal((await stat(row.path)).mode & 0o777, 0o600);
    assert.doesNotMatch(await readFile(row.path, "utf8"), /secret-value|sk-abcdefghijklmnop/);
    await assert.rejects(writeComplaint(home, ""), /Invalid/);
    const abort = new AbortController(); abort.abort();
    await assert.rejects(writeComplaint(home, "feedback", abort.signal), /cancelled/);
    const h = harness(); complain(h.pi);
    assert.equal(h.handlers.has("tool_call"), false);
    assert.match(h.tools.get("lane_complain").description, /without approval/);
    const bad = await mkdtemp(join(homedir(), ".tmp/worklane-complaint-bad-"));
    try {
      await symlink(home, join(bad, ".local"));
      await assert.rejects(writeComplaint(bad, "test"), /Unsafe/);
    } finally { await rm(bad, { recursive: true }); }
  } finally { await rm(home, { recursive: true }); }
});

test("questionnaire is always registered, supports RPC/free text and cancellation", async () => {
  const h = harness(); questionnaire(h.pi);
  assert.equal(h.tools.has("questionnaire"), true);
  assert.equal(h.handlers.size, 0);
  const questions = [{ id: "scope", prompt: "Which scope?", options: [{ label: "Small" }] }];
  h.ctx.mode = "rpc";
  h.ctx.ui.select = async (_title: any, labels: string[]) => labels[0];
  assert.deepEqual((await askQuestions(questions, h.ctx)).answers, [{ id: "scope", answer: "Small", wasCustom: false }]);
  h.ctx.ui.select = async (_title: any, labels: string[]) => labels[1];
  h.ctx.ui.editor = async () => "Custom scope";
  assert.equal((await askQuestions(questions, h.ctx)).answers[0].wasCustom, true);
  h.ctx.ui.select = async () => undefined;
  assert.equal((await askQuestions(questions, h.ctx)).cancelled, true);
  h.ctx.mode = "json";
  assert.equal((await askQuestions(questions, h.ctx)).reason, "ui_unavailable");
  h.ctx.mode = "tui";
  await assert.rejects(askQuestions([...questions, ...questions], h.ctx), /unique/);
});

test("plans are conversational artifacts with no mode/tool/model restrictions", async () => {
  const h = harness();
  let row: any = { id: "plan-id", revision: 1, content_sha256: "hash", status: "review", plan: "Plan", steps: [] };
  const calls: any[] = [];
  plans(h.pi, async (action, fields = {}) => { calls.push({ action, fields }); return row; });
  assert.equal(h.handlers.has("tool_call"), false);
  assert.equal(h.handlers.has("before_provider_request"), false);
  await h.tools.get("plan_propose").execute("p", { plan: "Plan", steps: [] }, undefined, undefined, h.ctx);
  assert.equal(calls[0].fields.project, h.ctx.cwd);
  assert.equal(calls[0].fields.source, "agent");
  await h.emit("input", { source: "extension", text: "Implement the plan." });
  assert.equal(calls.length, 1); // Peer/extension messages are not human consent.
  const result = await h.emit("input", { source: "interactive", text: "Implement the plan." });
  assert.match(result.text, /human-approved/);
  assert.deepEqual(calls.slice(1).map(call => call.action), ["plan.get", "plan.approve", "plan.implement"]);
  row = { ...row, revision: 2, content_sha256: "new-hash" };
  const stale = await h.emit("input", { source: "interactive", text: "Approve the plan." });
  assert.match(stale.text, /stale approval was not accepted/);
  await h.emit("session_start");
  assert.equal(h.tools.has("plan_update"), true);
});

test("subscription defaults use normal APIs and preserve deliberate choices", async () => {
  const h = harness(); const thinking: string[] = [];
  h.pi.setThinkingLevel = (value: string) => thinking.push(value);
  subscription(h.pi);
  await h.emit("session_start");
  assert.equal(h.ctx.model.id, "gpt-6-sol"); assert.deepEqual(thinking, ["medium"]);
  await h.emit("model_select", { source: "set", model: { provider: "openai-codex", id: "other-subscription-model" } });
  await h.emit("session_start");
  assert.equal(h.ctx.model.id, "other-subscription-model");
  await h.emit("thinking_level_select", { level: "high", previousLevel: "medium" });
  await h.emit("session_start");
  assert.equal(thinking.at(-1), "high");
  const argv = [...process.argv];
  try {
    process.argv.push("--model", "openai-codex/cli-choice");
    h.ctx.model = { provider: "openai-codex", id: "cli-choice" };
    await h.emit("session_start");
    assert.equal(h.ctx.model.id, "cli-choice");
    assert.equal(h.entries.at(-1).data.model, "cli-choice");
  } finally { process.argv.splice(0, process.argv.length, ...argv); }
  await h.emit("before_provider_request");
  h.ctx.modelRegistry.isUsingOAuth = (model: any) => ['openai-codex', 'anthropic'].includes(model.provider);
  h.ctx.model = { provider: "anthropic", id: "claude-subscription" };
  await assert.rejects(h.emit("before_provider_request"), /Unexpected/); // No silent provider switch.
  await h.emit("model_select", { source: "set", model: h.ctx.model });
  await h.emit("session_start");
  await h.emit("before_provider_request"); // Deliberate subscription alternative is allowed.
  assert.equal(h.ctx.model.provider, "anthropic");
  h.ctx.modelRegistry.getProviderAuthStatus = () => ({ configured: true, source: "runtime" });
  await assert.rejects(h.emit("before_provider_request"), /No API-key/);
  h.ctx.modelRegistry.getProviderAuthStatus = () => ({ configured: true, source: "stored" });
  h.ctx.modelRegistry.getProvider = () => ({ auth: { oauth: { isSubscription: false } } });
  await assert.rejects(h.emit("before_provider_request"), /No API-key/); // OAuth alone is not a subscription.
  h.ctx.modelRegistry.getProvider = () => ({ auth: { oauth: { isSubscription: true } } });
  h.ctx.model = { provider: "openrouter", id: "paid-model" };
  await h.emit("model_select", { source: "set", model: h.ctx.model });
  await assert.rejects(h.emit("before_provider_request"), /No API-key/);
  h.ctx.model = { provider: "openai-codex", id: "gpt-6-sol" };
  await h.emit("model_select", { source: "set", model: h.ctx.model });
  h.ctx.modelRegistry.isUsingOAuth = () => false;
  await assert.rejects(h.emit("before_provider_request"), /own openai-codex/);
});

test("missing subscription catalog keeps normal login available but cannot fall back", async () => {
  const h = harness(); h.ctx.modelRegistry.find = () => undefined;
  subscription(h.pi);
  await h.emit("session_start");
  await assert.rejects(h.emit("before_provider_request"), /no silent fallback/);
});

test("unauthenticated model selection failure keeps login open and requests fail closed", async () => {
  const h = harness();
  h.ctx.model = { provider: "openrouter", id: "legacy" };
  h.pi.setModel = async () => { throw new Error("No API key"); };
  subscription(h.pi);
  await h.emit("session_start");
  await assert.rejects(h.emit("before_provider_request"), /no silent fallback/);
  h.ctx.model = { provider: "openai-codex", id: "gpt-6-sol" };
  await h.emit("model_select", { source: "set", model: h.ctx.model });
  await h.emit("before_provider_request");
});

test("notifications use settled boundary, omit question contents and sanitize terminal codes", async () => {
  const h = harness();
  const events: any[] = [];
  notifications(h.pi, async fields => { events.push(fields); return { id: "event", terminal: false }; }, () => {});
  await h.emit("session_start"); await h.emit("agent_start");
  await h.emit("message_end", { message: { role: "assistant", stopReason: "stop" } });
  assert.equal(events.length, 0);
  await h.emit("agent_settled");
  assert.equal(events.length, 1); assert.equal(events[0].kind, "response");
  await h.emit("agent_settled"); assert.equal(events.length, 1);
  await h.emit("agent_start");
  h.pi.events.emit("worklane:notification", { kind: "question", key: "question:session:tool", secretQuestion: "never publish" });
  await h.emit("agent_settled"); await h.emit("session_shutdown");
  assert.equal(events.length, 2); assert.doesNotMatch(JSON.stringify(events), /never publish/);
  assert.doesNotMatch(terminalTitle("bad\x1b]9;injection\x07"), /[\x00-\x1f]/);
});
