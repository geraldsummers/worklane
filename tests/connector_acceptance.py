#!/usr/bin/env python3
"""Black-box acceptance for the Worklane API, Herdr bridge, and MCP adapter."""

import argparse
import http.client
import json
import os
import socket
import subprocess
import time


def api(worklane, method, params):
    request = {
        "version": 1,
        "request_id": f"acceptance-{method}-{time.time_ns()}",
        "method": method,
        "params": params,
    }
    environment = dict(os.environ)
    environment["WORKLANE_API_INLINE"] = "1"
    completed = subprocess.run(
        [worklane, "api"],
        input=json.dumps(request),
        text=True,
        capture_output=True,
        check=True,
        env=environment,
    )
    response = json.loads(completed.stdout)
    if not response["ok"]:
        raise RuntimeError(response["error"])
    return response["result"]


def native(worklane, lane, method, params):
    response = api(
        worklane,
        "herdr.call",
        {"lane": lane, "method": method, "params": params},
    )
    if "error" in response:
        raise RuntimeError(response["error"])
    return response


def snapshot(worklane, lane):
    return native(worklane, lane, "session.snapshot", {})["result"]["snapshot"]


def post(port, body, session=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
    }
    if session:
        headers["MCP-Session-Id"] = session
    connection.request("POST", "/mcp", json.dumps(body), headers)
    response = connection.getresponse()
    payload = response.read().decode()
    response_headers = dict(response.getheaders())
    connection.close()
    if response.status not in (200, 202):
        raise RuntimeError((response.status, payload))
    messages = []
    for line in payload.splitlines():
        if line.startswith("data: ") and line[6:].strip():
            messages.append(json.loads(line[6:]))
    return response_headers, messages


def mcp_acceptance(mcp, worklane):
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    process = subprocess.Popen(
        [
            mcp,
            "--unsafe-disable-auth",
            "--listen",
            f"127.0.0.1:{port}",
            "--worklane-bin",
            worklane,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        for _ in range(50):
            try:
                connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
                connection.request("GET", "/healthz")
                if connection.getresponse().status == 200:
                    connection.close()
                    break
            except OSError:
                pass
            time.sleep(0.1)
        else:
            raise RuntimeError("worklane-mcp did not become healthy")
        headers, messages = post(
            port,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "worklane-acceptance", "version": "1"},
                },
            },
        )
        session = headers["mcp-session-id"]
        assert messages[-1]["result"]["capabilities"]["tools"] == {}
        post(port, {"jsonrpc": "2.0", "method": "notifications/initialized"}, session)
        _, messages = post(
            port,
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
            session,
        )
        tools = {tool["name"]: tool for tool in messages[-1]["result"]["tools"]}
        assert set(tools) == {
            "worklane_schema",
            "worklane_read",
            "worklane_change",
            "operation_get",
            "operation_cancel",
            "herdr_schema",
            "herdr_call",
        }
        assert tools["herdr_call"]["annotations"]["destructiveHint"] is True
        assert tools["worklane_read"]["annotations"]["readOnlyHint"] is True
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("worklane")
    parser.add_argument("worklane_mcp")
    parser.add_argument("lane")
    arguments = parser.parse_args()

    opened = api(arguments.worklane, "lane.open", {"lane": arguments.lane})
    assert opened["state"] == "succeeded", opened
    schema = api(arguments.worklane, "herdr.schema", {"lane": arguments.lane})
    methods = {
        item["properties"]["method"]["const"]
        for item in schema["schemas"]["request"]["oneOf"]
    }
    assert {"pane.split", "pane.send_input", "pane.read", "agent.prompt"} <= methods

    before = snapshot(arguments.worklane, arguments.lane)
    shell_targets = [
        pane
        for pane in before["panes"]
        if pane.get("label") != "git diff" and not pane.get("agent")
    ]
    target = shell_targets[0] if shell_targets else before["panes"][0]
    native(
        arguments.worklane,
        arguments.lane,
        "pane.split",
        {
            "target_pane_id": target["pane_id"],
            "direction": "down",
            "cwd": "/home/dev",
            "focus": False,
        },
    )
    after = snapshot(arguments.worklane, arguments.lane)
    previous_ids = {pane["pane_id"] for pane in before["panes"]}
    shell_pane = next(pane for pane in after["panes"] if pane["pane_id"] not in previous_ids)
    marker = "worklane-connector-shell-ok"
    native(
        arguments.worklane,
        arguments.lane,
        "pane.send_input",
        {"pane_id": shell_pane["pane_id"], "text": f"printf '{marker}\\n'", "keys": ["enter"]},
    )
    for _ in range(50):
        output = native(
            arguments.worklane,
            arguments.lane,
            "pane.read",
            {"pane_id": shell_pane["pane_id"], "source": "recent_unwrapped", "lines": 40},
        )
        if marker in json.dumps(output):
            break
        time.sleep(0.1)
    else:
        raise RuntimeError("arbitrary shell output did not appear")

    closed = native(
        arguments.worklane,
        arguments.lane,
        "pane.close",
        {"pane_id": shell_pane["pane_id"]},
    )
    assert closed["result"]["type"] == "ok"
    for _ in range(50):
        remaining_ids = {
            pane["pane_id"] for pane in snapshot(arguments.worklane, arguments.lane)["panes"]
        }
        if shell_pane["pane_id"] not in remaining_ids:
            break
        time.sleep(0.1)
    else:
        raise RuntimeError("acceptance shell pane did not close")

    agents = native(arguments.worklane, arguments.lane, "agent.list", {})["result"]["agents"]
    codex = next(agent for agent in agents if agent.get("agent") == "codex")
    prompt = "Worklane connector acceptance prompt; do not modify files."
    prompted = native(
        arguments.worklane,
        arguments.lane,
        "agent.prompt",
        {"target": codex["pane_id"], "text": prompt},
    )
    assert prompted["result"]["type"] == "agent_prompted"
    assert prompted["result"]["agent"]["pane_id"] == codex["pane_id"]
    mcp_acceptance(arguments.worklane_mcp, arguments.worklane)


if __name__ == "__main__":
    main()
