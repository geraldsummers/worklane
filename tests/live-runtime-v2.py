#!/usr/bin/env python3
"""Real installed UNIX runtime/SQLite acceptance, with synthetic artifacts.

No subscription/model request, broker publish or device-delivery claim.
Run inside the disposable acceptance lane after the user service is started.
"""
import json
import os
from pathlib import Path
import socket
import time
import uuid

SOCKET = Path.home() / '.local/share/worklane/collab.sock'


def request(action, *, version=2, refused=False, **fields):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(10)
        connection.connect(str(SOCKET))
        connection.sendall(json.dumps({'version': version, 'action': action, **fields}).encode() + b'\n')
        line = connection.makefile('rb').readline(1024 * 1024)
    result = json.loads(line)
    if refused:
        assert result['ok'] is False, 'Expected refusal: ' + action
        return result
    assert result['ok'] is True, 'Installed runtime refused fixture operation: ' + action + ': ' + str(result.get('error', 'unknown refusal'))
    return result['result']


def main():
    for _ in range(100):
        if SOCKET.is_socket():
            try:
                description = request('protocol.describe')
                break
            except (OSError, ValueError):
                pass
        time.sleep(0.1)
    else:
        raise RuntimeError('Installed protocol v2 runtime unavailable')
    assert description['version'] == 2
    request('plan.list', version=1, refused=True)
    print('runtime.protocol_v2')
    plan = request('plan.propose', source='agent', project=str(Path.home()),
                   plan='# Synthetic acceptance plan\nVerify installed artifact operations.',
                   steps=[{'id': 'verify', 'title': 'Verify the installed runtime'}])
    binding = {'id': plan['id'], 'revision': plan['revision'], 'content_sha256': plan['content_sha256']}
    request('plan.approve', source='agent', refused=True, **binding)
    request('plan.approve', source='human', refused=True, **{**binding, 'content_sha256': 'stale'})
    request('plan.approve', source='human', **binding)
    request('plan.implement', source='human', id=plan['id'], revision=1)
    request('plan.step', id=plan['id'], revision=1, step_id='verify', status='done', evidence=['Actual installed UNIX/SQLite round trip'])
    finished = request('plan.finish', id=plan['id'], revision=1, evidence=['Synthetic artifact checks passed'])
    assert finished['status'] == 'completed'
    revised = request('plan.update', source='agent', id=plan['id'], revision=1,
                      plan='# Revised synthetic plan\nReview again.', steps=[])
    assert revised['revision'] == 2 and revised['status'] == 'review'
    assert 'approved_sha256' not in revised
    request('plan.approve', source='human', refused=True, **binding)
    assert len(request('plan.history', id=plan['id'])) == 2
    print('runtime.plans')
    session = str(uuid.uuid4())
    agent = request('agent.register', agent_id=session, session=os.environ['WORKLANE_SESSION'],
                    pane_id='w1:p1', workspace_id='w1', cwd=str(Path.home()), pid=os.getpid(), role='peer')
    actor = {'agent_id': agent['id'], 'lease': agent['lease']}
    try:
        fields = {**actor, 'kind': 'question', 'key': 'fixture:' + session, 'title': 'Synthetic question waiting', 'outcome': 'waiting'}
        notification = request('notification.enqueue', **fields)
        assert notification['ntfy_status'] == 'pending', 'Unprovisioned delivery must remain pending'
        assert request('notification.enqueue', **fields)['id'] == notification['id']
        request('notification.enqueue', refused=True, topic='wrong-owner', **fields)
        request('notification.terminal', id=notification['id'], **actor)
        print('runtime.notification_outbox')
    finally:
        request('agent.heartbeat', status='stopped', **actor)


if __name__ == '__main__':
    main()
