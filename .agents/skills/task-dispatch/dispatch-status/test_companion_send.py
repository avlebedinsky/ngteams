"""`companion.sh send` against a fake ngtermctl: which way a message reaches a companion."""

import json
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / 'scripts' / 'companion.sh'
SESSION = 'companion-1'
WORKING = '• Working (22s • esc to interrupt)\n› Ask Codex to do anything\n'
PROMPT = '■ Conversation interrupted\n› Ask Codex to do anything\n'
DIALOG = 'Allow command?\nPress enter to confirm or esc to cancel\n'

FAKE_NGTERMCTL = r'''#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_DIR/calls"
if [ "$1 $2" = 'session text' ]; then
  [ ! -e "$FAKE_DIR/screen_exit" ] || exit 1
  count=$(cat "$FAKE_DIR/screen_reads" 2>/dev/null || echo 0)
  count=$((count + 1))
  echo "$count" > "$FAKE_DIR/screen_reads"
  cat "$FAKE_DIR/screen$count" 2>/dev/null || cat "$FAKE_DIR/screen"
  exit 0
fi
case $1 in
  tree)
    count=$(cat "$FAKE_DIR/reads" 2>/dev/null || echo 0)
    count=$((count + 1))
    echo "$count" > "$FAKE_DIR/reads"
    cat "$FAKE_DIR/tree$count" 2>/dev/null || cat "$FAKE_DIR/tree"
    ;;
  msg)
    exit "$(cat "$FAKE_DIR/mail_exit" 2>/dev/null || echo 0)"
    ;;
esac
'''


def tree(status, pending=0, agent='claude'):
    surface = {'kind': 'left'}
    if pending:
        surface['pendingMessages'] = pending
    node = {'id': SESSION, 'surfaces': [surface]}
    if status is not None:
        node['status'] = status
    if agent is not None:
        node['foreground'] = ['/usr/bin/' + agent, '--flag']
    return json.dumps({'ok': True, 'result': {'sessions': [node]}})


class CompanionSendTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name, body in (('ngtermctl', FAKE_NGTERMCTL), ('sleep', '#!/bin/sh\n')):
            path = self.root / name
            path.write_text(body)
            path.chmod(path.stat().st_mode | stat.S_IEXEC)
        self.brief = self.root / 'brief.md'
        self.brief.write_text('steer')
        self.show(WORKING)
        self.install_hooks()

    def show(self, *screens):
        (self.root / 'screen_reads').unlink(missing_ok=True)
        for old in self.root.glob('screen[0-9]*'):
            old.unlink()
        (self.root / 'screen').write_text(screens[-1])
        for number, text in enumerate(screens, 1):
            (self.root / f'screen{number}').write_text(text)

    def install_hooks(self, claude=True, codex=True):
        settings = self.root / 'home' / '.claude' / 'settings.json'
        config = self.root / 'codex' / 'config.toml'
        for path, text, wanted in ((settings, 'ngterm agent-mail', claude), (config, 'ngterm-codex-status', codex)):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text if wanted else 'no hooks')

    def send(self, *extra, code=0):
        env = {**os.environ, 'PATH': f'{self.root}:{os.environ["PATH"]}', 'FAKE_DIR': str(self.root),
               'NGTERM_SOCKET': '', 'NGTERM_WINDOW_ID': '', 'HOME': str(self.root / 'home'),
               'CODEX_HOME': str(self.root / 'codex')}
        env.pop('CLAUDE_CONFIG_DIR', None)
        command = ['bash', str(SCRIPT), 'send', '--session', SESSION, '--file', str(self.brief), *extra]
        done = subprocess.run(command, capture_output=True, text=True, env=env, check=False)
        self.assertEqual(done.returncode, code, done.stderr)
        calls = [line for line in (self.root / 'calls').read_bytes().split(b'\n') if line]
        writes = [call for call in calls if not call.startswith((b'tree', b'session text'))]
        return done.stdout.strip(), [call.split()[0].decode() for call in writes], writes

    def set_trees(self, *trees):
        (self.root / 'calls').unlink(missing_ok=True)
        (self.root / 'reads').unlink(missing_ok=True)
        (self.root / 'screen_reads').unlink(missing_ok=True)
        for old in self.root.glob('tree*'):
            old.unlink()
        (self.root / 'tree').write_text(trees[-1])
        for number, text in enumerate(trees, 1):
            (self.root / f'tree{number}').write_text(text)

    def test_an_agent_in_mid_turn_gets_mail_at_its_primary_pane(self):
        for agent in ('claude', 'codex'):
            with self.subTest(agent):
                self.set_trees(tree('active', 1, agent))
                how, writes, calls = self.send()
                self.assertEqual((how, writes), ('mail', ['msg']))
                self.assertIn(f'Read {self.brief} and follow it. --to {SESSION}:left'.encode(), calls[0])

    def test_a_wrapped_agent_is_recognised(self):
        wrapped = json.dumps({'ok': True, 'result': {'sessions': [
            {'id': SESSION, 'status': 'active', 'foreground': ['ngswap', 'claude', '--flag'], 'surfaces': []}]}})
        self.set_trees(wrapped)
        self.assertEqual(self.send()[:2], ('mail', ['msg']))

    def test_a_dialog_holds_a_claude_mail_and_nothing_is_typed(self):
        self.set_trees(tree('blocked', 1))
        self.assertEqual(self.send()[:2], ('mail-held', ['msg']))

    def test_a_codex_reported_blocked_with_a_dialog_is_neither_mailed_nor_typed_to(self):
        self.show(DIALOG)
        self.set_trees(tree('blocked', agent='codex'))
        self.assertEqual(self.send(code=3)[:2], ('blocked', []))

    def test_a_dialog_gone_by_the_second_read_is_plain_mail(self):
        self.set_trees(tree('blocked', 1), tree('active', 1))
        self.assertEqual(self.send()[:2], ('mail', ['msg']))

    def test_an_idle_claude_is_woken_by_mail(self):
        for status in ('completed', None):
            with self.subTest(status):
                self.set_trees(tree(status), tree(status))
                self.assertEqual(self.send()[:2], ('mail', ['msg']))

    def test_an_idle_claude_taking_the_mail_late_is_not_typed_to(self):
        self.set_trees(tree('completed'), tree('completed', 1), tree('completed', 1), tree('completed'))
        self.assertEqual(self.send()[:2], ('mail', ['msg']))

    def test_an_idle_claude_leaving_the_mail_unread_is_typed_to(self):
        self.set_trees(tree('completed'), tree('completed', 1))
        self.assertEqual(self.send()[:2], ('typed', ['msg', 'session', 'session']))

    def test_an_idle_codex_is_typed_to_without_mail(self):
        self.show(PROMPT)
        for status in ('completed', None):
            with self.subTest(status):
                self.set_trees(tree(status, agent='codex'))
                self.assertEqual(self.send()[:2], ('typed', ['session', 'session']))

    def test_typing_sends_the_message_then_a_separate_enter_to_the_primary_pane(self):
        self.show(PROMPT)
        self.set_trees(tree('completed', agent='codex'))
        _, _, writes = self.send()
        self.assertEqual(writes[0], f'session type --target {SESSION} --pane left Read {self.brief} and follow it.'.encode())
        self.assertEqual(writes[1], f'session type --target {SESSION} --pane left \r'.encode())

    def test_a_codex_status_is_read_from_its_primary_pane(self):
        self.show(PROMPT)
        self.set_trees(tree('active', agent='codex'))
        self.send()
        self.assertIn(f'session text --target {SESSION} --lines 12 --pane left', (self.root / 'calls').read_text())

    def test_a_codex_dialog_behind_an_idle_status_is_never_typed_on(self):
        for screen in (DIALOG, 'Do you trust this folder?\nPress Enter to continue\n'):
            for status in ('completed', None):
                with self.subTest(status=status, screen=screen):
                    self.show(screen)
                    self.set_trees(tree(status, agent='codex'))
                    self.assertEqual(self.send(code=3)[:2], ('blocked', []))
                    self.set_trees(tree(status, agent='codex'))
                    self.assertEqual(self.send('--via', 'type', code=3)[:2], ('blocked', []))

    def test_a_quoted_interrupt_hint_is_not_a_running_turn(self):
        self.show('• Press esc to interrupt it, I said.\n› Ask Codex to do anything\n')
        self.set_trees(tree('active', agent='codex'))
        self.assertEqual(self.send()[:2], ('typed', ['session', 'session']))

    def test_an_idle_codex_with_an_unreadable_screen_is_typed_to(self):
        (self.root / 'screen_exit').write_text('')
        for status in ('completed', None):
            with self.subTest(status):
                self.set_trees(tree(status, agent='codex'))
                self.assertEqual(self.send()[:2], ('typed', ['session', 'session']))

    def test_a_codex_interrupted_after_the_mail_is_typed_to_as_well(self):
        self.show(WORKING, PROMPT)
        self.set_trees(tree('active', agent='codex'), tree('active', 1, 'codex'))
        self.assertEqual(self.send()[:2], ('typed', ['msg', 'session', 'session']))

    def test_via_type_never_mails(self):
        self.set_trees(tree('active'))
        self.assertEqual(self.send('--via', 'type')[:2], ('typed', ['session', 'session']))

    def test_nothing_is_typed_on_a_dialog(self):
        self.show(DIALOG)
        for agent in ('claude', 'codex'):
            with self.subTest(agent):
                self.set_trees(tree('blocked', agent=agent))
                self.assertEqual(self.send('--via', 'type', code=3)[:2], ('blocked', []))

    def test_a_codex_that_stopped_on_a_question_is_typed_to(self):
        self.show(PROMPT)
        self.set_trees(tree('blocked', agent='codex'))
        self.assertEqual(self.send()[:2], ('typed', ['session', 'session']))
        self.set_trees(tree('blocked', agent='codex'))
        self.assertEqual(self.send('--via', 'type')[:2], ('typed', ['session', 'session']))

    def test_an_interrupted_codex_is_typed_to(self):
        self.show(PROMPT)
        self.set_trees(tree('active', agent='codex'))
        self.assertEqual(self.send()[:2], ('typed', ['session', 'session']))

    def test_a_codex_dialog_the_status_has_not_caught_up_with_is_held(self):
        self.show(DIALOG)
        self.set_trees(tree('active', agent='codex'))
        self.assertEqual(self.send()[:2], ('mail-held', ['msg']))
        self.set_trees(tree('active', agent='codex'))
        self.assertEqual(self.send('--via', 'type', code=3)[:2], ('blocked', []))

    def test_a_codex_screen_that_cannot_be_read_keeps_the_reported_status(self):
        (self.root / 'screen_exit').write_text('')
        self.set_trees(tree('blocked', agent='codex'))
        self.assertEqual(self.send('--via', 'type', code=3)[:2], ('blocked', []))
        self.set_trees(tree('active', agent='codex'))
        self.assertEqual(self.send()[:2], ('mail', ['msg']))

    def test_rejected_mail_is_typed_instead_unless_a_dialog_is_up(self):
        (self.root / 'mail_exit').write_text('1')
        self.set_trees(tree('active'))
        self.assertEqual(self.send()[:2], ('typed', ['msg', 'session', 'session']))
        self.set_trees(tree('blocked'))
        self.assertEqual(self.send(code=3)[:2], ('blocked', ['msg']))

    def test_mail_left_unread_when_the_turn_ends_is_typed_as_well(self):
        self.show(WORKING, PROMPT)
        self.set_trees(tree('active', agent='codex'), tree('completed', 1, 'codex'))
        self.assertEqual(self.send()[:2], ('typed', ['msg', 'session', 'session']))

    def test_mail_taken_as_the_turn_ends_is_not_repeated(self):
        self.show(WORKING, PROMPT)
        self.set_trees(tree('active', agent='codex'), tree('completed', agent='codex'))
        self.assertEqual(self.send()[:2], ('mail', ['msg']))

    def test_an_agent_without_the_mail_hook_is_typed_to(self):
        for agent, hooks in (('claude', {'claude': False}), ('codex', {'codex': False})):
            with self.subTest(agent):
                self.install_hooks(**hooks)
                self.set_trees(tree('active', agent=agent))
                self.assertEqual(self.send()[:2], ('typed', ['session', 'session']))

    def test_an_agent_ngterm_cannot_name_is_typed_to(self):
        self.set_trees(tree('active', agent=None))
        self.assertEqual(self.send()[:2], ('typed', ['session', 'session']))

    def test_an_unreadable_tree_falls_back_to_typing(self):
        self.set_trees('not json')
        self.assertEqual(self.send()[:2], ('typed', ['session', 'session']))


if __name__ == '__main__':
    unittest.main()
