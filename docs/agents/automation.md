# Automation in a Worklane lane

Read this guide before creating user services, allowance monitors, or bulk Codex jobs.
These are workflows for authorized work. The allowance monitor and dispatcher are recipes
an agent can implement; Worklane does not install them automatically.

## Engineering autonomous lane behavior

Standard-image lanes run systemd as the container init and keep a persistent `systemd --user` manager
for `dev`. The operating-system root and system units are immutable; agents may freely engineer
user services, timers, paths, sockets, targets, and slices below
`$HOME/.config/systemd/user/`. These files persist with the lane home across stop, start, and
upgrade, including enablement links and persistent timer state stored in the home.
Runtime-only transient units created with `systemd-run --user` live below `/run` and do
not persist across container recreation.

First check `systemctl --user show-environment`; legacy or custom-image lanes may not
provide a user manager. Use only the user manager. Do not try to change system units,
use `sudo`, or connect to the system manager for lane automation. Before editing persistent unit files, include their paths or unit
names in your normal agent claim and check other live claims. Validate new unit files with
`systemd-analyze --user verify PATH`, then use `systemctl --user daemon-reload` and
`systemctl --user enable --now UNIT` as appropriate. Inspect behavior with
`systemctl --user status UNIT` and `journalctl --user-unit UNIT`.

Use unit dependencies and activation instead of polling when possible. `systemd-run --user` is
appropriate for one-shot or dynamically named work; persistent `.timer`, `.path`, and `.socket`
units are appropriate for recurring or event-driven behavior. Apply service-level controls such
as `TimeoutStartSec=` for oneshot jobs, `RuntimeMaxSec=` for long-running services,
`TasksMax=`, `MemoryHigh=`, and `MemoryMax=` in proportion to the job. Never
put API keys, access tokens, or other secrets in unit files or `Environment=` directives. Services
can use the existing credential files mounted into the lane home.

The lane container must be running for its user manager to act. For calendar timers that should
catch up after downtime, use `Persistent=true`; do not assume a stopped lane can wake itself.
User-unit controls supervise and constrain processes inside the lane, while Podman's rootless
container, read-only root, network mode, devices, mounts, and outer PID limit remain the host
security boundary.

### Dispatching work when Codex allowance replenishes

Agents can engineer a systemd user service that starts queued, authorized work automatically
when Codex allowance returns to full. For example, use replenished capacity for a backlog of
documentation reviews, issue triage, or bulk classification. Systemd schedules and supervises
the monitor and dispatcher; a helper queries Codex and decides whether capacity is ready. This
is an automation pattern agents can build, not a preinstalled Worklane allowance service.

