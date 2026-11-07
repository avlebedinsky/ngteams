import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / 'scripts' / 'companion-wait.py'
spec = importlib.util.spec_from_file_location('companion_wait', SCRIPT)
wait = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wait)


def tree(*sessions):
    return {'ok': True, 'result': {'tree': {'workspaces': [{'id': 'W', 'sessions': list(sessions)}]}}}


def agent(status=None, changed=0, program='claude'):
    return {'status': status, 'statusChangedAt': changed, 'foreground': [program, '--flag']}


class CompanionWaitTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.report = self.root / 'report.md'

    def tearDown(self):
        self.tmp.cleanup()

    def task(self, **fields):
        return {'num': 1, 'state': 'running', 'session': 'S1', 'report': str(self.report), **fields}

    def test_sessions_include_new_ones_without_status_and_refusals_are_unknown(self):
        found = wait.parse_sessions(tree({'id': 'S1'}, {'id': 'S2', 'status': 'active', 'surfaces': [{'id': 'x'}]}))
        self.assertEqual(sorted(found), ['S1', 'S2'])
        self.assertIsNone(wait.parse_sessions({'ok': False}))
        self.assertIsNone(wait.parse_sessions([]))

    def test_report_waits_for_the_turn_to_end_then_fires_once(self):
        self.report.write_text('done')
        mtime = self.report.stat().st_mtime
        board = {'tasks': [self.task()]}
        self.assertEqual(wait.events(board, {None: {'S1': agent('active', 5)}}), [])
        found = wait.events(board, {None: {'S1': agent('completed', 6, '/usr/bin/codex')}})
        self.assertEqual([(e['event'], e['mtime'], e['status_changed_at']) for e in found], [('report', mtime, 6)])
        seen = {'tasks': [self.task(report_seen=mtime, status_seen=6)]}
        self.assertEqual(wait.events(seen, {None: {'S1': agent('completed', 6)}}), [])

    def test_stops_without_a_report_closed_sessions_and_panes_without_an_agent(self):
        board = {'tasks': [self.task(status_seen=3), self.task(num=2, session='S2'), self.task(num=3, session='S3'),
                           self.task(num=4, session='S4'), self.task(num=5, session='S5')]}
        found = wait.events(board, {None: {'S1': agent('completed', 3), 'S2': agent('blocked', 4),
                                           'S4': agent('completed', 9, 'fish'),
                                           'S5': {'status': None, 'foreground': None, 'foregroundShell': 'fish'}}})
        self.assertEqual([(e['task'], e['event']) for e in found],
                         [(2, 'blocked'), (3, 'gone'), (4, 'exited'), (5, 'exited')])

    def test_a_pane_ngterm_cannot_read_is_asleep_never_exited(self):
        board = {'tasks': [self.task(status_seen=3), self.task(num=2, session='S2')]}
        unread = {'S1': {'status': 'completed', 'statusChangedAt': 3},
                  'S2': {'status': 'active', 'statusChangedAt': 5, 'foreground': [], 'foregroundShell': ''}}
        self.assertEqual(wait.events(board, {None: unread}),
                         [{'task': 1, 'event': 'asleep'}, {'task': 2, 'event': 'asleep'}])
        unread['S1']['statusChangedAt'] = 4
        self.assertEqual(wait.events(board, {None: unread})[0],
                         {'task': 1, 'event': 'completed', 'status_changed_at': 4})
        self.report.write_text('done before the relaunch')
        self.assertEqual(wait.events({'tasks': [self.task(status_seen=4)]}, {None: unread})[0]['event'], 'report')

    def test_waiting_tasks_are_watched_and_other_states_are_not(self):
        board = {'tasks': [self.task(state='waiting on user', status_seen=3), self.task(num=2, session=None), None,
                           {'num': 4, 'state': 'queued'}, self.task(num=5, state='landed abc', session='S5')]}
        self.assertEqual(wait.events(board, {None: {'S1': agent('completed', 3)}}), [])
        found = wait.events(board, {None: {'S1': agent('completed', 8)}})
        self.assertEqual([(e['task'], e['event']) for e in found], [(1, 'completed')])

    def test_each_task_reads_its_own_window_and_an_unread_window_reports_nothing(self):
        board = {'tasks': [self.task(window='W1'), self.task(num=2, session='S2')]}
        self.assertEqual(wait.events(board, {'W1': None, 'W0': {}}, 'W0'), [{'task': 2, 'event': 'gone'}])
        self.assertEqual(wait.events(board, {'W1': {}, 'W0': None}, 'W0'), [{'task': 1, 'event': 'gone'}])

    def test_closures_need_two_reads_in_a_row(self):
        gone, done = {'task': 1, 'event': 'gone'}, {'task': 2, 'event': 'completed'}
        kept, suspects = wait.confirmed([gone, done], set())
        self.assertEqual(kept, [done])
        kept, suspects = wait.confirmed([gone], suspects)
        self.assertEqual(kept, [gone])
        kept, suspects = wait.confirmed([], suspects)
        self.assertEqual((kept, suspects), ([], set()))

    def run_waiter(self, board, *extra):
        ledger = self.root / 'progress.json'
        ledger.write_text(board if isinstance(board, str) else json.dumps(board))
        env = {**os.environ, 'PATH': str(self.root)}
        env.pop('NGTERM_SOCKET', None)
        return subprocess.run([sys.executable, '-B', str(SCRIPT), '--ledger', str(ledger), *extra],
                              env=env, capture_output=True, text=True)

    def test_unreachable_ngterm_never_reports_a_running_companion_gone(self):
        board = {'tasks': [self.task()]}
        once = self.run_waiter(board, '--once')
        self.assertEqual((once.returncode, once.stdout), (0, ''))
        timed = self.run_waiter(board, '--timeout', '0.3', '--interval', '0.1')
        self.assertEqual((timed.returncode, timed.stdout), (124, ''))

    def test_fake_ngterm_closure_confirmed_on_the_second_read(self):
        fake = self.root / 'ngtermctl'
        fake.write_text('#!/bin/sh\necho \'{"ok": true, "result": {"tree": {"workspaces": []}}}\'\n')
        fake.chmod(0o755)
        result = self.run_waiter({'tasks': [self.task()]}, '--once')
        self.assertEqual(result.returncode, 0)
        self.assertEqual([json.loads(line) for line in result.stdout.splitlines()], [{'task': 1, 'event': 'gone'}])

    def test_unreadable_ledger(self):
        self.assertEqual(self.run_waiter('{', '--once').returncode, 2)


if __name__ == '__main__':
    unittest.main()
