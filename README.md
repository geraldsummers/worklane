# Worklane

Rust-native management for disposable, rootless Podman development lanes. The
directory selected for a lane is mounted once, as the container's `/home/dev`
and working directory. Project files, dotfiles, caches, agent state, and
user-space tools therefore remain together in the user-managed directory. The
container root filesystem is read-only and intentionally disposable.

<p align="center">
  <strong>Keep the project. Keep the agent session. Throw away the machine.</strong>
</p>

## First lane

```sh
worklane lane create my-project
worklane lane attach my-project
```

`create` defaults the project and build context to the current directory and
uses the `default` profile. New standard lanes select Pi by default and include
both Pi and Codex in the image. If the declared image is missing, it is built automatically before
the lane starts. `lane attach` starts or reattaches the lane's default persistent Herdr session
and starts the lane's selected agent when that workspace does not already have one.
Each local lane inherits its host's time zone when its container is created; use
`worklane lane upgrade --all` to apply this to existing lanes.
Detach with `Ctrl-B q`; panes and agents keep running in the lane. Use
`worklane lane attach my-project --shell` for a plain zsh login shell. Manage session lifecycle through Worklane; agents should not start a second
Herdr server for the lane. On the first Herdr attach, Worklane installs Herdr's
Codex integration into the selected directory so supported Codex sessions can
be restored after a Herdr server restart. Worklane also bridges Herdr agent
alerts to the outer terminal as a terminal bell, allowing terminal emulators
such as Konsole to turn background completions into desktop notifications even
though the lane itself is containerized.

For image review in Ghostty, run `worklane-show-image /absolute/path/to/image` from a managed
pane. Worklane enables Herdr's experimental Kitty graphics support and the helper opens a dedicated
review workspace, converts common image formats when necessary, and presents the image through
Herdr's native pane-graphics API.

## Persistent terminal IDE

`worklane lane ide my-project` opens the lane's Neovim IDE tab through the same
Herdr and SSH path as `lane attach`. In the `lane` dashboard, press `v` on a
lane. The editor tab is created on demand and reused after disconnecting or
reattaching. It has a clickable file tree, buffer tabs, statusline actions,
diagnostics, Git diffs, and debugger panels. Neovim requests mouse input from
Herdr; use a terminal with true color. The
terminal client controls font size. Pi and Codex remain in their own tabs.

Inside a lane, agents and shell users can control the live IDE from another
Herdr tab without changing the user's focus:

```sh
worklane-ide start
worklane-ide open src/main.rs --line 42
worklane-ide diagnostics
worklane-ide task test
worklane-ide debug Launch
worklane-ide ex 'edit README.md'
worklane-ide keys '<Esc>'
worklane-ide focus
```

`open --focus` switches to the editor; other bridge commands leave focus where
it is. `ex` and `keys` intentionally permit full control of the live editor.
Project tasks are declared in `.worklane/ide.toml`, for example:

```toml
[tasks.test]
command = "cargo test"
cwd = "."
```

Tasks run in labeled Herdr tabs. DAP configurations use `.vscode/launch.json`;
language servers and debugger adapters are optional user-space installations.
Neovim and Tree-sitter are in the image, while the pinned editor plugins and
parsers are installed into the persistent lane home on first use. The standard
lane has outbound networking for this first setup. Later image upgrades keep
the IDE's writable state.

`worklane appearance status` and `worklane appearance set warm|cool` manage an
account-wide dark theme; warm is the default. The shared palette updates the
Worklane dashboard, Herdr, zsh, Neovim, Pi, and Codex's syntax theme without
changing credentials or other agent settings. The two presets and generated
settings live in writable lane homes; the host preference is
`~/.config/worklane/theme`. Running Neovim editors refresh their palette when
the account setting changes. Terminal font and point size remain device settings.

## Agents and account credentials

