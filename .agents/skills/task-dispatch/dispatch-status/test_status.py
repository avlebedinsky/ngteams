"""Synthetic boards, event logs, Git repositories and localhost HTTP; no account/session data."""

import json
import os
import re
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from status import Server, Status, progress
from transcript import Transcript, command_signals, redact, shown, tool_activity

ROOT = Path(__file__).resolve().parents[1]
STAMP = '2026-11-03T12:00:00Z'


class Files(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='dispatch-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.ledger = self.root / 'progress.json'
        self.events = self.root / 'events.jsonl'

    def board(self, tasks, **fields):
        self.ledger.write_text(json.dumps({'_synthetic': True, 'tasks': tasks, **fields}))

    def event(self, phase, text='Synthetic activity', minute=0):
        event = {'type': 'dispatch', 'timestamp': f'2026-11-03T12:{minute:02}:00Z', 'phase': phase, 'text': text}
        with self.events.open('a') as stream:
            stream.write(json.dumps(event) + '\n')


class DisplayTests(unittest.TestCase):
    def test_secrets_are_redacted_in_all_display_forms(self):
        cases = [
            ('Authorization: Bearer abc.def-123', 'abc.def-123'),
            ('authorization=Basic dXNlcjpwYXNz', 'dXNlcjpwYXNz'),
            ('cookie=SID=abc123xyz; other=val99', 'val99'),
            ('Cookie: a=b; c=d', 'c=d'),
            ('{"Cookie": "a=b; c=d"}', 'a=b'),
            ('x?access_token=abc123&y=2', 'abc123'),
            ('{"refresh_token": "abc"}', '"abc"'),
            ('CLIENT_SECRET=s3cr3t ./run', 's3cr3t'),
            ('u?X-Amz-Signature=abc&b=1', '=abc'),
            ('login --password hunter2 now', 'hunter2'),
            ('x --signoff --token abc --api-key "a b" y', 'abc'),
            ('x --token abc --api-key "a b" y', 'a b'),
            ('payload=\'{"password":"hunter2"}\'', 'hunter2'),
            ('--command "login --password hunter2"', 'hunter2'),
            ('payload=\'{"password":"abc\\"def"}\'', 'def'),
            ('token bearer abcdefghij', 'abcdefghij'),
            ('t eyJhbGciOi.eyJzdWIiOiIx.c2lnbmF0dXJl x', 'eyJ'),
            ('Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb7dEf0hIj', 'Ab3dEf'),
            ('QUJDREVGR0hJSktMTU5PUFFSU1RVVldY/WVphYmNkZWZnaGlqa2xtbm9w+cXI=', 'QUJDREV'),
        ]
        for text, secret in cases:
            with self.subTest(text=text):
                result = redact(text)
                self.assertNotIn(secret, result)
                self.assertIn('[redacted]', result)

    def test_paths_identifiers_and_harmless_flags_survive(self):
        for text in ('teams_auth::token::NATIVE_CLIENT_REDIRECT', 'crates/teams-fence/src/chat.rs',
                     'git commit --signoff -m msg', 'gh pr edit --assignee me',
                     'a_plain_build_at_the_root_leaves_the_personal_build_out', 'https://example.com/a/b'):
            self.assertEqual(redact(text), text)
        self.assertEqual(shown('ab  cd', 4), 'ab…')
        self.assertEqual(shown('  a\n b '), 'a b')

    def test_long_nonassignment_is_linear(self):
        # A separate process bounds regressions without leaving a stuck regex thread behind.
        code = 'from transcript import shown; print(shown("a" * 200000))'
        result = subprocess.run([sys.executable, '-B', '-c', code], cwd=ROOT / 'dispatch-status',
                                capture_output=True, timeout=5, check=True)
        self.assertLess(len(result.stdout), 200)


class SignalTests(unittest.TestCase):
    def test_only_executed_programs_emit_phase_signals(self):
        for command in ('head -20 scripts/land.sh', 'sed -n 1,20p scripts/land.sh',
                        'ls .revmux/tasks', 'echo "revmux; cargo test"'):
            self.assertEqual(command_signals(command), [], command)
        for command, expected in (
            ('env X=y timeout 30 nice -n 5 cargo --locked test', ['check']),
            ('flock -w 1 /tmp/lock cargo test', ['check']),
            ('cargo check\ngit -C /repo commit -m x', ['build', 'commit']),
            ('cargo test 2>&1 | tee /tmp/log', ['check', 'scratch_implement']),
            ('revmux --task x', ['review']),
        ):
            self.assertEqual(command_signals(command), expected, command)

    def test_scratch_edits_do_not_end_editors_check_phase(self):
        log = Transcript('unused')
        log.first = 0
        log.signals = [(1, 'implement'), (2, 'check')]
        for name, data in [('Write', {'file_path': '/tmp/msg.txt'}),
                           ('Bash', {'command': 'cargo test | tee /tmp/log'})]:
            signals = tool_activity(name, data)[1]
            log.signals.extend((3, signal) for signal in signals)
            self.assertEqual(progress('edit', log)[0], 2)
        log.signals = [(1, 'probe')]
        self.assertEqual(progress('scratch', log)[0], 1)
        log.signals = [(1, 'scratch_implement'), (2, 'probe')]
        self.assertEqual(progress('scratch', log)[0], 2)


class TranscriptTests(Files):
    def test_missing_event_file_is_pending_not_an_error(self):
        log = Transcript(self.events)
        log.poll()
        self.assertIsNone(log.first)
        self.assertIsNone(log.error)
        self.event('investigate')
        log.poll()
        self.assertIsNotNone(log.first)

    def test_partial_append_and_rotation(self):
        entry = json.dumps({'type': 'dispatch', 'timestamp': STAMP, 'phase': 'implement', 'text': 'Editing'})
        self.events.write_text(entry[:30])
        log = Transcript(self.events)
        log.poll()
        self.assertIsNone(log.first)
        with self.events.open('a') as stream:
            stream.write(entry[30:] + '\n')
        log.poll()
        self.assertEqual(log.signals[0][1], 'implement')
        replacement = self.root / 'replacement'
        replacement.write_text('bad json\n' + json.dumps({'type': 'dispatch', 'timestamp': STAMP,
                                                      'phase': 'investigate', 'text': 'Fresh'}) + '\n')
        replacement.replace(self.events)
        log.poll()
        self.assertEqual(log.doing['text'], 'Fresh')
        self.assertEqual(len(log.signals), 1)

    def test_claude_tools_background_and_handback(self):
        log = Transcript(self.events)
        def entry(kind, content, **fields):
            log.entry({'type': kind, 'timestamp': STAMP, 'message': {'content': content}, **fields})
        entry('assistant', [{'type': 'tool_use', 'id': 't1', 'name': 'Bash',
                             'input': {'command': 'cargo test', 'run_in_background': True}}])
        self.assertEqual(log.signals[-1][1], 'check')
        entry('user', [{'type': 'tool_result', 'tool_use_id': 't1', 'content': 'with ID: bg1'}])
        self.assertIsNotNone(log.doing['finished_ms'])
        self.assertEqual(log.background['t1']['task_id'], 'bg1')
        log.entry({'type': 'attachment', 'attachment': {'prompt': '<task-notification><task-id>bg1</task-id>'
                   '<status>completed</status></task-notification>'}})
        self.assertFalse(log.background)
        entry('assistant', [{'type': 'tool_use', 'name': 'SubagentHandback', 'input': {}}])
        self.assertIsNotNone(log.handback)
        entry('assistant', [{'type': 'tool_use', 'name': 'Read', 'input': {'file_path': 'file.rs'}}])
        self.assertIsNone(log.handback)

    def test_portable_phases_and_rework(self):
        log = Transcript(self.events)
        for minute, phase in enumerate(('investigate', 'implement', 'check', 'implement', 'check', 'commit', 'done')):
            self.event(phase, minute=minute)
            log.poll()
            current, _ = progress('edit', log)
            self.assertEqual(current, {'investigate': 0, 'implement': 1, 'check': 2, 'commit': 3, 'done': 4}[phase])
        self.assertEqual(log.doing['finished_ms'], log.handback)

    def test_review_and_scratch_phases(self):
        for flow, phases in [('review', ('review', 'gate', 'done')),
                             ('scratch', ('investigate', 'implement', 'verify', 'done'))]:
            log = Transcript(self.events)
            for index, phase in enumerate(phases):
                log.entry({'type': 'dispatch', 'timestamp': STAMP, 'phase': phase, 'text': phase})
                self.assertEqual(progress(flow, log)[0], index)

    def test_event_helper_writes_unicode_and_rejects_bad_phase(self):
        helper = ROOT / 'scripts/task-event.py'
        subprocess.run([sys.executable, '-B', helper, self.events, 'check', 'Check ✓'], check=True)
        log = Transcript(self.events)
        log.poll()
        self.assertEqual(log.doing['text'], 'Check ✓')
        bad = subprocess.run([sys.executable, '-B', helper, self.events, 'typo', 'x'], capture_output=True, check=False)
        self.assertEqual(bad.returncode, 2)


class BoardTests(Files):
    def test_relative_subagents_resolve_from_cwd_not_ledger(self):
        agents = self.root / 'agents'
        agents.mkdir()
        (agents / 'worker.meta.json').write_text('{"name":"worker"}')
        (agents / 'worker.jsonl').write_text(json.dumps({'type': 'dispatch', 'timestamp': STAMP,
                                                       'phase': 'investigate', 'text': 'Found'}) + '\n')
        nested = self.root / 'nested'
        nested.mkdir()
        ledger = nested / 'progress.json'
        ledger.write_text(json.dumps({'tasks': [{'num': 1, 'agent': 'worker', 'state': 'running'}]}))
        relative = os.path.relpath(agents)
        self.assertEqual(Status(ledger, subagents=relative).snapshot()['tasks'][0]['doing']['text'], 'Found')

    def test_malformed_optional_fields_do_not_break_the_board(self):
        self.board([None, {}, {'num': 1, 'agent': [], 'flow': {}, 'after': [], 'state': 'queued'}], slots='3')
        result = Status(self.ledger).snapshot()
        self.assertEqual(len(result['tasks']), 1)
        self.assertEqual(result['tasks'][0]['flow'], 'edit')
        self.assertIsNone(result['tasks'][0]['after'])
        self.assertEqual(result['slots_max'], 2)

    def test_runtime_is_shown_and_malformed_runtime_is_blank(self):
        self.board([{'num': 1, 'runtime': 'codex', 'state': 'running'},
                    {'num': 2, 'runtime': ['x'], 'state': 'queued'}])
        rows = Status(self.ledger).snapshot()['tasks']
        self.assertEqual([row['runtime'] for row in rows], ['codex', ''])

    def test_portable_board_without_claude_metadata_and_last_good_copy(self):
        self.event('investigate')
        self.event('implement', 'TOKEN=synthetic-secret', 1)
        self.event('check', minute=2)
        self.board([{'num': 1, 'state': 'running', 'flow': 'edit', 'transcript': str(self.events)},
                    {'num': 2, 'state': 'queued', 'after': 1}], slots=3)
        status = Status(self.ledger)
        board = status.snapshot(1_793_707_400_000)
        self.assertEqual(board['slots_used'], 1)
        self.assertEqual(board['slots_max'], 3)
        self.assertEqual(board['tasks'][0]['phase'], 2)
        self.assertNotIn('synthetic-secret', json.dumps(board))
        self.assertIsNone(board['tasks'][1]['queue']['start_ms'])
        self.ledger.write_text('{')
        stale = status.snapshot()
        self.assertEqual(len(stale['tasks']), 2)
        self.assertIsNotNone(stale['ledger_error'])
        self.board([], slots=0)
        fresh = status.snapshot()
        self.assertIsNone(fresh['ledger_error'])
        self.assertEqual(fresh['slots_max'], 2)

    def test_claude_discovery_and_newest_agent(self):
        agents = self.root / 'projects' / 'project' / 'session' / 'subagents'
        self.board([{'num': 1, 'agent': 'worker', 'state': 'running'}])
        status = Status(self.ledger, projects=self.root / 'projects', session='session')
        self.assertNotIn('doing', status.snapshot()['tasks'][0])
        agents.mkdir(parents=True)
        for name, clock, text in [('old', 1, 'Old'), ('new', 2, 'New')]:
            (agents / (name + '.meta.json')).write_text(json.dumps({'name': 'worker'}))
            path = agents / (name + '.jsonl')
            path.write_text(json.dumps({'type': 'assistant', 'timestamp': STAMP,
                                       'message': {'content': [{'type': 'text', 'text': text}]}}) + '\n')
            os.utime(path, (clock, clock))
        self.assertEqual(status.snapshot()['tasks'][0]['said']['text'], 'New')

    def test_sample_medians_and_waiting_handback(self):
        for minute, phase in [(0, 'investigate'), (2, 'implement'), (5, 'check'), (6, 'commit'), (9, 'done')]:
            self.event(phase, minute=minute)
        self.board([{'num': 1, 'state': 'landed abc', 'flow': 'edit', 'transcript': str(self.events)},
                    {'num': 2, 'state': 'waiting on user', 'flow': 'edit', 'transcript': str(self.events)}])
        rows = Status(self.ledger).snapshot()['tasks']
        self.assertTrue(rows[0]['done'])
        self.assertFalse(rows[1]['done'])
        self.assertEqual(rows[1]['phase_typical_ms'], 180_000)


class SlotTests(Files):
    def test_only_running_task_companions_hold_slots(self):
        self.board([{'num': 1, 'state': 'running', 'role': 'task'},
                    {'num': 2, 'state': 'in review abc', 'role': 'task',
                     'review': {'mode': 'focused', 'round': 2, 'state': 'fixing'}},
                    {'num': 3, 'state': 'running', 'role': 'revmux', 'of': 2, 'flow': 'review'},
                    {'num': 4, 'state': 'running', 'role': 'fix', 'of': 2, 'flow': 'review'},
                    {'num': 5, 'state': 'queued'}], slots=2)
        board = Status(self.ledger).snapshot()
        rows = {row['num']: row for row in board['tasks']}
        self.assertEqual(board['slots_used'], 1)
        self.assertEqual(rows[2]['group'], 'review')
        self.assertEqual(rows[2]['review'], 'focused · round 2 · fixing')
        self.assertEqual(rows[3]['group'], 'running')
        self.assertEqual(rows[5]['queue']['start_ms'], 0)

    def test_a_prerequisite_in_review_still_holds_its_dependent(self):
        self.board([{'num': 1, 'state': 'in review abc'}, {'num': 2, 'state': 'queued', 'after': 1}], slots=2)
        rows = Status(self.ledger).snapshot()['tasks']
        self.assertIsNone(rows[1]['queue']['start_ms'])

    def test_running_prerequisite_frees_a_slot_for_independent_work_at_handback(self):
        self.event('investigate')
        self.board([{'num': 1, 'state': 'running', 'flow': 'edit', 'transcript': str(self.events)},
                    {'num': 2, 'state': 'queued', 'after': 1},
                    {'num': 3, 'state': 'queued'}], slots=1)
        rows = Status(self.ledger).snapshot(1_793_707_400_000)['tasks']
        self.assertIsNotNone(rows[0]['eta_ms'])
        self.assertIsNone(rows[1]['queue']['start_ms'])
        self.assertEqual(rows[2]['queue']['start_ms'], rows[0]['eta_ms'])

    def test_a_task_in_review_shows_its_phases_finished(self):
        self.event('investigate')
        self.event('commit', minute=1)
        self.board([{'num': 1, 'state': 'in review abc', 'flow': 'edit', 'transcript': str(self.events)}])
        row = Status(self.ledger).snapshot()['tasks'][0]
        self.assertTrue(row['done'])
        self.assertIsNone(row['eta_ms'])

    def test_canceled_tasks_take_no_slot_and_are_the_last_section(self):
        self.board([{'num': 1, 'state': 'canceled'}, {'num': 2, 'state': 'Cancelled by user'},
                    {'num': 3, 'state': 'running'}, {'num': 4, 'state': 'landed abc'}])
        board = Status(self.ledger).snapshot()
        self.assertEqual([row['group'] for row in board['tasks']], ['canceled', 'canceled', 'running', 'completed'])
        self.assertEqual(board['slots_used'], 1)
        page = (Path(__file__).with_name('assets') / 'index.html').read_text()
        order = re.findall(r'\["(\w+)", "[^"]+"\]', re.search(r'const GROUPS = (.*?);', page, re.S).group(1))
        self.assertEqual(order, ['waiting', 'failed', 'running', 'review', 'queued', 'completed', 'canceled'])

    def test_canceled_handback_keeps_the_last_work_phase(self):
        self.event('implement')
        self.event('done', 'Question for the user', 1)
        for flow in ('edit', 'review', 'scratch'):
            for state in ('canceled', 'Cancelled by user'):
                with self.subTest(flow=flow, state=state):
                    task = {'num': 1, 'state': 'waiting on user', 'flow': flow,
                            'transcript': str(self.events)}
                    self.board([task])
                    status = Status(self.ledger)
                    waiting = status.snapshot()['tasks'][0]
                    task['state'] = state
                    self.board([task])
                    canceled = status.snapshot()['tasks'][0]
                    self.assertFalse(canceled['done'])
                    self.assertEqual(canceled['phase'], waiting['phase'])
                    self.assertEqual(canceled['phase_since_ms'], waiting['phase_since_ms'])
                    self.assertIsNone(canceled['eta_ms'])

    def test_canceled_prerequisite_releases_queue_and_counts_the_slot(self):
        for state in ('canceled', 'Cancelled by user', 'landed abc'):
            with self.subTest(state=state):
                self.board([{'num': 1, 'state': state},
                            {'num': 2, 'state': 'queued', 'after': 1},
                            {'num': 3, 'state': 'queued'}], slots=1)
                rows = Status(self.ledger).snapshot()['tasks']
                self.assertEqual(rows[1]['queue'], {'place': 1, 'start_ms': 0})
                self.assertEqual(rows[2]['queue'], {'place': 2, 'start_ms': None})


class OneCardTests(Files):
    NOW = 1_794_000_000_000
    MODES = {'mode': 'comprehensive', 'round': 1}

    def transcript(self, name, text):
        path = self.root / name
        entry = {'type': 'dispatch', 'timestamp': STAMP, 'phase': 'review', 'text': text}
        path.write_text(json.dumps(entry) + '\n')
        return str(path)

    def snapshot(self, tasks, **fields):
        self.board(tasks, **fields)
        return Status(self.ledger).snapshot(self.NOW)

    def card(self, state='landed abc', review=None):
        return {'num': 1, 'title': 'Work', 'agent': 'work', 'runtime': 'codex', 'role': 'task', 'state': state,
                'flow': 'edit', 'review': {**self.MODES, 'state': 'reviewing', **(review or {})}}

    @staticmethod
    def child(num=2, role='revmux', state='running', **fields):
        return {'num': num, 'title': 'Review #1', 'agent': f'{role}-{num}', 'runtime': 'claude', 'role': role, 'of': 1,
                'state': state, 'flow': 'review', **fields}

    def test_children_have_no_card_and_no_place_in_the_counts(self):
        board = self.snapshot([self.card(), self.child(), self.child(3, 'fix'),
                               {'num': 4, 'title': 'Next', 'state': 'queued'}], slots=1)
        self.assertEqual([row['num'] for row in board['tasks']], [1, 4])
        self.assertEqual(board['slots_used'], 0)
        self.assertEqual(board['queue_len'], 1)
        self.assertEqual(board['tasks'][1]['queue'], {'place': 1, 'start_ms': 0})

    def test_a_landed_task_under_review_reads_as_running_and_follows_its_companion(self):
        path = self.transcript('review.jsonl', 'Reading the diff')
        row = self.snapshot([self.card(), self.child(transcript=path, note='Round one')])['tasks'][0]
        self.assertEqual((row['group'], row['state'], row['stage']), ('running', 'reviewing', 'reviewing'))
        self.assertEqual(row['review'], 'comprehensive · round 1 · reviewing')
        self.assertEqual((row['agent'], row['runtime'], row['note']), ('revmux-2', 'claude', 'Round one'))
        self.assertEqual(row['doing']['text'], 'Reading the diff')
        self.assertFalse(row['done'])
        self.assertIsNone(row['ended_ms'])
        self.assertIsNone(row['eta_ms'])
        self.assertEqual(row['phases'][row['phase']], 'review')

    def test_the_newest_live_companion_names_the_stage(self):
        path = self.transcript('fix.jsonl', 'Fixing')
        row = self.snapshot([self.card(), self.child(state='done'), self.child(3, 'fix', transcript=path)])['tasks'][0]
        self.assertEqual((row['state'], row['agent']), ('fixing', 'fix-3'))
        self.assertEqual(row['phases'][row['phase']], 'fixing')
        self.assertEqual(row['review'], 'comprehensive · round 1 · fixing')

    def test_a_canceled_companion_is_not_working(self):
        row = self.snapshot([self.card(review={'state': 'reviewed'}), self.child(state='canceled')])['tasks'][0]
        self.assertEqual((row['group'], row['state']), ('completed', 'landed abc'))
        self.assertNotIn('stage', row)

    def test_stage_from_the_review_object_without_a_live_companion(self):
        done = self.child(state='done')
        for state, review, group, shown_state in (
            ('landed abc', 'queued', 'running', 'review queued'),
            ('landed abc', 'fixing', 'running', 'fixing'),
            ('landed abc', 'blocked', 'failed', 'review blocked'),
            ('in review abc', 'reviewed', 'running', 'merging'),
            ('committed abc, under review', 'skipped', 'running', 'merging'),
            ('landing abc', 'reviewed', 'running', 'merging'),
        ):
            with self.subTest(state=state, review=review):
                row = self.snapshot([self.card(state, {'state': review}), done])['tasks'][0]
                self.assertEqual((row['group'], row['state']), (group, shown_state))
                self.assertIsNone(row['doing'])

    def test_a_settled_review_leaves_a_finished_card(self):
        for state, review in (('landed abc', 'reviewed'), ('landed abc', 'skipped')):
            with self.subTest(review=review):
                row = self.snapshot([self.card(state, {'state': review}), self.child(state='done')])['tasks'][0]
                self.assertEqual((row['group'], row['state']), ('completed', state))

    def test_a_waiting_companion_puts_the_card_in_waiting(self):
        child = self.child(state='waiting on user', note='Which option?')
        row = self.snapshot([self.card(), child])['tasks'][0]
        self.assertEqual((row['group'], row['note']), ('waiting', 'Which option?'))

    def test_a_companion_still_implementing_keeps_its_card_and_slot(self):
        board = self.snapshot([self.card('running'), self.child()])
        row = board['tasks'][0]
        self.assertEqual((row['group'], row['state'], row['agent']), ('running', 'running', 'work'))
        self.assertEqual(row['review'], 'comprehensive · round 1 · reviewing')
        self.assertEqual(board['slots_used'], 1)

    def test_a_task_waiting_on_a_landed_parent_is_not_held_up_by_its_review(self):
        board = self.snapshot([self.card(), self.child(), {'num': 4, 'state': 'queued', 'after': 1}], slots=1)
        self.assertEqual(board['tasks'][1]['queue']['start_ms'], 0)

    def test_a_finished_or_canceled_parent_is_left_alone(self):
        for state in ('canceled', 'waiting on user', 'failed: no'):
            with self.subTest(state=state):
                row = self.snapshot([self.card(state), self.child()])['tasks'][0]
                self.assertEqual(row['state'], state)
                self.assertNotIn('stage', row)

    def test_a_board_without_children_is_unchanged(self):
        row = self.snapshot([self.card()])['tasks'][0]
        self.assertEqual((row['group'], row['state']), ('completed', 'landed abc'))
        for field in ('stage', 'review', 'role'):
            self.assertNotIn(field, row)


class HttpTests(Files):
    def test_real_owner_exit_and_explicit_ownership(self):
        environment = dict(os.environ, CLAUDE_PID='0', DISPATCH_OWNER_PID='0')
        base = [sys.executable, '-B', str(ROOT / 'dispatch-status/status.py'), '--ledger', str(self.ledger), '--port', '0']
        owner = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
        self.addCleanup(lambda: owner.poll() is None and owner.kill())
        server = subprocess.Popen([*base, '--exit-with', str(owner.pid)], stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True, env=environment)
        self.addCleanup(lambda: server.poll() is None and server.kill())
        self.assertIn('http://127.0.0.1:', server.stdout.readline())
        self.assertIsNone(server.poll())
        owner.terminate()
        owner.wait(timeout=3)
        output, error = server.communicate(timeout=4)
        self.assertEqual(server.returncode, 0, error)
        self.assertIn(f'process {owner.pid} ended, stopping', output)
        # Omission must not bind a Codex page to inherited Claude environment variables.
        unbound = subprocess.Popen(base, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=environment)
        try:
            self.assertIn('http://127.0.0.1:', unbound.stdout.readline())
            time.sleep(0.6)
            self.assertIsNone(unbound.poll())
        finally:
            unbound.terminate()
            unbound.communicate(timeout=3)

    def setUp(self):
        super().setUp()
        self.board([])
        self.server = Server(0, Status(self.ledger))
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={'poll_interval': 0.01})
        self.thread.start()
        self.addCleanup(self.stop)
        self.port = self.server.server_address[1]

    def stop(self):
        self.server.shutdown()
        self.thread.join(timeout=4)
        self.server.server_close()

    def request(self, head):
        with socket.create_connection(('127.0.0.1', self.port), timeout=4) as stream:
            stream.sendall(head)
            stream.shutdown(socket.SHUT_WR)
            result = b''
            try:
                while part := stream.recv(65536):
                    result += part
            except ConnectionResetError:
                pass
            return result

    def get(self, target='/', host=None):
        return self.request(f'GET {target} HTTP/1.1\r\nHost: {host or "127.0.0.1:" + str(self.port)}\r\n\r\n'.encode())

    def test_page_and_json_headers(self):
        wire = self.get()
        self.assertTrue(wire.startswith(b'HTTP/1.1 200 OK'))
        self.assertIn(b'Cache-Control: no-store', wire)
        self.assertIn(b'X-Content-Type-Options: nosniff', wire)
        self.assertEqual(wire.split(b'\r\n\r\n', 1)[1], self.server.page)
        data = json.loads(self.get('/status.json?cache=1').split(b'\r\n\r\n', 1)[1])
        self.assertEqual(data['tasks'], [])
        self.assertIn(b'404 Not Found', self.get('/absent'))

    def test_host_methods_and_duplicate_hosts(self):
        for host in ('evil.example', '127.0.0.1:1', f'127.0.0.1:{self.port}.evil'):
            self.assertIn(b'403 Forbidden', self.get(host=host))
        self.assertIn(b'403 Forbidden', self.request(b'GET / HTTP/1.1\r\n\r\n'))
        head = f'GET / HTTP/1.1\r\nHost: localhost:{self.port}\r\nHost: evil.example\r\n\r\n'.encode()
        self.assertIn(b'403 Forbidden', self.request(head))
        self.assertIn(b'405 Method Not Allowed', self.request(b'POST / HTTP/1.1\r\n\r\n'))
        self.assertIn(b'200 OK', self.get(host=f'localhost:{self.port}'))

    def test_incomplete_oversized_and_stalled_clients(self):
        self.assertEqual(self.request(b'GET / HTTP/1.1\r\n'), b'')
        self.assertEqual(self.request(b'GET / HTTP/1.1\r\nX-Large: ' + b'x' * 16384), b'')
        with socket.create_connection(('127.0.0.1', self.port), timeout=4) as stream:
            stream.sendall(b'GET / HTTP/1.1\r\n')
            self.assertEqual(stream.recv(1), b'')
        self.assertIn(b'200 OK', self.get())

    def test_byte_limit_received_in_parts(self):
        head = f'GET / HTTP/1.1\r\nHost: localhost:{self.port}\r\nX-Large: '.encode()
        head += b'x' * (16384 - len(head) - 4) + b'\r\n\r\n'
        self.assertIn(b'200 OK', self.request(head))

    def test_launcher_no_build_and_owner_exit(self):
        environment = {key: val for key, val in os.environ.items() if not key.startswith(('CLAUDE_', 'DISPATCH_'))}
        launch = [str(ROOT / 'scripts/status-page.sh'), '--ledger', str(self.ledger), '--port', '0', '--exit-with', '0']
        result = subprocess.run(launch, capture_output=True, text=True, timeout=5, env=environment, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('http://127.0.0.1:', result.stdout)
        self.assertIn('process 0 ended, stopping', result.stdout)
        for args in ([], ['--ledger', 'x', '--port', '65536'], ['--ledger', 'x', '--exit-with', '-1']):
            bad = subprocess.run([sys.executable, '-B', ROOT / 'dispatch-status/status.py', *args], capture_output=True, check=False)
            self.assertEqual(bad.returncode, 2)


class RecoveryTests(Files):
    def test_unknown_task_has_diagnostic(self):
        self.board([])
        result = subprocess.run([ROOT / 'scripts/agent-state.sh', '--ledger', self.ledger, '--task', '99'],
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn('no task 99', result.stderr)

    def git(self, *args, cwd=None):
        return subprocess.run(['git', *args], cwd=cwd or self.root, capture_output=True, text=True, check=True).stdout.strip()

    def test_recovery_on_either_host_and_preservation_of_unlanded_edits(self):
        remote = self.root / 'remote.git'
        repo = self.root / 'repo'
        wt = self.root / 'worker'
        self.git('init', '--bare', str(remote))
        self.git('clone', str(remote), str(repo))
        self.git('config', 'user.email', 'synthetic@example.invalid', cwd=repo)
        self.git('config', 'user.name', 'Synthetic', cwd=repo)
        self.git('checkout', '-b', 'main', cwd=repo)
        (repo / 'file').write_text('base')
        self.git('add', 'file', cwd=repo)
        self.git('commit', '-m', 'base', cwd=repo)
        self.git('push', 'origin', 'main', cwd=repo)
        base = self.git('rev-parse', 'HEAD', cwd=repo)
        self.git('worktree', 'add', '-b', 'task', str(wt), 'origin/main', cwd=repo)
        self.board([{'num': 1, 'agent': 'worker', 'state': 'running', 'worktree': str(wt), 'branch': 'task',
                     'base_commit': base, 'text': 'Synthetic brief', 'question': 'Synthetic question'}])
        def inspect():
            environment = dict(os.environ, HOME=str(self.root))
            result = subprocess.run([ROOT / 'scripts/agent-state.sh', '--ledger', self.ledger, '--task', '1'],
                                    capture_output=True, text=True, env=environment, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('Synthetic brief', result.stdout)
            self.assertIn('Synthetic question', result.stdout)
            return result.stdout
        self.assertIn('work: none', inspect())
        self.git('checkout', '-b', 'other', cwd=wt)
        self.assertIn('work: unlanded', inspect())
        self.git('checkout', 'task', cwd=wt)
        (wt / 'file').write_text('change')
        self.assertIn('work: unlanded', inspect())
        self.git('add', 'file', cwd=wt)
        self.git('commit', '-m', 'task', cwd=wt)
        self.assertIn('work: unlanded', inspect())
        self.git('push', 'origin', 'HEAD:main', cwd=wt)
        self.assertIn('work: landed ', inspect())
        (wt / 'untracked').write_text('keep')
        refused = subprocess.run([ROOT / 'scripts/cleanup.sh', wt], capture_output=True, check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual((wt / 'untracked').read_text(), 'keep')


if __name__ == '__main__':
    unittest.main()
