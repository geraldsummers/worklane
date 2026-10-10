// Adapted from Pi's upstream questionnaire example (MIT): use native dialogs
// so the same clarification tool works in TUI and RPC, independently of plans.
import { Type } from "@earendil-works/pi-ai";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { toolData } from "./collab.ts";

export interface Question { id: string; prompt: string; options: { label: string; description?: string }[]; allowOther?: boolean }
export async function askQuestions(questions: Question[], ctx: Pick<ExtensionContext, "hasUI" | "mode" | "ui">, signal?: AbortSignal) {
  if (!ctx.hasUI || !["tui", "rpc"].includes(ctx.mode)) return { cancelled: true, reason: "ui_unavailable", answers: [] };
  if (!questions.length || questions.length > 3 || new Set(questions.map(q => q.id)).size !== questions.length) throw new Error("Expected 1–3 questions with unique IDs");
  const answers: { id: string; answer: string; wasCustom: boolean }[] = [];
  for (const question of questions) {
    if (signal?.aborted) return { cancelled: true, reason: "cancelled", answers };
    if (!question.id || !question.prompt || !question.options.length || question.options.length > 8 || new Set(question.options.map(o => o.label)).size !== question.options.length) throw new Error("Invalid question/options");
    const labels = question.options.map((o, index) => `${index + 1}. ${o.label}${o.description ? " — " + o.description : ""}`);
    if (question.allowOther !== false) labels.push("Type another answer…");
    const selected = await ctx.ui.select(question.prompt, labels, { signal });
    if (selected === undefined || signal?.aborted) return { cancelled: true, reason: "cancelled", answers };
    const index = labels.indexOf(selected);
    if (index < 0) throw new Error("Invalid question selection");
    const wasCustom = index === question.options.length;
    const answer = wasCustom ? await ctx.ui.editor(question.prompt) : question.options[index].label;
    if (answer === undefined || signal?.aborted) return { cancelled: true, reason: "cancelled", answers };
    if (!answer.trim() || answer.length > 4000) throw new Error("Answer must contain 1–4000 characters");
    answers.push({ id: question.id, answer, wasCustom });
  }
  return { cancelled: false, answers };
}

export default function questionnaire(pi: ExtensionAPI) {
  pi.registerTool({ name: "questionnaire", label: "Ask questions", exposure: "model-only", executionMode: "sequential",
    description: "Ask 1–3 clarifying questions whenever needed, with choices or free text. Available independently of plan management; answers are not plan approval.",
    parameters: Type.Object({ questions: Type.Array(Type.Object({ id: Type.String({ minLength: 1, maxLength: 64 }),
      prompt: Type.String({ minLength: 1, maxLength: 2000 }),
      options: Type.Array(Type.Object({ label: Type.String({ minLength: 1, maxLength: 200 }), description: Type.Optional(Type.String({ maxLength: 500 })) }), { minItems: 1, maxItems: 8 }),
      allowOther: Type.Optional(Type.Boolean()) }), { minItems: 1, maxItems: 3 }) }),
    async execute(id, args, signal, _update, ctx) {
      if (ctx.hasUI) pi.events.emit("worklane:notification", { kind: "question", key: `question:${ctx.sessionManager.getSessionId()}:${id}`, outcome: "waiting" });
      return toolData(await askQuestions(args.questions, ctx, signal));
    },
  });
}
