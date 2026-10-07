"""Durable collaboration authority/state tests, no models or live services."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('collab_runtime', ROOT / 'assets/worklane-runtime/runtime.py')
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class RuntimeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        self.runtime = mod.Runtime(self.home, launch=False)
        self.a = self.register('w1:p1')
        self.b = self.register('w1:p2')

    def tearDown(self):
        self.runtime.db.close()
        self.tmp.cleanup()

    def call(self, action, **kwargs):
        return self.runtime.dispatch({'version': 1, 'action': action, **kwargs})

    def register(self, pane, run=None):
        fields = {'agent_id': str(uuid.uuid4()), 'session': self.runtime.session or 'test', 'pane_id': pane, 'pid': os.getpid()}
        if run:
            run.update(status='starting', started=time.time(), pane_id=pane)
            with self.runtime.db:
                self.runtime.put('run', run)
            fields['run_id'] = run['id']
        result = self.call('agent.register', **fields)
        return {'agent_id': result['id'], 'lease': result['lease']}

    def planned(self, actions=None):
        goal = self.call('goal.create', statement='Implement a bounded test goal')
        run = self.runtime.get('run', goal['last_run_id'])
        planner = self.register('w1:p3', run)
        goal = self.call('goal.plan', **planner, id=goal['id'], plan='# Test plan\nImplement and verify.', actions=actions or [])
        return goal, planner

    def running(self, actions=None):
        goal, planner = self.planned(actions)
        goal = self.call('goal.approve', id=goal['id'], plan_sha256=goal['plan_sha256'])
        run = self.runtime.get('run', goal['last_run_id'])
        worker = self.register('w1:p4', run)
        return self.runtime.get('goal', goal['id']), run, {**worker, 'run_id': run['id']}

    def test_message_delivery_ack_and_reply_ownership(self):
        message = self.call('message.send', **self.a, to=self.b['agent_id'], subject='coordination', body='test', idempotency_key='one')
        repeated = self.call('message.send', **self.a, to=self.b['agent_id'], subject='coordination', body='test', idempotency_key='one')
        self.assertEqual(message['id'], repeated['id'])
        first = self.call('message.read', **self.b)
        self.assertEqual(first[0]['delivery'], 'delivered')
        self.assertEqual(len(self.call('message.read', **self.b)), 1)  # at least once, until ack
        with self.assertRaises(mod.Refused):
            self.call('message.ack', **self.a, id=message['id'])
        with self.assertRaises(mod.Refused):
            self.call('message.reply', **self.a, reply_to=message['id'], subject='reply', body='wrong recipient')
        reply = self.call('message.reply', **self.b, reply_to=message['id'], subject='reply', body='received')
        self.assertEqual(reply['to'], self.a['agent_id'])
        self.call('message.ack', **self.b, id=message['id'])
        self.assertEqual(self.call('message.read', **self.b), [])

    def test_invalid_lease_session_and_oversized_payload(self):
        with self.assertRaises(mod.Refused):
            self.call('message.read', agent_id=self.a['agent_id'], lease='wrong')
        with self.assertRaises(mod.Refused):
            self.call('message.send', **self.a, to=self.b['agent_id'], subject='test', body='x' * 16001)
        with self.assertRaises(mod.Refused):
            self.call('goal.create', statement='test', budget={'helpers': 999})
        self.runtime.session = 'bound-lane'
        with self.assertRaises(mod.Refused):
            self.call('agent.register', agent_id=str(uuid.uuid4()), session='other-lane', pane_id='w1:p5', pid=os.getpid())

    def test_messages_are_private_durable_and_lane_local(self):
        message = self.call('message.send', **self.a, to=self.b['agent_id'], subject='durable', body='survive restart')
        self.runtime.db.close()
        self.runtime = mod.Runtime(self.home, launch=False)
        self.assertEqual(self.call('message.read', **self.b)[0]['id'], message['id'])
        with tempfile.TemporaryDirectory() as other:
            other_runtime = mod.Runtime(other, launch=False)
            with self.assertRaises(mod.Refused):
                other_runtime.dispatch({'version': 1, 'action': 'message.read', **self.b})
            other_runtime.db.close()
        self.assertEqual((self.home / '.local/share/worklane/collab/messages.sqlite3').stat().st_mode & 0o777, 0o600)
        self.assertNotIn('lease', json.dumps(self.call('snapshot')))

    def test_hash_bound_human_approval_and_no_self_approval(self):
        goal, planner = self.planned()
        with self.assertRaises(mod.Refused):
            self.call('goal.approve', **planner, id=goal['id'], plan_sha256=goal['plan_sha256'])
        with self.assertRaises(mod.Refused):
            self.call('goal.approve', id=goal['id'], plan_sha256='stale')
        self.assertEqual(self.call('goal.get', id=goal['id'])['status'], 'awaiting_approval')
        approved = self.call('goal.approve', id=goal['id'], plan_sha256=goal['plan_sha256'])
        self.assertEqual(approved['status'], 'approved')
        with self.assertRaises(mod.Refused):
            self.call('goal.approve', id=goal['id'], plan_sha256=goal['plan_sha256'])

    def test_exact_external_authority_and_helper_limits(self):
        descriptor = {'operation': 'p0_mail_send', 'arguments': {'to': 'test@example.test', 'subject': 'test', 'body': 'exact'}}
        goal, run, worker = self.running([descriptor])
        self.assertTrue(self.call('run.authorize', **worker, **descriptor)['allowed'])
        self.assertFalse(self.call('run.authorize', **worker, operation='p0_mail_send', arguments={'to': 'other@example.test'})['allowed'])
        task = self.call('task.create', **worker, statement='Check tests', paths=['tests'])
        with self.assertRaises(mod.Refused):
            self.call('task.create', **worker, statement='overlap', paths=['tests/unit'])
        with self.assertRaises(mod.Refused):
            self.call('task.create', **worker, statement='escape', paths=['../other'])
        self.call('task.create', **worker, statement='docs', paths=['docs'])
        with self.assertRaises(mod.Refused):
            self.call('task.create', **worker, statement='third helper', paths=['src'])
        helper = self.register('w1:p6', task)
        self.call('run.finish', **helper, run_id=task['id'], status='done', summary='Tests passed', evidence=['test log'])
        envelope = self.call('message.read', **worker)[0]
        self.assertIn('test log', envelope['body'])
        with self.assertRaises(mod.Refused):
            self.call('run.finish', **worker, status='done', summary='Premature')

    def test_turn_budget_and_recovery_do_not_replay(self):
        _, run, worker = self.running()
        for _ in range(20):
            self.assertTrue(self.call('run.turn', **worker)['allowed'])
        self.assertFalse(self.call('run.turn', **worker)['allowed'])
        self.assertEqual(self.runtime.get('run', run['id'])['status'], 'blocked')
        _, second, worker2 = self.running()
        self.runtime.recover()
        self.assertEqual(self.runtime.get('run', second['id'])['status'], 'blocked')
        self.assertFalse(self.call('run.turn', **worker2)['allowed'])

    def test_chat_whitespace_cursors_busy_and_cancellation(self):
        chat = self.call('chat.submit', prompt='Respond', source='device')
        with self.assertRaises(mod.Refused) as error:
            self.call('chat.submit', prompt='second')
        self.assertEqual(error.exception.code, 'busy')
        agent = self.register('w1:p7', chat)
        for delta in ('hello', ' ', '\n', ''):
            self.call('chat.event', **agent, run_id=chat['id'], event={'type': 'text_delta', 'delta': delta})
        first = self.call('chat.events', id=chat['id'], after=0)
        self.assertEqual(''.join(e.get('delta', '') for e in first['events']), 'hello \n')
        cursor = first['events'][-1]['seq']
        self.assertEqual(self.call('chat.events', id=chat['id'], after=cursor)['events'], [])
        self.call('chat.cancel', id=chat['id'])
        self.assertEqual(self.call('chat.events', id=chat['id'])['status'], 'cancelled')
        self.assertEqual(self.call('chat.submit', prompt='third')['status'], 'queued')

    def test_scheduled_occurrences_require_fresh_plan_and_skip_overlap(self):
        goal, run, worker = self.running()
        self.assertTrue(self.call('goal.occurrence', id=goal['id'])['skipped'])
        self.call('run.finish', **worker, status='done', summary='verified', evidence=['verification log'])
        occurrence = self.call('goal.occurrence', id=goal['id'])
        self.assertEqual(occurrence['status'], 'planning')
        self.assertNotIn('approved_at', occurrence)
        self.assertNotIn('plan_sha256', occurrence)
        self.assertTrue(self.call('goal.occurrence', id=goal['id'])['skipped'])
        self.call('goal.cancel', id=goal['id'])
        self.assertTrue(self.call('goal.occurrence', id=goal['id'])['skipped'])

    def test_herdr_pane_run_accepts_empty_success_and_requires_json_elsewhere(self):
        self.runtime.session = 'test'
        with patch.object(mod, 'command', return_value=''):
            self.assertEqual(self.runtime.herdr('pane', 'run', 'w1:p1', 'echo test'), {})
            with self.assertRaises(mod.Refused):
                self.runtime.herdr('agent', 'list')

    def test_disposable_chat_run_creates_nonfocused_visible_tab(self):
        run = self.call('chat.submit', prompt='Hello from test')
        self.runtime.session = 'test'
        self.runtime.launch = True
        invocations = []
        def fake_herdr(*args):
            invocations.append(args)
            if args[:2] == ('api', 'snapshot'):
                return {'snapshot': {'workspaces': [{'workspace_id': 'w1', 'label': 'test'}]}}
            if args[:2] == ('tab', 'create'):
                return {'root_pane': {'pane_id': 'w1:p2'}}
            if args[:2] == ('pane', 'run'):
                return {}  # Real Herdr pane run success has empty stdout.
            raise AssertionError(args)
        self.runtime.herdr = fake_herdr
        self.runtime.launching.add(run['id'])
        self.runtime.start_run(run['id'])
        started = self.runtime.get('run', run['id'])
        self.assertEqual(started['status'], 'starting')
        self.assertEqual(started['pane_id'], 'w1:p2')
        tab = next(args for args in invocations if args[:2] == ('tab', 'create'))
        self.assertIn('--no-focus', tab)
        self.assertIn('WORKLANE_RUN_ID=' + run['id'], tab)
        self.assertEqual((self.home / '.pi/agent/sessions/worklane-runs').stat().st_mode & 0o777, 0o700)
        self.assertNotIn(run['id'], self.runtime.launching)

    def test_calendar_injection_and_invalid_protocol_are_refused(self):
        goal = self.call('goal.create', statement='scheduled')
        for calendar in ('daily\nExecStart=bad', '%n', ''):
            with self.assertRaises(mod.Refused):
                self.call('goal.schedule', id=goal['id'], calendar=calendar)
        with self.assertRaises(mod.Refused):
            self.runtime.dispatch({'version': 2, 'action': 'snapshot'})


if __name__ == '__main__':
    unittest.main()
