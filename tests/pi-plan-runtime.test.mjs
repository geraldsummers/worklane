import assert from "node:assert/strict";
import test from "node:test";
import worklanePlan from "../assets/pi-pilot/plan.ts";

function harness(entries = []) {
  const commands = new Map();
  const handlers = new Map();
  const sent = [];
  const saved = [];
  const notices = [];
  const choices = [];
  let active = [
    "read",
    "bash",
    "edit",
    "write",
    "web_search",
    "fetch_content",
    "questionnaire",
  ];
  const pi = {
    registerCommand: (name, command) => commands.set(name, command),
    on: (name, handler) => handlers.set(name, handler),
    getActiveTools: () => [...active],
    setActiveTools: (names) => {
      active = [...names];
    },
    appendEntry: (name, data) => saved.push({ name, data }),
    sendUserMessage: (message, options) => sent.push({ message, options }),
  };
  const ctx = {
    mode: "tui",
    ui: {
      theme: { fg: (_kind, text) => text },
      setStatus: () => {},
      notify: (message) => notices.push(message),
      select: async () => choices.shift(),
      editor: async () => choices.shift(),
    },
    sessionManager: { getBranch: () => entries },
  };
  worklanePlan(pi);
  return {
    commands,
    handlers,
    sent,
    saved,
    notices,
    choices,
    ctx,
    getActive: () => active,
  };
}

test("/plan removes mutating tools and blocks stray calls", async () => {
  const h = harness();
  await h.commands.get("plan").handler("", h.ctx);
  assert.equal(h.getActive().includes("bash"), false);
  assert.equal(h.getActive().includes("edit"), false);
  assert.equal(h.getActive().includes("questionnaire"), true);
  assert.equal(h.getActive().includes("web_search"), true);
  assert.match(
    (await h.handlers.get("tool_call")({ toolName: "bash" })).reason,
    /read-only/,
  );
  assert.equal(
    await h.handlers.get("tool_call")({ toolName: "questionnaire" }),
    undefined,
  );
  assert.match(
    (await h.handlers.get("before_agent_start")()).message.content,
    /PLAN MODE ACTIVE/,
  );
  await h.commands.get("plan").handler("", h.ctx);
  assert.equal(h.getActive().includes("bash"), true);
  assert.equal(
    await h.handlers.get("tool_call")({ toolName: "write" }),
    undefined,
  );
});

test("review waits for explicit execution and supports refinement", async () => {
  const h = harness();
  await h.commands.get("plan").handler("", h.ctx);
  const event = {
    messages: [
      {
        role: "assistant",
        content: [
          { type: "text", text: "Plan:\n1. Inspect\n2. Implement\n3. Test" },
        ],
      },
    ],
  };
  h.choices.push("Stay in plan mode");
  await h.handlers.get("agent_end")(event, h.ctx);
  assert.equal(h.sent.length, 0);
  assert.equal(h.getActive().includes("bash"), false);
  h.choices.push("Refine the plan", "Add a rollback check");
  await h.handlers.get("agent_end")(event, h.ctx);
  assert.equal(h.sent[0].message, "Add a rollback check");
  assert.equal(h.getActive().includes("bash"), false);
  h.choices.push("Execute the plan");
  await h.handlers.get("agent_end")(event, h.ctx);
  assert.equal(h.getActive().includes("bash"), true);
  assert.match(h.sent.at(-1).message, /Implement the plan just approved/);
});

test("session resume restores the planning guard", async () => {
  const h = harness([
    {
      type: "custom",
      customType: "worklane-plan",
      data: {
        enabled: true,
        toolsBeforePlan: ["read", "bash", "edit", "write"],
      },
    },
  ]);
  await h.handlers.get("session_start")({}, h.ctx);
  assert.equal(h.getActive().includes("bash"), false);
  assert.match(
    (await h.handlers.get("tool_call")({ toolName: "write" })).reason,
    /read-only/,
  );
  await h.commands.get("plan").handler("", h.ctx);
  assert.deepEqual(h.getActive(), ["read", "bash", "edit", "write"]);
});
