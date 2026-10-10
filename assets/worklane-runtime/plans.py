"""Revisioned plan management, not a tool-permission or shell policy."""
import hashlib
import json
from pathlib import Path
import re
import time
import uuid


class PlanError(ValueError):
    pass


def document(plan, steps):
    if not isinstance(plan, str) or not plan.strip() or len(plan) > 50000 or '\x00' in plan:
        raise PlanError('Plan must contain 1–50000 characters')
    if not isinstance(steps, list) or len(steps) > 100:
        raise PlanError('Expected at most 100 steps')
    normalized = []
    for step in steps:
        if not isinstance(step, dict) or set(step) != {'id', 'title'} or not re.fullmatch(r'[a-zA-Z0-9_-]{1,64}', str(step.get('id', ''))):
            raise PlanError('Steps require stable id/title fields')
        title = step['title']
        if not isinstance(title, str) or not title.strip() or len(title) > 500:
            raise PlanError('Invalid step title')
        normalized.append({'id': step['id'], 'title': title, 'status': 'pending', 'evidence': []})
    if len({step['id'] for step in normalized}) != len(normalized):
        raise PlanError('Duplicate step ID')
    canonical = json.dumps({'plan': plan, 'steps': steps}, sort_keys=True, ensure_ascii=False)
    return plan, normalized, hashlib.sha256(canonical.encode()).hexdigest()


class Plans:
    """Uses the runtime's private transactional store. Same-UID is trusted.

    Agent tools always tag source=agent; human commands may approve/implement.
    This is a workflow distinction, not isolation against arbitrary same-UID code.
    """
    def __init__(self, store):
        self.store = store

    def save_revision(self, row):
        self.store.put('plan_revision', {**row, 'id': row['id'] + ':' + str(row['revision']), 'plan_id': row['id']})
        self.store.put('plan', row)
        self.store.event('plan_' + row['status'], plan_id=row['id'], revision=row['revision'])
        return row

    def dispatch(self, request):
        action = request['action']
        if action == 'plan.list':
            project = request.get('project')
            return [row for row in self.store.rows('plan') if not project or row['project'] == project]
        if action == 'plan.propose':
            plan, steps, digest = document(request.get('plan'), request.get('steps', []))
            project = request.get('project')
            if not isinstance(project, str) or not project.startswith('/') or '\x00' in project or len(project) > 4096:
                raise PlanError('Expected absolute project path')
            row = {'id': str(uuid.uuid4()), 'revision': 1, 'plan': plan, 'steps': steps,
                   'content_sha256': digest, 'status': 'review', 'project': project,
                   'session_id': request.get('session_id'), 'created': time.time(), 'updated': time.time()}
            return self.save_revision(row)
        try:
            ident = str(uuid.UUID(request.get('id', '')))
        except (ValueError, TypeError, AttributeError):
            raise PlanError('Expected canonical plan UUID') from None
        if ident != request.get('id'):
            raise PlanError('Expected canonical plan UUID')
        row = self.store.get('plan', ident)
        if action == 'plan.get':
            return row
        if action == 'plan.history':
            return [item for item in self.store.rows('plan_revision') if item['plan_id'] == ident]
        if request.get('revision') != row['revision']:
            raise PlanError('Plan revision is stale; reload before changing it')
        if action == 'plan.update':
            if row['status'] == 'archived':
                raise PlanError('Archived plans cannot be revised')
            plan, steps, digest = document(request.get('plan'), request.get('steps', []))
            row.update(plan=plan, steps=steps, content_sha256=digest, status='review',
                       revision=row['revision'] + 1, updated=time.time())
            row.pop('approved_sha256', None)
            return self.save_revision(row)
        if action in ('plan.approve', 'plan.implement', 'plan.revise', 'plan.archive'):
            if request.get('source') != 'human' or request.get('agent_id'):
                raise PlanError('Human plan review is required')
            if action == 'plan.approve':
                if row['status'] != 'review' or request.get('content_sha256') != row['content_sha256']:
                    raise PlanError('Plan is not ready or approval hash is stale')
                row.update(status='approved', approved_sha256=row['content_sha256'])
            elif action == 'plan.implement':
                if row['status'] != 'approved' or row.get('approved_sha256') != row['content_sha256']:
                    raise PlanError('Exact plan revision must be approved first')
                row['status'] = 'implementing'
            elif action == 'plan.revise':
                if row['status'] == 'archived':
                    raise PlanError('Archived plan cannot be reopened')
                row.update(status='draft')
                row.pop('approved_sha256', None)
            else:
                row['status'] = 'archived'
            row['updated'] = time.time()
            self.store.event(action, plan_id=ident, revision=row['revision'])
            return self.store.put('plan', row)
        if action == 'plan.step':
            if row['status'] not in ('implementing', 'blocked'):
                raise PlanError('Step tracking requires an implementing plan')
            step = next((s for s in row['steps'] if s['id'] == request.get('step_id')), None)
            status = request.get('status')
            evidence = request.get('evidence', [])
            if not step or status not in ('pending', 'in_progress', 'blocked', 'done'):
                raise PlanError('Invalid step/status')
            if not isinstance(evidence, list) or len(evidence) > 32 or any(not isinstance(e, str) or not e.strip() or len(e) > 1000 for e in evidence):
                raise PlanError('Invalid evidence')
            if status == 'done' and not evidence:
                raise PlanError('Completed steps require verification evidence')
            step.update(status=status, evidence=evidence)
            row.update(status='blocked' if any(s['status'] == 'blocked' for s in row['steps']) else 'implementing', updated=time.time())
            self.store.event('plan_step', plan_id=ident, step_id=step['id'], status=status)
            return self.store.put('plan', row)
        if action == 'plan.finish':
            evidence = request.get('evidence')
            if row['status'] != 'implementing' or any(s['status'] != 'done' for s in row['steps']):
                raise PlanError('Plan still has pending work')
            if not isinstance(evidence, list) or not evidence or len(evidence) > 32 or any(not isinstance(e, str) or not e.strip() or len(e) > 1000 for e in evidence):
                raise PlanError('Completion requires bounded verification evidence')
            row.update(status='completed', evidence=evidence, updated=time.time())
            self.store.event('plan_completed', plan_id=ident)
            return self.store.put('plan', row)
        raise PlanError('Unknown plan action')
