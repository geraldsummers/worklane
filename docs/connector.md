# ChatGPT connector

Worklane's connector is deliberately split into two small programs:

```text
ChatGPT -> HTTPS/Caddy -> 127.0.0.1:47831/mcp -> worklane-mcp -> worklane api
                                                        -> Podman -> lane Herdr socket
```

`worklane` remains the only lifecycle authority. `worklane api` accepts one versioned JSON
request on standard input and writes one correlated response on standard output. Mutating lane
methods return durable operation records and run in transient `systemd --user` units. Records,
events, results, and errors remain under `$XDG_STATE_HOME/worklane/api-jobs` (normally
`~/.local/state/worklane/api-jobs`) across controller and MCP restarts.

`worklane-mcp` is a Streamable HTTP adapter. It binds to `127.0.0.1:47831` by default and exposes:

- `/mcp` for MCP Streamable HTTP;
- `/healthz` for an unauthenticated liveness check; and
- `/.well-known/oauth-protected-resource` for OAuth resource discovery.

The connector exposes seven tools: `worklane_schema`, `worklane_read`, `worklane_change`,
`operation_get`, `operation_cancel`, `herdr_schema`, and `herdr_call`. Lifecycle changes are
asynchronous. `herdr_call` is the full terminal/Codex surface: discover the installed schema,
then use native methods such as `pane.split`, `pane.send_input`, `pane.read`, `agent.prompt`,
`agent.wait`, and `agent.read`. `pane.send_input` with command text plus the `enter` key
intentionally permits arbitrary shell commands inside the selected lane.
The bridge requires explicit stable IDs, limits calls to 60 seconds and 1 MiB, and rejects
unbounded subscriptions, live handoff, focused-pane lookup, and graphics transfer.

## Install the three binaries

Build and install `worklane`, `lane`, and `worklane-mcp` for the controller user's architecture.
All three belong in `~/.local/bin`. The MCP service must run as the same user that owns the
rootless Podman inventory; do not run it in a lane or as root.

Inspect the machine contract directly:

```sh
worklane api schema
printf '%s\n' '{"version":1,"request_id":"example","method":"lane.list","params":{}}' |
  worklane api
```

## Configure OAuth and systemd

Use the existing OIDC provider as the OAuth 2.1 authorization server. It must support the
authorization-code flow with PKCE and issue signed JWT access tokens whose issuer, audience,
expiry, and scopes can be verified. Configure these scopes:

- `worklane:read`
- `worklane:write`
- `worklane:terminal`

Create `~/.config/worklane/mcp.env` with mode `0600`:

```sh
WORKLANE_MCP_LISTEN=127.0.0.1:47831
WORKLANE_MCP_RESOURCE_URL=https://lanes.example.internal
WORKLANE_MCP_OIDC_ISSUER=https://identity.example.internal
WORKLANE_MCP_OIDC_AUDIENCE=https://lanes.example.internal
```

The resource URL is the canonical public resource identifier and should normally omit `/mcp`.
The authorization server must echo the OAuth `resource` parameter into the access token audience.
The connector refreshes JWKS when it sees an unknown `kid`. It requires all three scopes on this
internal first-version endpoint.

Install and validate the supplied user unit:

```sh
mkdir -p ~/.config/systemd/user
cp contrib/systemd/worklane-mcp.service ~/.config/systemd/user/
systemd-analyze --user verify ~/.config/systemd/user/worklane-mcp.service
systemctl --user daemon-reload
systemctl --user enable --now worklane-mcp.service
curl --fail http://127.0.0.1:47831/healthz
```

The development-only `--unsafe-disable-auth` flag works only with a loopback listener. Never use
it behind a reachable reverse proxy.

## Put Caddy in front

Adapt [the example Caddy route](../contrib/caddy/worklane-mcp.Caddyfile) in Platform Zero. Caddy
terminates HTTPS and reverse-proxies to loopback; it must preserve `Authorization`,
`MCP-Session-Id`, `Last-Event-ID`, content type, and streaming responses. The example disables
response buffering for SSE. Do not expose port 47831 on a non-loopback interface.

Point the ChatGPT connector at `https://lanes.example.internal/mcp`. Before adding it, verify the
endpoint with MCP Inspector and exercise initialization, tool listing, invalid identifiers,
authentication failures, annotations, a lifecycle operation, `pane.send_input`/`pane.read`, and a
Codex `agent.prompt`/`agent.wait`/`agent.read` round trip.
