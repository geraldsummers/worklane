# Human-approved, visible autonomous Worklane goals

Worklane's lane-local `worklane-collab.service` durably stores messages, goals and
runs. `worklane-goal-runner.timer` supervises queue health; per-goal user-systemd
timers schedule new occurrences. The lane must be running; stopped lanes do not
wake themselves. These services never approve plans or replay interrupted work.

## Submit, review, approve

In Platform Zero, select an owned agent lane and use the Goals page, or:

```sh
p0 --json goal create 'Review and fix the failing project tests' --lane LANE
p0 --json goal list --lane LANE
p0 --json goal get GOAL_UUID --lane LANE
p0 --json goal approve GOAL_UUID --lane LANE --plan-sha256 EXACT_HASH
p0 --json goal reject GOAL_UUID --lane LANE
p0 --json goal cancel GOAL_UUID --lane LANE
p0 --json goal schedule GOAL_UUID --lane LANE --calendar daily
```

The UUID and hash come from the returned record, not a guessed name. Approval
binds both the full Markdown plan and exact external action descriptors. A stale
hash is rejected. Every scheduled occurrence plans afresh and needs new human
approval. Overlap (including a pending unapproved occurrence) is skipped and
logged. Cancelling a schedule's goal prevents future occurrences. There are no
preapproved recurring templates.

From a lane's shell the same local API is available via stdin NDJSON:

```sh
printf '%s\n' '{"version":1,"action":"goal.list"}' | worklane-collab request
```

For submission send `goal.create` with `statement`; to approve send
`goal.approve` with `id` and `plan_sha256`. Approval is a human CLI/GUI action,
not a Pi model tool. This is a workflow boundary inside a trusted same-UID lane,
not a security sandbox against malicious code already running as `dev`.

## Herdr visibility and execution

A planning run appears in its own `plan:UUID` tab and invokes the real pinned
`/plan` plugin. Questions can be answered in that visible pane. Completing the
plan records it as `awaiting_approval`; it cannot approve itself. After approval,
a new durable Pi session appears in a `goal:UUID` tab. Helpers appear in
`task:UUID` tabs, with relative path claims and parent/run identity. The
`collaboration` tab shows peers, messages, goal/run states and failures without
stealing focus. Use stable IDs, never the currently focused pane as identity.

Only an approved execution goal may call `task_delegate`. Defaults are two
helpers, 30 minutes per run and 20 logical provider requests per agent. Relative
claims reject traversal and overlaps between active helpers. Claims are advisory,
not filesystem isolation: helpers share the lane's workspace and permissions.

Wait for helper result messages, inspect their evidence, and verify the combined
work. Finish through `run_finish` with explicit status, summary and verification
evidence. Idle/settlement alone is not success. Helper result custom messages can
continue only already authorized, budgeted execution runs; ordinary interactive
or planning peers do not automatically launch new work.

Timeout, exhausted budget, lost daemon authorization, restart or failure produces
a blocked/failed run. The queue does not silently resume model work. Inspect the
state, then submit a fresh goal explicitly if retry is intended. User-systemd
scopes own managed run processes so cancellation does not kill unrelated panes.

## External Platform Zero actions

During planning, call `goal_plan` to stage exact `{operation,arguments}` actions
before `plan_mode_complete`. For example `p0.mail.send` with the exact recipient,
subject and body. P0 tools cannot infer broad permission from a goal statement.
Different arguments require a new approval. Existing broker grants, suspension,
revocation and Gerald's mandatory approvals remain authoritative. Do not expose
new host broker sockets or mount owner-wide control APIs to ordinary lanes.

## Runtime and recovery

```sh
systemctl --user status worklane-collab.service worklane-goal-runner.timer
journalctl --user-unit worklane-collab.service
```

Durable state is private SQLite under
`~/.local/share/worklane/collab/messages.sqlite3`; the private lane-local socket is
`~/.local/share/worklane/collab.sock`. No TCP listener or cross-lane relay exists.
Run transcripts persist under `~/.pi/agent/sessions/worklane-runs`.

The bounded extension requires provider retries disabled, cache warming off and
automatic compaction disabled. Those features can issue invisible extra model
requests; managed runs fail closed if settings re-enable them. Arbitrary custom
extensions making their own nested model calls are not covered by this budget.
Review custom loadouts before delegating work.
