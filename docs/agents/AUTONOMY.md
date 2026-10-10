# Conversational plans and managed goals

## Plans are artifacts, not a permission mode

Ask normally to make or revise a plan. `plan_propose`, `plan_read`, `plan_update`,
`plan_step_update` and `plan_finish` keep versioned Markdown, steps and evidence
in private runtime storage. `/plan` is optional; `/plan export PATH` writes a
private, exclusive Markdown export.
Questions remain available through `questionnaire`; no mode entry is required.
These tools do not restrict shell/edit/delegation tools or change models.

A human's interactive/RPC “approve the plan” or “implement the plan” input binds
the displayed ID/revision/hash. Peer/extension messages never count as approval.
A stale revision must be displayed again. Approval alone does not start work;
implementation is a separate human instruction. Content revisions invalidate
approval; progress/evidence updates do not. Verified completion needs evidence.
When multiple project plans match, select the intended ID rather than guessing.

## Managed P0 goals

Managed goals are separate owner-authorized jobs. Their external actions still
need the existing exact-descriptor approvals and broker grants:

```sh
p0 --json goal create 'Review and fix failing tests' --lane LANE
p0 --json goal list --lane LANE
p0 --json goal get GOAL_UUID --lane LANE
p0 --json goal approve GOAL_UUID --lane LANE --plan-sha256 EXACT_HASH
p0 --json goal reject GOAL_UUID --lane LANE
p0 --json goal cancel GOAL_UUID --lane LANE
p0 --json goal schedule GOAL_UUID --lane LANE --calendar daily
```

Each scheduled occurrence plans afresh and needs fresh approval. Pending
occurrences do not overlap. Planning runs submit Markdown/actions directly with
`goal_plan`; no read-only Plan plugin is used. Goals do not approve themselves.
A local shell can use NDJSON, for example:

```sh
printf '%s\n' '{"version":2,"action":"goal.list"}' | worklane-collab request
```

## Helpers and settlement

Relevant `task_delegate` work is allowed by default, including ordinary
interactive sessions. Defaults are two concurrent helpers, 30 minutes/20 logical
requests per helper and bounded parent automatic continuation. One helper runs
per tab in a separate agent workspace, without focusing it. Claims are advisory;
shared project permissions are unchanged. Verify combined results before finish.

`agent_settled` means the visible response has ended, not verified root-task
completion. Plan review, waiting questions, response settlement and verified
completion are distinct notification events. Terminal and private owner ntfy
channels have independent dismissal; there is no companion or dismissal relay.
The runtime outbox retries unavailable remote delivery rather than claiming it
succeeded. Provisioned VM-owned routing chooses recipients; agents cannot.

## Recovery and boundaries

```sh
systemctl --user status worklane-collab.service worklane-goal-runner.timer
journalctl --user-unit worklane-collab.service
```

State: `~/.local/share/worklane/collab/messages.sqlite3`; socket:
`~/.local/share/worklane/collab.sock`; managed sessions:
`~/.pi/agent/sessions/worklane-runs`. Stopped lanes do not wake themselves.
Interrupted queued/running work becomes blocked and requires explicit retry.
Cancellation stops owned scopes/children, never unrelated panes.

Managed requests and automatic continuations require provider retries=0 and use
scoped hooks to suppress cache warming and compaction for bounded accounting.
Ordinary human-driven Pi uses normal resources/settings and may compact. Every account uses its own subscription
OAuth; no silent API-key or OpenRouter fallback is permitted. Local complaints
need no approval; forwarding externally is a separate authorized action.

Same-UID is a workflow trust boundary, not a sandbox against malicious `dev`
code. No owner-wide APIs, credential files or new host sockets belong in ordinary
lanes. Live SSO/service/device proof is separate from source/contract tests.
