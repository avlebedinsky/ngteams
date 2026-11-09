#!/usr/bin/env python3
"""Read-only task dispatch page; Python standard library only, loopback HTTP."""

import argparse
import json
import os
import socketserver
import statistics
import sys
import time
from pathlib import Path

from transcript import Transcript, shown

PHASES = {
    'edit': ['investigate', 'implement', 'check', 'commit', 'done'],
    'review': ['review', 'gate', 'done'],
    'scratch': ['investigate', 'implement', 'verify', 'done'],
}
DEFAULTS = {'edit': [10, 30, 4, 1], 'review': [45, 30], 'scratch': [5, 20, 10]}
HEAD_MAX = 16 * 1024
# A review or fix companion's card fields that follow whichever companion is working.
ACTIVITY = ('doing', 'said', 'background', 'waiting_for', 'written_ms', 'transcript_error')


def group(state):
    state = state.strip().lower()
    for prefix, name in [('waiting', 'waiting'), ('fail', 'failed'), ('queued', 'queued'), ('in review', 'review'),
                         ('landed', 'completed'), ('done', 'completed'),
                         ('canceled', 'canceled'), ('cancelled', 'canceled')]:
        if state.startswith(prefix):
            return name
    return 'running'


def holds_slot(row):
    # Only a running task companion counts; folded review and fix stages never hold slots.
    return row['group'] == 'running' and row['role'] == 'task' and not row.get('stage')


def companion_stage(child):
    # Companions review or fix; the dispatcher owns merging after the review gate.
    if child['group'] not in ('running', 'waiting') or child['state'].lower().startswith('cancel'):
        return None
    return 'fixing' if child['role'] == 'fix' else 'reviewing'


def review_stage(row, review_state, working):
    # Where a task is in its review life cycle, or None when its card should read as usual. A companion still
    # implementing keeps its own card, even with a review of an earlier stage running beside it.
    state = row['state'].lower()
    if row['group'] not in ('running', 'review', 'completed') or state.startswith('cancel'):
        return None
    if state.startswith('running') and not (working and row.get('done')):
        return None
    if state.startswith(('merging', 'landing')):
        return 'merging'
    if review_state in ('reviewed', 'skipped') and state.startswith(('in review', 'committed')):
        return 'merging'
    if working:
        return companion_stage(working)
    if review_state in ('reviewing', 'fixing'):
        return review_state
    if review_state in ('queued', 'blocked'):
        return 'review ' + review_state
    # Handback frees the slot even before the dispatcher has started its review companion.
    return 'reviewing' if row['group'] == 'review' else None


def review_line(review, stage):
    review = review if isinstance(review, dict) else {}
    if review.get('mode') == 'none':
        return ''
    round_text = f"round {review['round']}" if type(review.get('round')) is int else None
    now = stage.removeprefix('review ') if stage else review.get('state')
    parts = [review.get('mode'), round_text, now]
    return ' · '.join(shown(part, 40) for part in parts if isinstance(part, str) and part)


def progress(flow, log, completed=False, waiting=False):
    phases = PHASES[flow]
    entered = [log.first] + [None] * (len(phases) - 1)
    phase = 0
    edited = reviewed = False
    checking = None
    for at, signal in log.signals:
        target = phase
        if flow == 'edit':
            if signal == 'implement':
                edited, checking = True, None
                target = max(phase, 1)
            elif signal in ('build', 'check'):
                target = max(phase, 1)
                if edited and target == 1:
                    checking = checking if checking is not None else at
            elif signal == 'commit':
                if checking is not None:
                    entered[2] = checking
                target = max(phase, 3)
        elif flow == 'review':
            if signal == 'review':
                reviewed = True
            elif signal == 'gate' or (signal == 'check' and reviewed):
                target = max(phase, 1)
        else:
            if signal in ('implement', 'scratch_implement'):
                edited = True
                target = max(phase, 1)
            elif signal in ('check', 'verify', 'probe', 'review'):
                target = max(phase, 2 if edited or signal == 'verify' else 1)
            elif signal == 'build':
                target = max(phase, 1)
        for index in range(phase + 1, target + 1):
            if entered[index] is None:
                entered[index] = at
        phase = target
    if flow == 'edit' and phase == 1 and checking is not None:
        phase, entered[2] = 2, checking
    done_at = (log.handback if log.handback is not None else log.last) if completed else log.handback
    if waiting:
        done_at = None
    if done_at is not None:
        for index in range(phase + 1, len(phases)):
            entered[index] = done_at
        phase = len(phases) - 1
    return phase, entered


