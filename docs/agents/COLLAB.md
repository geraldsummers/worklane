# Lane-local Pi collaboration

`collab.ts` uses protocol v2 over the lane-local Unix socket. It registers
`agent_send`, `agent_receive`, `agent_reply`, `agent_list_peers`, `agent_ack`,
`task_delegate`, `run_finish`, and `goal_plan`. It neither starts the daemon nor
exposes owner-wide service controls. See [PROTOCOL.md](../../assets/worklane-runtime/PROTOCOL.md).

## Identity and messages

Registration starts at `session_start`, not discovery. It binds the durable Pi
session UUID, exact Herdr session/workspace/pane, PID, project directory, and
optional managed run. The session lease stays in memory; shutdown clears timers,
owned socket operations, and the lease. An unavailable daemon fails explicitly.

Peer messages are displayed as custom coordination messages, never human
consent. Only runtime-tagged helper results correlated to the current parent
run/group may automatically continue its model work. Ordinary peer messages
cannot wake interactive sessions. Result IDs are deduplicated against the active
session branch. Reading does not acknowledge messages; use `agent_ack`.

## Delegation without a separate planning permission

`task_delegate` can open a bounded interactive parent group when no managed run
exists. No approved plan or autonomous goal is needed. Default limits are two
concurrent helpers, 30 minutes and 20 logical requests per helper, and bounded
parent automatic continuation. Relative claims reject escapes and overlap;
claims coordinate work, not filesystem permissions. Helpers share project files.

The runtime allocates a separate agent workspace without focusing it. Each
helper gets one tab and a durable session; the human workspace stays intact.
Queued/running work is blocked after a daemon restart, never silently replayed.
Cancelling or failing a parent stops its unfinished children, not unrelated tabs.

## Managed goals remain independently authorized

A managed run receives its direct prompt once; no third-party Plan plugin or
read-only planning mode is involved. `goal_plan` submits full Markdown plus at
most 50 exact `{operation, arguments}` descriptors to an existing planning run.
It does not create or approve goals. Human approval binds the full goal hash.

`run_finish` records status, summary and evidence. Successful goal/task execution
requires explicit verification; response settlement alone is not success.
Parents wait for pending helper results and verify the combined work. Chat
`settled`/`error` is emitted at `agent_settled`, not the earlier `agent_end`.

## Accounting and resources

Managed requests and automatic interactive continuations use the SDK's
`before_provider_request` hook to count logical requests and require provider
HTTP retries=0. Scoped hooks suppress cache warming and compaction for that
automatic work without changing normal interactive compaction settings. Ordinary
human-driven interactive sessions may use normal compaction. Custom nested model calls are not covered by
this accounting; the managed extension set is explicit and bounded.

Plan artifacts (`plan_*`), questions (`questionnaire`), and local feedback
(`lane_complain`) are independent of these budgets and external-service grants.
Plan approval is not blanket mail/Matrix/stack authority. See [AUTONOMY.md](AUTONOMY.md).

## Validation

Run `node tests/run-pi-extensions.mjs --typecheck`, then the same command without
`--typecheck`, against the locked SDK. Python runtime tests cover group limits,
protocol compatibility and durable artifacts. These are source/contract tests,
not subscription, live-service, terminal or phone acceptance.
