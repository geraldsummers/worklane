# Worklane runtime protocol v1

Local-only NDJSON requests/responses over `$HOME/.local/share/worklane/collab.sock`.
Request `{version:1, action:NAME, ...fields}`; response `{ok:true,result:DATA}` or `{ok:false,error:TEXT}`.
CLI: `worklane-collab request` reads one JSON request from stdin and prints one JSON response. Daemon: `worklane-collab serve`. Viewer: `worklane-collab watch`.

Actions:
- `agent.register`: `agent_id` (UUID Pi session id), `session`, `pane_id`, `pid`, `role`, `run_id?`; result agent record plus session-scoped `lease` token. Mutating agent operations require agent_id + lease. `agent.heartbeat`: `agent_id,lease,status`. `agent.list`: list active peers.
- `message.send`: `agent_id,lease,to,subject,body,kind?,correlation_id?,idempotency_key?`; result envelope. `message.read`: agent_id,lease,limit<=50; result unread envelopes. `message.ack`: agent_id,lease,id; marks consumed. `message.reply`: as send plus `reply_to` original message UUID; daemon resolves recipient.
- `goal.create`: statement, schedule? (systemd OnCalendar), budget?; draft goal. `goal.list`; `goal.get`: id; `goal.plan`: id,agent_id,lease,plan (Markdown), actions (array of exact external action descriptors); awaiting_approval. `goal.approve`: id, plan_sha256; human CLI/GUI only, cannot be called via Pi tools. `goal.reject`: id. `goal.cancel`: id. `goal.schedule`: id, calendar. All schedule occurrences require new approval.
- `run.finish`: agent_id,lease,run_id,status (done/failed/blocked),summary,evidence?; persists final state. `run.turn`: agent_id,lease,run_id; increments budget counter, returns run state. `run.authorize`: agent_id,lease,run_id,operation,arguments; true only for an approved current run whose actions include exact operation + argument object.
- `task.create`: agent_id,lease,run_id,statement,paths (relative path claims); creates visible helper, max 2. `snapshot`: peers, messages, goals, runs, tasks, events; read-only.
- `chat.submit`: prompt,source (gateway/matrix/device/librechat); starts a distinct durable visible Herdr agent run; result `{id}`. `chat.events`: id,after sequence; result `{events:[{seq,type,delta?}],status,error?}`. `chat.cancel`: id. Pi extension sends `chat.event`: agent_id,lease,run_id,event `{type: text_delta|settled|error,delta?}`. This API is lane-local; host gateway verifies human identity/ownership before entering lane.

Per-run identity is injected by Herdr tab create --env WORKLANE_RUN_ID=... --env WORKLANE_RUN_KIND=chat|plan|goal|task. The daemon starts `worklane-pi-agent` with a durable Pi session file. Extensions claim the matching run on session_start, obtain work via `run.input` (agent_id,lease,run_id), and inject peer messages as custom messages, never human prompts. plan run input instructs `/plan` activation; approval is always external to the model. `run.input` returns `{prompt,kind,plan?,actions?}` once per run and is idempotent across reload. Resume interrupted runs only explicitly; never automatic.

Same-UID is a trust boundary, not adversarial isolation. No cross-lane relay. All P0 external authority checks remain in existing brokers. Plan-mode helpers may register goal plans but never approve or delegate implementation before approval. Native P0 wrappers are separate from this transport.
