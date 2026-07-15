# Worklane

Rust-native management for disposable, rootless Podman development lanes. A lane keeps its `/home/dev` in host storage and bind-mounts its project at `/home/dev/<lane-name>`; the container root filesystem is read-only and intentionally disposable. Agents have no `sudo`; user-space tooling belongs under `/home/dev`.

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
`~/.local/share/worklane/Containerfile`. The file is never overwritten, so it can be customized
directly or opened with `worklane image edit` (the `e` key in `lane`). Standard image builds and
lane upgrades use this file. Pass `--file PATH` to override it for a single build. Worklane v1
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
separate Herdr server. On the first Herdr attach, Worklane installs Herdr's Codex integration into the lane's
persistent home so supported Codex sessions can be restored after a Herdr server restart.

To build for a managed host, the Containerfile and build context must already exist on that host:

```sh
worklane image build --host lab --context /home/gerald/worklane --tag localhost/worklane:latest
worklane lane create my-project --host lab --project /home/gerald/projects/my-project \
  --profile default
```

The canonical controller registry is `~/.local/share/worklane/worklane.db`. Each local lane has a recoverable specification at `~/.local/share/worklane/lanes/<lane-name>/lane.toml` and persistent home under the same directory. New containers, Herdr sessions, Herdr workspace labels, project mount directories, and lane-owned data directories are named after the lane; legacy lanes retain their existing generated names. `lane delete NAME` (the `D` key in `lane`) removes an undrifted disposable container and its registry entry, but never alters any bind-mounted source. If the cached lane state is `unknown`, `delete` behaves like `forget` and removes only the local registry entry. `lane forget NAME` is the explicit registry-only form for `unknown` cached entries and never contacts a host or container runtime. `destroy` archives the lane-owned directory and never touches the project. `purge --yes` is explicitly destructive.

## Hosts and deployment

```sh
worklane host add lab --ssh dev@192.168.0.11
worklane host bootstrap lab
worklane host deploy lab --binary target/x86_64-unknown-linux-gnu/release/worklane
```

The SSH transport uses the existing OpenSSH configuration with `StrictHostKeyChecking=yes`; unknown or changed keys are rejected. Deployment copies a versioned binary, verifies SHA-256 on the host, then atomically updates `~/.local/bin/worklane`.

All control commands accept `--json`. `image build` and `image inspect` accept `--host`; `image push` is intentionally unavailable. `lane upgrade` rebuilds embedded/default lanes from the current standard Containerfile, rebuilds custom-image lanes from their stored host-native build context, and then recreates the container so its immutable root filesystem is fresh. Run `lane` for the keyboard-first terminal view; it uses cached lane state when hosts are unreachable.

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
target = "/home/gerald/.cache/pip"
read_only = false
```

Create with `worklane lane create docs --profile docs`. Mount sources and
targets must be absolute; Worklane rejects mounts that overlap its managed home
or project workspace.
