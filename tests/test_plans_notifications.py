"""Protocol v2 artifact/notification/interactive contracts; not live proof."""
import os
import time
from pathlib import Path
import tempfile
import unittest
import uuid
from test_collab_runtime import mod


class V2Test(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        self.runtime = mod.Runtime(self.home, launch=False)
        row = self.call('agent.register', agent_id=str(uuid.uuid4()), session='test', pane_id='w1:p1', workspace_id='w1', cwd=str(self.home), pid=os.getpid())
        self.agent = {'agent_id': row['id'], 'lease': row['lease']}

    def tearDown(self):
        self.runtime.db.close()
        self.tmp.cleanup()

    def call(self, action, **fields):
        return self.runtime.dispatch({'version': 2, 'action': action, **fields})

    def propose(self):
        return self.call('plan.propose', source='agent', project=str(self.home), plan='# Plan\nTest it.', steps=[{'id': 'test', 'title': 'Verify'}])

    def test_plan_revision_history_stale_approval_and_progress(self):
        row = self.propose()
        with self.assertRaises(mod.Refused):
            self.call('plan.approve', source='agent', id=row['id'], revision=1, content_sha256=row['content_sha256'])
        with self.assertRaises(mod.Refused):
            self.call('plan.approve', source='human', id=row['id'], revision=1, content_sha256='stale')
        self.call('plan.approve', source='human', id=row['id'], revision=1, content_sha256=row['content_sha256'])
        self.call('plan.implement', source='human', id=row['id'], revision=1)
        with self.assertRaises(mod.Refused):
            self.call('plan.step', id=row['id'], revision=1, step_id='test', status='done')
        updated = self.call('plan.step', id=row['id'], revision=1, step_id='test', status='done', evidence=['test passed'])
        self.assertEqual(updated['revision'], 1)
        self.assertEqual(updated['content_sha256'], row['content_sha256'])
        self.assertEqual(self.call('plan.finish', id=row['id'], revision=1, evidence=['verified'])['status'], 'completed')
        revision = self.call('plan.update', source='agent', id=row['id'], revision=1, plan='Replacement plan', steps=[])
        self.assertEqual(revision['status'], 'review')
        self.assertNotIn('approved_sha256', revision)
        with self.assertRaises(mod.Refused):
            self.call('plan.update', id=row['id'], revision=1, plan='stale')
        self.assertEqual(len(self.call('plan.history', id=row['id'])), 2)
        self.runtime.db.close()
        self.runtime = mod.Runtime(self.home, launch=False)
        self.assertEqual(self.call('plan.get', id=row['id'])['revision'], 2)

    def test_invalid_plans_are_not_written(self):
        for plan, steps in [('', []), ('x' * 50001, []), ('plan', [{'id': 'x', 'title': 'a'}, {'id': 'x', 'title': 'b'}]), ('plan', [{'id': '../bad', 'title': 'a'}])]:
            with self.assertRaises(mod.Refused):
                self.call('plan.propose', plan=plan, steps=steps, project=str(self.home))
        self.assertEqual(self.call('plan.list'), [])

    def test_notifications_private_idempotent_and_routing_not_agent_selected(self):
        fields = {**self.agent, 'kind': 'question', 'key': 'question:1', 'title': 'Question waiting', 'outcome': 'waiting'}
        first = self.call('notification.enqueue', **fields)
        repeated = self.call('notification.enqueue', **fields)
        self.assertEqual(first['id'], repeated['id'])
        self.assertEqual(first['ntfy_status'], 'pending')
        with self.assertRaises(mod.Refused):
            self.call('notification.enqueue', **fields, topic='other-owner')
        with self.assertRaises(mod.Refused):
            self.call('notification.settings', **self.agent, source='human', changes={'ntfy': False})
        self.call('notification.settings', source='human', changes={'ntfy': False, 'categories': {'response': False}})
        row = self.call('notification.enqueue', **{**fields, 'key': 'question:2'})
        self.assertEqual(row['ntfy_status'], 'disabled')
        row = self.call('notification.enqueue', **{**fields, 'kind': 'response', 'key': 'response:1'})
        self.assertFalse(row['terminal'])
        with self.assertRaises(mod.Refused):
            self.call('notification.enqueue', **{**fields, 'title': '\x1b]9;injection', 'key': 'bad'})
        self.call('notification.terminal', **self.agent, id=first['id'])
        self.assertTrue(self.runtime.get('notification', first['id'])['terminal_delivered'])

    def test_unprovisioned_notify_does_not_claim_delivery(self):
        row = self.call('notification.enqueue', **self.agent, kind='response', key='response:1', title='Finished response')
        self.runtime.deliver_notifications()
        self.assertEqual(self.runtime.get('notification', row['id'])['ntfy_status'], 'pending')
        folder = self.home / '.p0'
        folder.mkdir()
        (folder / 'notify.py').write_text('import json; print(json.dumps({"accepted": True}))\n')
        self.runtime.deliver_notifications()
        self.assertEqual(self.runtime.get('notification', row['id'])['ntfy_status'], 'delivered')
        self.assertEqual(self.runtime.get('notification', row['id'])['attempts'], 1)
        # This is a helper-contract fixture, explicitly not a real publish test.

    def test_interactive_delegation_no_goal_and_no_external_authority(self):
        parent = self.call('interactive.open', **self.agent)
        self.assertEqual(self.call('interactive.open', **self.agent)['id'], parent['id'])
        first = self.call('task.create', **self.agent, statement='Tests', paths=['tests'])
        second = self.call('task.create', **self.agent, statement='Docs', paths=['docs'])
        self.assertEqual(first['cwd'], str(self.home))
        self.assertEqual(first['parent_run_id'], parent['id'])
        for paths in (['../x'], ['x//y'], ['x\\y'], ['tests/unit']):
            with self.assertRaises(mod.Refused):
                self.call('task.create', **self.agent, statement='invalid', paths=paths)
        self.assertEqual(len(self.call('interactive.children', **self.agent)['pending']), 2)
        with self.assertRaises(mod.Refused):
            self.call('interactive.finish', **self.agent)
        for _ in range(20):
            self.assertTrue(self.call('interactive.turn', **self.agent)['allowed'])
        self.assertFalse(self.call('interactive.turn', **self.agent)['allowed'])
        self.assertEqual(self.runtime.get('run', parent['id'])['status'], 'blocked')

    def test_interactive_verification_is_distinct_from_response_settlement(self):
        group = self.call('interactive.open', **self.agent)
        with self.assertRaises(mod.Refused):
            self.call('interactive.finish', **self.agent, status='done', summary='Not verified')
        finished = self.call('interactive.finish', **self.agent, status='done', summary='Verified', evidence=['Combined checks passed'])
        self.assertEqual(finished['status'], 'done')
        notice = self.runtime.rows('notification')[0]
        self.assertEqual(notice['kind'], 'task')
        self.assertEqual(notice['key'], 'verified-task:' + group['id'])
        self.assertEqual(notice['outcome'], 'completed')
        # Replaying the presentation signal does not enqueue a second publish.
        self.call('notification.enqueue', **self.agent, kind='task', key=notice['key'], title='Task complete', outcome='completed')
        self.assertEqual(len(self.runtime.rows('notification')), 1)

    def test_planning_helpers_need_no_implementation_approval_or_external_grants(self):
        goal = self.call('goal.create', statement='Plan a bounded local change')
        parent = self.runtime.get('run', goal['last_run_id'])
        parent.update(status='starting', started=time.time(), pane_id='w1:p2')
        self.runtime.put('run', parent)
        registered = self.call('agent.register', agent_id=str(uuid.uuid4()), session='test', pane_id='w1:p2', workspace_id='w1', cwd=str(self.home), pid=os.getpid(), run_id=parent['id'])
        planner = {'agent_id': registered['id'], 'lease': registered['lease'], 'run_id': parent['id']}
        child = self.call('task.create', **planner, statement='Check plan assumptions', paths=['tests'])
        self.assertIsNone(child['goal_id'])
        self.assertEqual(child['actions'], [])
        with self.assertRaises(mod.Refused):
            self.call('goal.plan', **planner, id=goal['id'], plan='Proposed plan', actions=[])
        child.update(status='starting', started=time.time(), pane_id='w2:p1')
        self.runtime.put('run', child)
        registered = self.call('agent.register', agent_id=str(uuid.uuid4()), session='test', pane_id='w2:p1', workspace_id='w2', cwd=str(self.home), pid=os.getpid(), run_id=child['id'])
        helper = {'agent_id': registered['id'], 'lease': registered['lease'], 'run_id': child['id']}
        self.assertTrue(self.call('run.turn', **helper)['allowed'])
        self.assertFalse(self.call('run.authorize', **helper, operation='p0.mail.send', arguments={})['allowed'])
        with self.assertRaises(mod.Refused):
            self.call('task.create', **helper, statement='Recursive helper', paths=['nested'])
        self.call('run.finish', **helper, status='done', summary='Assumptions checked', evidence=['Local test passed'])
        proposed = self.call('goal.plan', **planner, id=goal['id'], plan='Proposed plan', actions=[])
        self.assertEqual(proposed['status'], 'awaiting_approval')
        notice = self.runtime.rows('notification')[0]
        self.assertEqual(notice['kind'], 'plan_review')

    def test_outbox_is_bounded_and_never_drops_pending_remote_delivery(self):
        with self.runtime.db:
            for number in range(1000):
                self.runtime.put('notification', {'id': str(uuid.uuid4()), 'agent_id': self.agent['agent_id'], 'key': str(number), 'kind': 'question', 'outcome': 'waiting', 'ntfy_status': 'pending', 'created': number})
        fields = {**self.agent, 'kind': 'question', 'key': 'new', 'title': 'Question waiting', 'outcome': 'waiting'}
        with self.assertRaises(mod.Refused):
            self.call('notification.enqueue', **fields)
        oldest = self.runtime.rows('notification')[0]
        oldest['ntfy_status'] = 'disabled'
        self.runtime.put('notification', oldest)
        self.call('notification.enqueue', **fields)
        self.assertEqual(len(self.runtime.rows('notification')), 1000)
        self.assertEqual(sum(row['ntfy_status'] == 'pending' for row in self.runtime.rows('notification')), 1000)

    def test_v1_only_old_capabilities_and_negotiation(self):
        self.assertEqual(self.call('protocol.describe')['version'], 2)
        with self.assertRaises(mod.Refused):
            self.runtime.dispatch({'version': 1, 'action': 'plan.list'})
        self.assertIsInstance(self.runtime.dispatch({'version': 1, 'action': 'snapshot'}), dict)


if __name__ == '__main__':
    unittest.main()
