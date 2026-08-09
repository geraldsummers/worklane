# Worklane

Rust-native management for disposable, rootless Podman development lanes. The
directory selected for a lane is mounted once, as the container's `/home/dev`
and working directory. Project files, dotfiles, caches, agent state, and
user-space tools therefore remain together in the user-managed directory. The
container root filesystem is read-only and intentionally disposable.

## Build

Build the two static-distribution candidates on a Rust-enabled Linux workstation:

```sh
cargo build --release --target x86_64-unknown-linux-gnu
sha256sum target/x86_64-unknown-linux-gnu/release/{worklane,lane}
```

Shell completions can be generated from the installed binary:

```sh
worklane completions bash > ~/.local/share/bash-completion/completions/worklane
worklane completions zsh > ~/.zfunc/_worklane
```

## Release validation

Every distributable build must pass the local quality suite and the isolated full-feature
acceptance matrix on the lab workstation. The harness uses a unique registry, project, lane,
image, and remote state directory for each run. It exercises real host credentials only through
read-only login checks and captures each TUI interaction from a real PTY as text, HTML, and PNG.

```sh
scripts/release-check
```

The default lab is `gerald@192.168.0.11`; override it with `WORKLANE_LAB_SSH` when necessary.
Inspect every reported PNG, create the requested `visual-review.txt` record, and publish only that
successful candidate with `scripts/release-check --publish RUN_ID`. Artifacts are retained below
`artifacts/lab-acceptance/RUN_ID`; `dist/` is untouched until the reviewed run is published.

On first startup, Worklane seeds its embedded standard image recipe to
`~/.local/share/worklane/Containerfile`. Custom edits are preserved, while a
recognized older stock recipe can be refreshed by a newer binary. Open it with
`worklane image edit` (the `e` key in `lane`). Standard image builds and
lane upgrades use this file. Pass `--file PATH` to override it for a single build and
`--no-cache` only when a clean Podman build is required. Worklane currently
deliberately has no OCI registry: each host builds and retains its own Podman image.

The standard image includes Python (pip and venv), Node.js/npm, TypeScript,
Prettier for JS/TS/HTML/CSS, Rust/Cargo, zsh, and native build tooling, as well
as Codex, Herdr, Git, and GitHub CLI. JVM SDKs such as Java, Kotlin, Gradle, and
Maven are installed and updated by lane users through SDKMAN under
`$HOME/.sdkman`, rather than being tied to Debian's package versions.

## First lane

```sh
worklane lane create my-project
worklane lane attach my-project
```

`create` defaults the project and build context to the current directory and
uses the `default` profile. If the declared image is missing, it is built automatically before
the lane starts. `lane attach` starts or reattaches the lane's default persistent Herdr session
and starts Codex when that workspace does not already have a Codex agent.
Detach with `Ctrl-B q`; panes and agents keep running in the lane. Use
`worklane lane attach my-project --shell` for a plain zsh login shell. Use
Herdr's own session commands only after attaching when you deliberately need a
separate Herdr server. On the first Herdr attach, Worklane installs Herdr's
Codex integration into the selected directory so supported Codex sessions can
be restored after a Herdr server restart. Worklane also bridges Herdr agent
alerts to the outer terminal as a terminal bell, allowing terminal emulators
such as Konsole to turn background completions into desktop notifications even
though the lane itself is containerized.

### Programmatic Codex runs

The attached Codex agent is useful for interactive work, but Codex can also be reinvoked
non-interactively from scripts with `codex exec`. This is a convenient way for an agent or an
ordinary program to delegate independent, well-bounded jobs without opening another TUI. For
bulk work, write a small driver that pushes structured JSON or JSONL records into separate Codex
invocations through standard input. Select the appropriate model with `--model`, and use a JSON
Schema when the caller needs stable machine-readable results:

```sh
codex exec --ephemeral --model gpt-5.6-luna \
  -c 'model_reasoning_effort="low"' \
  --output-schema ./classification.schema.json \
  "Classify the supplied record. Return only the schema-defined result." < record.json
```

