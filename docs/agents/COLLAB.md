# Bounded Pi collaboration extension

`collab.ts` registers `agent_send`, `agent_receive`, `agent_reply`,
`agent_list_peers`, `agent_ack`, `task_delegate`, `run_finish`, and `goal_plan`.
It implements protocol v1 in `PROTOCOL.md` over the lane-local Unix socket;
it does not start a daemon, invoke hidden agents, or expose owner goal controls.

## Lifecycle and identity

No resources start during extension discovery. `session_start` requires a real
Herdr pane, `HERDR_SESSION` (or `WORKLANE_SESSION`), and a durable Pi session file.
Registration sends the Pi session UUID, exact Herdr session/pane, PID, session
file, role, and optional `WORKLANE_RUN_ID`. The returned lease stays in memory,
never tool output or persisted entries. Heartbeat and unread-message polling run
at five-second intervals without overlapping polls. Shutdown reports completion,
clears the timer, cancels session-owned socket operations, and discards the lease.
A stopped/unavailable daemon causes tools to fail closed; it is not restarted.

Peer envelopes use **custom messages with `display: true`**, explicitly marked as
peer coordination rather than human authorization. They never use
`sendUserMessage`. Only an already human-approved, budgeted goal/task run may
continue on a custom peer result message; interactive/planning peers never
trigger a model turn. Their IDs are deduplicated against the
active session branch, including reloads. `agent_ack` marks an envelope consumed
only when explicitly called; polling does not acknowledge unread work.

## Managed runs and planning

The daemon owns visible Herdr tabs and durable sessions. On registration,
`run.input` supplies work for the exact current `WORKLANE_RUN_ID`.
Non-plan kickoff uses the human-authorized run prompt, with a persisted branch
marker preventing repeat delivery on `/reload`. Interrupted work is not resumed
automatically. The daemon dispatches planning runs through the real interactive
`/plan` command from the pinned Plan plugin; this extension never fakes Plan
activation or injects a second planning kickoff.

Humans create goals through the owner-checked P0/Worklane CLI, not Pi tools.
Within that managed planning run, `goal_plan` stages at most 50 exact
`{operation, arguments}` descriptors. It does **not** create a goal, grant
approval, or delegate execution. The installer must include `goal_plan` in the
Plan plugin's read-only allowlist. A successful version-1 `plan_mode_complete`
result is reflected into `goal.plan`, with staged descriptors restored across
reloads. Without staging, actions default to the daemon-provided list (normally
empty). Approval remains an external human action over the plan hash.

`task_delegate` requires a current execution run and validates relative path
claims; the daemon enforces approval, ownership overlap, and its two-helper cap.
`run_finish` records explicit status/summary/evidence and avoids duplicate
settlement. Successful execution requires explicit verification evidence;
ordinary settlement is not success. Parents with pending helpers wait for
custom result messages before verifying and completing. Successful chat deltas stream through `chat.event`; terminal
`settled`/`error` is sent at `agent_settled`, not the earlier `agent_end` boundary.
Budget exhaustion settles as blocked through `run.finish` instead.

## Execution budget contract

The installed SDK's `before_provider_request` hook counts each **logical model
request**, including new-turn and Pi recovery retries. `before_agent_start` and
`turn_start` reject already exhausted runs; charging them as well would double
count requests. A denied or unavailable `run.turn` aborts the request and blocks
subsequent tools.

Provider-internal HTTP retries are **not individually observable** through this
SDK hook. Managed runs therefore fail closed unless the effective settings are:

```json
{
  "retry": { "provider": { "maxRetries": 0 } },
  "cacheWarming": "off",
  "compaction": { "enabled": false }
}
```

This check happens before each request, so project overrides cannot silently
re-enable invisible HTTP retries, cache warming, or automatic compaction calls.
Arbitrary third-party extensions making nested model calls are outside this
hook; the managed installed extension/loadout policy must remain bounded.
No extension-only claim of transport-level accounting is made.

## Focused validation

`collab.test.ts` and `p0.test.ts` run with Node's built-in test runner through the
installed Pi `jiti` loader. No SDK was installed or upgraded. Typecheck against
the installed `pi-coding-agent`, `pi-ai`, Node declarations, and TypeScript with
`strict`, `moduleResolution: bundler`, `allowImportingTsExtensions`, `noEmit`.
Module aliases/TypeScript paths may be needed when these assets live outside
Pi's package tree; point them at the installed packages, not mocked SDK types.

The delegated implementation's actual run used:

```sh
TMPDIR=/home/dev/.tmp node /home/gerald/.tmp/pi-extension-test-runner.mjs
TMPDIR=/home/dev/.tmp node "$TSC" -p /home/gerald/.tmp/pi-extension-tsconfig.json
```

The runner imports both adjacent test files via the installed Pi `createJiti`
with `fsCache: false` and an alias for the installed `@earendil-works/pi-ai`.
The generated runner/config are local validation artifacts, not runtime assets.
`/home/dev/.tmp` exists but is not writable by this host's `gerald` user; test
socket fixtures and generated configuration use `/home/gerald/.tmp` explicitly.
Full release/coverage/isolated acceptance validation belongs to the parent
integration checkpoint; these focused tests do not substitute for that gate.
