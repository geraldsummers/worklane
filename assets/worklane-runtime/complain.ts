import { lstat, mkdir, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { Type } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { toolData } from "./collab.ts";
import { redact } from "./p0.ts";

/** Local feedback is not an external mutation and needs no approval/daemon. */
export async function writeComplaint(home: string, body: string, signal?: AbortSignal) {
  if (!body.trim() || body.length > 8000 || body.includes("\0")) throw new Error("Invalid complaint body");
  if (signal?.aborted) throw new Error("Complaint cancelled");
  const directory = join(home, ".local/share/worklane/lane-complaints");
  // Validate each existing component: do not follow a redirected log directory.
  let cursor = home;
  for (const part of [".local", "share", "worklane", "lane-complaints"]) {
    cursor = join(cursor, part);
    await mkdir(cursor, { mode: 0o700 }).catch(error => { if (error.code !== "EEXIST") throw error; });
    const info = await lstat(cursor);
    if (!info.isDirectory() || info.isSymbolicLink() || info.uid !== process.getuid?.()) throw new Error("Unsafe complaint directory");
  }
  const timestamp = new Date().toISOString();
  const filename = `complaint--${timestamp.replace(/:/g, "-")}--${randomUUID()}.md`;
  const path = join(directory, filename);
  const metadata = { timestamp, pane_id: process.env.HERDR_PANE_ID ?? null,
    session: process.env.HERDR_SESSION ?? null, run_id: process.env.WORKLANE_RUN_ID ?? null };
  // Recognized patterns only: this is not a guarantee against arbitrary secrets.
  const cleaned = String(redact(body)).replace(/-----BEGIN [^-]*PRIVATE KEY-----[\s\S]*?-----END [^-]*PRIVATE KEY-----/g, "[private key redacted]");
  await writeFile(path, `${JSON.stringify(metadata)}\n\n${cleaned}\n`, { signal, flag: "wx", mode: 0o600 });
  return { complaint_id: filename, path };
}

export default function complain(pi: ExtensionAPI) {
  pi.registerTool({ name: "lane_complain", label: "Log lane complaint",
    description: "Write private lane-local feedback without approval. Recognized credential patterns are redacted; do not include secrets.",
    parameters: Type.Object({ body: Type.String({ minLength: 1, maxLength: 8000 }) }),
    async execute(_id, args, signal) { return toolData(await writeComplaint(homedir(), String(args.body), signal)); },
  });
}
