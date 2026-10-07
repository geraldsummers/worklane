import { spawn } from "node:child_process";
import { stat } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { Type } from "@earendil-works/pi-ai";
import type { ExtensionAPI, ExtensionToolContext } from "@earendil-works/pi-coding-agent";
import { authorizeRun, socketRequest, toolData, type Fields } from "./collab.ts";

const HELPER_HOME = join(homedir(), ".p0");
const SECRET_KEY = /(?:secret|token|password|credential|authorization|cookie|private.?key|lease)/i;

/** Defense in depth for broker/helper output; never expose credential-shaped fields. */
export function redact(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(redact);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).filter(([key]) => !SECRET_KEY.test(key)).map(([key, item]) => [key, redact(item)]));
  if (typeof value === "string") return value.replace(/\bBearer\s+\S+/gi, "Bearer [redacted]").replace(/\b(?:sk-[\w-]{12,}|eyJ[\w-]+\.[\w-]+\.[\w-]+)\b/g, "[redacted]");
  return value;
}

export type HelperOperation = "mail.list" | "mail.read" | "mail.send" | "matrix.whoami" | "matrix.rooms" | "matrix.sync" | "matrix.send" | "matrix.join" | "matrix.create_room" | "memory.add" | "memory.search";
export function helperCommand(operation: HelperOperation, args: Fields): { file: string; argv: string[]; input?: string; mutation: boolean } {
  switch (operation) {
    case "mail.list": return { file: "mail.py", argv: ["list", "--limit", String(args.limit ?? 20)], mutation: false };
    case "mail.read": return { file: "mail.py", argv: ["read", String(args.uid)], mutation: false };
    case "mail.send":
      if (/[\r\n]/.test(String(args.to)) || /[\r\n]/.test(String(args.subject))) throw new Error("Invalid mail header");
      return { file: "mail.py", argv: ["send", "--to", String(args.to), "--subject", String(args.subject), "--body-file", "-"], input: String(args.body), mutation: true };
    case "matrix.whoami": return { file: "matrix.py", argv: ["whoami"], mutation: false };
    case "matrix.rooms": return { file: "matrix.py", argv: ["rooms"], mutation: false };
    case "matrix.sync": return { file: "matrix.py", argv: ["sync", "--timeout", String(args.timeout ?? 0), ...(args.since ? ["--since", String(args.since)] : [])], mutation: false };
    case "matrix.send": return { file: "matrix.py", argv: ["send", String(args.room_id), String(args.message)], mutation: true };
    case "matrix.join": return { file: "matrix.py", argv: ["join", String(args.room_id)], mutation: true };
    case "matrix.create_room": return { file: "matrix.py", argv: ["create-room", String(args.name)], mutation: true };
    case "memory.add": return { file: "memory.py", argv: ["add", String(args.content)], mutation: true };
    case "memory.search": return { file: "memory.py", argv: ["search", String(args.query)], mutation: false };
    default: throw new Error("Unsupported P0 helper operation");
  }
}

/** Foreground, awaited helper only. No shell, background agents, credentials in argv, or raw stderr. */
export async function runHelper(file: string, argv: string[], input: string | undefined, signal?: AbortSignal, timeoutMs = 60000): Promise<unknown> {
  const path = join(HELPER_HOME, file);
  if (!(await stat(path).catch(() => undefined))?.isFile()) throw new Error(`Lane-scoped .p0/${file} is not provisioned`);
  if (signal?.aborted) throw new Error("P0 operation cancelled");
  return new Promise((resolve, reject) => {
    const child = spawn("python3", [path, ...argv], { stdio: ["pipe", "pipe", "pipe"], signal });
    let stdout = "";
    let finished = false;
    const finish = (error?: Error, value?: unknown) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      error ? reject(error) : resolve(value);
    };
    const timer = setTimeout(() => { child.kill("SIGKILL"); finish(new Error("P0 helper timed out")); }, timeoutMs);
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
      if (Buffer.byteLength(stdout) > 2 * 1024 * 1024) { child.kill("SIGKILL"); finish(new Error("P0 helper output too large")); }
    });
    child.stderr.resume(); // Error messages may contain credentials; never return them.
    child.stdin.on("error", () => {});
    child.on("error", () => finish(new Error("P0 helper failed or was cancelled")));
    child.on("close", code => {
      if (code !== 0) { finish(new Error("P0 helper rejected the operation")); return; }
      try { finish(undefined, redact(JSON.parse(stdout))); }
      catch { finish(new Error("Invalid P0 helper response")); }
    });
    child.stdin.end(input ?? "");
  });
}

