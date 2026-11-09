"""land.sh's review gate against synthetic Git repositories; cargo is a stub, nothing leaves the temp directory."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

LAND = Path(__file__).resolve().parents[1] / 'scripts/land.sh'


class LandGate(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='dispatch-land-')
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        self.remote, self.main, self.wt = root / 'remote.git', root / 'main', root / 'task'
        stubs = root / 'bin'
        stubs.mkdir()
        (stubs / 'cargo').write_text('#!/bin/sh\nexit 0\n')
        (stubs / 'cargo').chmod(0o755)
        self.env = dict(os.environ, PATH=f'{stubs}:{os.environ["PATH"]}', GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_CONFIG_SYSTEM=os.devnull, GIT_AUTHOR_NAME='Synthetic', GIT_COMMITTER_NAME='Synthetic',
                        GIT_AUTHOR_EMAIL='synthetic@example.invalid', GIT_COMMITTER_EMAIL='synthetic@example.invalid')
        self.git('init', '--bare', '-b', 'main', self.remote, cwd=root)
        self.git('clone', self.remote, self.main, cwd=root)
        self.git('checkout', '-B', 'main', cwd=self.main)
        self.commit(self.main, 'shared', ''.join(f'line {n}\n' for n in range(40)))
        self.commit(self.main, 'other', 'base\n')
        self.git('push', 'origin', 'main', cwd=self.main)
        self.git('worktree', 'add', '-b', 'task', self.wt, 'origin/main', cwd=self.main)

    def git(self, *args, cwd):
        done = subprocess.run(['git', *map(str, args)], cwd=cwd, env=self.env, capture_output=True, text=True, check=True)
        return done.stdout.strip()

    def commit(self, tree, name, text):
        (tree / name).write_text(text)
        self.git('add', name, cwd=tree)
        self.git('commit', '-m', f'change {name}', cwd=tree)
        return self.git('rev-parse', 'HEAD', cwd=tree)

    def land(self, *args):
        return subprocess.run([LAND, *args], cwd=self.wt, env=self.env, capture_output=True, text=True, check=False)

    def origin_head(self):
        return self.git('rev-parse', 'main', cwd=self.remote)

    def move_main(self, name, text):
        self.commit(self.main, name, text)
        self.git('push', 'origin', 'main', cwd=self.main)

    def edit_shared(self, tree, old, new):
        text = (tree / 'shared').read_text().replace(old, new)
        return self.commit(tree, 'shared', text)

    def test_refuses_without_a_review_decision(self):
        head = self.commit(self.wt, 'feature', 'new\n')
        before = self.origin_head()
        for args in ([], ['--reviewed'], ['--bogus'], ['--reviewed', 'no-such-commit'], ['--no-review', head]):
            self.assertEqual(self.land(*args).returncode, 1, args)
        self.assertEqual(self.origin_head(), before)

    def test_no_review_lands(self):
        self.commit(self.wt, 'notes', 'text\n')
        result = self.land('--no-review')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('LANDED ', result.stdout)
        self.assertEqual(self.origin_head(), self.git('rev-parse', 'HEAD', cwd=self.wt))

    def test_a_clean_rebase_keeping_the_reviewed_changes_lands(self):
        head = self.edit_shared(self.wt, 'line 30\n', 'reviewed 30\n')
        self.move_main('other', 'moved\n')
        self.edit_shared(self.main, 'line 2\n', 'main 2\n')
        self.git('push', 'origin', 'main', cwd=self.main)
        result = self.land('--reviewed', head)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.origin_head(), self.git('rev-parse', 'HEAD', cwd=self.wt))

    def test_commits_after_the_review_stop_it(self):
        head = self.commit(self.wt, 'feature', 'reviewed\n')
        self.commit(self.wt, 'feature', 'not reviewed\n')
        before = self.origin_head()
        result = self.land('--reviewed', head)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.origin_head(), before)

    def test_a_rebase_that_alters_the_reviewed_changes_stops_it(self):
        # Main already made one of the branch's two changes, so the rebase drops that commit without a conflict.
        self.commit(self.wt, 'other', 'base\nsame\n')
        head = self.edit_shared(self.wt, 'line 30\n', 'reviewed 30\n')
        before_main = self.edit_shared(self.main, 'line 30\n', 'reviewed 30\n')
        self.git('push', 'origin', 'main', cwd=self.main)
        result = self.land('--reviewed', head)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.origin_head(), before_main)

    def test_a_retry_after_a_rebase_still_matches_the_review(self):
        head = self.edit_shared(self.wt, 'line 30\n', 'reviewed 30\n')
        self.move_main('other', 'moved\n')
        self.git('rebase', 'origin/main', cwd=self.wt)
        self.assertNotEqual(self.git('rev-parse', 'HEAD', cwd=self.wt), head)
        result = self.land('--reviewed', head)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
