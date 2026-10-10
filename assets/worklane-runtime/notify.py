#!/usr/bin/env python3
"""Lane-scoped notification client. No credentials, recipients or URLs here."""
import argparse
import json
import socket
import sys
import uuid

SOCKET = '/run/p0-notifications/notify.sock'
LIMIT = 8192


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['event', 'send', 'capabilities'])
    args = parser.parse_args()
    if args.action == 'capabilities':
        from pathlib import Path
        print(json.dumps({'notify': Path(SOCKET).is_socket()}))
        return 0
    body = sys.stdin.buffer.read(LIMIT + 1)
    if len(body) > LIMIT:
        raise ValueError('Notification too large')
    request = json.loads(body)
    if args.action == 'send':
        request = {'id': str(uuid.uuid4()), 'kind': 'manual', 'title': request['title'], 'outcome': 'manual', 'pane_id': ''}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(15)
        connection.connect(SOCKET)
        connection.sendall(json.dumps(request).encode() + b'\n')
        response = connection.makefile('rb').readline(LIMIT + 1)
    if len(response) > LIMIT or not response.endswith(b'\n'):
        raise ValueError('Invalid broker response')
    result = json.loads(response)
    print(json.dumps(result))
    return 0 if result.get('accepted') is True else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, TypeError):
        print('{"accepted":false,"error":"Scoped notification service unavailable"}')
        raise SystemExit(1)