export async function requireMutationApproval(operation: string, args: Fields, ctx: Pick<ExtensionToolContext, "mode" | "ui">, signal?: AbortSignal) {
  // An execution run cannot fall back to an interactive prompt and escape its exact plan.
  if (process.env.WORKLANE_RUN_ID) {
    if (await authorizeRun(operation, args, signal)) return;
    throw new Error("External action does not exactly match this approved run");
  }
  if (ctx.mode !== "tui" || !await ctx.ui.confirm("Approve external P0 action?", `${operation}\n${JSON.stringify(args)}`)) throw new Error("Explicit human TUI confirmation is required");
}

export function stackReadFields(params: Fields): { action: string; args: Fields } {
  switch (params.action) {
    case "status": case "diagnostics": return { action: params.action, args: {} };
    case "verify":
      if (!params.release) throw new Error("verify requires a release hash");
      return { action: "verify", args: { release: params.release } };
    case "operation-status":
      if (!params.operation_id) throw new Error("operation-status requires an operation ID");
      return { action: "operation-status", args: { operation_id: params.operation_id } };
    case "logs": {
      const domain = params.domain ?? "rootful";
      if (!params.unit || (domain !== "rootful" && !params.release)) throw new Error("logs requires a unit and rootless domains require a release hash");
      return { action: "logs", args: { domain, unit: params.unit, ...(params.release ? { release: params.release } : {}) } };
    }
    default: throw new Error("Unsupported stack read action");
  }
}

