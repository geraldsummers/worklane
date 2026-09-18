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

Use `$HOME/.local/bin` as the single standard entry point for locally installed executables. It
is already added to `PATH` by the lane bootstrap and base image; do not create a new PATH fragment
for each tool. Install a tool there directly, or place an executable symlink or small launcher in
`$HOME/.local/bin` when the tool must live under another user-space prefix. Launchers must resolve
their paths relative to `$HOME`, must forward arguments with `"$@"`, and must not depend on the
installing agent's transient environment. After installation, verify the command from a fresh
login shell with `zsh -lic 'command -v TOOL && TOOL --version'`. Only add an idempotent export to
`$HOME/.zshenv` when a tool fundamentally cannot be exposed through `$HOME/.local/bin`; never
overwrite shell startup files.

The base image already provides common build, debugging, search, archive, network, browser, and
language tooling. Prefer the installed tool before adding a new one.

## GPU acceleration

Lanes may expose one or more NVIDIA GPUs, but GPU access is profile- and host-dependent. Before
compute-heavy model, media, numerical, or parallel work, probe the environment with `nvidia-smi`
and the selected framework's own availability check. When a usable GPU is present and acceleration
will materially improve the task, prefer an appropriate GPU implementation; retain a CPU fallback
for unavailable, incompatible, or too-small workloads.

Do not install or change host GPU drivers, and do not attempt to add CUDA system packages to the
immutable root filesystem. Install required user-space libraries in the project or other writable
locations described above. Treat GPU memory and compute as shared resources: inspect utilization
when it matters, avoid unnecessary monopolization, and stop GPU processes when the work finishes.

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

Herdr is the lane session manager. `worklane lane attach` enters a persistent session
with the lane workspace at `/home/dev`. Use Herdr-managed sessions unless the user asks
for a plain shell. The session name is fixed at lane creation; renaming the lane changes
its display name, not its Herdr session identity.

## Operating Herdr from an agent

Treat Herdr as a structured control plane for the lane's persistent terminals. Use its CLI
instead of sending interactive prefix-key sequences. Use the injected `HERDR_SESSION`
as the session target, placing
`--session SESSION` immediately after `herdr`. In the examples, set `LANE` to that exact
session name. Outside a managed pane, discover and select the session explicitly.

- Discover state before acting: use `herdr api snapshot`, `herdr workspace list`,
  `herdr pane list`, and `herdr agent list`. These commands return JSON; select stable IDs from
  their output rather than scraping labels or assuming pane order.
- Identify your own pane before interpreting global state. Herdr injects
  `HERDR_SESSION`, `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, and `HERDR_PANE_ID`; record those exact
  values and match `HERDR_PANE_ID` against `agent list` or the snapshot. Do not use the currently
  focused pane as a proxy for your identity because another client or agent may hold focus. If the
  variables are unset, explicitly record that the agent is outside a Herdr pane.
- At the start of every turn, pair the collaboration claims described below with live Herdr agent
  state. `herdr --session "$HERDR_SESSION" agent list` reports recognized agents and their
  `agent_status` (`working`, `blocked`, `idle`, `done`, or `unknown`); the claim files explain what
  those agents intend to change. Use `.pane_id` as the unambiguous live target when an agent has
  no name.
- Treat the two views as complementary rather than interchangeable. A live agent without a claim
  may still own in-progress work, while a claim without a live agent may be stale. Never delete or
  rewrite another agent's claim to reconcile them. When ownership or overlap is unclear, inspect
  the live pane with `herdr agent read PANE_ID --lines 60`, then coordinate with
  `herdr agent prompt PANE_ID 'TEXT'` before editing the same area.
- Inspect work without taking focus using `herdr pane read PANE_ID --source recent-unwrapped
  --lines N` or `herdr agent read TARGET`. Use `herdr pane process-info PANE_ID` when ownership
  of a terminal is unclear.
- Run a command in an existing shell with `herdr pane run PANE_ID 'COMMAND'`. To send a message to
  Codex or another detected agent, use `herdr agent prompt TARGET 'TEXT'`; it submits the text with
  Enter. `herdr pane send-text PANE_ID 'TEXT'` only types into the terminal and does not press
  Enter, so follow it with `herdr agent send-keys TARGET enter` or `herdr pane send-keys PANE_ID
  enter` when submission is intended. Never assume visible typed text was sent. Use raw key input
  only when the higher-level prompt or run commands cannot express the interaction.
- Create isolated work with `herdr pane split PANE_ID --direction right|down --cwd PATH
  --no-focus`, or start a detected agent in an available shell pane with `herdr agent start NAME
  --kind KIND --pane PANE_ID -- [ARG ...]`. Re-list state after creation and retain the returned
  IDs.
- Observe completion with `herdr agent wait TARGET --until idle --until blocked --timeout MS`, then
  read the agent or pane output. Use bounded timeouts and report timeouts as inconclusive, not
  success.
- Do not focus panes merely to inspect them. Do not close panes or workspaces, stop/delete
  sessions, or restart the Herdr server unless the user requested that lifecycle change and the
  exact target was resolved first. Those operations can disrupt other agents and persistent work.

### Sending keystrokes to a Herdr tab

Herdr sends input to panes, not tabs. Resolve the target tab and then the exact pane within it;
never assume that a tab has only one pane or that its first pane is the intended recipient.

```sh
state="$(herdr --session "$LANE" api snapshot)"
tab_id="$(printf '%s\n' "$state" | jq -r '.result.snapshot.tabs[] | select(.label == "TAB_LABEL") | .tab_id')"
printf '%s\n' "$state" | jq -r --arg tab "$tab_id" \
  '.result.snapshot.panes[] | select(.tab_id == $tab) | [.pane_id, (.label // ""), (.agent // "")] | @tsv'
