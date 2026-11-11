#!/usr/bin/env python3
"""End-to-end control/reconnect tests: real supervisors/tmux, isolated pipe/PTY SSH fixture."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/release/tmux-agents"


class Control(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ta-control-", dir="/tmp")
        self.root = Path(self.temporary.name)
        self.state = self.root / "state"
        self.state.mkdir(mode=0o700)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        fixture = self.tools / "ssh"
        fixture.write_text(f"#!{sys.executable}\n" + (ROOT / "tests/ssh_fixture.py").read_text())
        fixture.chmod(0o700)
        self.socket = self.root / "local.sock"
        self.homes = {}
        self.profiles = []
        self.env = dict(os.environ, PATH=f"{self.tools}:{os.environ['PATH']}", TMUX_AGENTS_HOME=str(self.state),
                        TMUX_AGENTS_BIN=str(BINARY), TEST_CONTROL_ROOT=str(self.root))
        self.env.pop("TMUX", None)
        self.add_host("alpha")
        self.start_local()
        self.wait_machine("local", "connected")
        self.wait_machine("alpha", "connected")

    def tearDown(self):
        self.tmux(self.socket, "kill-server", check=False)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline and any(
            len(path.name) == 17 and path.is_socket() for path in self.state.glob("c*")
        ):
            time.sleep(0.1)
        self.stop_backend(self.state)
        for home in self.homes.values():
            state = home / ".local/state/tmux-agents"
            self.stop_backend(state)
        for state in [self.state, *[home / ".local/state/tmux-agents" for home in self.homes.values()]]:
            self.tmux(state / "tmux.sock", "kill-server", check=False)
        self.temporary.cleanup()

    def add_host(self, host):
        home = self.root / f"remote-{host}"
        home.mkdir(mode=0o700)
        target = {("darwin", "x86_64"): "x86_64-apple-darwin", ("darwin", "arm64"): "aarch64-apple-darwin",
                  ("linux", "x86_64"): "x86_64-unknown-linux-musl",
                  ("linux", "aarch64"): "aarch64-unknown-linux-musl"}[(sys.platform, os.uname().machine)]
        directory = home / ".local/share/tmux-agents/bin/pre-0.1" / target
        directory.mkdir(parents=True)
        current = directory
        while current != home:
            current.chmod(0o700)
            current = current.parent
        executable = directory / "tmux-agents"
        shutil.copyfile(BINARY, executable)
        executable.chmod(0o700)
        self.homes[host] = home
        (self.root / "homes.json").write_text(json.dumps({host: str(home) for host, home in self.homes.items()}))
        self.env["TEST_CONTROL_HOMES"] = json.dumps({host: str(home) for host, home in self.homes.items()})
        self.profiles.append({"destination": host, "release_tag": "pre-0.1"})
        temporary = self.state / "registry.tmp"
        temporary.write_text(json.dumps({"version": 1, "machines": self.profiles}))
        temporary.chmod(0o600)
        temporary.replace(self.state / "machines.json")

    def start_local(self):
        self.tmux(self.socket, "-f", "/dev/null", "new-session", "-d", "-s", "local", "-x", "120", "-y", "30",
                  "bash", "--noprofile", "--norc")
        for key in ["PATH", "TEST_CONTROL_ROOT", "TEST_CONTROL_HOMES", "TMUX_AGENTS_BIN", "TMUX_AGENTS_HOME"]:
            self.tmux(self.socket, "set-environment", "-g", key, self.env[key])
        self.tmux(self.socket, "run-shell", f'bash "{ROOT}/tmux-agents.tmux"')

    def cli(self, *arguments, check=True):
        return subprocess.run([BINARY, "client", *arguments[:1], self.socket, *arguments[1:]], env=self.env,
                              capture_output=True, text=True, check=check, timeout=20)

    def status(self, host):
        return json.loads(self.cli("status", host).stdout)

    def wait(self, predicate, seconds=15):
        deadline = time.monotonic() + seconds
        last = None
        while time.monotonic() < deadline:
            try:
                value = predicate()
                if value:
                    return value
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                last = error
            time.sleep(0.1)
        logs = "\n".join(path.read_text()[-2500:] for path in self.state.glob("c*.log"))
        self.fail(f"condition timed out: {last}\n{logs}")

    def wait_machine(self, host, expected):
        def ready():
            value = self.status(host)
            return value if value["state"] == expected else None
        return self.wait(ready, seconds=20)

    def tmux(self, socket, *arguments, check=True):
        return subprocess.run(["tmux", "-S", socket, *arguments], env=self.env, capture_output=True, text=True,
                              check=check, timeout=10)

    def backend(self, host, method="status", *arguments):
        state = self.homes[host] / ".local/state/tmux-agents"
        environment = dict(self.env, TMUX_AGENTS_HOME=str(state), HOME=str(self.homes[host]))
        result = subprocess.run([BINARY, "request", method, *arguments], env=environment, capture_output=True,
                                text=True, check=True, timeout=10)
        return json.loads(result.stdout)

    def stop_backend(self, state):
        environment = dict(self.env, TMUX_AGENTS_HOME=str(state))
        result = subprocess.run([BINARY, "request", "status"], env=environment, capture_output=True, text=True)
        if result.returncode == 0:
            pid = json.loads(result.stdout)["pid"]
            command = subprocess.run(["ps", "-p", str(pid), "-o", "args="], capture_output=True, text=True).stdout
            if "tmux-agents supervisor" in command:
                os.kill(pid, signal.SIGTERM)

    def logs(self):
        path = self.root / "ssh.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def disconnect(self, host):
        (self.root / f"offline-{host}").touch()
        for event in self.logs():
            if event.get("host") == host and "pid" in event:
                pid = event["pid"]
                command = subprocess.run(["ps", "-p", str(pid), "-o", "args="],
                                         capture_output=True, text=True).stdout
                if str(self.tools / "ssh") in command:
                    os.kill(pid, signal.SIGKILL)
        self.wait_machine(host, "reconnecting")

    def create(self, name="demo"):
        result = self.cli("request", "alpha", "workspace.create", name, str(self.homes["alpha"]))
        return result.stdout.split("\t")[0]

    def remote_socket(self):
        return self.homes["alpha"] / ".local/state/tmux-agents/tmux.sock"

    def attachment_panes(self):
        output = self.tmux(self.socket, "list-panes", "-a", "-F",
                           "#{pane_id}\t#{@tmux-agents-machine}\t#{@tmux-agents-attachment}").stdout
        return [line.split("\t")[0] for line in output.splitlines() if line.endswith("\talpha\t1")]

    def test_remote_routing_and_application_errors_keep_connection(self):
        self.add_host("beta")
        self.tmux(self.socket, "set-environment", "-g", "TEST_CONTROL_HOMES", self.env["TEST_CONTROL_HOMES"])
        self.wait_machine("beta", "connected")
        self.cli("request", "beta", "workspace.create", "beta-work", str(self.homes["beta"]))
        self.assertIn("beta-work", self.cli("request", "beta", "workspace.list").stdout)
        self.assertNotIn("beta-work", self.cli("request", "alpha", "workspace.list").stdout)
        marker = self.root / "INJECTED"
        name = f"a'b;$(touch {marker})"
        workspace = self.create(name)
        self.assertEqual(len(workspace), 32)
        self.assertIn(name, self.cli("request", "alpha", "workspace.list").stdout)
        self.assertIn(name, self.cli("workspaces").stdout)
        connections = len([event for event in self.logs() if event.get("host") == "alpha" and "pid" in event])
        duplicate = self.cli("request", "alpha", "workspace.create", name, str(self.homes["alpha"]), check=False)
        self.assertNotEqual(duplicate.returncode, 0)
        self.assertEqual(self.status("alpha")["state"], "connected")
        self.assertEqual(connections, len([event for event in self.logs()
                                         if event.get("host") == "alpha" and "pid" in event]))
        self.assertFalse(marker.exists())
        unsafe_directory = self.homes["alpha"] / "directory;"
        unsafe_directory.mkdir()
        rejected = self.cli("request", "alpha", "workspace.create", "rejected", str(unsafe_directory), check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("trailing semicolon", rejected.stderr)
        self.assertNotIn("rejected", self.cli("request", "alpha", "workspace.list").stdout)
        for event in self.logs():
            if "command" in event:
                self.assertNotIn(name, event["command"])

    def test_disconnect_stale_cache_and_no_mutation_replay(self):
        workspace = self.create()
        self.disconnect("alpha")
        self.assertIn("stale", self.cli("workspaces", "alpha").stdout)
        self.assertNotEqual(self.cli("request", "alpha", "workspace.create", "offline", "/tmp",
                                     check=False).returncode, 0)
        (self.root / "offline-alpha").unlink()
        self.wait_machine("alpha", "connected")
        self.assertIn(workspace, self.cli("workspaces", "alpha").stdout)
        (self.root / "drop-create-alpha").touch()
        result = self.cli("request", "alpha", "workspace.create", "uncertain", "/tmp", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outcome unknown", result.stderr)
        self.wait_machine("alpha", "connected")
        self.assertIn("uncertain", self.cli("request", "alpha", "workspace.list").stdout)
        created = [event for event in self.logs() if event.get("method") == "workspace.create"]
        self.assertEqual(len(created), 2)

    def test_supervisor_crash_and_remote_restart_restore(self):
        workspace = self.create()
        self.tmux(self.remote_socket(), "split-window", "-h", "-t", f"workspace-{workspace}",
                  "-c", str(self.homes["alpha"]))
        self.wait(lambda: self.backend("alpha", "workspace.snapshot", workspace)["windows"][0]["panes"]
                  if len(self.backend("alpha", "workspace.snapshot", workspace)["windows"][0]["panes"]) == 2 else None)
        before = self.backend("alpha")
        pane_pids = self.tmux(self.remote_socket(), "list-panes", "-a", "-F", "#{pane_pid}").stdout
        os.kill(before["pid"], signal.SIGKILL)
        self.wait(lambda: self.status("alpha")["instance"] != before["instance"]
                  and self.status("alpha")["state"] == "connected")
        self.assertEqual(self.tmux(self.remote_socket(), "list-panes", "-a", "-F", "#{pane_pid}").stdout, pane_pids)
        saved = self.backend("alpha", "workspace.snapshot", workspace)
        self.disconnect("alpha")
        os.kill(self.backend("alpha")["pid"], signal.SIGTERM)
        self.tmux(self.remote_socket(), "kill-server")
        (self.root / "offline-alpha").unlink()
        self.wait_machine("alpha", "connected")
        restored = self.backend("alpha", "workspace.snapshot", workspace)
        self.assertEqual([pane["id"] for pane in restored["windows"][0]["panes"]],
                         [pane["id"] for pane in saved["windows"][0]["panes"]])
        self.assertEqual([pane["directory"] for pane in restored["windows"][0]["panes"]],
                         [pane["directory"] for pane in saved["windows"][0]["panes"]])
        self.assertNotEqual(self.tmux(self.remote_socket(), "list-panes", "-a", "-F", "#{pane_pid}").stdout, pane_pids)

    def test_authentication_failure_waits_for_visible_recovery(self):
        self.disconnect("alpha")
        (self.root / "auth-alpha").touch()
        (self.root / "offline-alpha").unlink()
        self.wait_machine("alpha", "needs-attention")
        attempts = len(self.logs())
        time.sleep(1.5)
        self.assertEqual(len(self.logs()), attempts)
        self.assertIn("Connected to alpha", self.cli("connect", "alpha").stdout)
        self.wait_machine("alpha", "connected")

    def test_changed_host_key_is_not_bypassed(self):
        self.disconnect("alpha")
        marker = self.root / "hostkey-alpha"
        marker.touch()
        (self.root / "offline-alpha").unlink()
        self.wait_machine("alpha", "needs-attention")
        attempts = len(self.logs())
        time.sleep(1)
        self.assertEqual(len(self.logs()), attempts)
        result = self.cli("connect", "alpha", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Host key verification failed", result.stderr)
        self.assertTrue(marker.exists())
        marker.unlink()
        self.cli("connect", "alpha")
        self.wait_machine("alpha", "connected")

    def test_attachment_recovers_when_remote_tmux_is_lost(self):
        workspace = self.create()
        self.cli("open", "alpha", workspace)
        pane = self.wait(lambda: self.attachment_panes())[0]
        self.wait(lambda: self.tmux(self.remote_socket(), "list-clients", "-F", "#{client_session}").stdout.strip())
        before = self.tmux(self.remote_socket(), "list-panes", "-a", "-F", "#{pane_pid}").stdout
        self.tmux(self.remote_socket(), "kill-server")
        self.wait(lambda: self.tmux(self.remote_socket(), "list-clients", "-F", "#{client_session}").stdout.strip())
        self.assertEqual(self.attachment_panes(), [pane])
        self.assertNotEqual(self.tmux(self.remote_socket(), "list-panes", "-a", "-F", "#{pane_pid}").stdout, before)

    def test_attachment_reconnect_detach_and_local_restart(self):
        workspace = self.create()
        self.cli("open", "alpha", workspace)
        pane = self.wait(lambda: self.attachment_panes())[0]
        self.wait(lambda: self.tmux(self.remote_socket(), "list-clients", "-F", "#{client_session}").stdout.strip())
        remote_pids = self.tmux(self.remote_socket(), "list-panes", "-a", "-F", "#{pane_pid}").stdout
        self.disconnect("alpha")
        marker = Path("/tmp") / ("ta-outage-" + uuid.uuid4().hex)
        try:
            time.sleep(0.5)
            self.tmux(self.socket, "send-keys", "-t", pane, "-l", f"touch {marker}")
            self.tmux(self.socket, "send-keys", "-t", pane, "Enter")
            (self.root / "offline-alpha").unlink()
            self.wait_machine("alpha", "connected")
            self.wait(lambda: self.tmux(self.remote_socket(), "list-clients", "-F", "#{client_session}").stdout.strip())
            self.assertEqual(self.attachment_panes(), [pane])
            self.assertEqual(self.tmux(self.remote_socket(), "list-panes", "-a", "-F", "#{pane_pid}").stdout,
                             remote_pids)
            self.assertFalse(marker.exists())
        finally:
            marker.unlink(missing_ok=True)
        self.tmux(self.socket, "send-keys", "-t", pane, "C-a", "d")
        self.wait(lambda: not self.attachment_panes())
        time.sleep(2.5)
        self.assertEqual(self.attachment_panes(), [])
        self.cli("open", "alpha", workspace)
        self.wait(lambda: self.attachment_panes())
        self.tmux(self.socket, "kill-server")
        self.wait(lambda: not any(len(path.name) == 17 and path.is_socket()
                                  for path in self.state.glob("c*")), seconds=8)
        self.start_local()
        self.wait_machine("alpha", "connected")
        self.wait(lambda: self.attachment_panes())
        self.assertEqual(len(self.attachment_panes()), 1)
        self.assertEqual(self.tmux(self.remote_socket(), "list-panes", "-a", "-F", "#{pane_pid}").stdout, remote_pids)


if __name__ == "__main__":
    unittest.main()