The repository includes `scripts/codex-bulk`, a standard-library Python driver that reads one
JSON object per input line, requires a stable `id` field, and appends durable result events to
an output JSONL file:

```sh
scripts/codex-bulk records.jsonl \
  --output classifications.jsonl \
  --schema classification.schema.json \
  --prompt "Classify the supplied record. Return only the schema-defined result."
```

Successful IDs are skipped when the command is rerun. Rate-limited records produce a persisted
`retrying` event before they return to the queue; final rows use `ok` or `error`. Override
`--id-field`, `--initial-workers`, or other limits when required; run
`scripts/codex-bulk --help` for the complete interface.

This pattern is especially useful for bulk classification, extraction, or tagging: split the
input into independent records, invoke bounded `codex exec` jobs with controlled concurrency,
and aggregate their structured outputs. Treat 256 parallel Codex instances as a hard upper
ceiling, not a launch target: OpenAI limits vary by organization, project, model, requests per
minute, and tokens per minute, so no fixed concurrency is universally safe. Start with 32
workers and adapt from observed results. The driver should preserve a stable input ID in every
result, validate each response against the schema, and route low-confidence or terminal failures
to a fallback model. Keeping one independent record per invocation makes failures isolated and
results easy to resume or reorder; batch records together only when the classification requires
cross-record context.

Use one shared scheduler for the entire queue. After 32 consecutive successful completions,
increase its worker target by one, up to 256. If any invocation exits with `429 Too Many Requests`
or `exceeded retry limit`, stop launching new work, halve the worker target (with a floor of one),
and apply a queue-wide 60-second cooldown plus random jitter before retrying the affected record.
Do not let each subprocess immediately retry independently: unsuccessful requests also consume
rate-limit capacity, and `codex exec` has already exhausted its own retries when it reports that
message. Bound outer retries to three attempts and 15 minutes total per record, persist retry
state for resumability, and distinguish temporary rate limits from quota or billing failures that
require user action. When using the API directly instead of the CLI, honor `Retry-After` and the
`x-ratelimit-*` response headers. See OpenAI's
[rate-limit guidance](https://developers.openai.com/api/docs/guides/rate-limits).

Default to `gpt-5.6-luna` with low reasoning for these simple, high-volume delegations. It offers
enough capability for structured classification while keeping reasoning latency and token use
bounded. Reserve a stronger model or the persistent interactive agent for ambiguous cases,
synthesis, and repository-wide changes. Use `--json` instead when the caller needs the full JSONL
event stream, and keep the default read-only sandbox unless a job genuinely needs workspace writes.

To build for a managed host, the Containerfile and build context must already exist on that host:

```sh
worklane image build --host lab --context /home/gerald/worklane --tag localhost/worklane:latest
worklane lane create my-project --host lab --project /home/gerald/projects/my-project \
  --profile default
```

The portable, schema-v5 `.worklane/lane.toml` in each selected directory is
the authority for lane configuration. The controller registry at
`~/.local/share/worklane/worklane-v5.db` is only a rebuildable locator and
status cache. Recover a missing registry entry with
`worklane lane import PATH`, where `PATH` is either the selected directory or
its manifest. A lane's display name can change (`lane rename`, or `n` in the
TUI), while its UUID, Podman container name, and creation-time Herdr
session/workspace name remain stable. Stable Podman names use
`worklane-<creation-name>-<uuid>`, so runtime diagnostics always carry both
human and machine identity.

This is an explicit clean break. If Worklane finds `worklane.db` or
`worklane-v2.db`, `worklane-v3.db`, or `worklane-v4.db` without a v5 registry,
it stops and leaves the old database untouched. Acknowledge a clean registry with
`worklane registry init --fresh`, then import only manifests you have verified.
Older or unknown manifest schemas are rejected and never rewritten.

