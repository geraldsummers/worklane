# Working on Worklane

These instructions apply to this repository. When working inside a lane, also read
[the shared lane guide](docs/agents/lane.md) before acting; it defines filesystem,
Herdr, coordination, and progress-reporting requirements. Outside a lane, use the host's
normal build environment and record that you are outside a Herdr pane when applicable.

## Documentation ownership

- `docs/agents/lane.md` is the canonical prompt seeded into new lanes.
- `docs/agents/automation.md` contains task-specific recipes shipped with that prompt.
- `docs/agents/MIGRATE.md` explains agent-assisted adoption in existing lanes.
- This file contains Worklane contributor requirements and is not embedded in lane prompts.
- The README documents the user interface and links to these guides. Keep examples aligned
  with the CLI, image recipe, helpers, and tests; distinguish implemented features from recipes.

Before editing, inspect `git status --short`, read all agent claims, and correlate them with
live Herdr state as described in the shared guide. Preserve existing dirty and untracked work;
coordinate before overlapping. Update only your own claim.

## Local release toolchain

Release validation uses Rust 1.88.0, installed with rustup under
`/home/dev/.rustup-worklane` (Cargo state: `/home/dev/.cargo-worklane`). Reproduce with
`TMPDIR=/home/dev/.tmp RUSTUP_HOME=/home/dev/.rustup-worklane CARGO_HOME=/home/dev/.cargo-worklane rustup toolchain install 1.88.0 --profile minimal --component rustfmt,clippy`; activate individual
commands with the same environment followed by `rustup run 1.88.0 cargo ...`.

For all local checks, set `TMPDIR=/home/dev/.tmp` and ensure that directory exists.
Record any SDK installation or upgrade here with its resolved version, installation method,
location, and activation command. This inventory describes the lane's user-managed release
toolchain, not the Rust version bundled in the standard image.

## Repository testing cadence

The complete Worklane gate is intentionally heavy. Do not rerun the full test, coverage, and
isolated acceptance suite after every small edit or exploratory iteration. During implementation,
use the narrowest relevant unit test, compile check, lint, or static validation and batch related
changes before advancing to another expensive checkpoint.

Coverage is not optional for substantive implementation changes: Worklane is mission-critical
infrastructure, and its full coverage and acceptance evidence is worth the cost. Run the complete
CI-equivalent gate at meaningful integration checkpoints, after the implementation stabilizes,
before publishing a fresh release build, and before pushing substantive runtime or behavior
changes. Documentation and prompt-only edits may use focused static checks without the complete
gate unless the user explicitly requests it. Never skip the final full gate merely because it is
slow, and do not push when a required checkpoint is failing.

## Fresh dist builds

After substantive user-facing changes to `worklane`, `lane`, their shared core, or their
embedded runtime assets, automatically run the required validation, build the release binaries,
and refresh the ignored `dist/` directory. Do this once the change has stabilized rather than
after every intermediate edit. Also refresh it whenever the user explicitly asks for a fresh dist:

```sh
scripts/release-check
# Inspect every PNG named by the command, record visual-review.txt, then:
scripts/release-check --publish RUN_ID
```

This gate runs formatting, linting, tests, coverage, the complete isolated acceptance matrix on
`podman_lab@192.168.0.11`, and real-PTY TUI capture. Directly copying binaries into `dist/` is not an
acceptable substitute. Report the three SHA-256 hashes. Do not commit `dist/`, `target/`, or
`artifacts/`; they are ignored.