The `lane` TUI uses `A` to select Pi or Codex for the highlighted lane. The
selection is stored in that lane's portable manifest. Switching starts or
focuses the selected agent and leaves previous agent panes and sessions intact.
Use `worklane lane agent NAME pi` or `worklane lane agent NAME codex` from a shell.
The standard image contains both agents, so switching does not require a rebuild.
The standard bundle pins Pi **1.0.2**, pi-web-access **0.35.0**,
and optional pi-lightpanda-extension **0.1.0**, including
transitive dependencies through
`assets/pi-bundle/package-lock.json`. Image builds use `npm ci --ignore-scripts`;
seed versions are recorded in `/etc/worklane/pi-version` and `pi-web-version`.
Worklane's local plan, questionnaire, notification, subscription, complaint,
collaboration and P0 extensions are seeded with the image; no third-party
read-only Plan mode is part of the standard loadout. On first Pi use, Worklane copies the complete locked tree into
`~/.local/share/worklane/pi-bundle/node_modules`, links its packages through
`~/.local/lib/node_modules`, and creates `~/.local/bin/pi`. The lane runs only
this writable installation; no network package install happens at startup. Later image upgrades never replace it: use
`pi update self` or npm in the lane to update its own Pi, and edit or install
extensions in `~/.pi/agent/extensions` or the project's `.pi/extensions`.
Pi discovers these extensions and their tools on startup; restart Pi or use
`/reload` after editing an extension. Existing lanes must first be upgraded to
an image containing the installer. A custom build context using the repository's
`Containerfile` must also contain its `assets/` files; embedded standard builds
seed those files automatically.

Web access defaults to the configured SearXNG service with direct fetching and
no silent search-provider, summary-model or hosted-fetch fallback. Lightpanda
is installed but not enabled automatically. When explicitly enabled, the pinned
`pi-lightpanda-extension` gives Pi agent-friendly web tools backed by
the image's **Lightpanda** headless browser: `lightpanda_search`, `lightpanda_fetch`
(markdown/html/semantic tree), and CDP navigate/eval/command/events for stateful
browsing. The `lightpanda` binary is on `PATH` (version recorded in
`/etc/worklane/lightpanda-version`). `playwright_screenshot` is optional and needs
`npx playwright install chromium`; Lightpanda's own `Page.captureScreenshot`
returns a placeholder because it has no rendering engine.

Pi defaults to **openai-codex/gpt-6-sol** at **medium** thinking for new and
resumed sessions. Each Linux account signs into its own Codex subscription with
`/login openai-codex` inside Pi; the TUI `K` key displays this guidance, not
an API-key prompt. OAuth state and the model selection live in the account's
private `~/.config/worklane/pi-codex` directory (mode 0700, files 0600),
mounted writable only into that account's lanes so Pi can refresh OAuth. `M`
selects a model from the installed openai-codex catalog;
`worklane credentials models`, `set-model MODEL` and `status` are the shell
commands. Deliberate subscription-model/thinking selections persist with the Pi session;
explicit standard Pi CLI model choices are respected. Request-time checks reject
non-subscription/provider fallback instead of inspecting OAuth JSON shapes.
Legacy `set-openrouter` remains an explicit compatibility command, but writes
only to `~/.config/worklane/pi` and is never mounted or automatically selected
by managed Pi. No OpenRouter or metered OpenAI API-key fallback is installed.
Codex CLI credentials remain separate; never copy credentials between owners.

The lane's selected directory remains its `/home/dev`, so Pi reads the same
project files and WIP notes as Codex. Existing Codex conversation files remain
there but Pi does not automatically import or search them.

## Agent guidance

Pi is the default for new standard profiles; explicit existing selections remain
portable manifest data until changed. The image retains Codex for manual fallback.
Worklane installs the selected agent's Herdr integration and provides Pi with a
writable pinned bundle, native collaboration/P0 extensions and shared lane guidance.
The old `Containerfile.pi`/`assets/pi-pilot` are historical pilot fixtures, not the
standard rollout recipe; their legacy `/plan` extension is not shipped or loaded
by standard images.

On attach (including `--shell`), the owning host's Worklane binary publishes its embedded
reference bundle at `$HOME/.local/share/worklane/agent-docs/current` inside the lane.
The bundle includes the lane guide, automation recipes, collaboration/autonomy/P0
guides, migration checklist, and a content-derived `REVISION`. Pi's absent
`~/.pi/agent/AGENTS.md` is linked to this lane guide; existing custom instructions
are never replaced. Attach seeds `$HOME/.codex/AGENTS.md` only if neither it nor
`AGENTS.override.md` exists. Existing files, including empty files and symlinks, are preserved.
The repository's contributor `AGENTS.md` is not part of the lane prompt.