export default function p0(pi: ExtensionAPI) {
  const text = Type.String({ minLength: 1, maxLength: 16000 });
  const run = async (operation: HelperOperation, args: Fields, signal: AbortSignal | undefined, ctx: ExtensionToolContext) => {
    const command = helperCommand(operation, args);
    if (command.mutation) await requireMutationApproval(`p0.${operation}`, args, ctx, signal);
    return toolData(await runHelper(command.file, command.argv, command.input, signal, operation === "memory.search" ? 95000 : 60000));
  };
  pi.registerTool({ name: "p0_status", label: "P0 capabilities", description: "Report available lane-scoped helper files only. Never reads or returns identity secrets.", parameters: Type.Object({}), async execute() {
    const capabilities = await Promise.all(["mail", "matrix", "memory"].map(async name => [name, (await stat(join(HELPER_HOME, `${name}.py`)).catch(() => undefined))?.isFile() ?? false]));
    let memorySearch = false;
    try {
      const info = await runHelper("memory.py", ["capabilities"], undefined, undefined, 5000) as {actions?: string[]};
      memorySearch = info.actions?.includes("search") === true;
    } catch { /* Older/unprovisioned scoped helpers do not imply search authority. */ }
    return toolData({ helpers: Object.fromEntries(capabilities), memory_search: memorySearch });
  } });
  pi.registerTool({ name: "p0_mail_list", label: "List agent inbox", description: "Read the lane agent mailbox through existing .p0/mail.py.", parameters: Type.Object({ limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 100 })) }), async execute(_id, args, signal, _update, ctx) { return run("mail.list", { limit: args.limit ?? 20 }, signal, ctx); } });
  pi.registerTool({ name: "p0_mail_read", label: "Read agent mail", description: "Read one lane agent mailbox UID without marking it read.", parameters: Type.Object({ uid: Type.Integer({ minimum: 1 }) }), async execute(_id, args, signal, _update, ctx) { return run("mail.read", args, signal, ctx); } });
  pi.registerTool({ name: "p0_mail_send", label: "Send agent mail", description: "External mutation: requires exact approved run action p0.mail.send or human TUI confirmation.", parameters: Type.Object({ to: Type.String({ minLength: 1, maxLength: 320 }), subject: Type.String({ maxLength: 200 }), body: text }), async execute(_id, args, signal, _update, ctx) { return run("mail.send", args, signal, ctx); } });
  pi.registerTool({ name: "p0_matrix_whoami", label: "Matrix identity", description: "Return public Matrix user identity, never access tokens.", parameters: Type.Object({}), async execute(_id, args, signal, _update, ctx) { return run("matrix.whoami", {}, signal, ctx); } });
  pi.registerTool({ name: "p0_matrix_rooms", label: "Matrix rooms", description: "List rooms joined by this lane's Matrix identity.", parameters: Type.Object({}), async execute(_id, args, signal, _update, ctx) { return run("matrix.rooms", {}, signal, ctx); } });
  pi.registerTool({ name: "p0_matrix_sync", label: "Read Matrix events", description: "Read bounded events for the lane Matrix identity.", parameters: Type.Object({ since: Type.Optional(text), timeout: Type.Optional(Type.Integer({ minimum: 0, maximum: 30000 })) }), async execute(_id, args, signal, _update, ctx) { return run("matrix.sync", { ...args, timeout: args.timeout ?? 0 }, signal, ctx); } });
  pi.registerTool({ name: "p0_matrix_send", label: "Send Matrix message", description: "External mutation: approved exact p0.matrix.send action or human TUI confirmation required.", parameters: Type.Object({ room_id: text, message: text }), async execute(_id, args, signal, _update, ctx) { return run("matrix.send", args, signal, ctx); } });
  pi.registerTool({ name: "p0_matrix_join", label: "Join Matrix room", description: "External mutation: approved exact p0.matrix.join action or human TUI confirmation required.", parameters: Type.Object({ room_id: text }), async execute(_id, args, signal, _update, ctx) { return run("matrix.join", args, signal, ctx); } });
  pi.registerTool({ name: "p0_matrix_create_room", label: "Create private Matrix room", description: "External mutation: approved exact p0.matrix.create_room action or human TUI confirmation required.", parameters: Type.Object({ name: Type.String({ minLength: 1, maxLength: 200 }) }), async execute(_id, args, signal, _update, ctx) { return run("matrix.create_room", args, signal, ctx); } });
  pi.registerTool({ name: "p0_memory_add", label: "Queue project memory", description: "Queue a memory using existing .p0/memory.py add; requires p0.memory.add approval or human TUI confirmation.", parameters: Type.Object({ content: Type.String({ minLength: 1, maxLength: 4000 }) }), async execute(_id, args, signal, _update, ctx) { return run("memory.add", args, signal, ctx); } });
  pi.registerTool({ name: "p0_memory_search", label: "Search scoped memory", description: "Search owner personal and this exact lane's project memories through .p0/memory.py search. No owner/project routing options; fails closed if helper/drain is unavailable.", parameters: Type.Object({ query: Type.String({ minLength: 1, maxLength: 4000 }) }), async execute(_id, args, signal, _update, ctx) { return run("memory.search", args, signal, ctx); } });

  const digest = Type.String({ pattern: "^[a-f0-9]{64}$" });
  const identifier = Type.String({ pattern: "^[a-f0-9]{32}$" });
  const broker = async (action: string, args: Fields, signal?: AbortSignal) => toolData(redact(await socketRequest("/run/platform-zero/host-broker.sock", action, args, signal, 180000)));
  pi.registerTool({ name: "p0_stack_read", label: "Read stack broker evidence", description: "Typed status/diagnostics/verify/logs/operation-status only. Broker peer authorization and scrubbing remain authoritative.", parameters: Type.Object({
    action: Type.Union(["status", "diagnostics", "verify", "logs", "operation-status"].map(action => Type.Literal(action))),
    release: Type.Optional(digest),
    domain: Type.Optional(Type.String({ pattern: "^[a-z][a-z0-9-]{0,63}$", default: "rootful" })),
    unit: Type.Optional(Type.String({ pattern: "^webservices-[a-zA-Z0-9_.@-]+\\.service$", maxLength: 160 })),
    operation_id: Type.Optional(identifier),
  }), async execute(_id, params, signal) { const { action, args } = stackReadFields(params); return broker(action, args, signal); } });
  pi.registerTool({ name: "p0_stack_plan", label: "Create broker deployment proposal", description: "Stages a bundle and writes a broker proposal, never applies it. Requires exact p0_stack_plan approval or human TUI confirmation. Existing broker bundle-path/hash checks unchanged.", parameters: Type.Object({ bundle: Type.String({ minLength: 1, maxLength: 1024 }), sha256: digest }), async execute(_id, params, signal, _update, ctx) {
    await requireMutationApproval("p0_stack_plan", params, ctx, signal);
    return broker("plan", params, signal);
  } });
  pi.registerTool({ name: "p0_stack_apply", label: "Apply broker proposal", description: "External mutation: requires exact approved p0_stack_apply {plan_id} or explicit human TUI confirmation. Mandatory host-broker approval remains unchanged.", parameters: Type.Object({ plan_id: identifier }), async execute(_id, params, signal, _update, ctx) {
    await requireMutationApproval("p0_stack_apply", params, ctx, signal);
    return broker("apply", params, signal);
  } });
  pi.registerTool({ name: "p0_stack_rollback", label: "Roll back broker operation", description: "External mutation: exact approved p0_stack_rollback {operation_id} or human TUI confirmation. Configuration rollback only, no data rollback or break-glass approval.", parameters: Type.Object({ operation_id: identifier }), async execute(_id, params, signal, _update, ctx) {
    await requireMutationApproval("p0_stack_rollback", params, ctx, signal);
    return broker("rollback", params, signal);
  } });
  pi.registerTool({ name: "p0_stack_test", label: "Queue broker synthetic test", description: "External mutation: exact approved p0_stack_test {suite} or human TUI confirmation. Host broker retains its synthetic-data allowlist.", parameters: Type.Object({ suite: Type.Union(["ts-unit", "ts-sso", "ts-mobile-smoke", "ts-mobile-auth", "ts-e2e-visual", "ts-e2e-huly", "ts-e2e-jupyterhub", "android-apps", "android-apps-matrix"].map(name => Type.Literal(name))) }), async execute(_id, params, signal, _update, ctx) {
    await requireMutationApproval("p0_stack_test", params, ctx, signal);
    return broker("test", params, signal);
  } });
}
