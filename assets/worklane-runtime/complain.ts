import { mkdir, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { Type } from "@earendil-works/pi-ai";
import type { ExtensionAPI, ExtensionToolContext } from "@earendil-works/pi-coding-agent";
import { authorizeRun, toolData, type Fields } from "./collab.ts";

const COMPLAINT_DIR = join(homedir(), ".local", "share", "worklane", "lane-complaints");

export async function requireMutationApproval(operation: string, args: Fields, ctx: Pick<ExtensionToolContext, "mode" | "ui">, signal?: AbortSignal) {
  if (process.env.WORKLANE_RUN_ID) {
    if (await authorizeRun(operation, args, signal)) return;
    throw new Error("External action does not exactly match this approved run");
  }
  if (ctx.mode !== "tui" || !await ctx.ui.confirm(`Approve ${operation}?`, `${operation}\n${JSON.stringify(args)}`)) {
    throw new Error("Explicit human TUI confirmation is required");
  }
}

export default function complain(pi: ExtensionAPI) {
  const text = Type.String({ minLength: 1, maxLength: 8000 });

  pi.registerTool({
    name: "lane_complain",
    label: "Log lane complaint",
    description: "Write a free-text complaint about the lane environment to the lane-scoped complaint log. Never exposes secrets.",
    parameters: Type.Object({ body: text }),
    async execute(_id, args, signal, _update, ctx) {
      const body = String(args.body);
      await requireMutationApproval("lane_complain", { body }, ctx, signal);

      const timestamp = new Date().toISOString();
      const paneId = process.env.HERDR_PANE_ID ?? null;
      const session = process.env.HERDR_SESSION ?? null;
      const runId = process.env.WORKLANE_RUN_ID ?? null;
      const id = randomUUID();

      const filename = `complaint--${timestamp.replace(/:/g, "-")}--${id}.md`;
      const filepath = join(COMPLAINT_DIR, filename);

      const frontmatter = [
        "---",
        `timestamp: "${timestamp}"`,
        `pane_id: ${paneId === null ? "null" : `"${paneId}"`}`,
        `session: ${session === null ? "null" : `"${session}"`}`,
        `run_id: ${runId === null ? "null" : `"${runId}"`}`,
        "---",
        "",
        body,
      ].join("\n");

      await mkdir(COMPLAINT_DIR, { recursive: true });
      await writeFile(filepath, frontmatter, { signal });

      return toolData({ complaint_id: filename, path: filepath });
    },
  });
}
