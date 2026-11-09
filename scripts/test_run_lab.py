#!/usr/bin/env python3
"""Offline launcher contracts, with synthetic Cargo and sandbox peers only."""

import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import unittest


RUNNER = r'''#!/usr/bin/env python3
import json, os, pathlib, signal, socket, sys
args = sys.argv[1:]
command = args.pop(0)
index = args.index('--dir')
root = pathlib.Path(args[index + 1])
del args[index:index + 2]
endpoint = root / 'control.sock'
if command != 'serve':
    with socket.socket(socket.AF_UNIX) as peer:
        peer.connect(str(endpoint))
        peer.sendall((json.dumps([command, *args]) + '\n').encode())
        print(peer.recv(65536).decode(), end='')
    sys.exit(0)
root.mkdir(parents=True, exist_ok=True)
(root / 'holder.pid').write_text(str(os.getpid()) + '\n')
(root / 'serve.json').write_text(json.dumps(args))
if os.environ.get('SYNTHETIC_START_FAIL'):
    print('synthetic startup failure', flush=True)
    sys.exit(1)
active = True
def stop(signum, *unused):
    global active
    (root / 'signal').write_text(str(signum))
    active = False
signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
with socket.socket(socket.AF_UNIX) as server:
    server.bind(str(endpoint))
    server.listen()
    server.settimeout(0.05)
    if not os.environ.get('SYNTHETIC_NO_READY'):
        print('ready sandbox=synthetic', flush=True)
    while active:
        try:
            peer, _ = server.accept()
        except TimeoutError:
            continue
        with peer:
            line = peer.makefile().readline()
            words = json.loads(line)
            with (root / 'events.jsonl').open('a') as events:
                events.write(json.dumps(words) + '\n')
            peer.sendall(b'Window ID 1\n' if words[0] == 'windows' else b'ok synthetic\n')
            if words[0] == 'stop' and not os.environ.get('SYNTHETIC_IGNORE_STOP'):
                active = False
endpoint.unlink(missing_ok=True)
'''

CARGO = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
with pathlib.Path(os.environ['SYNTHETIC_CARGO_LOG']).open('a') as log:
    log.write(json.dumps({'args': sys.argv[1:], 'lab': os.environ.get('TEAMS_LAB_BUILD'),
                          'personal': os.environ.get('TEAMS_PERSONAL_INSTALL')}) + '\n')