For an existing lane, attach with the updated owning-host binary, then ask an agent:

> Read `$HOME/.local/share/worklane/agent-docs/current/MIGRATE.md` and migrate my active
> instructions to that guidance, preserving my custom rules and backing up affected files.

The agent performs the merge; attach never rewrites active instructions or launches a
migration agent. Custom `CODEX_HOME` locations need explicit migration. A running Codex
session does not automatically reload initial instructions; start a new run after migration.
The bundle is refreshed from the attaching binary, so an older host binary carries older
guidance. Update the owning host before adopting a new version.

- [Lane guide](docs/agents/lane.md): writable paths, tools, Herdr, coordination, and progress.
- [Automation](docs/agents/automation.md): user services, allowance-driven work, and `scripts/codex-bulk`.
- [Migration checklist](docs/agents/MIGRATE.md): backup, merge, verification, and rollback.
- [Collaboration](docs/agents/COLLAB.md): durable lane-local messaging and exact Herdr identities.
- [Autonomy](docs/agents/AUTONOMY.md): goal submission, hash-bound human approvals, schedules and budgets.
- [Platform Zero](docs/agents/P0.md): typed existing capabilities and external-action authority.

Standard images use internal UID/GID **1001** independently of the host owner.
`keep-id:uid=1001,gid=1001` maps the host owner into that identity and uses the
matching user-systemd bus. This permits the same tested image on the server and
agent VM without changing project ownership or copying divergent workspaces.

For an explicitly authorized cold Pi migration, stop the lane, preserve its
manifest/runtime metadata, upgrade its image, then run the installer with
`--replace` inside it. Private timestamped backups live under
`~/.local/share/worklane/pi-backups`; `pi-last-backup` records the last path.
The replacement preserves credentials, unrelated config, custom instructions
and sessions. Normal attaches never replace existing user packages/extensions.
Rollback stops the lane, restores the prior image/manifest, and restores the
backed-up `pi-bundle`, package links, launcher and `.pi` tree to their original
paths. Restore the prior agent selection, not a blanket Codex selection.

The lane-local `worklane-collab.service` owns private durable state and a Herdr
collaboration tab. Goals, helper agents and remote P0 requests use named visible
tabs and durable sessions. Every recurring occurrence requires fresh approval;
there is no recurring-template preapproval or cross-lane message relay. Budgeted
runs disable cache warming, provider retries and automatic compaction; settings
that re-enable invisible requests cause managed runs to fail closed.

The standard image configures Codex with approvals disabled and `danger-full-access`.
The lane's immutable root and container configuration apply to both interactive and scripted
runs; do not assume `codex exec` uses a read-only sandbox in this image.

## Lane configuration and lifecycle

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

To build for a managed host, the Containerfile and build context must already exist on that host:

```sh
worklane image build --host lab --context /home/dev/worklane --tag localhost/worklane:latest
worklane lane create my-project --host lab --project /home/dev/projects/my-project \
  --profile default
```

The SSH transport uses the existing OpenSSH configuration with `BatchMode=yes` and
`StrictHostKeyChecking=yes`; it rejects password prompts and unknown or changed keys. Deployment
copies a versioned binary, verifies SHA-256 on the host, then atomically updates
`~/.local/bin/worklane`.

All control commands accept `--json`. `image build` and `image inspect` accept
`--host`; `image push` is intentionally unavailable. Image builds use Podman's
cache by default and accept `--no-cache` explicitly. Lane upgrades always rebuild
the standard embedded image without cache and pull its base image. The build
installs the Codex release pinned by the standard Containerfile, and
verifies the installed CLI before replacing a lane. `lane upgrade --all` builds each
distinct effective image once per host, then recreates its lanes so their
immutable root filesystems are fresh. Use `lane upgrade --all --skip-running`
to leave active agents running while refreshing idle lanes. In the TUI, `u` upgrades one lane and `U`
upgrades all effective images. For custom Containerfiles, pass `--no-cache` when
a completely fresh rebuild is required; local-only base images remain under the
custom Containerfile's control.
Run `lane` for the keyboard-first terminal
view. Host operations and bounded state checks run in the background; cached
state is rendered immediately with its age, navigation remains available, and
an unreachable host does not hide other lanes. Errors stay attached to the
affected lane until dismissed with `Esc` or cleared by a successful retry.

