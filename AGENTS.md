# Lane environment

Worklane lanes have an immutable operating-system root filesystem. Agents do not have `sudo`,
must not attempt `apt`, and must not modify files outside the explicitly writable locations below.
When additional tools are needed, install or build them in user space instead of changing the
system image from inside the lane.

Use these locations for additions and generated state:

- Project dependencies and generated files: `/home/dev`
- Personal tools and binaries: `/home/dev/.local/bin`
- Python virtual environments: `/home/dev/.venv` or a project `.venv`
- Node global packages: `/home/dev/.local`
- Rust-installed binaries: `/home/dev/.local/bin` (`CARGO_INSTALL_ROOT` is configured there)
- Go-installed binaries: `/home/dev/.local/bin` (`GOBIN` is configured there)
- Temporary files: `/tmp` or `/var/tmp`; these are RAM-backed tmpfs mounts, limited to 1 GiB
  each, and discarded when the container stops

The base image already provides common build, debugging, search, archive, network, browser, and
language tooling. Prefer the installed tool before adding a new one.

Herdr is the lane session manager. A normal `worklane lane attach` enters the lane through
a stable lane session, with the lane workspace focused at `/home/dev`.
Use Herdr-managed sessions unless the user explicitly asks for a plain shell.

## Programmatic bulk delegation

For independent bulk classification, extraction, or tagging, prefer the repository's
`scripts/codex-bulk` driver when it is available. Feed it JSONL records with stable IDs and a
JSON output schema. It defaults to `gpt-5.6-luna` with low reasoning, starts with four workers,
adapts concurrency after successful runs, and coordinates queue-wide backoff after HTTP 429s.
Its 256-worker setting is a hard ceiling, not a launch target. Preserve its append-only result
file so completed IDs and retry state survive restarts. Do not replace its shared scheduler with
unbounded background `codex exec` processes or per-process immediate retry loops.

## Git and agent coordination

- Before editing repository files, inspect `git status --short`. Treat existing
  dirty and untracked files as user-owned unless the user explicitly says
  otherwise.
- Agents may delegate concrete, bounded subtasks to other agents when useful
  and remain responsible for the combined result.
- Coordinate through `$HOME/.local/share/worklane/agent-work/`. Use one
  Markdown claim per agent named `agent--<id>.md`, where the canonical agent ID
  loses its leading `/` and every remaining `/` becomes `--`. For example,
  `/root/api` uses `agent--root--api.md`.
- Read all claims at the start of every turn and immediately before editing.
  Each agent updates and removes only its own file. Record the agent ID,
  files/area, status, and an updated UTC timestamp. Treat claims as advisory
  soft locks and coordinate before overlapping.
- Before pushing, run the repository's CI-equivalent tests and validation
  locally. Do not push while a required local check fails.

## Fresh dist builds

When asked for a fresh dist, build the release binaries and refresh the ignored `dist/` directory:

```sh
cargo build --release --target x86_64-unknown-linux-gnu
mkdir -p dist
cp target/x86_64-unknown-linux-gnu/release/worklane target/x86_64-unknown-linux-gnu/release/lane dist/
sha256sum dist/worklane dist/lane
```

Report the two SHA-256 hashes. Do not commit `dist/` artifacts; `dist/` and `target/` are ignored.