sys.exit(int(os.environ.get('SYNTHETIC_BUILD_FAIL', '0')))
'''


class Launcher(unittest.TestCase):
    def setUp(self):
        # Short paths also fit the real AF_UNIX length limit. Every byte here is synthetic.
        self.scratch = tempfile.TemporaryDirectory(prefix="ngl-")
        self.root = Path(self.scratch.name)
        self.script = self.root / "scripts/run-lab.sh"
        self.script.parent.mkdir()
        shutil.copyfile(Path(__file__).with_name("run-lab.sh"), self.script)
        self.script.chmod(0o700)
        self.bin = self.root / "target/lab/debug"
        self.bin.mkdir(parents=True)
        self.executable(self.bin / "ui-sandbox-run", RUNNER)
        self.executable(self.bin / "ngteams", "#!/bin/sh\nexit 99\n")
        tools = self.root / "tools"
        tools.mkdir()
        self.executable(tools / "cargo", CARGO)
        self.env = os.environ.copy()
        self.env.update(
            PATH=f"{tools}:{self.env['PATH']}",
            XDG_STATE_HOME=str(self.root / "state"),
            XDG_DATA_HOME=str(self.root / "data"),
            HOME=str(self.root / "home"),
            XDG_CONFIG_HOME=str(self.root / "config"),
            SYNTHETIC_CARGO_LOG=str(self.root / "cargo.jsonl"),
            TEAMS_PERSONAL_INSTALL="must-be-unset",
        )
        self.started = set()

    def tearDown(self):
        for alias in self.started:
            self.invoke(alias, "--stop", check=False)
        self.scratch.cleanup()

    @staticmethod
    def executable(path, content):
        path.write_text(content)
        path.chmod(0o700)

    def invoke(self, *args, check=True, extra=None):
        result = subprocess.run(
            [str(self.script), *args],
            env=self.env | (extra or {}),
            text=True,
            capture_output=True,
            timeout=30,
        )
        if check:
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        return result

    def run_dir(self, alias):
        return self.root / "state/ngteams-lab" / alias

    def start(self, alias, *args):
        self.started.add(alias)
        return self.invoke(alias, *args)

    def exclusive_available(self):
        lock = self.root / "home/.local/share/teams-lab/live.lock"
        return subprocess.run(["flock", "-xn", str(lock), "true"], check=False).returncode == 0

    def test_build_only_sets_only_lab_marker_and_never_starts_anything(self):
        self.invoke("--build-only")
        calls = [json.loads(line) for line in (self.root / "cargo.jsonl").read_text().splitlines()]
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["lab"], "1")
        self.assertIsNone(calls[0]["personal"])
        self.assertEqual(
            calls[0]["args"],
            ["build", "--locked", "-p", "ngteams", "-p", "teams-wpe", "-p", "ui-sandbox", "--bin", "ngteams",
             "--bin", "ngteams-web", "--bin", "ui-sandbox-run", "--target-dir", str(self.root / "target/lab")],
        )
        self.assertFalse((self.root / "state").exists())

    def test_two_aliases_share_the_live_lock_and_controls_remain_separate(self):
        self.start("a", "--pipewire")
        self.start("b")
        self.assertFalse(self.exclusive_available())
        duplicate = self.invoke("a", check=False)
        self.assertNotEqual(duplicate.returncode, 0)
        self.assertIn("already running", duplicate.stderr)
        for alias in ["a", "b"]:
            args = json.loads((self.run_dir(alias) / "serve.json").read_text())
            self.assertEqual("--pipewire" in args, alias == "a")
            def values(flag):
                return [args[index + 1] for index, value in enumerate(args) if value == flag]
            self.assertEqual(values("--broker"), [str(self.bin / "ngteams")])
            self.assertEqual(values("--app"), ["/usr/bin/env"])
            self.assertEqual(values("--broker-arg"), ["secrets", "--lab", alias, "--fd", "{broker-fd}"])
            self.assertEqual(values("--app-arg"), [f"XDG_DATA_HOME={self.env['XDG_DATA_HOME']}",
                                                   f"XDG_CONFIG_HOME={self.env['XDG_CONFIG_HOME']}",
                                                   str(self.bin / "ngteams"), "--lab", alias, "--secrets-fd", "{secrets-fd}",
                                                   "--trace-fd", "{trace-fd}",
                                                   # where teams_ctl::socket_path(Mode::Lab(alias)) looks for it
                                                   "--ctl-socket", str(self.run_dir(alias) / "ctl.sock")])
            self.assertIn("running\n", self.invoke(alias, "--status").stdout)
        controls = [
            ("--shot", "synthetic"), ("--screen", "synthetic_screen"), ("--key", "ctrl+k"),
            ("--type", "synthetic $(never-run) text"), ("--click", "12", "34"), ("--windows",),
        ]
        for control in controls:
            self.invoke("a", *control)
        events = [json.loads(line) for line in (self.run_dir("a") / "events.jsonl").read_text().splitlines()]
        self.assertEqual(events, [[control[0][2:], *control[1:]] for control in controls])
        self.assertFalse((self.run_dir("b") / "events.jsonl").exists())
        self.invoke("a", "--stop")
        self.assertFalse(self.exclusive_available(), "b still owns its shared lifetime lock")
        self.assertTrue((self.run_dir("a") / "holder.log").exists(), "stop keeps diagnostic state")
        self.invoke("b", "--stop")
        self.assertTrue(self.exclusive_available())
        self.assertIn("not running", self.invoke("b", "--status").stdout)

    def test_build_or_startup_failure_releases_alias_and_live_locks(self):
        result = self.invoke("a", check=False, extra={"SYNTHETIC_BUILD_FAIL": "17"})
        self.assertEqual(result.returncode, 17)
        self.assertFalse((self.run_dir("a") / "holder.pid").exists())
        result = self.invoke("a", check=False, extra={"SYNTHETIC_START_FAIL": "1"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("sandbox failed", result.stderr)
        self.assertTrue(self.exclusive_available())
        self.start("a")
        self.invoke("a", "--stop")
        self.assertTrue(self.exclusive_available())

    def test_reused_holder_pid_does_not_control_or_block_an_unrelated_process(self):
        self.start("a")
        self.invoke("a", "--stop")
        with subprocess.Popen(["sleep", "30"]) as unrelated:
            try:
                directory = self.run_dir("a")
                (directory / "holder.pid").write_text(str(unrelated.pid) + "\n")
                # Even a stale socket cannot make an unlocked alias's reused PID a live sandbox.
                endpoint = directory / "control.sock"
                with socket.socket(socket.AF_UNIX) as stale:
                    stale.bind(str(endpoint))
                self.assertIn("not running", self.invoke("a", "--status").stdout)
                self.assertIn("not running", self.invoke("a", "--stop").stdout)
                control = self.invoke("a", "--windows", check=False)
                self.assertNotEqual(control.returncode, 0)
                self.assertIn("not running", control.stderr)
                self.assertIsNone(unrelated.poll())
                endpoint.unlink()
                self.start("a")
                self.assertNotEqual(int((directory / "holder.pid").read_text()), unrelated.pid)
                self.invoke("a", "--stop")
                self.assertIsNone(unrelated.poll(), "runner must never signal the unrelated process")
            finally:
                unrelated.terminate()
                unrelated.wait(timeout=3)

    def test_an_exclusive_legacy_run_blocks_live_start_without_starting_a_holder(self):
        directory = self.root / "home/.local/share/teams-lab"
        directory.mkdir(parents=True)
        with (directory / "live.lock").open("w") as legacy:
            fcntl.flock(legacy, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.invoke("a", check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("held exclusively", result.stderr)
            self.assertFalse((self.run_dir("a") / "holder.pid").exists())
        self.start("a")
        self.invoke("a", "--stop")
        self.assertTrue(self.exclusive_available())

    def test_failed_stop_keeps_state_and_holder_lock_until_cleanup_finishes(self):
        self.started.add("a")
        self.invoke("a", extra={"SYNTHETIC_IGNORE_STOP": "1"})
        pid = int((self.run_dir("a") / "holder.pid").read_text())
        try:
            result = self.invoke("a", "--stop", check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("did not finish cleanup", result.stderr)
            self.assertTrue((self.run_dir("a") / "holder.log").exists())
            self.assertFalse(self.exclusive_available())
        finally:
            os.kill(pid, signal.SIGTERM)
            released = subprocess.run(
                ["flock", "-x", "-w", "3", str(self.root / "home/.local/share/teams-lab/live.lock"), "true"],
                check=False,
            )
            self.assertEqual(released.returncode, 0)
        self.start("a")
        self.invoke("a", "--stop")
        self.assertTrue(self.exclusive_available())

    def test_invalid_aliases_and_control_arguments_do_not_build(self):
        for args in [["../personal"], ["a", "--type"], ["a", "--click", "12"],
                     ["a", "--pipewire", "extra"], ["--build-only", "extra"],
                     ["a", "--timeout", "0"], ["a", "--timeout", "inf"], ["a", "--timeout"]]:
            self.assertNotEqual(self.invoke(*args, check=False).returncode, 0, args)
        self.assertFalse((self.root / "cargo.jsonl").exists())
        self.assertIn("not running", self.invoke("a", "--status").stdout)

    def test_alias_named_locks_and_coincident_xdg_homes_preserve_account_files(self):
        self.env["XDG_DATA_HOME"] = self.env["XDG_STATE_HOME"]
        directory = self.run_dir("locks")
        directory.mkdir(parents=True)
        account = directory / "accounts.toml"
        account.write_text('[[account]]\nupn = "synthetic@example.test"\n')
        expected = account.read_bytes()
        self.start("locks")
        self.assertNotEqual(self.invoke("locks", check=False).returncode, 0)
        self.assertEqual(account.read_bytes(), expected)
        self.invoke("locks", "--stop")
        self.assertEqual(account.read_bytes(), expected)
        self.start("locks")
        self.assertEqual(account.read_bytes(), expected)

    def test_live_deadline_requests_holder_cleanup_and_releases_locks_after_exit(self):
        self.start("a", "--timeout", "1")
        self.assertFalse(self.exclusive_available())
        result = subprocess.run(
            ["flock", "-x", "-w", "4", str(self.root / "home/.local/share/teams-lab/live.lock"), "true"],
            check=False,
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual((self.run_dir("a") / "signal").read_text(), str(signal.SIGTERM))
        self.assertIn("not running", self.invoke("a", "--status").stdout)

    def test_interrupted_startup_stops_its_own_holder_and_keeps_state(self):
        self.started.add("a")
        process = subprocess.Popen(
            [str(self.script), "a"], env=self.env | {"SYNTHETIC_NO_READY": "1"},
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        try:
            for _ in range(200):
                if (self.run_dir("a") / "control.sock").exists():
                    break
                threading.Event().wait(0.02)
            self.assertTrue((self.run_dir("a") / "control.sock").exists())
            process.terminate()
            _, error = process.communicate(timeout=30)
            self.assertEqual(process.returncode, 130, error)
            self.assertIn("interrupted startup; holder stopped", error)
            self.assertTrue((self.run_dir("a") / "holder.log").exists())
            self.assertTrue(self.exclusive_available())
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=3)


class ReadOnlyLauncher(unittest.TestCase):
    """scripts/run-personal-read-only.sh over a synthetic installed binary and sandbox runner."""

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="ngr-")
        self.root = Path(self.scratch.name)
        self.script = self.root / "scripts/run-personal-read-only.sh"
        self.script.parent.mkdir()
        shutil.copyfile(Path(__file__).with_name("run-personal-read-only.sh"), self.script)
        self.script.chmod(0o700)
        bin_dir = self.root / "target/lab/debug"
        bin_dir.mkdir(parents=True)
        Launcher.executable(bin_dir / "ui-sandbox-run", RUNNER)
        tools = self.root / "tools"
        tools.mkdir()
        Launcher.executable(tools / "cargo", CARGO)
        self.env = os.environ.copy()
        self.env.update(
            PATH=f"{tools}:{self.env['PATH']}",
            XDG_STATE_HOME=str(self.root / "state"),
            HOME=str(self.root / "home"),
            SYNTHETIC_CARGO_LOG=str(self.root / "cargo.jsonl"),
        )
        self.run_dir = self.root / "state/ngteams-read-only/run"

    def tearDown(self):
        self.invoke("--stop", check=False)
        self.scratch.cleanup()

    def installed(self, flags):
        # the strings the runner probes for, as a build's own flags would carry them
        teams = self.root / "installed/ngteams"
        teams.parent.mkdir(exist_ok=True)
        Launcher.executable(teams, f'#!/bin/sh\n# {flags}\n[ "$1" = snapshot ] && mkdir -p "$3" && exit 0\nexit 99\n')
        return teams

    def invoke(self, *args, check=True, teams=None):
        result = subprocess.run(
            [str(self.script), *args],
            env=self.env | ({"TEAMS_BIN": str(teams)} if teams else {}),
            text=True,
            capture_output=True,
            timeout=30,
        )
        if check:
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        return result

    def app_args(self):
        args = json.loads((self.run_dir / "serve.json").read_text())
        return [args[index + 1] for index, value in enumerate(args) if value == "--app-arg"]

    def test_the_app_is_told_its_control_socket_in_the_run_dir(self):
        teams = self.installed("TEAMS_READ_ONLY ctl-socket")
        started = self.invoke("synthetic@example.test", teams=teams)
        self.assertIn(f"ctl socket:   {self.run_dir / 'ctl.sock'}", started.stdout)
        args = self.app_args()
        # where teams_ctl::socket_path(Mode::ReadOnly) looks for it
        self.assertEqual(args[-2:], ["--ctl-socket", str(self.run_dir / "ctl.sock")])
        self.assertEqual(args[:2], ["--read-only", "--account"])
        self.invoke("--stop")
        self.assertFalse(self.run_dir.exists())

    def test_an_installed_build_without_the_socket_is_given_none(self):
        teams = self.installed("TEAMS_READ_ONLY")
        started = self.invoke("synthetic@example.test", teams=teams)
        self.assertIn("serves no control socket", started.stderr)
        self.assertNotIn("--ctl-socket", self.app_args())


if __name__ == "__main__":
    unittest.main()
