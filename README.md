# Worklane

Rust-native management for disposable, rootless Podman development lanes. The
directory selected for a lane is mounted once, as the container's `/home/dev`
and working directory. Project files, dotfiles, caches, agent state, and
user-space tools therefore remain together in the user-managed directory. The
container root filesystem is read-only and intentionally disposable.

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

## Agent guidance

Worklane currently bundles Codex and installs its Herdr integration. Herdr can recognize
other agents, but Worklane does not install agent-specific instructions for them.

On attach (including `--shell`), the owning host's Worklane binary publishes its embedded
reference bundle at `$HOME/.local/share/worklane/agent-docs/current` inside the lane.
The bundle includes the lane guide, automation recipes, migration checklist, and a
content-derived `REVISION`. Attach seeds `$HOME/.codex/AGENTS.md` only if neither it nor
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
the standard embedded image without cache and pull its base image so bundled
tools such as Codex are actually refreshed. `lane upgrade --all` builds each
distinct effective image once per host, then recreates its lanes so their
immutable root filesystems are fresh. In the TUI, `u` upgrades one lane and `U`
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
and native build tooling, as well as Codex, Herdr, Git, GitHub CLI, and systemd. JVM SDKs
such as Java, Kotlin, Gradle, and Maven are installed and updated by lane users through SDKMAN under
`$HOME/.sdkman`, rather than being tied to Debian's package versions.

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

Every distributable build must pass the local quality suite and the isolated full-feature
acceptance matrix on the lab workstation. The harness uses a unique registry, project, lane,
image, and remote state directory for each run. It verifies credential mounts with isolated
fixture files and captures each TUI interaction from a real PTY as text, HTML, and PNG.

```sh
scripts/release-check
```

The default lab is `podman_lab@192.168.0.11`; override it with `WORKLANE_LAB_SSH` when necessary.
Inspect every reported PNG, create the requested `visual-review.txt` record, and publish only that
successful candidate with `scripts/release-check --publish RUN_ID`. Artifacts are retained below
`artifacts/lab-acceptance/RUN_ID`; `dist/` is untouched until the reviewed run is published.
