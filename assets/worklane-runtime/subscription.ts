import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const SUBSCRIPTION_PROVIDER = "openai-codex";
const DEFAULT_MODEL = "gpt-6-sol";

/** Normal Pi OAuth/settings/session APIs; no credential-shape parsing or token stripping. */
export default function subscription(pi: ExtensionAPI) {
  let applying = false;
  let choice: { provider: string; model: string } | undefined;
  pi.on("session_start", async (_event, ctx) => {
    const entry = ctx.sessionManager.getBranch().filter((item: any) => item.type === "custom" && item.customType === "worklane-subscription-choice").at(-1) as any;
    const saved = entry?.data as { provider?: string; model?: string; thinking?: string } | undefined;
    const cliChoice = process.argv.some(arg => arg === '--model' || arg.startsWith('--model=') || arg === '-m' || arg === '--provider' || arg.startsWith('--provider='));
    // Standard CLI model/provider choices are deliberate; OAuth is checked only
    // when a request is made, so normal /login remains usable without auth.
    if (cliChoice && ctx.model) {
      choice = { provider: ctx.model.provider, model: ctx.model.id };
      pi.appendEntry("worklane-subscription-choice", { ...choice, thinking: pi.getThinkingLevel() });
      return;
    }
    const selected = ctx.modelRegistry.find(saved?.provider || SUBSCRIPTION_PROVIDER, saved?.model || DEFAULT_MODEL);
    if (!selected) {
      choice = undefined;
      ctx.ui.notify("Subscription model unavailable. Use normal /login and /model; no automatic fallback is allowed.", "warning");
      return;
    }
    applying = true;
    try {
      if (ctx.model?.provider !== selected.provider || ctx.model.id !== selected.id) {
        // The SDK requires configured auth for setModel. Unauthenticated fresh
        // sessions must still open normally so /login remains available.
        try {
          if (!await pi.setModel(selected)) throw new Error("Model selection unavailable");
        } catch {
          choice = undefined;
          ctx.ui.notify("Subscription selection unavailable. Use normal /login and /model; requests remain blocked without an explicit subscription choice.", "warning");
          return;
        }
      }
      choice = { provider: selected.provider, model: selected.id };
      const cliThinking = process.argv.some(arg => arg === '--thinking' || arg.startsWith('--thinking='));
      if (saved?.thinking && ['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'].includes(saved.thinking)) {
        pi.setThinkingLevel(saved.thinking as ReturnType<typeof pi.getThinkingLevel>);
      } else if (!cliThinking) pi.setThinkingLevel("medium");
      if (cliThinking) pi.appendEntry("worklane-subscription-choice", { ...choice, thinking: pi.getThinkingLevel() });
    } finally { applying = false; }
  });
  pi.on("model_select", async (event) => {
    if (applying || event.source === "restore") return;
    // Normal Pi set/cycle selections persist, including deliberate alternatives
    // backed by the account's own OAuth subscription. API-key models still fail.
    choice = { provider: event.model.provider, model: event.model.id };
    pi.appendEntry("worklane-subscription-choice", { ...choice, thinking: pi.getThinkingLevel() });
  });
  pi.on("thinking_level_select", async (event, ctx) => {
    if (!applying && ctx.model && choice?.provider === ctx.model.provider && choice.model === ctx.model.id) {
      pi.appendEntry("worklane-subscription-choice", { ...choice, thinking: event.level });
    }
  });
  pi.on("before_provider_request", async (_event, ctx) => {
    if (!ctx.model || !choice || ctx.model.provider !== choice.provider || ctx.model.id !== choice.model) {
      ctx.abort();
      throw new Error("Unexpected subscription model change. Choose explicitly through normal Pi; no silent fallback is allowed.");
    }
    const provider = ctx.modelRegistry.getProvider(ctx.model.provider);
    const auth = ctx.modelRegistry.getProviderAuthStatus(ctx.model.provider);
    if (!ctx.modelRegistry.isUsingOAuth(ctx.model) || provider?.auth.oauth?.isSubscription !== true || !auth.configured || auth.source !== "stored") {
      ctx.abort();
      throw new Error(`Sign in to your own ${ctx.model.provider} subscription using normal Pi /login. No API-key or metered-provider fallback is allowed.`);
    }
  });
}
