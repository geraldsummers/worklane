import type { AgentMessage } from "@earendil-works/pi-agent-core";
import type { AssistantMessage, TextContent } from "@earendil-works/pi-ai";
import type {
  ExtensionAPI,
  ExtensionContext,
} from "@earendil-works/pi-coding-agent";
import { extractPlan, isPlanToolAllowed, PLAN_TOOLS } from "./plan-utils.mjs";

type PlanState = {
  enabled: boolean;
  toolsBeforePlan?: string[];
};

function assistantText(message: AssistantMessage): string {
  return message.content
    .filter((part): part is TextContent => part.type === "text")
    .map((part) => part.text)
    .join("\n");
}

function isAssistant(message: AgentMessage): message is AssistantMessage {
  return message.role === "assistant" && Array.isArray(message.content);
}

export default function worklanePlan(pi: ExtensionAPI): void {
  let enabled = false;
  let toolsBeforePlan: string[] | undefined;

  function updateStatus(ctx: ExtensionContext): void {
    ctx.ui.setStatus(
      "worklane-plan",
      enabled ? ctx.ui.theme.fg("warning", "plan · read-only") : undefined,
    );
  }

  function save(): void {
    pi.appendEntry("worklane-plan", {
      enabled,
      toolsBeforePlan,
    } satisfies PlanState);
  }

  function enter(ctx: ExtensionContext): void {
    if (enabled) return;
    toolsBeforePlan = pi.getActiveTools();
    enabled = true;
    pi.setActiveTools([...PLAN_TOOLS]);
    updateStatus(ctx);
    save();
    ctx.ui.notify(
      "Plan mode enabled. Write and shell tools are unavailable to Pi.",
      "info",
    );
  }

  function leave(ctx: ExtensionContext): void {
    if (!enabled) return;
    enabled = false;
    pi.setActiveTools(
      toolsBeforePlan ?? [
        "read",
        "bash",
        "edit",
        "write",
        "web_search",
        "fetch_content",
        "questionnaire",
      ],
    );
    toolsBeforePlan = undefined;
    updateStatus(ctx);
    save();
    ctx.ui.notify("Plan mode ended. Normal pilot tools restored.", "info");
  }

  pi.registerCommand("plan", {
    description: "Toggle read-only planning with interactive questions",
    handler: async (_args, ctx) => (enabled ? leave(ctx) : enter(ctx)),
  });

  pi.on("tool_call", async (event) => {
    if (enabled && !isPlanToolAllowed(event.toolName)) {
      return {
        block: true,
        reason: `Plan mode is read-only: ${event.toolName} is unavailable.`,
      };
    }
  });

  pi.on("before_agent_start", async () => {
    if (!enabled) return;
    return {
      message: {
        customType: "worklane-plan-guidance",
        display: false,
        content: `[PLAN MODE ACTIVE]
Explore and clarify before proposing an implementation. Do not change files or system state.
Use the questionnaire tool for material choices: concise options, a recommended first option, and free-text answers. Ask only questions that inspection cannot settle.
Web search and HTTP page fetching are available. When the plan is decision-complete, finish with a "Plan:" heading followed by numbered steps. Include tests and any deployment boundary. Do not start execution until the user selects it.`,
      },
    };
  });

  pi.on("agent_end", async (event, ctx) => {
    if (!enabled || ctx.mode !== "tui") return;
    const last = [...event.messages].reverse().find(isAssistant);
    if (!last) return;
    const steps = extractPlan(assistantText(last));
    if (!steps) return;

    const choice = await ctx.ui.select("Plan ready — what next?", [
      "Execute the plan",
      "Refine the plan",
      "Stay in plan mode",
    ]);
    if (choice === "Execute the plan") {
      leave(ctx);
      pi.sendUserMessage(
        "Implement the plan just approved. Follow its steps and verify the result.",
        {
          deliverAs: "followUp",
        },
      );
    } else if (choice === "Refine the plan") {
      const refinement = await ctx.ui.editor(
        "What should change in the plan?",
        "",
      );
      if (refinement?.trim()) {
        pi.sendUserMessage(refinement.trim(), { deliverAs: "followUp" });
      }
    }
  });

  pi.on("session_start", async (_event, ctx) => {
    const entry = ctx.sessionManager
      .getBranch()
      .filter(
        (item) => item.type === "custom" && item.customType === "worklane-plan",
      )
      .at(-1);
    if (entry?.type === "custom") {
      const state = entry.data as PlanState;
      enabled = state.enabled === true;
      toolsBeforePlan = state.toolsBeforePlan;
    }
    if (enabled) pi.setActiveTools([...PLAN_TOOLS]);
    updateStatus(ctx);
  });
}
