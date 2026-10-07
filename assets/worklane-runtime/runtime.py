#!/usr/bin/env python3
"""Lane-local durable collaboration and bounded, human-approved Pi runs.

No network listener or host relay. Same-UID processes are trusted; workflow
approval is not a security boundary against arbitrary code running as dev.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import socket
import socketserver
import sqlite3
import stat
import subprocess
import threading
import time
import uuid

MAX_REQUEST = 128 * 1024
TERMINAL = {'done', 'failed', 'blocked', 'cancelled', 'settled', 'awaiting_approval'}
DEFAULT_BUDGET = {'helpers': 2, 'seconds': 1800, 'turns': 20}


class Refused(ValueError):
    def __init__(self, message, code='invalid_request'):
        super().__init__(message)
        self.code = code


def identifier(value):
    try:
        if str(uuid.UUID(str(value))) != value:
            raise ValueError()
    except (ValueError, TypeError, AttributeError):
        raise Refused('Expected canonical UUID') from None
    return value


def text(value, maximum=16000):
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > maximum or '\x00' in value:
        raise Refused('Invalid or oversized text')
    return value


def private_dir(path):
    path = Path(path)
    # Do not follow symlinks for runtime state.
    if path.is_symlink():
        raise Refused('Runtime directory must not be a symlink')
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    if path.stat().st_uid != os.getuid():
        raise Refused('Runtime state must belong to the lane user')
    path.chmod(0o700)
    return path


def command(argv, timeout=20):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise Refused(f'{argv[0]} failed (exit {result.returncode})')
    return result.stdout


class Runtime:
    def __init__(self, home, launch=True):
        self.home = Path(home)
        self.root = private_dir(self.home / '.local/share/worklane/collab')
        path = self.root / 'messages.sqlite3'
        if path.is_symlink():
            raise Refused('Database must not be a symlink')
        self.db = sqlite3.connect(path, check_same_thread=False)
        path.chmod(0o600)
        self.db.execute('PRAGMA journal_mode=WAL')
        self.db.execute('PRAGMA synchronous=FULL')
        self.db.execute('CREATE TABLE IF NOT EXISTS objects (kind TEXT, id TEXT, value TEXT, PRIMARY KEY(kind,id))')
        self.db.execute('CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY AUTOINCREMENT, run TEXT, value TEXT)')
        self.db.commit()
        self.lock = threading.RLock()
        self.launch = launch
        self.session = os.environ.get('WORKLANE_SESSION', '')
        self.launching = set()
        self.last_view_check = 0

    def get(self, kind, ident):
        row = self.db.execute('SELECT value FROM objects WHERE kind=? AND id=?', (kind, ident)).fetchone()
        if not row:
            raise Refused(f'{kind} not found')
        return json.loads(row[0])

    def rows(self, kind):
        return [json.loads(row[0]) for row in self.db.execute('SELECT value FROM objects WHERE kind=? ORDER BY rowid', (kind,))]

    def put(self, kind, obj):
        self.db.execute('INSERT OR REPLACE INTO objects VALUES (?,?,?)', (kind, obj['id'], json.dumps(obj)))
        return obj

    def event(self, kind, run='', **fields):
        self.db.execute('INSERT INTO events(run,value) VALUES (?,?)', (run, json.dumps({'type': kind, 'ts': time.time(), **fields})))

    def agent(self, request):
        row = self.get('agent', identifier(request.get('agent_id')))
        lease = request.get('lease', '')
        if not isinstance(lease, str) or not secrets.compare_digest(lease, row['lease']):
            raise Refused('Invalid agent lease')
        if time.time() - row['last_seen'] > 90:
            raise Refused('Agent lease expired; register again')
        return row

    def run_for_agent(self, request):
        agent = self.agent(request)
        ident = identifier(request.get('run_id'))
        if agent.get('run_id') != ident:
            raise Refused('Agent does not own this run')
        return self.get('run', ident)

    def new_run(self, kind, prompt, goal=None, parent=None):
        if kind in ('goal', 'task'):
            prompt += '\nComplete only after verification. Call run_finish with explicit status, summary and evidence. If helpers are active, await their peer result messages and verify the combined result. Do not claim success from idle/settlement alone.'
        run = {'id': str(uuid.uuid4()), 'kind': kind, 'prompt': prompt, 'status': 'queued',
               'created': time.time(), 'started': None, 'turns': 0, 'agent_id': None,
               'budget': (goal or parent or {}).get('budget', DEFAULT_BUDGET).copy()}
        if goal:
            run.update(goal_id=goal['id'], plan=goal.get('plan'), actions=goal.get('actions', []))
        if parent:
            run.update(parent_run_id=parent['id'], goal_id=parent.get('goal_id'),
                       plan=parent.get('plan'), actions=parent.get('actions', []))
        self.event('run_queued', run['id'], run_kind=kind)
        return self.put('run', run)

    def budget(self, supplied):
        if not isinstance(supplied, dict) or set(supplied) - set(DEFAULT_BUDGET):
            raise Refused('Invalid run budget')
        result = {**DEFAULT_BUDGET, **supplied}
        for key, maximum in [('helpers', 2), ('seconds', 1800), ('turns', 20)]:
            if type(result[key]) is not int or not 1 <= result[key] <= maximum:
                raise Refused(f'{key} must be between 1 and {maximum}')
        return result

    def recover(self):
        # Never replay in-flight model work after a daemon/container restart.
        with self.lock, self.db:
            for run in self.rows('run'):
                if run['status'] in ('running', 'starting'):
                    run.update(status='blocked', summary='Interrupted runtime; explicit human retry required')
                    self.put('run', run)
                    self.event('error', run['id'], error=run['summary'])
                    if run.get('goal_id'):
                        goal = self.get('goal', run['goal_id'])
                        goal.update(status='blocked')
                        self.put('goal', goal)

    def finish(self, run, status, summary):
        if run['status'] in TERMINAL:
            return run
        run.update(status=status, summary=summary, ended=time.time())
        self.put('run', run)
        self.event('settled' if status in ('done', 'settled') else 'error', run['id'], summary=summary)
        if run.get('goal_id') and run['kind'] != 'task':
            goal = self.get('goal', run['goal_id'])
            goal.update(status=status, last_run_id=run['id'])
            self.put('goal', goal)
        if run.get('parent_run_id'):
            parent = self.get('run', run['parent_run_id'])
            if parent.get('agent_id'):
                self.put('message', {'id': str(uuid.uuid4()), 'from': run.get('agent_id', run['id']),
                                    'reply_to': run.get('agent_id', run['id']), 'to': parent['agent_id'],
                                    'subject': 'Helper result: ' + run['id'], 'kind': 'notice',
                                    'body': json.dumps({'status': status, 'summary': summary, 'evidence': run.get('evidence', [])}),
                                    'ts': time.time(), 'delivery': 'queued', 'correlation_id': run['id']})
        if run.get('unit') and self.launch:
            # Reap only the explicitly owned run scope, never main Herdr panes.
            threading.Thread(target=self.stop_run, args=(run,), daemon=True).start()
        return run

    def stop_run(self, run):
        try:
            command(['systemctl', '--user', 'stop', run['unit']], timeout=10)
        except (Refused, subprocess.TimeoutExpired):
            pass

    def dispatch(self, request):
        if not isinstance(request, dict) or request.get('version') != 1:
            raise Refused('Request version must be 1')
        action = request.get('action')
        with self.lock, self.db:
            if action == 'agent.register':
                ident = identifier(request.get('agent_id'))
                pane = text(request.get('pane_id'), 128)
                session = text(request.get('session'), 128)
                if self.session and session != self.session:
                    raise Refused('Agent belongs to another lane session')
                if not re.fullmatch(r'w[0-9A-Za-z]+:p[0-9A-Za-z]+', pane):
                    raise Refused('Invalid Herdr pane identity')
                pid = request.get('pid')
                if type(pid) is not int or pid <= 0:
                    raise Refused('Invalid process identity')
                row = {'id': ident, 'session': session, 'pane_id': pane, 'pid': pid,
                       'role': request.get('role', 'agent'), 'session_file': request.get('session_file'), 'status': 'idle',
                       'last_seen': time.time(), 'lease': secrets.token_urlsafe(32)}
                if request.get('run_id'):
                    run = self.get('run', identifier(request['run_id']))
                    if run['status'] not in ('starting', 'running') or run.get('pane_id') != pane:
                        raise Refused('Run does not belong to this pane or is no longer active')
                    row['run_id'] = run['id']
                    run.update(agent_id=ident, status='running')
                    self.put('run', run)
                    if run['kind'] == 'goal':
                        goal = self.get('goal', run['goal_id'])
                        goal['status'] = 'running'
                        self.put('goal', goal)
                return self.put('agent', row)
            if action == 'agent.heartbeat':
                agent = self.agent(request)
                agent.update(last_seen=time.time(), status=text(request.get('status', 'idle'), 40))
                return self.put('agent', agent)
            if action == 'agent.list':
                return [{k: v for k, v in a.items() if k != 'lease'} for a in self.rows('agent') if time.time() - a['last_seen'] <= 90]
            if action in ('message.send', 'message.reply'):
                agent = self.agent(request)
                if action == 'message.reply':
                    original = self.get('message', identifier(request.get('reply_to')))
                    if original['to'] != agent['id']:
                        raise Refused('Not your message to reply to')
                    to = original['from']
                else:
                    to = identifier(request.get('to'))
                self.get('agent', to)
                key = request.get('idempotency_key')
                if key:
                    text(key, 256)
                    previous = next((m for m in self.rows('message') if m.get('key') == key and m['from'] == agent['id']), None)
                    if previous:
                        return previous
                kind = request.get('kind', 'message')
                if kind not in ('message', 'request', 'reply', 'handoff', 'notice'):
                    raise Refused('Invalid message kind')
                row = {'id': str(uuid.uuid4()), 'from': agent['id'], 'reply_to': agent['id'],
                       'to': to, 'subject': text(request.get('subject'), 256), 'body': text(request.get('body')),
                       'kind': kind, 'ts': time.time(), 'delivery': 'queued', 'key': key,
                       'correlation_id': request.get('correlation_id', request.get('reply_to'))}
                self.event('message_sent', message_id=row['id'], sender=agent['id'], recipient=to)
                return self.put('message', row)
            if action == 'message.read':
                agent = self.agent(request)
                limit = request.get('limit', 20)
                if type(limit) is not int or not 1 <= limit <= 50:
                    raise Refused('Invalid read limit')
                rows = [m for m in self.rows('message') if m['to'] == agent['id'] and m['delivery'] != 'read'][:limit]
                for row in rows:
                    row['delivery'] = 'delivered'
                    self.put('message', row)
                return rows
            if action == 'message.ack':
                agent = self.agent(request)
                row = self.get('message', identifier(request.get('id')))
                if row['to'] != agent['id']:
                    raise Refused('Not your message')
                row.update(delivery='read', read_at=time.time())
                return self.put('message', row)
            if action == 'goal.create':
                if request.get('agent_id'):
                    raise Refused('Goals require human submission')
                row = {'id': str(uuid.uuid4()), 'statement': text(request.get('statement')), 'status': 'draft',
                       'budget': self.budget(request.get('budget', {})), 'created': time.time(), 'actions': []}
                self.put('goal', row)
                run = self.new_run('plan', row['statement'], goal=row)
                row.update(status='planning', last_run_id=run['id'])
                return self.put('goal', row)
            if action == 'goal.list':
                return self.rows('goal')
            if action == 'goal.get':
                return self.get('goal', identifier(request.get('id')))
            if action == 'goal.plan':
                agent = self.agent(request)
                goal = self.get('goal', identifier(request.get('id')))
                run = self.get('run', agent.get('run_id', ''))
                if run.get('goal_id') != goal['id'] or run['kind'] != 'plan' or run['status'] != 'running' or goal['status'] != 'planning':
                    raise Refused('Not the active planner for this goal')
                plan = text(request.get('plan'), 50000)
                actions = request.get('actions', [])
                if not isinstance(actions, list) or len(actions) > 50 or any(not isinstance(a, dict) or set(a) != {'operation', 'arguments'} or not isinstance(a['operation'], str) or not isinstance(a['arguments'], dict) for a in actions):
                    raise Refused('External actions must be exact operation/arguments descriptors')
                digest = hashlib.sha256(json.dumps({'plan': plan, 'actions': actions}, sort_keys=True).encode()).hexdigest()
                goal.update(plan=plan, actions=actions, plan_sha256=digest, status='awaiting_approval')
                self.put('goal', goal)
                run.update(status='awaiting_approval', ended=time.time())
                self.put('run', run)
                self.event('plan_ready', run['id'], goal_id=goal['id'], plan_sha256=digest)
                if run.get('unit') and self.launch:
                    threading.Thread(target=self.stop_run, args=(run,), daemon=True).start()
                return goal
            if action in ('goal.approve', 'goal.reject', 'goal.cancel'):
                if request.get('agent_id'):
                    raise Refused('Approval is human-only')
                goal = self.get('goal', identifier(request.get('id')))
                if action == 'goal.approve':
                    if goal['status'] != 'awaiting_approval' or request.get('plan_sha256') != goal.get('plan_sha256'):
                        raise Refused('Plan is not ready or approval hash is stale')
                    goal.update(status='approved', approved_at=time.time())
                    run = self.new_run('goal', 'Implement the exact approved plan, verify it, and report evidence.\n' + goal['plan'], goal=goal)
                    goal['last_run_id'] = run['id']
                    self.event('plan_approved', run['id'], goal_id=goal['id'], plan_sha256=goal['plan_sha256'])
                else:
                    goal['status'] = 'rejected' if action == 'goal.reject' else 'cancelled'
                    for run in self.rows('run'):
                        if run.get('goal_id') == goal['id'] and run['status'] not in TERMINAL:
                            self.finish(run, 'cancelled', 'Cancelled by human')
                return self.put('goal', goal)
            if action in ('goal.schedule', 'goal.occurrence'):
                goal = self.get('goal', identifier(request.get('id')))
                if action == 'goal.schedule':
                    calendar = text(request.get('calendar'), 128)
                    if any(c in calendar for c in '\r\n%'):
                        raise Refused('Invalid systemd calendar')
                    command(['systemd-analyze', 'calendar', calendar])
                    self.schedule(goal, calendar)
                    goal['schedule'] = calendar
                    return self.put('goal', goal)
                if goal['status'] == 'cancelled':
                    return {'skipped': True, 'reason': 'Schedule cancelled'}
                if any(g.get('template_id') == goal['id'] and g['status'] in ('draft', 'planning', 'awaiting_approval', 'approved', 'running') for g in self.rows('goal')) or goal['status'] in ('draft', 'planning', 'awaiting_approval', 'approved', 'running'):
                    self.event('schedule_skipped', goal_id=goal['id'], reason='An occurrence is still pending')
                    return {'skipped': True}
                occurrence = self.dispatch({'version': 1, 'action': 'goal.create', 'statement': goal['statement'], 'budget': goal['budget']})
                occurrence['template_id'] = goal['id']
                return self.put('goal', occurrence)
            if action == 'run.input':
                run = self.run_for_agent(request)
                return {**run, 'run_id': run['id']}
            if action == 'run.turn':
                run = self.run_for_agent(request)
                run['turns'] += 1
                allowed = run['status'] == 'running' and run['turns'] <= run['budget']['turns'] and time.time() - run['started'] < run['budget']['seconds']
                if not allowed:
                    self.finish(run, 'blocked', 'Run budget exhausted or run stopped')
                else:
                    self.put('run', run)
                return {'allowed': allowed, 'turns': run['turns'], 'max_turns': run['budget']['turns'], 'status': run['status']}
            if action == 'run.authorize':
                run = self.run_for_agent(request)
                goal = self.get('goal', run['goal_id']) if run.get('goal_id') else {}
                descriptor = {'operation': request.get('operation'), 'arguments': request.get('arguments')}
                return {'allowed': run['kind'] in ('goal', 'task') and run['status'] == 'running' and goal.get('approved_at') is not None and goal.get('status') in ('approved', 'running') and descriptor in run.get('actions', [])}
            if action == 'run.children':
                run = self.run_for_agent(request)
                children = [r for r in self.rows('run') if r.get('parent_run_id') == run['id']]
                return {'pending': [r['id'] for r in children if r['status'] not in TERMINAL], 'children': children}
            if action == 'run.finish':
                run = self.run_for_agent(request)
                status = request.get('status')
                if status not in ('done', 'failed', 'blocked'):
                    raise Refused('Invalid completion status')
                children = [r for r in self.rows('run') if r.get('parent_run_id') == run['id'] and r['status'] not in TERMINAL]
                if status == 'done' and children:
                    raise Refused('Helpers are still active')
                evidence = request.get('evidence', [])
                if status == 'done' and run['kind'] in ('goal', 'task') and not evidence:
                    raise Refused('Successful completion requires verification evidence')
                if len(json.dumps(evidence).encode()) > 16000:
                    raise Refused('Completion evidence is oversized')
                run['evidence'] = evidence
                return self.finish(run, status, text(request.get('summary'), 16000))
            if action == 'task.create':
                run = self.run_for_agent(request)
                if run['kind'] != 'goal' or run['status'] != 'running':
                    raise Refused('Only approved running goals can delegate')
                tasks = [r for r in self.rows('run') if r.get('parent_run_id') == run['id']]
                if len(tasks) >= run['budget']['helpers']:
                    raise Refused('Helper budget exhausted')
                paths = request.get('paths', [])
                if not isinstance(paths, list) or not paths or any(not isinstance(p, str) or p.startswith('/') or '..' in Path(p).parts or not p for p in paths):
                    raise Refused('Helper requires relative ownership paths')
                if any(p == q or p.startswith(q.rstrip('/') + '/') or q.startswith(p.rstrip('/') + '/') for p in paths for t in tasks if t['status'] not in TERMINAL for q in t.get('paths', [])):
                    raise Refused('Helper ownership overlaps active work')
                task = self.new_run('task', text(request.get('statement')), parent=run)
                task['paths'] = paths
                return self.put('run', task)
            if action == 'chat.submit':
                if any(r['kind'] == 'chat' and r['status'] not in TERMINAL for r in self.rows('run')):
                    raise Refused('This agent is already handling a request', 'busy')
                source = request.get('source', 'gateway')
                if source not in ('gateway', 'matrix', 'device', 'librechat'):
                    raise Refused('Unknown chat source')
                run = self.new_run('chat', text(request.get('prompt'), 65536))
                run.update(source=source, budget={**DEFAULT_BUDGET, 'seconds': 300})
                return self.put('run', run)
            if action == 'chat.events':
                run = self.get('run', identifier(request.get('id')))
                after = request.get('after', 0)
                if type(after) is not int or after < 0:
                    raise Refused('Invalid cursor')
                events = [{'seq': row[0], **json.loads(row[1])} for row in self.db.execute('SELECT seq,value FROM events WHERE run=? AND seq>? ORDER BY seq LIMIT 100', (run['id'], after))]
                status = 'settled' if run['status'] == 'done' else run['status']
                return {'events': events, 'status': status, 'error': run.get('summary') if status in ('failed', 'blocked', 'cancelled') else None}
            if action == 'chat.event':
                run = self.run_for_agent(request)
                if run['kind'] != 'chat' or run['status'] != 'running':
                    raise Refused('Not an active chat run')
                event = request.get('event', {})
                kind = event.get('type')
                if kind == 'text_delta':
                    delta = event.get('delta')
                    if not isinstance(delta, str) or len(delta.encode()) > 16000 or '\x00' in delta:
                        raise Refused('Invalid chat delta')
                    self.event(kind, run['id'], delta=delta)
                elif kind == 'settled':
                    self.finish(run, 'done', 'Chat completed')
                elif kind == 'error':
                    self.finish(run, 'failed', 'Chat failed')
                else:
                    raise Refused('Invalid chat event')
                return {'accepted': True}
            if action == 'chat.cancel':
                run = self.get('run', identifier(request.get('id')))
                if run['kind'] != 'chat':
                    raise Refused('Not a chat run')
                return self.finish(run, 'cancelled', 'Request cancelled')
            if action == 'snapshot':
                return {'peers': self.dispatch({'version': 1, 'action': 'agent.list'}),
                        'messages': [{k: v for k, v in m.items() if k != 'key'} for m in self.rows('message')[-100:]],
                        'goals': self.rows('goal'), 'runs': self.rows('run')[-100:],
                        'tasks': [r for r in self.rows('run') if r['kind'] == 'task'][-100:],
                        'events': [{'seq': n, **json.loads(v)} for n, v in self.db.execute('SELECT seq,value FROM events ORDER BY seq DESC LIMIT 100')]}
            raise Refused('Unknown runtime action')

    def schedule(self, goal, calendar):
        directory = private_dir(self.home / '.config/systemd/user')
        unit = 'worklane-goal-' + goal['id']
        (directory / (unit + '.service')).write_text('[Unit]\nDescription=Plan a scheduled Worklane goal\nRequires=worklane-collab.service\nAfter=worklane-collab.service\n[Service]\nType=oneshot\nExecStart=/usr/local/bin/worklane-collab occurrence ' + goal['id'] + '\nTimeoutStartSec=30\n')
        (directory / (unit + '.timer')).write_text('[Unit]\nDescription=Schedule a Worklane goal\n[Timer]\nOnCalendar=' + calendar + '\nPersistent=true\n[Install]\nWantedBy=timers.target\n')
        command(['systemctl', '--user', 'daemon-reload'])
        command(['systemctl', '--user', 'enable', '--now', unit + '.timer'])

    def herdr(self, *args):
        if not self.session:
            raise Refused('WORKLANE_SESSION is missing')
        output = command(['herdr', '--session', self.session, *args]).strip()
        # Herdr pane run can succeed with no JSON body. Other calls must have
        # structured results: never invent an ID from a focused pane.
        if args[:2] == ('pane', 'run') and not output:
            return {}
        try:
            return json.loads(output)['result']
        except (ValueError, KeyError, TypeError) as error:
            raise Refused('Herdr returned no structured result') from error

    def start_run(self, ident):
        try:
            with self.lock, self.db:
                run = self.get('run', ident)
                if run['status'] != 'queued':
                    return
                run.update(status='starting', started=time.time(), unit='worklane-run-' + ident + '.scope')
                self.put('run', run)
            snapshot = self.herdr('api', 'snapshot')['snapshot']
            workspace = next((w['workspace_id'] for w in snapshot['workspaces'] if w.get('label') == self.session), None)
            if not workspace:
                raise Refused('Managed Herdr workspace is unavailable')
            tab = self.herdr('tab', 'create', '--workspace', workspace, '--cwd', str(self.home), '--label', run['kind'] + ':' + ident[:8],
                             '--env', 'WORKLANE_RUN_ID=' + ident, '--env', 'WORKLANE_RUN_KIND=' + run['kind'], '--no-focus')
            pane = tab['root_pane']['pane_id']
            with self.lock, self.db:
                run = self.get('run', ident)
                run.update(pane_id=pane)
                self.put('run', run)
            session_dir = private_dir(self.home / '.pi/agent/sessions/worklane-runs')
            argv = ['systemd-run', '--user', '--scope', '--quiet', '--collect', '--unit', run['unit'],
                    '--property=RuntimeMaxSec=' + str(run['budget']['seconds']) + 's',
                    'worklane-pi-agent', '--session', str(session_dir / (ident + '.jsonl'))]
            self.herdr('pane', 'run', pane, shlex.join(argv))
            if run['kind'] == 'plan':
                for _ in range(60):
                    agents = self.herdr('agent', 'list')['agents']
                    if any(a.get('pane_id') == pane and a.get('agent_status') in ('idle', 'done') for a in agents):
                        prompt = '/plan ' + run['prompt'] + '\nGoal ID: ' + run['goal_id'] + '\nResolve scope and tests. If external mutations are needed, call goal_plan with exact operation/arguments descriptors before plan_mode_complete. Complete with plan_mode_complete. Do not implement or approve your own plan.'
                        self.herdr('agent', 'prompt', pane, prompt)
                        break
                    time.sleep(1)
                else:
                    raise Refused('Planner did not become ready')
        except (Refused, OSError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
            with self.lock, self.db:
                self.finish(self.get('run', ident), 'failed', 'Run startup failed: ' + str(error))
        finally:
            with self.lock:
                self.launching.discard(ident)

    def ensure_view(self):
        try:
            snapshot = self.herdr('api', 'snapshot')['snapshot']
            workspace = next((w['workspace_id'] for w in snapshot['workspaces'] if w.get('label') == self.session), None)
            if not workspace or any(t.get('label') == 'collaboration' for t in snapshot['tabs']):
                return
            tab = self.herdr('tab', 'create', '--workspace', workspace, '--cwd', str(self.home), '--label', 'collaboration', '--no-focus')
            self.herdr('pane', 'run', tab['root_pane']['pane_id'], 'worklane-collab watch')
        except (Refused, KeyError, OSError, subprocess.TimeoutExpired):
            pass  # Herdr may not yet be up; retry without affecting the durable queue.

    def tick(self):
        if self.launch and time.time() - self.last_view_check > 10:
            self.last_view_check = time.time()
            threading.Thread(target=self.ensure_view, daemon=True).start()
        with self.lock, self.db:
            for run in self.rows('run'):
                if run['status'] in ('starting', 'running') and time.time() - run['started'] >= run['budget']['seconds']:
                    self.finish(run, 'blocked', 'Run time budget exhausted')
                if self.launch and run['status'] == 'queued' and run['id'] not in self.launching:
                    # One top-level goal/planner at a time; helper slots belong to it.
                    busy = any(r['kind'] in ('goal', 'plan') and r['status'] in ('starting', 'running') for r in self.rows('run')) or any(self.get('run', ident)['kind'] in ('goal', 'plan') for ident in self.launching)
                    if run['kind'] in ('goal', 'plan') and busy:
                        continue
                    self.launching.add(run['id'])
                    threading.Thread(target=self.start_run, args=(run['id'],), daemon=True).start()


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.request.settimeout(10)
        try:
            raw = self.rfile.readline(MAX_REQUEST + 1)
            if len(raw) > MAX_REQUEST or not raw.endswith(b'\n'):
                raise Refused('Oversized or incomplete request')
            request = json.loads(raw)
            result = self.server.runtime.dispatch(request)
            response = {'ok': True, 'result': result}
        except Refused as error:
            response = {'ok': False, 'error': str(error), 'error_code': error.code}
        except (ValueError, TypeError, KeyError, OSError, sqlite3.Error, subprocess.TimeoutExpired):
            response = {'ok': False, 'error': 'Invalid, unavailable or unauthorized runtime request', 'error_code': 'invalid_request'}
        try:
            self.wfile.write((json.dumps(response) + '\n').encode())
        except (BrokenPipeError, ConnectionResetError):
            pass  # A polling client left; never log sensitive payloads.


def client(home, payload):
    path = Path(home) / '.local/share/worklane/collab.sock'
    info = path.lstat()
    if not stat.S_ISSOCK(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise Refused('Unsafe runtime socket')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(30)
        connection.connect(str(path))
        connection.sendall((json.dumps(payload) + '\n').encode())
        raw = b''
        while not raw.endswith(b'\n') and len(raw) <= 4 * 1024 * 1024:
            chunk = connection.recv(65536)
            if not chunk:
                break
            raw += chunk
        if len(raw) > 4 * 1024 * 1024 or not raw.endswith(b'\n'):
            raise Refused('Invalid runtime response')
        return json.loads(raw)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['serve', 'request', 'watch', 'tick', 'occurrence'])
    parser.add_argument('id', nargs='?')
    args = parser.parse_args()
    home = Path.home()
    if args.mode == 'serve':
        os.umask(0o077)
        runtime = Runtime(home)
        lock_path = runtime.root / 'daemon.lock'
        with lock_path.open('a') as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise Refused('Lane collaboration daemon already owns this store') from None
            sock = home / '.local/share/worklane/collab.sock'
            if sock.exists():
                # Only this lock holder can unlink the stale socket.
                probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                try:
                    probe.connect(str(sock))
                except ConnectionRefusedError:
                    sock.unlink()
                else:
                    raise Refused('Runtime already running')
                finally:
                    probe.close()
            with Server(str(sock), Handler) as server:
                server.runtime = runtime
                server.timeout = 1
                sock.chmod(0o600)
                # Recovery is safe only after exclusive socket/store ownership.
                runtime.recover()
                try:
                    while True:
                        server.handle_request()
                        runtime.tick()
                finally:
                    sock.unlink(missing_ok=True)
    elif args.mode == 'watch':
        while True:
            try:
                response = client(home, {'version': 1, 'action': 'snapshot'})
                state = response.get('result', {})
                print('\x1b[2J\x1b[HWorklane collaboration — lane-local', flush=True)
                for section in ('peers', 'goals', 'runs', 'messages'):
                    print('\n' + section.upper())
                    for row in state.get(section, [])[-15:]:
                        line = json.dumps(row, ensure_ascii=True)
                        print(line[:800])
                print('\nApproval: worklane-collab request (goal.approve + plan_sha256). No cross-lane relay.', flush=True)
            except (OSError, Refused):
                print('Runtime unavailable; retrying', flush=True)
            time.sleep(2)
    else:
        if args.mode == 'occurrence':
            payload = {'version': 1, 'action': 'goal.occurrence', 'id': identifier(args.id)}
        elif args.mode == 'tick':
            payload = {'version': 1, 'action': 'snapshot'}
        else:
            import sys
            raw = sys.stdin.buffer.read(MAX_REQUEST + 1)
            if len(raw) > MAX_REQUEST:
                raise Refused('Oversized request')
            payload = json.loads(raw)
        response = client(home, payload)
        print(json.dumps(response))
        if not response.get('ok'):
            raise SystemExit(1)


if __name__ == '__main__':
    try:
        main()
    except (OSError, Refused, ValueError) as error:
        import sys
        print('worklane-collab: ' + str(error), file=sys.stderr)
        raise SystemExit(1)