Worklane does not impose a per-lane disk quota or preallocate storage. Because
all persistent state is in the selected directory, users can inspect, back up,
move, and constrain it with their filesystem's normal tools.

New lanes using the standard image run systemd as PID 1. The system manager and its unit files
remain on the immutable root, while `dev` receives a persistent user manager for autonomous lane
services. Legacy and custom-image lanes may continue using Podman's init process. Both runtimes
reap orphaned subprocesses, and Worklane raises the lane cgroup PID limit to 16,384 to provide
headroom for tool-heavy workloads while retaining a finite process bound. Existing standard lanes
migrate to systemd through the normal `lane upgrade` flow.

## Profiles

Profiles are named entries in `~/.config/worklane/profiles.toml`. The built-in
`default` profile uses the embedded Containerfile, outbound networking, and
mounts the owning host's Codex and GitHub CLI credentials at their standard
paths. Worklane mounts the host `CODEX_HOME` directory read only at
`/run/worklane-host-codex` and links the lane's `.codex/auth.json` to it. Host
credential replacements become visible on the next credential read; other host
Codex files are also readable at that mount path. Sign in, sign out, and refresh
credentials on the host: lane token refresh cannot write to the read only mount.
An existing lane `auth.json` is preserved as a mode-0600
`auth.json.worklane-backup-*` file when upgrading to this layout. For GitHub CLI, Worklane asks the
host `gh` process to export active tokens—including tokens held in a desktop
keyring—into a lane-specific managed file with mode 0600, then mounts that file.
The export is refreshed whenever the lane container is created. Credential
contents are never copied into the manifest, image, registry, or selected
directory. Missing credentials stop container creation with authentication
guidance; set the corresponding profile flag to `false` to opt out explicitly.
Worklane creates an empty, mode-0600 GitHub mountpoint file in the selected home
and refuses to hide an existing non-empty GitHub file. A profile is snapshotted when a lane
is created.
An omitted `build_context` follows the lane's selected directory when it is
moved or imported; an explicit path remains fixed.

```toml
[profiles.docs]
image = "worklane:latest"
network = "outbound"
runtime = "systemd"
devices = ["nvidia.com/gpu=all"]
mount_codex_credentials = true
mount_gh_credentials = true

[[profiles.docs.mounts]]
source = "/home/dev/.cache/pip"
target = "/home/dev/.cache/pip"
read_only = false
```

Create with `worklane lane create docs --profile docs`. Mount sources and
targets must be absolute. Targets may live below `/home/dev`, but they may not
replace `/home/dev`, contain it, or overlap another custom target. `devices`
contains CDI qualified device names; Worklane passes each entry to Podman as a
separate `--device` argument. For example, `nvidia.com/gpu=all` exposes every
GPU described by the host's NVIDIA CDI configuration. The host must already
provide the named CDI devices and grant the lane-owning account access to them.
`runtime` accepts `auto`, `systemd`, or `podman-init`. `auto` selects systemd for the embedded
standard image and Podman's init for custom images. A custom image may opt into systemd when it
contains a bootable `/sbin/init`, starts a persistent user manager for its `dev` account, exposes
that user's D-Bus below `/run/user/UID`, and supplies the Worklane Herdr/helper user units. Worklane
verifies the user manager before committing creation or upgrade. The image must create the runtime
directory without depending on host `systemd-logind` and pass `XDG_RUNTIME_DIR` and
`DBUS_SESSION_BUS_ADDRESS` into `user@UID.service`; the standard image supplies container-specific
drop-ins for both requirements.

## Standard image

On first startup, Worklane seeds its embedded standard image recipe to
`~/.local/share/worklane/Containerfile`. Custom edits are preserved, while a
recognized older stock recipe can be refreshed by a newer binary. Open it with
`worklane image edit` (the `e` key in `lane`). Standard image builds and
lane upgrades use this file. Pass `--file PATH` to override it for a single build and
`--no-cache` only when a clean Podman build is required. Worklane currently
deliberately has no OCI registry: each host builds and retains its own Podman image.

