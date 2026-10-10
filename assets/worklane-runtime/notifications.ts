import { randomUUID } from "node:crypto";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { agentRequest, type Fields } from "./collab.ts";

export function terminalTitle(value: string) {
  return value.replace(/[\x00-\x1f\x7f-\x9f]/g, " ").replace(/\s+/g, " ").slice(0, 200);
}
const TITLES: Record<string, string> = { plan_review: "Plan ready for review", question: "Question waiting for you", response: "Agent response finished", task: "Task complete" };

/** Independent terminal + durable ntfy delivery; no dismissal/approval coupling. */
export default function notifications(pi: ExtensionAPI, enqueue = (fields: Fields) => agentRequest("notification.enqueue", fields), terminal = (title: string) => { process.stdout.write(`\x1b]9;${title}\x1b\\`); }) {
  let ctx: ExtensionContext | undefined;
  let response: string | undefined;
  let outcome = "settled";
  let special = false;
  let pending: Promise<void> = Promise.resolve();
  const deliver = (kind: string, key: string, status: string) => {
    const current = ctx;
    if (!current) return;
    const title = terminalTitle(`${TITLES[kind] || "Worklane"} — ${process.env.WORKLANE_SESSION || process.env.HERDR_SESSION || "lane"}${process.env.HERDR_PANE_ID ? " / " + process.env.HERDR_PANE_ID : ""}${status !== "settled" ? " — " + status : ""}`);
    pending = pending.then(async () => {
      // The runtime chooses routing/settings. No recipient, topic, host or token
      // is accepted from the agent. Persist before any terminal/remote delivery.
      const result = await enqueue({ kind, key, title, outcome: status });
      if (result?.terminal !== false && !result?.terminal_delivered && current.mode === "tui" && process.stdout.isTTY) {
        terminal(title);
        await agentRequest("notification.terminal", { id: result.id }).catch(() => {});
      }
    }).catch(() => { if (current.hasUI) current.ui.notify("Notification delivery unavailable; no external delivery proof", "warning"); });
    return pending;
  };
  pi.on("session_start", async (_event, context) => {
    ctx = context;
    response = undefined;
    for (const entry of context.sessionManager.getBranch()) {
      if (entry.type === "custom" && entry.customType === "worklane-response") {
        const state = entry.data as { id: string; settled: boolean };
        response = state.settled ? undefined : state.id;
      }
    }
  });
  pi.on("agent_start", async () => {
    response ??= randomUUID();
    outcome = "settled";
    special = false;
    pi.appendEntry("worklane-response", { id: response, settled: false });
  });
  pi.on("message_end", async event => {
    if (event.message.role === "assistant" && ["error", "aborted"].includes(event.message.stopReason)) outcome = event.message.stopReason;
  });
  const unsubscribe = pi.events.on("worklane:notification", (event: any) => {
    if (!event || !["plan_review", "question", "task"].includes(event.kind) || typeof event.key !== "string") return;
    special = true;
    void deliver(event.kind, event.key, event.outcome || "waiting");
  });
  pi.on("agent_settled", async () => {
    if (!response) return;
    // Review/question already alerts for this response; errors still get their
    // own outcome. No token/tool/retry notifications or task-success inference.
    if (!special || outcome !== "settled") await deliver("response", `response:${response}`, outcome);
    await pending; // Flush special events before managed lifecycle teardown.
    pi.appendEntry("worklane-response", { id: response, settled: true });
    response = undefined;
  });
  pi.on("session_shutdown", async () => { unsubscribe(); await pending; ctx = undefined; });
}
