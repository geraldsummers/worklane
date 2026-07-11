# Worklane

Rust-native management for disposable, rootless Podman development lanes. A lane keeps its `/home/dev` in host storage and bind-mounts its project; the container root filesystem is intentionally disposable.

## Build

Build the two static-distribution candidates on a Rust-enabled Linux workstation:

```sh
cargo build --release --target x86_64-unknown-linux-gnu
sha256sum target/x86_64-unknown-linux-gnu/release/{worklane,lane}
```

`Containerfile` is compiled into the `worklane` binary, so `worklane image build --context . --tag localhost/worklane:latest` needs no separate recipe file. Pass `--file PATH` only to override the embedded standard image recipe. Worklane v1 deliberately has no OCI registry: each host builds and retains its own Podman image.

The standard image includes Python (pip and venv), Node.js/npm, TypeScript,
Prettier for JS/TS/HTML/CSS, Rust/Cargo, Java 21, Kotlin, zsh, and native
build tooling, as well as Codex, Herdr, Git, and GitHub CLI.

## First lane

```sh
worklane image build --context . --tag localhost/worklane:latest
worklane lane create my-project
worklane lane attach my-project
```

`create` defaults both `--project` and `--build-context` to the project path,
and uses Worklane's standard embedded Containerfile unless `--containerfile`
is supplied. If the declared image is missing, it is built automatically before
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
  --image worklane:latest
```

The canonical controller registry is `~/.local/share/worklane/worklane.db`. Each local lane has a recoverable specification at `~/.local/share/worklane/lanes/<id>/lane.toml` and persistent home under the same directory. `destroy` archives that directory and never touches the project. `purge --yes` is explicitly destructive.

## Hosts and deployment

```sh
worklane host add lab --ssh dev@192.168.0.11
worklane host bootstrap lab
worklane host deploy lab --binary target/x86_64-unknown-linux-gnu/release/worklane
```

The SSH transport uses the existing OpenSSH configuration with `StrictHostKeyChecking=yes`; unknown or changed keys are rejected. Deployment copies a versioned binary, verifies SHA-256 on the host, then atomically updates `~/.local/bin/worklane`.

All control commands accept `--json`. `image build` and `image inspect` accept `--host`; `image push` is intentionally unavailable. `lane upgrade` rebuilds the lane image from its stored host-native build context before recreating the container. Run `lane` for the keyboard-first terminal view; it uses cached lane state when hosts are unreachable.