The standard image includes Python (pip and venv), Node.js/npm, TypeScript,
Prettier for JS/TS/HTML/CSS, Rust/Cargo, zsh, GraphicsMagick for image conversion,
and native build tooling, as well as Codex, Herdr, the Lightpanda headless agent
browser, Git, GitHub CLI, and systemd. JVM SDKs
such as Java, Kotlin, Gradle, and Maven are installed and updated by lane users through SDKMAN under
`$HOME/.sdkman`, rather than being tied to Debian's package versions.

## Historical isolated Pi pilot

`Containerfile.pi` remains as the original opt-in pilot image. It installs Pi and the
`pi-web-access` extension but not Codex. The profile's `agent = "pi"` selector
starts Pi in Herdr with OpenAI Codex as its model provider, limits its tools to
`read`, `bash`, `edit`, `write`, `web_search`, `fetch_content`, and `questionnaire`, and seeds the
extension to use OpenAI-backed search and HTTP-only page fetching. The standard image now carries Pi and Codex together; this pilot image is kept
only to reproduce the earlier isolated test.

In the pilot, `/plan` toggles read-only planning. Pi can ask one or more
selectable questions with a free-text answer, inspect files, and use web search,
but cannot call shell or write tools while planning. A numbered `Plan:` proposal
opens an explicit Execute / Refine / Stay choice; cancelling leaves planning
active. Plan mode is off by default, and its state survives a session resume.

Build the candidate image on the lane-owning host, then put this profile in a
*separate* Worklane config directory for a disposable pilot:

```sh
worklane image build --file /path/to/worklane/Containerfile.pi \
  --context /path/to/worklane --tag localhost/worklane-pi-pilot:0.87.1
```

```toml
[profiles.pi-pilot]
image = "localhost/worklane-pi-pilot:0.87.1"
agent = "pi"
build_context = "/path/to/worklane"
containerfile = "Containerfile.pi"
embedded_containerfile = false
runtime = "systemd"
network = "outbound"
mount_codex_credentials = false
mount_gh_credentials = false
```

Use an empty selected directory and isolated `XDG_CONFIG_HOME` and
`XDG_DATA_HOME` when creating the pilot. Pi must be signed in separately with
`/login` inside the pilot lane; the host Codex auth file is deliberately not
mounted or copied. Search is not authenticated until that sign-in succeeds.
Do not upgrade production lanes to this profile during the pilot.

## Development and release

See [contributor instructions](AGENTS.md) for coordination, the local toolchain, and required
checks. The commands below run on the build/controller host from a Worklane checkout.

### Build

Build the three distribution candidates on a Rust-enabled Linux workstation:

```sh
cargo build --release --target x86_64-unknown-linux-gnu
sha256sum target/x86_64-unknown-linux-gnu/release/{worklane,lane,worklane-mcp}
```

For the internal ChatGPT connector, versioned machine API, OAuth setup, user service, and Caddy
route, see [the connector guide](docs/connector.md).

Shell completions can be generated from the installed binary:

```sh
mkdir -p ~/.local/share/bash-completion/completions ~/.zfunc
worklane completions bash > ~/.local/share/bash-completion/completions/worklane
worklane completions zsh > ~/.zfunc/_worklane
```

### Release validation

During development, run `scripts/check-local --quick` (or `make test`) for local
formatting, linting, Rust and Python tests, and script checks. It does not use the lab or
build release binaries. `scripts/check-local --full` (or `make coverage`) replaces the
Rust test run with one coverage-instrumented run and enforces the 90% line threshold.

Every distributable build must also pass the isolated full-feature acceptance matrix on
the lab workstation. The harness uses a unique registry, project, lane, image, and remote
state directory for each run. It verifies credential mounts with isolated fixture files and
captures each TUI interaction from a real PTY as text, HTML, and PNG. Each run uses a
new cache key for one full image build; a tiny fixture checks `--no-cache` separately, and
later lane upgrades reuse the full image cache. Phase durations go to `timings.tsv`.

```sh
scripts/release-check
```

The default lab is `podman_lab@192.168.0.11`; override it with `WORKLANE_LAB_SSH` when necessary.
Inspect every reported PNG, create the requested `visual-review.txt` record, and publish only that
successful candidate with `scripts/release-check --publish RUN_ID`. Artifacts are retained below
`artifacts/lab-acceptance/RUN_ID`; `dist/` is untouched until the reviewed run is published.

Licensed under [AGPL-3.0-or-later](LICENSE-NOTICE.md).