class Status:
    def __init__(self, ledger, subagents=None, projects=None, session=None):
        self.path = Path(ledger)
        self.subagents = Path(subagents).resolve() if subagents else None
        self.projects = Path(projects).resolve() if projects else None
        self.session = session
        self.board = {'tasks': [], 'slots': 2}
        self.error = None
        self.seen = None
        self.logs = {}

    def refresh(self):
        try:
            stat = self.path.stat()
            key = (stat.st_ino, stat.st_mtime_ns, stat.st_size)
            if key == self.seen:
                return
            board = json.loads(self.path.read_bytes())
            if not isinstance(board, dict) or not isinstance(board.get('tasks'), list):
                raise TypeError('no tasks list')
            self.board, self.seen, self.error = board, key, None
        except (OSError, ValueError, TypeError) as error:
            self.error = 'The ledger is unreadable (' + type(error).__name__ + '); showing its last good copy'

    def names(self):
        directory = self.subagents
        if directory is None and self.projects and self.session:
            directory = next((path for path in self.projects.glob('*/' + self.session + '/subagents') if path.is_dir()), None)
        found = {}
        if directory:
            for meta in directory.glob('*.meta.json'):
                try:
                    name = json.loads(meta.read_bytes()).get('name')
                    path = meta.with_name(meta.name.removesuffix('.meta.json') + '.jsonl')
                    modified = path.stat().st_mtime_ns
                    if isinstance(name, str) and (name not in found or modified > found[name][0]):
                        found[name] = (modified, path)
                except (OSError, ValueError, AttributeError):
                    continue
        return {name: pair[1] for name, pair in found.items()}

    def snapshot(self, now=None):
        now = int(time.time() * 1000) if now is None else now
        self.refresh()
        names = self.names()
        rows = []
        children = {}
        reviews = {}
        used_logs = set()
        for task in self.board['tasks']:
            if not isinstance(task, dict) or type(task.get('num')) is not int:
                continue
            row = {key: shown(task.get(key) or '', limit) for key, limit in (
                ('title', 120), ('agent', 40), ('state', 60), ('note', 240),
            )}
            row['state'] = row['state'] or 'queued'
            row['runtime'] = task.get('runtime') if task.get('runtime') in ('claude', 'codex') else ''
            row['role'] = task.get('role') if task.get('role') in ('revmux', 'fix') else 'task'
            row['review'] = review_line(task.get('review'), None)
            row['num'] = task['num']
            row['after'] = task.get('after') if type(task.get('after')) is int else None
            row['group'] = group(row['state'])
            agent = task.get('agent')
            path = task.get('transcript') or (names.get(agent) if row['group'] != 'queued' and isinstance(agent, str) else None)
            log = None
            if isinstance(path, (str, Path)) and path:
                path = Path(path)
                if not path.is_absolute():
                    path = self.path.parent / path
                used_logs.add(path)
                log = self.logs.setdefault(path, Transcript(path))
                log.poll()
            flow = task.get('flow')
            if not isinstance(flow, str) or flow not in PHASES:
                flow = 'scratch' if log and not log.worktree else 'edit'
            row.update(flow=flow, phases=PHASES[flow], eta_ms=None, long=False)
            if log:
                row.update(transcript_error=log.error, started_ms=log.first, written_ms=log.written,
                           doing=log.doing, said=log.said, background=list(log.background.values()), waiting_for=None)
                if log.first is not None:
                    finished = row['group'] in ('completed', 'review')
                    phase, entered = progress(flow, log, finished, row['group'] in ('waiting', 'canceled'))
                    row.update(phase=phase, phase_since_ms=entered[phase], done=phase == len(PHASES[flow]) - 1,
                               ended_ms=entered[-1] if entered[-1] is not None else (
                                   log.last if row['group'] != 'running' else None), _entered=entered)
                pending = log.doing and log.doing.get('finished_ms') is None
                if log.background and not pending and now - (log.written or now) >= 30_000:
                    row['waiting_for'] = list(log.background.values())[-1]
            if type(task.get('of')) is int:
                row['role'] = task.get('role')
                children.setdefault(task['of'], []).append(row)
            else:
                reviews[row['num']] = task.get('review')
                rows.append(row)
        self.logs = {path: log for path, log in self.logs.items() if path in used_logs}
        lengths = self.typical(rows)
        for row in rows:
            phase = row.get('phase')
            if phase is not None and phase < len(lengths[row['flow']]):
                typical = lengths[row['flow']]
                current = typical[phase]
                row['phase_typical_ms'] = current
                if row['group'] == 'running':
                    elapsed = max(0, now - row['phase_since_ms'])
                    row['eta_ms'] = max(current - elapsed, current // 5) + sum(typical[phase + 1:])
                    row['long'] = elapsed > current
            row.pop('_entered', None)
        self.lifecycle(rows, children, reviews)
        slots = self.board.get('slots', 2)
        slots = slots if type(slots) is int and slots >= 1 else 2
        self.queues(rows, slots)
        for row in rows:
            # After the queue: a task waiting for #N to land isn't held up by #N's review once #N has landed.
            if row.get('stage'):
                row['group'] = row.pop('_group')
        return {
            'now_ms': now, 'slots_max': slots,
            'slots_used': sum(holds_slot(row) for row in rows),
            'queue_len': sum(row['group'] == 'queued' for row in rows),
            'ledger_error': self.error, 'tasks': rows,
            'heuristic': 'An estimate, not a promise: remaining phase times use medians from completed tasks, '
                         'or defaults until samples exist. Phases shorter than 30 seconds are excluded. '
                         'A long phase retains at least a fifth of its typical time, plus later phases.',
        }

    @staticmethod
    def lifecycle(rows, children, reviews):
        # A task's review and fix companions have no card: its one card carries their progress.
        for row in rows:
            # Legacy completed tasks without companion bookkeeping keep their settled card.
            if row['group'] == 'completed' and row['num'] not in children:
                continue
            review = reviews.get(row['num'])
            review_state = review.get('state') if isinstance(review, dict) else None
            live = [child for child in children.get(row['num'], ()) if companion_stage(child)]
            working = max(live, key=lambda child: child['num'], default=None)
            stage = review_stage(row, review_state, working)
            row['review'] = review_line(review, stage or (companion_stage(working) if working else None))
            if not stage:
                continue
            kept = [name for name in row['phases'] if name not in ('review', 'gate', 'land', 'done')]
            phases = [*kept, 'review', 'fixing', 'merging', 'done']
            current = 'review' if stage.startswith('review') else stage
            display_group = 'review' if row['group'] == 'review' else 'running'
            if working and working['group'] == 'waiting':
                display_group = 'waiting'
            if stage == 'review blocked':
                display_group = 'failed'
            row.update(stage=stage, state=stage, phases=phases, phase=phases.index(current), done=False,
                       eta_ms=None, long=False, ended_ms=None, phase_typical_ms=None,
                       _group=display_group)
            source = working or {}
            row.update({key: source.get(key) for key in ACTIVITY})
            if working:
                row.update(agent=working['agent'], runtime=working['runtime'], note=working['note'] or row['note'])

    @staticmethod
    def typical(rows):
        lengths = {}
        for flow, defaults in DEFAULTS.items():
            result = []
            for index, minutes in enumerate(defaults):
                samples = []
                for row in rows:
                    if row['flow'] == flow and row['group'] == 'completed' and row.get('done'):
                        entered = row['_entered']
                        if entered[index] is not None and entered[index + 1] is not None:
                            length = entered[index + 1] - entered[index]
                            if length >= 30_000:
                                samples.append(length)
                result.append(int(statistics.median(samples)) if samples else minutes * 60_000)
            lengths[flow] = result
        return lengths

    @staticmethod
    def queues(rows, slots):
        free = max(0, slots - sum(holds_slot(row) for row in rows))
        queued = sorted((row for row in rows if row['group'] == 'queued'), key=lambda row: row['num'])
        etas = sorted(row['eta_ms'] for row in rows
                      if holds_slot(row) and row['eta_ms'] is not None)
        ahead = 0
        for place, row in enumerate(queued, 1):
            blocker = next((other for other in rows if other['num'] == row['after']
                            and other['group'] not in ('completed', 'canceled')), None)
            if blocker:
                # A task ETA ends at handback; its dependent still needs review and landing to finish.
                start = None
            else:
                index = ahead - free
                start = 0 if ahead < free else etas[index] if index < len(etas) else None
                ahead += 1
            row['queue'] = {'place': place, 'start_ms': start}


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        try:
            self.request.settimeout(2)
            head = bytearray()
            deadline = time.monotonic() + 2
            while True:
                self.request.settimeout(max(0.001, deadline - time.monotonic()))
                line = self.rfile.readline(HEAD_MAX + 1 - len(head))
                if not line:
                    return
                head.extend(line)
                if len(head) > HEAD_MAX or time.monotonic() > deadline:
                    return
                if line in (b'\r\n', b'\n'):
                    break
            code, kind, body = self.respond(head.decode('iso-8859-1'))
            self.wfile.write((f'HTTP/1.1 {code}\r\nContent-Type: {kind}\r\nContent-Length: {len(body)}\r\n'
                              'Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n').encode())
            self.wfile.write(body)
        except (OSError, ValueError):
            return

    def respond(self, head):
        words = head.splitlines()[0].split() if head.splitlines() else []
        text = 'text/plain; charset=utf-8'
        if not words or words[0] != 'GET':
            return '405 Method Not Allowed', text, b'GET only\n'
        hosts = [line.split(':', 1)[1].strip() for line in head.splitlines()[1:]
                 if line.partition(':')[0].lower() == 'host']
        port = self.server.server_address[1]
        if len(hosts) != 1 or hosts[0] not in (f'127.0.0.1:{port}', f'localhost:{port}'):
            return '403 Forbidden', text, b'wrong host\n'
        target = words[1].split('?', 1)[0] if len(words) > 1 else ''
        if target in ('/', '/index.html'):
            return '200 OK', 'text/html; charset=utf-8', self.server.page
        if target == '/status.json':
            body = json.dumps(self.server.status.snapshot(), ensure_ascii=False).encode()
            return '200 OK', 'application/json', body
        return '404 Not Found', text, b'not found\n'


class Server(socketserver.TCPServer):
    allow_reuse_address = True

    def __init__(self, port, status):
        self.status = status
        self.page = Path(__file__).with_name('assets').joinpath('index.html').read_bytes()
        super().__init__(('127.0.0.1', port), Handler)
        self.timeout = 0.5

    def handle_error(self, request, client_address):
        # Never print request data or tracebacks containing ledger/transcript values.
        print('dispatch-status: request failed', file=sys.stderr)


def alive(pid):
    if pid == 0:
        return False
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except ProcessLookupError:
        return False


def bounded_int(minimum, maximum):
    def parse(value):
        result = int(value)
        if not minimum <= result <= maximum:
            raise argparse.ArgumentTypeError(f'must be between {minimum} and {maximum}')
        return result
    return parse


def main():
    parser = argparse.ArgumentParser(prog='dispatch-status', description=__doc__)
    parser.add_argument('--ledger', required=True, type=Path)
    parser.add_argument('--port', type=bounded_int(0, 65535), default=47231)
    parser.add_argument('--subagents', type=Path)
    parser.add_argument('--exit-with', type=bounded_int(0, 2**31 - 1))
    args = parser.parse_args()
    session = os.environ.get('CLAUDE_CODE_SESSION_ID')
    projects = Path.home() / '.claude/projects' if session else None
    status = Status(args.ledger, args.subagents, projects, session)
    try:
        server = Server(args.port, status)
    except OSError as error:
        print(f"dispatch-status: can't listen on 127.0.0.1:{args.port}: {type(error).__name__}", file=sys.stderr)
        return 1
    agents = str(args.subagents or (str(projects) + '/*/' + session + '/subagents' if session else "the ledger's transcript paths only"))
    print(f'dispatch-status: http://127.0.0.1:{server.server_address[1]}/ (ledger {args.ledger}, agents in {agents})', flush=True)
    try:
        with server:
            while args.exit_with is None or alive(args.exit_with):
                server.handle_request()
        print(f'dispatch-status: process {args.exit_with} ended, stopping', flush=True)
    except KeyboardInterrupt:
        server.server_close()
    return 0


if __name__ == '__main__':
    sys.exit(main())
