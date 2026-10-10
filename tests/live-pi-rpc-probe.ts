// Acceptance-only commands loaded beside the installed production extensions.
// No fake provider, credentials, models or extension APIs. No model requests.
import { askQuestions } from "/home/dev/.pi/agent/extensions/worklane/questionnaire.ts";
import { writeComplaint } from "/home/dev/.pi/agent/extensions/worklane/complain.ts";
import { homedir } from "node:os";
import { stat, readFile } from "node:fs/promises";
export default function probe(pi: any) {
  pi.on("before_provider_request", () => { throw new Error("Acceptance probe forbids all model requests"); });
  pi.registerCommand("acceptance-probe", { handler: async (action: string, ctx: any) => {
    let value: any;
    if (action === "tools") value = { tools: pi.getAllTools().map((t: any) => t.name), mode: ctx.mode, hasUI: ctx.hasUI };
    else if (action === "question") value = await askQuestions([{ id: "scope", prompt: "Synthetic acceptance question", options: [{ label: "Bounded" }] }], ctx);
    else if (action === "complaint") {
      const row = await writeComplaint(homedir(), "Synthetic installed complaint: Bearer acceptance-secret");
      value = { private: ((await stat(row.path)).mode & 0o777) === 0o600, redacted: !(await readFile(row.path, "utf8")).includes("acceptance-secret") };
    } else throw new Error("Unknown acceptance action");
    pi.sendMessage({ customType: "acceptance-result", display: true, content: JSON.stringify(value) });
  }});
}
