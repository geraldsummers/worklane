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

On first startup, Worklane seeds its embedded standard image recipe to
`~/.local/share/worklane/Containerfile`. Custom edits are preserved, while a
recognized older stock recipe can be refreshed by a newer binary. Open it with
`worklane image edit` (the `e` key in `lane`). Standard image builds and
lane upgrades use this file. Pass `--file PATH` to override it for a single build and
`--no-cache` only when a clean Podman build is required. Worklane currently
deliberately has no OCI registry: each host builds and retains its own Podman image.

The standard image includes Python (pip and venv), Node.js/npm, TypeScript,
Prettier for JS/TS/HTML/CSS, Rust/Cargo, Java 21, Kotlin, zsh, and native
build tooling, as well as Codex, Herdr, Git, and GitHub CLI.

## First lane

```sh
worklane lane create my-project
worklane lane attach my-project
```

`create` defaults the project and build context to the current directory and
uses the `default` profile. If the declared image is missing, it is built automatically before
the lane starts. `lane attach` starts or reattaches the lane's default persistent Herdr session.
Detach with `Ctrl-B q`; panes and agents keep running in the lane. Use
`worklane lane attach my-project --shell` for a plain zsh login shell. Use
Herdr's own session commands only after attaching when you deliberately need a
separate Herdr server. On the first Herdr attach, Worklane installs Herdr's
Codex integration into the selected directory so supported Codex sessions can
be restored after a Herdr server restart.

To build for a managed host, the Containerfile and build context must already exist on that host:

```sh
worklane image build --host lab --context /home/gerald/worklane --tag localhost/worklane:latest
worklane lane create my-project --host lab --project /home/gerald/projects/my-project \
  --profile default
```

The canonical controller registry is `~/.local/share/worklane/worklane.db`.
Each selected directory also contains a portable, atomically written
`.worklane/lane.toml`. Recover a missing registry entry with
`worklane lane import PATH`, where `PATH` is either the selected directory or
its manifest. A lane's display name can change (`lane rename`, or `n` in the
TUI), while its Podman and Herdr identifiers remain stable.

`lane delete NAME` (the `D` key) always contacts the owning host, checks drift,
removes the disposable container, removes the registry record, and deletes only
the exact `.worklane/lane.toml` control file. It never deletes the selected
directory or its contents. `lane forget NAME` (the `F` key) removes only the
controller registry entry and deliberately leaves the manifest and Podman
state intact. There are no ambiguous archive, destroy, or purge commands.

Legacy lanes are migrated automatically before start, attach, or upgrade. Stop
a running legacy lane first. Migration copies and verifies supported entries
from the old hidden home into the selected directory, commits the manifest and
registry, and only then removes the exact legacy lane directory. Conflicting,
unsupported, or unreadable entries do not block the lane: they are moved to
`~/.local/share/worklane/quarantine/<lane-id>/items`, with recovery details in
`migration.log`. Interrupted migrations resume safely. Existing legacy archives
are left untouched.

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
view; host operations run in the background and it uses cached lane state when
hosts are unreachable.

Worklane does not impose a per-lane disk quota or preallocate storage. Because
all persistent state is in the selected directory, users can inspect, back up,
move, and constrain it with their filesystem's normal tools.

## Profiles

Profiles are named entries in `~/.config/worklane/profiles.toml`. The built-in
`default` profile uses the embedded Containerfile, outbound networking, and no
extra mounts. A profile is snapshotted when a lane is created.

```toml
[profiles.docs]
image = "worklane:latest"
network = "outbound"

[[profiles.docs.mounts]]
source = "/home/gerald/.cache/pip"
target = "/home/dev/.cache/pip"
read_only = false
```

Create with `worklane lane create docs --profile docs`. Mount sources and
targets must be absolute. Targets may live below `/home/dev`, but they may not
replace `/home/dev`, contain it, or overlap another custom target.
