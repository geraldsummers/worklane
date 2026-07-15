# Lane environment

Worklane lanes have an immutable operating-system root filesystem. Agents do not have `sudo`,
must not attempt `apt`, and must not modify files outside the explicitly writable locations below.
When additional tools are needed, install or build them in user space instead of changing the
system image from inside the lane.

Use these locations for additions and generated state:

- Project dependencies and generated files: `/home/dev/<lane-name>`
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
`herdr --session <lane-name>`, with the lane workspace focused at `/home/dev/<lane-name>`.
Use Herdr-managed sessions unless the user explicitly asks for a plain shell.
