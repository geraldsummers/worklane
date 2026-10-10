#!/usr/bin/env python3
"""Real installed Pi RPC/UI probe, no model/provider calls or owner credentials."""
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import threading
import time
import uuid

ROOT = Path.home() / '.tmp/pi-rpc-acceptance'
ROOT.mkdir(parents=True, exist_ok=True)
REQUIRED = {'questionnaire', 'plan_read', 'plan_propose', 'plan_update', 'plan_step_update', 'plan_finish', 'lane_complain', 'task_delegate', 'run_finish', 'p0_status', 'p0_notify'}

def runtime(action, **fields):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(10)
        connection.connect(str(Path.home() / '.local/share/worklane/collab.sock'))
        connection.sendall(json.dumps({'version': 2, 'action': action, **fields}).encode() + b'\n')
        result = json.loads(connection.makefile('rb').readline(1024 * 1024))
    assert result['ok'], result
    return result['result']

class Client:
    def __init__(self, argv, env=None):
        self.errors = (ROOT / ('stderr-' + str(uuid.uuid4()) + '.txt')).open('w')
        self.process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors, env=env)
        self.records = queue.Queue()
        self.history = []
        def reader():
            for line in self.process.stdout:
                try:
                    self.records.put(json.loads(line))
                except ValueError:
                    self.records.put({'invalid_protocol': line.decode(errors='replace')[:200]})
            self.records.put({'exit': self.process.poll()})
        threading.Thread(target=reader, daemon=True).start()
    def send(self, row):
        self.process.stdin.write(json.dumps(row).encode() + b'\n')
        self.process.stdin.flush()
    def command(self, kind, dialogs=None, **fields):
        ident = str(uuid.uuid4())
        self.send({'id': ident, 'type': kind, **fields})
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            row = self.records.get(timeout=max(0.01, deadline - time.monotonic()))
            self.history.append(row)
            assert 'invalid_protocol' not in row and 'exit' not in row, row
            assert row.get('type') != 'agent_start', 'Unexpected model run'
            if row.get('type') == 'extension_ui_request' and row.get('method') in ('select', 'confirm', 'editor', 'input'):
                assert dialogs, 'Unexpected dialog: ' + row['method']
                self.send({'type': 'extension_ui_response', 'id': row['id'], **dialogs(row)})
            if row.get('type') == 'response' and row.get('id') == ident:
                assert row['success'], row
                if kind == 'prompt':
                    assert row.get('data', {}).get('disposition') == 'handled', row
                return row.get('data')
        raise RuntimeError('RPC timeout')
    def result(self):
        for row in reversed(self.history):
            message = row.get('message', {})
            if row.get('type') == 'message_end' and message.get('customType') == 'acceptance-result':
                content = message['content']
                if isinstance(content, list):
                    content = ''.join(part.get('text', '') for part in content)
                return json.loads(content)
        raise AssertionError('No installed acceptance result message')
    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=10)
        self.errors.close()
        assert self.process.returncode == 0, 'Pi shutdown failed'


def main():
    cells = []
    argv = ['pi', '--mode', 'rpc', '--session-dir', str(ROOT / 'sessions'), '--extension', str(Path.home() / '.tmp/live-pi-rpc-probe.ts')]
    client = Client(argv)
    try:
        state = client.command('get_state')
        settings = json.loads((Path.home() / '.pi/agent/settings.json').read_text())
        assert settings['defaultProvider'] == 'openai-codex' and settings['defaultModel'] == 'gpt-6-sol'
        assert settings['defaultThinkingLevel'] == 'medium'
        effective_model = state.get('model', {}).get('id')
        client.command('prompt', message='/acceptance-probe tools')
        result = client.result()
        assert REQUIRED <= set(result['tools']), sorted(REQUIRED - set(result['tools']))
        assert result['mode'] == 'rpc' and result['hasUI'] is True
        cells.extend(['installed.pi.normal_discovery', 'installed.pi.default_model_configuration'])
        client.command('prompt', message='/acceptance-probe question', dialogs=lambda row: {'value': row['options'][0]})
        assert client.result()['answers'][0]['answer'] == 'Bounded'
        client.command('prompt', message='/acceptance-probe question', dialogs=lambda row: {'value': row['options'][-1]} if row['method'] == 'select' else {'value': 'Synthetic free text'})
        assert client.result()['answers'][0]['wasCustom'] is True
        client.command('prompt', message='/acceptance-probe question', dialogs=lambda row: {'cancelled': True})
        assert client.result()['cancelled'] is True
        cells.append('installed.pi.questionnaire_real_rpc_dialogs')
        client.command('prompt', message='/acceptance-probe complaint')
        assert client.result() == {'private': True, 'redacted': True}
        cells.append('installed.pi.complaint_private_write_without_dialog')
        commands = client.command('get_commands')['commands']
        assert any(row['name'] == 'plan' and row['source'] == 'extension' for row in commands)
        document = '# Synthetic installed RPC plan\nCheck human review dialog binding.'
        plan = runtime('plan.propose', source='agent', project=str(Path.home()), plan=document)
        client.command('prompt', message='/plan review', dialogs=lambda row: {'value': next(value for value in row['options'] if value.startswith(plan['id']))})
        confirmations = []
        def confirm(row):
            assert row['method'] == 'confirm' and row['message'] == document
            confirmations.append(row['title'])
            return {'confirmed': True}
        client.command('prompt', message='/plan approve', dialogs=confirm)
        assert runtime('plan.get', id=plan['id'])['status'] == 'approved'
        client.command('prompt', message='/plan archive', dialogs=confirm)
        assert runtime('plan.get', id=plan['id'])['status'] == 'archived'
        assert len(confirmations) == 2
        cells.append('installed.pi.plan_real_rpc_human_review')
        assert client.command('get_session_stats')['tokens']['total'] == 0
    finally:
        client.close()
    report = {'status': 'passed', 'cells': cells, 'model_requests': 0,
              'subscription_owner_auth': 'not-tested', 'terminal_phone': 'not-tested',
              'effective_model_without_auth': effective_model,
              'questionnaire': 'actual installed exported implementation via acceptance command, real Pi RPC UI',
              'complaint': 'actual installed exported implementation via acceptance command, not model-invoked'}
    print(json.dumps(report, sort_keys=True))

if __name__ == '__main__':
    main()
