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
- Temporary files: Do not use `/tmp` for anything. Store temporary files under
  `/home/dev/.tmp` (or a project-local temporary directory) instead. `/var/tmp` is also
  unavailable unless the user explicitly authorizes it.

The base image already provides common build, debugging, search, archive, network, browser, and
language tooling. Prefer the installed tool before adding a new one.

JVM SDKs are intentionally user-managed rather than installed from Debian packages. Use SDKMAN
under `$HOME/.sdkman` to install and update Java, Kotlin, Gradle, Maven, and other supported SDKs.
If SDKMAN is absent, install it as the lane user with its upstream installer; do not install it
system-wide. SDKMAN-managed tools persist with the lane workspace and can be upgraded by agents
without rebuilding the immutable image.

Whenever you install or upgrade an SDK, record its name, resolved version, installation method,
and user-space location in the nearest applicable `AGENTS.md` before handing off. Include any
activation or reproducibility command another agent will need. Keep this inventory current when
an SDK is replaced or removed; do not leave essential toolchain state discoverable only from the
current shell or an agent transcript.

Herdr is the lane session manager. A normal `worklane lane attach` enters the lane through
a stable lane session, with the lane workspace focused at `/home/dev`.
Use Herdr-managed sessions unless the user explicitly asks for a plain shell.

## Progress and ETA updates

For work lasting more than a few minutes, send regular progress updates at meaningful phase
boundaries and at least about every five minutes while work is active. Use real UTC timestamps,
an honest phase count, concrete completed work, the current decision or blocker, elapsed time,
and a recalibrated ETA. Do not repeat a stale ETA after new evidence changes the estimate.

Use a compact shape such as:

```text
2026-08-09 13:45 UTC — Status — phase 1/6: contract audit nearly complete
Progress: command routing, reachability model, and dirty-worktree ownership verified
Decision: isolate the implementation in a new analyzer and make only narrow additive edits to the already-dirty CLI/test files
Elapsed: ~6m
ETA: ~45–60m to the deterministic proposal checkpoint; recalibrate after the first successful full-graph run
Tokens: ~18k used / ~55–70k estimated total (best-effort estimate)
```

Estimate token usage when practical, clearly label it as approximate, and revise the estimate as
scope changes. Prefer a useful range over false precision. If exact usage is unavailable, say so
and estimate from elapsed work, tool output, and remaining phases.

## Programmatic bulk delegation

For independent bulk classification, extraction, or tagging, prefer the repository's
`scripts/codex-bulk` driver when it is available. Feed it JSONL records with stable IDs and a
JSON output schema. It defaults to `gpt-5.6-luna` with low reasoning, starts with 32 workers,
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

After every user-facing change to `worklane`, `lane`, their shared core, or their
embedded runtime assets, automatically run the required validation, build the
release binaries, and refresh the ignored `dist/` directory. Do this before
handing the change back; the user must not need to ask for a fresh dist
separately. Also refresh it whenever the user explicitly asks for a fresh dist:

```sh
scripts/release-check
# Inspect every PNG named by the command, record visual-review.txt, then:
scripts/release-check --publish RUN_ID
```

This gate runs formatting, linting, tests, coverage, the complete isolated acceptance matrix on
`gerald@192.168.0.11`, and real-PTY TUI capture. Directly copying binaries into `dist/` is not an
acceptable substitute. Report the two SHA-256 hashes. Do not commit `dist/`, `target/`, or
`artifacts/`; they are ignored.