`lane delete NAME` (the `D` key) always contacts the owning host, checks drift,
removes the disposable container, removes the registry record, and deletes only
the exact `.worklane/lane.toml` control file. It never deletes the selected
directory or its contents. `lane forget NAME` (the `F` key) removes only the
controller registry entry and deliberately leaves the manifest and Podman
state intact. There are no ambiguous archive, destroy, or purge commands.

Lifecycle changes use a per-lane operation journal and commit the portable
manifest and registry projection only after runtime preparation succeeds.
Retries are safe. `worklane lane reconcile NAME` reports manifest, cache,
runtime, and interrupted-operation differences without changing them;
`--apply` performs only verified repairs and reports every change or unresolved
condition. The TUI exposes the same commands with `c` (report) and a confirmed
`C` (apply).

## Hosts and deployment

```sh
worklane host add lab --ssh dev@192.168.0.11
worklane host bootstrap lab
worklane host deploy lab --binary target/x86_64-unknown-linux-gnu/release/worklane
```

The SSH transport uses the existing OpenSSH configuration with `StrictHostKeyChecking=yes`; unknown or changed keys are rejected. Deployment copies a versioned binary, verifies SHA-256 on the host, then atomically updates `~/.local/bin/worklane`.

All control commands accept `--json`. `image build` and `image inspect` accept
`--host`; `image push` is intentionally unavailable. Builds use Podman's cache
by default and accept `--no-cache` explicitly. `lane upgrade --all` builds each
distinct effective image once per host, then recreates its lanes so their
immutable root filesystems are fresh. In the TUI, `u` performs a cached upgrade
of one lane, while `U` performs a fresh `--no-cache` upgrade of all effective
images and pulls the standard image's base. Custom Containerfiles retain
control over local-only base images. Run `lane` for the keyboard-first terminal
view. Host operations and bounded state checks run in the background; cached
state is rendered immediately with its age, navigation remains available, and
an unreachable host does not hide other lanes. Errors stay attached to the
affected lane until dismissed with `Esc` or cleared by a successful retry.

Worklane does not impose a per-lane disk quota or preallocate storage. Because
all persistent state is in the selected directory, users can inspect, back up,
move, and constrain it with their filesystem's normal tools.

Every lane runs with Podman's init process as PID 1 so orphaned subprocesses are reaped after
cancelled or interrupted agent tools. Worklane also raises the lane cgroup PID limit to 16,384;
this provides headroom for tool-heavy workloads while retaining a finite process bound. Existing
lanes acquire these runtime settings when they are upgraded and recreated.

## Profiles

Profiles are named entries in `~/.config/worklane/profiles.toml`. The built-in
`default` profile uses the embedded Containerfile, outbound networking, and
mounts the owning host's Codex and GitHub CLI credentials at their standard
paths. Codex `auth.json` is mounted directly. For GitHub CLI, Worklane asks the
host `gh` process to export active tokens—including tokens held in a desktop
keyring—into a lane-specific managed file with mode 0600, then mounts that file.
The export is refreshed whenever the lane container is created. Credential
contents are never copied into the manifest, image, registry, or selected
directory. Missing credentials stop container creation with authentication
guidance; set the corresponding profile flag to `false` to opt out explicitly.
Worklane creates empty, mode-0600 mountpoint files in the selected home and
refuses to hide existing non-empty files. A profile is snapshotted when a lane
is created.
An omitted `build_context` follows the lane's selected directory when it is
moved or imported; an explicit path remains fixed.

```toml
[profiles.docs]
image = "worklane:latest"
network = "outbound"
mount_codex_credentials = true
mount_gh_credentials = true

[[profiles.docs.mounts]]
source = "/home/gerald/.cache/pip"
target = "/home/dev/.cache/pip"
read_only = false
```

Create with `worklane lane create docs --profile docs`. Mount sources and
targets must be absolute. Targets may live below `/home/dev`, but they may not
replace `/home/dev`, contain it, or overlap another custom target.
