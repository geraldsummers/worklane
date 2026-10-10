# Worklane runtime protocol v2

Local NDJSON over `$HOME/.local/share/worklane/collab.sock`:
`{version:2,action:NAME,...}` → `{ok:true,result:DATA}` or
`{ok:false,error:TEXT}`. CLI: `worklane-collab request`; daemon: `serve`; viewer:
`watch`. `protocol.describe` reports version/capabilities. Version 1 remains
accepted for legacy actions; plans, interactive groups and notifications need v2.

## Identity and coordination

- `agent.register`: session UUID `agent_id`, `session`, `pane_id`, `workspace_id`,
  `cwd`, `pid`, `role`, optional `run_id`; returns the session lease. Mutating
  agent operations require `agent_id,lease`. Heartbeat and list are lane-local;
  shutdown reports `agent.heartbeat` with `status=stopped`. Registration is bound to the durable Pi session and exact pane.
- `message.send/read/ack/reply`: bounded envelopes with correlation/reply IDs.
  Messages are custom coordination, not human instructions or authorization.
- `interactive.open/children/turn/finish`: agent-owned bounded helper group,
  child result/pending records, continuation request charging, explicit finish.
- `task.create`: agent lease, optional current managed `run_id`, statement and
  bounded relative `paths`; max two concurrent helpers. Without a managed run it
  uses an interactive parent group. Each helper gets a no-focus agent tab.

## Goals and runs

- `goal.create/list/get/plan/approve/reject/cancel/schedule`: owner workflows.
  `goal.plan` submits full Markdown and exact external `{operation,arguments}`
  descriptors from a matching planning run. `goal.approve` binds the returned
  hash and is human CLI/GUI only. Scheduled occurrences need fresh approval.
- `run.input/turn/authorize/finish`: current run plus agent lease. Prompt delivery
  is session-deduplicated; request counters and exact external approvals are
  independently enforced. Verified `done` needs evidence; settlement is not it.
- `chat.submit/events/cancel/event`: legacy host-gateway chat, owner checked
  before entering the lane. `settled/error` uses Pi's final `agent_settled`
  boundary, not internal model/tool turn completion.
- `snapshot`: read-only peers/messages/goals/runs/tasks/events.

Managed identity is injected as `WORKLANE_RUN_ID`/`WORKLANE_RUN_KIND`. The daemon
uses durable sessions and scopes. No `/plan` activation, shell allowlist or
model-changing planning hook exists. Interrupted queued/running work is blocked,
not replayed. Failed/cancelled parents stop unfinished owned children.

## Plans

`plan.propose/list/get/update/history/approve/implement/revise/step/finish/archive`
store revisioned project Markdown/steps. Proposals include source/project/session;
step and finish requests carry verification evidence. Export is a private Pi
`/plan export PATH` command, not a runtime action. Updates use revision CAS. Human approval
binds ID/revision/content hash; implementation is separate. Content changes
invalidate approval; status/evidence changes do not. Completed steps/finish need
verification evidence. Pi only exposes artifact/status tools; trusted human
input or CLI handles approval/implementation. Same-UID remains trusted, not an
adversarial security boundary.

## Notifications

`notification.settings`: human channel/category preferences (no agent lease).
`notification.enqueue`: agent lease, `kind,key,title,outcome`; no topic, URL,
recipient or credential fields. Keys deduplicate durable events. `notification.terminal`
records local presentation. Remote delivery retries through the scoped
`~/.p0/notify.py` helper to VM-owned routing; missing provisioning stays pending.
Kinds distinguish review/question/response/task. `response` is not verified root
completion. OSC 9 terminal titles are sanitized and content-minimized.
No cross-channel dismissal, approval coupling, companion or phone listener exists.

No TCP listener or cross-lane relay. Existing P0 brokers remain authoritative
for external mail/Matrix/memory/stack grants. Local feedback is approval-free;
external forwarding is not. Wrappers/contracts do not prove live service delivery.