For ChatGPT-backed Codex, start `codex app-server --listen stdio://`, send `initialize`,
then the `initialized` notification. Query `account/rateLimits/read`. Full remaining
allowance means a window's `usedPercent` is near zero, not 100. Define which window triggers dispatch and the required headroom in other
applicable buckets/windows, including any reserve for interactive work. Use server-reported
limit states and credit information when available; `account/usage/read` reports token activity
and does not establish remaining allowance. See the
[OpenAI app-server documentation](https://learn.chatgpt.com/docs/app-server#6-rate-limits-chatgpt).

Check the actual window durations and relevant buckets returned by Codex. For example,
a short-term refill can trigger work only while the weekly window retains the agreed
interactive reserve. API-key-backed workflows require their provider's own limit and
spend signals; this ChatGPT allowance check does not establish API capacity.

Use one monitor and one serialized dispatcher per shared account queue. A user timer can check
periodically and near the reported `resetsAt`; connected monitors can also consume
`account/rateLimits/updated`. Always fetch fresh state before dispatch: a predicted reset time
or missing/stale data does not establish that allowance is full. Persist refill/dispatch state
and durable job claims below `$HOME/.local/state/` so repeated timer ticks, notifications, or
service restarts do not launch the same batch again while the allowance still appears full.

On a qualifying refill, claim a bounded batch from the user's authorized backlog and start it
through a user service. Recheck capacity between batches and keep queue-wide concurrency and
retry controls: other lanes may spend the same allowance, and a quota check reserves nothing.
Use the existing `scripts/codex-bulk` scheduler for suitable bulk jobs. Let unfinished work wait
for the next qualifying refill. Restrict dispatch to the agreed tasks and resource/spend budget;
available allowance alone does not authorize new work, credit purchases, or consuming earned
resets. Log the observed allowance, dispatch decision, and claimed job IDs for inspection.

## A bounded user service

Create the workflow executable at `$HOME/.local/bin/lane-review` before enabling a unit
that invokes it. For example, place this at `$HOME/.config/systemd/user/lane-review.service`:

```ini
[Unit]
Description=Review authorized lane work

[Service]
Type=oneshot
WorkingDirectory=%h
ExecStart=%h/.local/bin/lane-review
TimeoutStartSec=20min
TasksMax=512
MemoryHigh=3G
MemoryMax=4G
```

Validate and start it with:

```sh
systemd-analyze --user verify "$HOME/.config/systemd/user/lane-review.service"
systemctl --user daemon-reload
systemctl --user start lane-review.service
journalctl --user-unit lane-review.service
```

Attach a timer, path, or socket unit when the workflow needs activation. Enable that
activation unit once its schedule and workload are agreed. A transient job can instead use:

```sh
systemd-run --user --collect --unit lane-review-now \
  --property RuntimeMaxSec=20min --property TasksMax=512 \
  "$HOME/.local/bin/lane-review"
```

The resource values are examples, not machine-wide defaults. A oneshot service needs
`TimeoutStartSec=` to bound its command; `RuntimeMaxSec=` does not limit that service type.
See the [systemd service reference](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html).

## Programmatic Codex jobs

`codex exec` runs an independent job without opening a TUI. Supply a JSON Schema for
structured final output. The installed CLI appends piped input to an explicit prompt:

```sh
codex exec --ephemeral --model gpt-5.6-luna \
  -c 'model_reasoning_effort="low"' \
  --output-schema ./classification.schema.json \
  "Classify the supplied record. Return only the schema-defined result." < record.json
```

Use `--json` when the caller needs the event stream rather than only the final response.
Check the installed `codex exec --help` when adapting examples to another CLI version.
The standard Worklane image configures Codex with approvals disabled and
`danger-full-access`; its launcher also applies these settings. Do not assume bulk jobs
run in a read-only Codex sandbox. The lane's immutable root and container configuration
still apply. Inspect the launcher and effective settings when a job needs different access.
See [non-interactive Codex](https://learn.chatgpt.com/docs/non-interactive-mode).

### The repository bulk driver

When the Worklane checkout is available, prefer `scripts/codex-bulk` for independent
classification, extraction, or tagging. It is not installed globally in every lane.
Run it from the checkout, with a schema file and JSONL input containing one object per
line and a unique, stable `id` field:

```sh
scripts/codex-bulk records.jsonl \
  --output classifications.jsonl \
  --schema classification.schema.json \
  --prompt "Classify the supplied record. Return only the schema-defined result."
```

Run `scripts/codex-bulk --help` for overrides. The current defaults are:

| Setting | Default behavior |
| --- | --- |
| Model / reasoning | `gpt-5.6-luna` / `low` |
| Workers | Start at 32; add one after 32 consecutive successes; hard ceiling 256 |
| Rate-limit detection | Stderr contains `429 Too Many Requests` or `exceeded retry limit`, case-insensitively |
| Backoff | Halve target concurrency, floor one; pause new launches for 60 seconds plus 0–10 seconds jitter |
| Retry budget | Three total attempts and 900 seconds elapsed per record |
| Results | Append and fsync `ok`, `retrying`, or `error` JSONL events with the input ID |
| Resume | Skip IDs with an `ok` event; restore attempts and start time from `retrying` events |

Keep the append-only output when restarting. Use the same IDs, records, prompt, and schema
for a resumed job; choose a new output file when the task changes. Terminal `error` events
are not completed IDs and can run again on a later invocation. Retry-state restoration
comes from `retrying` events, not all final errors. Blank input lines are skipped; duplicate
IDs are rejected after string conversion.

The driver passes the schema to Codex. It does not independently validate returned data
against that schema: a zero exit status is recorded as `ok`, with parsed JSON when possible
and otherwise raw text. It has no automatic confidence routing or fallback-model stage.
Consumers that need these checks must implement them before accepting results; reserve
stronger models or the interactive agent for ambiguous cases and synthesis.

Use one scheduler and one writer per queue/output file; the driver coordinates its own
workers, not other driver processes. Preserve its shared cooldown rather than launching
unbounded background runs or immediate per-process retries. In-flight work continues during
a cooldown. The default worker count is project policy, not a guarantee of available capacity;
adjust it to the account and workload. Group records only when they require shared context.

The stderr heuristic does not reliably distinguish exhausted quota or billing failures from
transient rate limits. Inspect terminal errors before retrying them. When writing a direct
API client, honor `Retry-After` and `x-ratelimit-*` headers and retain queue-wide backoff.
See [OpenAI rate-limit guidance](https://developers.openai.com/api/docs/guides/rate-limits).
