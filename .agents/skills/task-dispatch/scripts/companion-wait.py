#!/usr/bin/env python3
"""Block until a companion of the board needs the dispatcher, then print why.

Reads the ledger (never writes it) and ngterm's session tree of each companion's window. It watches every task whose
state is `running` or `waiting on user` and that has a `session` (the user may answer a waiting companion in its own
pane). One JSON line is printed per event:

  {"task": 3, "event": "report", "report": "...", "mtime": 1794..., "status_changed_at": 1794...}
        its report file is new and its turn has ended
  {"task": 3, "event": "completed"|"blocked", "status_changed_at": 1794...}
        it stopped without a new report, or a dialog or question holds it
  {"task": 3, "event": "exited"}   ngterm sees another program or an idle shell in its pane, not the agent (a Codex
                                   companion after an ngterm relaunch); a pane it can't read reports no `exited`
  {"task": 3, "event": "asleep"}   ngterm can't see any program in its pane: a restored pane that hasn't been shown
                                   since an ngterm relaunch spawns nothing until something wakes it
  {"task": 3, "event": "gone"}     its session is no longer open in its window

A report counts once its mtime is newer than the task's `report_seen`; a status once its change time is newer than
the task's `status_seen`. The dispatcher records both when it handles an event, so a handled one doesn't fire again.
`exited`, `asleep` and `gone` fire only when two reads in a row agree, and a window whose tree can't be read reports
nothing for its tasks: a failed or partial read is never taken for a closed session.

Exit codes: 0 events printed (with --once, also none); 124 --timeout passed with none; 2 bad arguments or an
unreadable ledger.
"""

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

WATCHED = ('running', 'waiting on user')
AGENTS = ('claude', 'codex')
CONFIRM = ('gone', 'exited', 'asleep')


def window_sessions(window):
    """The open sessions of one window by id, or None when ngterm can't be read or refuses."""
    command = ['ngtermctl', 'tree', '--json']
    if window:
        command += ['--window', window]
    if os.environ.get('NGTERM_SOCKET'):
        command += ['--socket', os.environ['NGTERM_SOCKET']]
    try:
        done = subprocess.run(command, capture_output=True, text=True, timeout=20, check=True)
        tree = json.loads(done.stdout)
    except (OSError, subprocess.SubprocessError, ValueError):
        return None
    return parse_sessions(tree)


def parse_sessions(tree):
    """Every open session in a `tree --json` reply by id, companions included; None for a refused reply."""
    if not isinstance(tree, dict) or not tree.get('ok'):
        return None
    found = {}
    stack = [tree.get('result')]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            for session in node.get('sessions') or []:
                if isinstance(session, dict) and isinstance(session.get('id'), str):
                    found[session['id']] = session
            stack.extend(node.values())
        elif isinstance(node, list):
            stack.extend(node)
    return found


def number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) else 0


def agent_running(session):
    """True when Claude or Codex holds the pane (directly or under a wrapper such as `ngswap claude ...`), False when
    ngterm positively sees another program or an idle shell there, None when it can't read the pane: an unshown pane
    after a relaunch, or a failed live-daemon listing, has neither field."""
    foreground = session.get('foreground')
    if isinstance(foreground, list) and foreground:
        return any(isinstance(word, str) and os.path.basename(word) in AGENTS for word in foreground[:2])
    shell = session.get('foregroundShell')
    return False if isinstance(shell, str) and shell else None


def watched(board):
    tasks = board.get('tasks') if isinstance(board, dict) else None
    return [task for task in tasks or [] if isinstance(task, dict) and task.get('state') in WATCHED
            and isinstance(task.get('session'), str) and task['session']]


def window_of(task, default_window):
    return task['window'] if isinstance(task.get('window'), str) and task['window'] else default_window


def events(board, windows, default_window=None):
    """Events for the watched tasks. `windows` maps a window id (or None) to its sessions, or None when unread."""
    found = []
    for task in watched(board):
        num = task.get('num')
        open_sessions = windows.get(window_of(task, default_window))
        if open_sessions is None:
            continue
        session = open_sessions.get(task['session'])
        if session is None:
            found.append({'task': num, 'event': 'gone'})
            continue
        running = agent_running(session)
        if running is False:
            found.append({'task': num, 'event': 'exited'})
            continue
        status, changed = session.get('status'), number(session.get('statusChangedAt'))
        report = task.get('report')
        mtime = None
        if isinstance(report, str) and report:
            try:
                mtime = Path(report).stat().st_mtime
            except OSError:
                mtime = None
        if mtime is not None and mtime > number(task.get('report_seen')) and status != 'active':
            found.append({'task': num, 'event': 'report', 'report': report, 'mtime': mtime,
                          'status_changed_at': changed})
        elif status in ('completed', 'blocked') and changed > number(task.get('status_seen')):
            found.append({'task': num, 'event': status, 'status_changed_at': changed})
        elif running is None:
            found.append({'task': num, 'event': 'asleep'})
    return found


def confirmed(found, suspects):
    """Keep `gone`/`exited`/`asleep` only when the previous read saw the same; return them and this read's suspects."""
    seen = {(event['task'], event['event']) for event in found if event['event'] in CONFIRM}
    kept = [event for event in found if event['event'] not in CONFIRM or (event['task'], event['event']) in suspects]
    return kept, seen


def read(board, default_window):
    return {window: window_sessions(window) for window in {window_of(task, default_window) for task in watched(board)}}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--ledger', type=Path, required=True)
    parser.add_argument('--timeout', type=float, default=0, help='seconds before exiting 124; 0 waits forever')
    parser.add_argument('--interval', type=float, default=10)
    parser.add_argument('--once', action='store_true', help='check now (a second read confirms a closure)')
    args = parser.parse_args()
    default_window = os.environ.get('NGTERM_WINDOW_ID') or None
    deadline = time.monotonic() + args.timeout if args.timeout > 0 else None
    suspects, reads = set(), 0
    while True:
        try:
            board = json.loads(args.ledger.read_text(encoding='utf-8'))
        except (OSError, ValueError) as error:
            print(f'companion-wait: cannot read the ledger ({type(error).__name__})', file=sys.stderr)
            return 2
        found, suspects = confirmed(events(board, read(board, default_window), default_window), suspects)
        reads += 1
        if found or (args.once and (not suspects or reads >= 2)):
            for event in found:
                print(json.dumps(event))
            return 0
        if deadline is not None and time.monotonic() >= deadline:
            return 124
        pause = min(args.interval, 2) if args.once else args.interval
        time.sleep(pause if deadline is None else max(0, min(pause, deadline - time.monotonic())))


if __name__ == '__main__':
    sys.exit(main())