```

Choose one returned `pane_id`, confirm its contents with `herdr --session "$LANE" pane read
PANE_ID --source recent-unwrapped --lines 40`, and send logical keys without focusing the tab:

```sh
herdr --session "$LANE" pane send-keys PANE_ID up up enter
herdr --session "$LANE" pane send-keys PANE_ID ctrl+c
herdr --session "$LANE" pane send-keys PANE_ID shift+tab enter
```

Key names are case-insensitive. Use printable keys such as `a`; special keys such as `enter`,
`tab`, `esc`, `backspace`, and the arrow names `left`, `right`, `up`, and `down`; modifier chords
such as `ctrl+h`, `alt+x`, and `shift+tab`; function keys such as `f1`; and named punctuation such
as `minus`, `plus`, and `backtick`. Each argument is one key event. Prefer `pane run` for a shell
command, `pane send-text` for literal text without Enter, and `agent prompt` or `agent send-keys`
when targeting a detected agent by name. Read the pane again after input when confirmation matters.

### Showing images to the human

The target terminal is Ghostty. When the human needs to inspect an image, use Worklane's Herdr
pane-graphics presenter, which creates a fresh, clearly labeled workspace and tab and focuses the
presentation only after the native image is ready:

```sh
worklane-show-image --session "$LANE" --label "image review" /absolute/path/to/image.png
```

The helper supports PNG directly and uses GraphicsMagick for other common formats. It requires
Herdr's experimental native pane-graphics API and a connected Ghostty frontend; there is no text or
external-renderer fallback. Keep image review in its dedicated workspace rather than reusing a
development pane.

Run the relevant subcommand with `--help` for its exact arguments. Prefer Worklane-managed
session lifecycle; do not manually start a second Herdr server for the lane.

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
- Read all claims at the start of every turn and immediately before editing, and correlate them
  with `herdr agent list` as described above so both declared ownership and current activity inform
  coordination.
  Each agent updates and removes only its own file. Record the agent ID; files/area; status; exact
  Herdr session, workspace, tab, and pane IDs from the injected environment; and an updated UTC
  timestamp. If the agent is outside Herdr, record that instead of inventing IDs. Treat claims as
  advisory soft locks and coordinate before overlapping. If an independent session already
  uses your nominal agent ID, choose a distinct ID using your session and pane identity;
  never overwrite its claim. Remove your claim when your work finishes.
- Follow the target repository’s testing and release requirements. Do not push while
  a required local check fails.

## Progress and ETA updates

For work lasting more than a few minutes, update the user at meaningful phase boundaries
and at least about every five minutes. Include the real UTC time, phase and total phase
count, concrete progress, current decision or blocker, elapsed time, and a recalibrated ETA.
Estimate tokens used and the expected total when practical; label estimates as approximate
and say when exact usage is unavailable. Revise estimates as evidence or scope changes.

```text
2026-09-08 11:15 UTC — phase 2/4: documentation rewrite complete
Progress: shared guidance separated from contributor requirements
Decision: verify migration behavior before the release gate
Elapsed: ~10m; ETA: ~30–45m, subject to acceptance results
Tokens: ~8k used / ~20–30k estimated total (approximate)
```

## Automation and bulk delegation

Before engineering user services, allowance-triggered work, or bulk Codex jobs, read the
companion `automation.md`. In a lane, it is installed at
`$HOME/.local/share/worklane/agent-docs/current/automation.md`; in the Worklane checkout,
it is beside this document. Standard lanes support systemd user units; legacy and custom
images may not. Inspect the user manager before choosing that workflow.

For independent classification, extraction, or tagging, prefer `scripts/codex-bulk` when
available in the repository. It uses `gpt-5.6-luna` with low reasoning, starts with 32 workers,
and caps concurrency at 256. Preserve its append-only results and shared retry scheduler.
The driver is a Worklane repository tool, not a command installed in every lane.

## Updating these instructions

Worklane refreshes reference documents on attach, but does not replace existing global
instructions. When asked to migrate, read
`$HOME/.local/share/worklane/agent-docs/current/MIGRATE.md` and follow its backup and merge
procedure. Do not treat a newly available bundle as authorization to rewrite custom rules.
Project-specific requirements belong in that project's instructions; Worklane's own
coverage and release gate apply only when developing Worklane.
