import { writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { resolve, join } from "node:path";
import { Type } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { socketRequest, toolData, type Request, type Fields } from "./collab.ts";

/** Plan management only: deliberately no tool_call/context/model/loadout hooks. */
type ReviewedPlan = { id: string; revision: number; content_sha256: string };
export default function plans(pi: ExtensionAPI, request: Request = (action, fields, signal) => socketRequest(join(homedir(), ".local/share/worklane/collab.sock"), action, fields, signal)) {
  let active: string | undefined;
  let reviewed: ReviewedPlan | undefined;
  const linked = (row: any) => {
    active = row.id;
    reviewed = { id: row.id, revision: row.revision, content_sha256: row.content_sha256 };
    pi.appendEntry("worklane-plan-link", reviewed);
    if (row.status === "review") pi.events.emit("worklane:notification", { kind: "plan_review", key: `plan:${row.id}:${row.revision}`, outcome: "waiting" });
    return toolData(row);
  };
  const planFields = { plan: Type.String({ minLength: 1, maxLength: 50000 }), steps: Type.Optional(Type.Array(Type.Object({
    id: Type.String({ pattern: "^[a-zA-Z0-9_-]{1,64}$" }), title: Type.String({ minLength: 1, maxLength: 500 }),
  }), { maxItems: 100 })) };
  const revisionFields = { id: Type.String(), revision: Type.Integer({ minimum: 1 }) };
  const evidence = Type.Array(Type.String({ minLength: 1, maxLength: 1000 }), { maxItems: 32 });
  pi.on("session_start", async (_event, ctx) => {
    active = undefined;
    reviewed = undefined;
    for (const entry of ctx.sessionManager.getBranch()) {
      if (entry.type === "custom" && entry.customType === "worklane-plan-link") {
        reviewed = entry.data as ReviewedPlan;
        active = reviewed?.id;
      }
    }
  });
  // Only actual human input may accept the displayed revision. Peer custom
  // messages and extension-generated prompts never constitute human review.
  pi.on("input", async (event, ctx) => {
    if (!["interactive", "rpc"].includes(event.source) || !reviewed || event.streamingBehavior) return;
    const match = /^(approve|implement) (?:the )?plan[.!]?$/i.exec(event.text.trim());
    if (!match) return;
    const row = await request("plan.get", { id: reviewed.id });
    if (row.revision !== reviewed.revision || row.content_sha256 !== reviewed.content_sha256) {
      if (ctx.hasUI) ctx.ui.notify("Plan changed since it was displayed. Review the current revision before approval.", "warning");
      return { action: "transform" as const, text: "The plan revision changed since review. Show the current plan for human review; the stale approval was not accepted." };
    }
    if (row.status === "review") await request("plan.approve", { ...reviewed, source: "human" });
    if (match[1].toLowerCase() === "implement") {
      const changed = await request("plan.implement", { ...reviewed, source: "human" });
      linked(changed);
      return { action: "transform" as const, text: `Implement human-approved plan ${row.id}, revision ${row.revision}. Track progress with plan_step_update.\n\n${row.plan}` };
    }
    return { action: "transform" as const, text: `Human approved plan ${row.id}, revision ${row.revision}. Approval alone did not start implementation.` };
  });
  pi.registerTool({ name: "plan_read", label: "Read tracked plan", description: "Read the current plan or list project plans; ask if multiple plans are applicable.",
    parameters: Type.Object({ id: Type.Optional(Type.String()) }),
    async execute(_id, args, signal, _update, ctx) {
      const id = args.id || active;
      if (id) return linked(await request("plan.get", { id }, signal));
      const rows = await request("plan.list", { project: ctx.cwd }, signal);
      return rows.length === 1 ? linked(rows[0]) : toolData({ plans: rows, selection_required: rows.length > 1 });
    },
  });
  pi.registerTool({ name: "plan_propose", label: "Propose plan", executionMode: "sequential",
    description: "Submit a complete Markdown plan and optional stable steps for human review. Does not restrict tools or start implementation.",
    parameters: Type.Object(planFields),
    async execute(_id, args, signal, _update, ctx) { return linked(await request("plan.propose", { ...args, source: "agent", project: ctx.cwd, session_id: ctx.sessionManager.getSessionId() }, signal)); },
  });
  pi.registerTool({ name: "plan_update", label: "Revise plan", executionMode: "sequential",
    description: "Submit a new complete plan revision with the expected current revision. Content changes invalidate approval; history is retained.",
    parameters: Type.Object({ ...revisionFields, ...planFields }),
    async execute(_id, args, signal) { return linked(await request("plan.update", { ...args, source: "agent" }, signal)); },
  });
  pi.registerTool({ name: "plan_step_update", label: "Track plan step", executionMode: "sequential",
    description: "Track an implementing plan step. Done requires verification evidence; tracking does not change plan content or approval.",
    parameters: Type.Object({ ...revisionFields, step_id: Type.String(), status: Type.Union([Type.Literal("pending"), Type.Literal("in_progress"), Type.Literal("blocked"), Type.Literal("done")]), evidence: Type.Optional(evidence) }),
    async execute(_id, args, signal) { return toolData(await request("plan.step", { ...args, source: "agent" }, signal)); },
  });
  pi.registerTool({ name: "plan_finish", label: "Complete tracked plan", executionMode: "sequential",
    description: "Record verified completion after all steps are done. Does not infer completion from idle state or send external notifications.",
    parameters: Type.Object({ ...revisionFields, evidence }),
    async execute(_id, args, signal) {
      const row = await request("plan.finish", { ...args, source: "agent" }, signal);
      pi.events.emit("worklane:notification", { kind: "task", key: `plan-finished:${row.id}:${row.revision}`, outcome: "verified" });
      return toolData(row);
    },
  });
  pi.registerCommand("plan", { description: "Manage plans: start, show, review, approve, revise, implement, archive, export PATH",
    async handler(input, ctx) {
      const [action = "show", ...rest] = input.trim().split(/\s+/).filter(Boolean);
      if (action === "start") {
        pi.sendUserMessage(rest.join(" ") || "Propose a plan for human review using plan_propose. Use questionnaire for clarification when useful.");
        return;
      }
      if (!active) {
        const rows = await request("plan.list", { project: ctx.cwd });
        if (!rows.length) throw new Error("No plan for this project; use /plan start");
        if (!ctx.hasUI) throw new Error("Use an interactive session to select a project plan");
        const choices = rows.map((row: any) => `${row.id} — ${row.status} — revision ${row.revision}`);
        const selected = await ctx.ui.select("Select a plan", choices);
        if (!selected) return;
        linked(rows[choices.indexOf(selected)]);
      }
      const row = await request("plan.get", { id: active });
      if (["show", "review"].includes(action)) {
        pi.sendMessage({ customType: "worklane-plan-review", display: true, content: `Plan ${row.id}, revision ${row.revision} (${row.status})\n\n${row.plan}\n\nSteps:\n${row.steps.map((step: any) => `${step.id}: ${step.status} — ${step.title}`).join("\n")}`, details: { id: row.id, revision: row.revision } });
        return;
      }
      if (action === "export") {
        if (!rest.length) throw new Error("Usage: /plan export PATH");
        const path = resolve(ctx.cwd, rest.join(" "));
        await writeFile(path, row.plan + "\n", { flag: "wx", mode: 0o600 });
        if (ctx.hasUI) ctx.ui.notify(`Exported plan to ${path}`, "info");
        return;
      }
      if (!["approve", "revise", "implement", "archive"].includes(action)) throw new Error("Unknown /plan action");
      if (!ctx.hasUI || !["tui", "rpc"].includes(ctx.mode)) throw new Error("Human plan review requires interactive UI");
      if (!await ctx.ui.confirm(`${action} plan revision ${row.revision}?`, row.plan)) return;
      const changed = await request("plan." + action, { id: row.id, revision: row.revision, content_sha256: row.content_sha256, source: "human" });
      linked(changed);
      if (action === "implement") pi.sendUserMessage(`Implement approved plan ${row.id}, revision ${row.revision}. Track steps with plan_step_update and verification evidence.\n\n${row.plan}`);
      else ctx.ui.notify(`Plan ${row.id}: ${changed.status}`, "info");
    },
  });
}
