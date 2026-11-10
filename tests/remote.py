#!/usr/bin/env python3
"""Offline remote provisioning checks. Run after cargo build --release."""
from concurrent.futures import ThreadPoolExecutor
import json
import os
import pty
import select
import signal
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

import installer

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/release/tmux-agents"


class RemoteSetup(unittest.TestCase):
    def setUp(self):
        self.fixture = installer.Installer()
        self.fixture.setUp()
        self.local = tempfile.TemporaryDirectory(prefix="ta-remote-", dir="/tmp")
        self.state = Path(self.local.name) / "state"
        self.remote = self.fixture.root / "remote home"
        self.remote.mkdir(mode=0o700)
        shutil.copyfile(ROOT / "scripts/machine-add.sh", self.fixture.root / "scripts/machine-add.sh")
        self.status = {"protocol": 1, "instance": "0123456789abcdef0123456789abcdef", "pid": 42, "capabilities": ["workspace.list", "workspace.create", "workspace.resolve"]}
        payload = b'''#!/bin/sh
set -eu
case "${1:-}" in
    ensure) printf 'ensure\\n' >> "${TEST_ROOT}/supervisor-calls"; exit 0 ;;
    request) printf '%s\\n' "${TEST_STATUS_JSON}" ;;
    *) exit 2 ;;
esac
'''
        (self.fixture.root / "payload").write_bytes(payload)
        self.env = dict(self.fixture.env, TMUX_AGENTS_BIN=str(BINARY), TMUX_AGENTS_HOME=str(self.state), TEST_REMOTE_HOME=str(self.remote), TEST_STATUS_JSON=json.dumps(self.status), TEST_OS="Linux", TEST_ARCH="x86_64", EXPECTED_TARGET="x86_64-unknown-linux-musl")
        self.fixture.make_tool("tmux", """
import os, sys
if sys.argv[1:] == ['-V']: print('tmux ' + os.environ.get('TEST_TMUX_VERSION', '3.6'))
""")
        self.fixture.make_tool("ssh", """
import os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['TEST_ROOT'])
script = sys.stdin.buffer.read()
is_probe = script.startswith(b"mode='probe'\\n")
with (root / 'ssh-phases').open('a') as log:
    log.write(('probe' if is_probe else 'setup') + '\\n')
with (root / 'ssh-calls').open('a') as log:
    log.write(sys.argv[sys.argv.index('--') + 1] + '\\n')
assert '-T' in sys.argv and sys.argv[-1] == 'sh -s'
assert not any('StrictHostKeyChecking=no' in arg for arg in sys.argv)
if os.environ.get('TEST_SSH_FAIL'): sys.exit(255)
if is_probe and 'TEST_PROBE_REPLY' in os.environ:
    print(os.environ['TEST_PROBE_REPLY'])
    sys.exit(0)
environment = dict(os.environ, HOME=os.environ['TEST_REMOTE_HOME'])
for name in ['TMUX_AGENTS_BIN', 'TMUX_AGENTS_HOME']: environment.pop(name, None)
result = subprocess.run(['sh', '-s'], input=script, env=environment)
if is_probe and os.environ.get('TEST_REMOVE_AFTER_PROBE'):
    (pathlib.Path(os.environ['TEST_REMOTE_HOME']) / '.local/share/tmux-agents/bin/pre-0.1/x86_64-unknown-linux-musl/tmux-agents').unlink()
sys.exit(255 if not is_probe and os.environ.get('TEST_SSH_FAIL_AFTER_STATUS') else result.returncode)
""")

    def tearDown(self):
        self.local.cleanup()
        self.fixture.tearDown()

    def add(self, destination="user@server", check=True, answer="y\n", **environment):
        return subprocess.run(["sh", self.fixture.root / "scripts/machine-add.sh", destination], input=answer, env=dict(self.env, **environment), capture_output=True, text=True, timeout=15, check=check)

    def registry(self):
        return json.loads((self.state / "machines.json").read_text())

    def remote_binary(self):
        return self.remote / ".local/share/tmux-agents/bin/pre-0.1/x86_64-unknown-linux-musl/tmux-agents"

    def test_installs_verifies_and_registers_idempotently(self):
        result = self.add()
        self.assertIn("Registered user@server", result.stdout)
        self.assertIn("[y/N]", result.stderr)
        for stage in range(1, 6):
            self.assertIn(f"[{stage}/5]", result.stderr)
        self.assertIn("Verifying SHA-256", result.stderr)
        self.assertTrue(all("--progress-bar" in call for call in self.fixture.downloads()))
        self.assertTrue(self.remote_binary().is_file())
        self.assertEqual(self.remote_binary().stat().st_mode & 0o777, 0o700)
        self.assertEqual(len(self.fixture.downloads()), 2)
        repeated = self.add(answer="")
        self.assertNotIn("[y/N]", repeated.stderr)
        self.assertIn("already installed; continuing", repeated.stderr)
        self.assertIn("Registered user@server", repeated.stdout)
        self.assertIn("no download needed", repeated.stderr)
        self.assertEqual(len(self.fixture.downloads()), 2)
        self.assertEqual(self.registry(), {"version": 1, "machines": [{"destination": "user@server", "release_tag": "pre-0.1"}]})
        listed = subprocess.run([BINARY, "machine", "list"], env=self.env, capture_output=True, text=True, check=True)
        self.assertEqual(listed.stdout, "user@server\tpre-0.1\n")
        self.assertEqual((self.state / "machines.json").stat().st_mode & 0o777, 0o600)

    def test_installed_binary_needs_no_prompt_or_installer_files(self):
        binary = self.remote_binary()
        binary.parent.mkdir(parents=True)
        (self.remote / ".local/share/tmux-agents").chmod(0o700)
        binary.write_bytes((self.fixture.root / "payload").read_bytes())
        binary.chmod(0o700)
        result = self.add(answer="")
        self.assertNotIn("[y/N]", result.stderr)
        self.assertIn("already installed; continuing", result.stderr)
        self.assertIn("Registered user@server", result.stdout)
        self.assertFalse((self.remote / ".local/share/tmux-agents/scripts").exists())
        self.assertEqual(self.fixture.downloads(), [])

    def test_probe_matches_all_supported_remote_platforms(self):
        for system, architecture, target in [("Darwin", "x86_64", "x86_64-apple-darwin"), ("Darwin", "arm64", "aarch64-apple-darwin"), ("Linux", "aarch64", "aarch64-unknown-linux-musl")]:
            with self.subTest(target=target):
                environment = dict(TEST_OS=system, TEST_ARCH=architecture, EXPECTED_TARGET=target)
                self.add(**environment)
                binary = self.remote / ".local/share/tmux-agents/bin/pre-0.1" / target / "tmux-agents"
                self.assertTrue(binary.is_file())
                repeated = self.add(answer="", **environment)
                self.assertNotIn("[y/N]", repeated.stderr)
                self.assertIn("already installed; continuing", repeated.stderr)
        self.assertEqual(len(self.fixture.downloads()), 6)

    def test_other_architecture_does_not_count_as_installed(self):
        binary = self.remote / ".local/share/tmux-agents/bin/pre-0.1/aarch64-unknown-linux-musl/tmux-agents"
        binary.parent.mkdir(parents=True)
        (self.remote / ".local/share/tmux-agents").chmod(0o700)
        binary.write_bytes((self.fixture.root / "payload").read_bytes())
        binary.chmod(0o700)
        result = self.add(answer="n\n")
        self.assertIn("is not installed", result.stderr)
        self.assertIn("Install it now? [y/N]", result.stderr)
        self.assertFalse((self.fixture.root / "supervisor-calls").exists())
        self.assertEqual(self.fixture.downloads(), [])

    def test_invalid_probe_reply_does_not_prompt_or_install(self):
        for reply in ["unknown", "installed\nextra", "", "missing\ninstalled"]:
            with self.subTest(reply=reply):
                result = self.add(TEST_PROBE_REPLY=reply, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("[y/N]", result.stderr)
                self.assertFalse((self.state / "machines.json").exists())
                self.assertFalse((self.remote / ".local/share/tmux-agents").exists())
                self.assertEqual(self.fixture.downloads(), [])

    def test_disappearing_binary_does_not_install_without_approval(self):
        self.add()
        saved = (self.state / "machines.json").read_bytes()
        result = self.add(answer="", TEST_REMOVE_AFTER_PROBE="1", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("[y/N]", result.stderr)
        self.assertIn("now missing", result.stderr)
        self.assertEqual((self.state / "machines.json").read_bytes(), saved)
        self.assertEqual(len(self.fixture.downloads()), 2)
        self.assertEqual((self.fixture.root / "supervisor-calls").read_text().splitlines(), ["ensure"])

    def test_confirmation_defaults_to_no_without_side_effects(self):
        for answer in ["n\n", "\n", "", "not sure\n"]:
            with self.subTest(answer=answer):
                result = self.add(answer=answer)
                self.assertIn("Setup cancelled", result.stderr)
                self.assertFalse((self.state / "machines.json").exists())
                self.assertFalse((self.fixture.root / "supervisor-calls").exists())
                self.assertFalse((self.remote / ".local/share/tmux-agents").exists())
                self.assertEqual(self.fixture.downloads(), [])
        self.assertEqual((self.fixture.root / "ssh-phases").read_text().splitlines(), ["probe"] * 4)

    def test_confirmation_accepts_case_insensitive_yes(self):
        self.add(answer="YeS\n")
        self.assertEqual(self.registry()["machines"][0]["destination"], "user@server")

    def test_terminal_confirmation_and_success_pause(self):
        pid, terminal = pty.fork()
        if pid == 0:
            os.execvpe("sh", ["sh", str(self.fixture.root / "scripts/machine-add.sh"), "user@server"], self.env)
        output = b""
        def wait_for(text):
            nonlocal output
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if select.select([terminal], [], [], 0.1)[0]:
                    output += os.read(terminal, 65536)
                if text in output:
                    return
            self.fail("terminal prompt timed out: " + repr(output[-1000:]))
        try:
            wait_for(b"[y/N]")
            self.assertEqual((self.fixture.root / "ssh-phases").read_text().splitlines(), ["probe"])
            self.assertFalse((self.fixture.root / "supervisor-calls").exists())
            os.write(terminal, b"yes\n")
            wait_for(b"Press Enter to close")
            self.assertEqual(os.waitpid(pid, os.WNOHANG), (0, 0))
            self.assertEqual(self.registry()["machines"][0]["destination"], "user@server")
            os.write(terminal, b"\n")
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if select.select([terminal], [], [], 0.05)[0]:
                    try:
                        output += os.read(terminal, 65536)
                    except OSError:
                        pass
                child, status = os.waitpid(pid, os.WNOHANG)
                if child:
                    pid = None
                    self.assertEqual(os.waitstatus_to_exitcode(status), 0)
                    break
                time.sleep(0.05)
            else:
                self.fail("setup did not exit after Enter")
        finally:
            os.close(terminal)
            if pid is not None:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                os.waitpid(pid, 0)

    def test_concurrent_registrations_do_not_lose_machines(self):
        def register(index):
            subprocess.run([BINARY, "machine", "register", f"host-{index}", "pre-0.1"], input=json.dumps(self.status), env=self.env, capture_output=True, text=True, check=True)
        with ThreadPoolExecutor(max_workers=4) as executor:
            list(executor.map(register, range(4)))
        self.assertEqual({machine["destination"] for machine in self.registry()["machines"]}, {f"host-{index}" for index in range(4)})

    def test_remote_status_capture_is_bounded(self):
        result = self.add(TEST_STATUS_JSON=" " * 70000, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.state / "machines.json").exists())

    def test_rejects_ssh_options_and_shell_characters(self):
        for destination in ["-oProxyCommand=evil", "host\nother", "host;touch", "$(id)", "host name"]:
            with self.subTest(destination=destination):
                self.assertNotEqual(self.add(destination, check=False).returncode, 0)
        self.assertFalse((self.fixture.root / "ssh-calls").exists())
        self.assertFalse((self.state / "machines.json").exists())

    def test_ssh_failure_does_not_register(self):
        result = self.add(TEST_SSH_FAIL="1", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("[y/N]", result.stderr)
        self.assertFalse((self.state / "machines.json").exists())
        self.assertEqual(self.fixture.downloads(), [])

    def test_ssh_failure_after_valid_status_does_not_register(self):
        result = self.add(TEST_SSH_FAIL_AFTER_STATUS="1", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(self.remote_binary().exists())
        self.assertFalse((self.state / "machines.json").exists())

    def test_checksum_failure_does_not_register(self):
        result = self.add(CHECKSUM_MODE="mismatch", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.remote_binary().exists())
        self.assertFalse((self.state / "machines.json").exists())

    def test_incompatible_supervisor_does_not_register(self):
        result = self.add(TEST_STATUS_JSON=json.dumps(dict(self.status, protocol=2)), check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("incompatible", result.stderr)
        self.assertFalse((self.state / "machines.json").exists())

    def test_remote_tmux_minimum_checked_before_installation(self):
        result = self.add(TEST_TMUX_VERSION="3.1", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.fixture.downloads(), [])
        self.assertFalse((self.state / "machines.json").exists())

    def test_refuses_symlinked_or_public_remote_installation_root(self):
        parent = self.remote / ".local/share"
        parent.mkdir(parents=True)
        outside = self.fixture.root / "outside"
        outside.mkdir()
        target = parent / "tmux-agents"
        target.symlink_to(outside)
        self.assertNotEqual(self.add(check=False).returncode, 0)
        self.assertEqual(list(outside.iterdir()), [])
        target.unlink()
        target.mkdir()
        target.chmod(0o777)
        self.assertNotEqual(self.add(check=False).returncode, 0)
        self.assertEqual(target.stat().st_mode & 0o777, 0o777)
        self.assertEqual(self.fixture.downloads(), [])

    def test_preserves_unreadable_or_incompatible_registry(self):
        self.state.mkdir(mode=0o700)
        saved = self.state / "machines.json"
        for data in [b"not json", b'{"version":99,"machines":[]}']:
            saved.write_bytes(data)
            self.assertNotEqual(self.add(check=False).returncode, 0)
            self.assertEqual(saved.read_bytes(), data)
        self.assertFalse((self.fixture.root / "ssh-calls").exists())

    def test_oversized_status_is_rejected(self):
        result = subprocess.run([BINARY, "machine", "register", "user@server", "pre-0.1"], input=" " * 65537, env=self.env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exceeds", result.stderr)
        self.assertFalse((self.state / "machines.json").exists())


if __name__ == "__main__":
    unittest.main()
