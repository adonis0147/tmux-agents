#!/usr/bin/env python3
"""Isolated local integration checks. Run after cargo build --release."""

import json
import os
import pty
import re
import select
import signal
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/release/tmux-agents"


class Prototype(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ta-test-", dir="/tmp")
        self.home = Path(self.temporary.name)

        self.env = dict(os.environ, TMUX_AGENTS_HOME=str(self.home), TMUX_AGENTS_BIN=str(BINARY))
        self.env.pop("TMUX", None)

        self.supervisor = None
        self.start_supervisor()

    def tearDown(self):
        self.tmux("local.sock", "kill-server", check=False)
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline and any(
            len(path.name) == 17 and path.is_socket() for path in self.home.glob("c*")
        ):
            time.sleep(0.1)

        if self.supervisor and self.supervisor.poll() is None:
            self.supervisor.terminate()
            self.supervisor.wait(timeout=5)

        status = self.cli("request", "status", check=False)
        if status.returncode == 0:
            os.kill(json.loads(status.stdout)["pid"], signal.SIGTERM)

        self.tmux("tmux.sock", "kill-server", check=False)
        self.temporary.cleanup()

    def start_supervisor(self):
        self.supervisor = subprocess.Popen(
            [BINARY, "supervisor"], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )

        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            status = self.cli("request", "status", check=False)
            if status.returncode == 0:
                return
            if self.supervisor.poll() is not None:
                self.fail("supervisor failed to start")
            time.sleep(0.05)

        self.fail("supervisor startup timed out")

    def cli(self, *args, check=True):
        return subprocess.run([BINARY, *args], env=self.env, text=True, capture_output=True, check=check, timeout=10)

    def tmux(self, name, *args, check=True):
        return subprocess.run(
            ["tmux", "-S", str(self.home / name), *args],
            env=self.env,
            text=True,
            capture_output=True,
            check=check,
            timeout=10,
        )

    def frame(self, value):
        data = json.dumps(value).encode()
        return struct.pack("!I", len(data)) + data

    def exchange(self, value):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5)
            connection.connect(str(self.home / "control.sock"))
            connection.sendall(self.frame(value))

            def exact(length):
                result = b""
                while len(result) < length:
                    chunk = connection.recv(length - len(result))
                    if not chunk:
                        raise AssertionError("truncated response")
                    result += chunk

                return result

            (size,) = struct.unpack("!I", exact(4))
            return json.loads(exact(size))

    def load_plugin(self):
        self.tmux("local.sock", "-f", "/dev/null", "new-session", "-d", "-s", "local")
        self.tmux("local.sock", "bind-key", "M", "display-message", "custom-M")
        self.tmux("local.sock", "bind-key", "a", "display-message", "custom-a")
        for key in ["TMUX_AGENTS_HOME", "TMUX_AGENTS_BIN"]:
            self.tmux("local.sock", "set-environment", "-g", key, self.env[key])

        result = self.tmux("local.sock", "run-shell", f"bash '{ROOT}/tmux-agents.tmux'")

        self.assertEqual(result.stdout, "")

    def test_protocol_and_validation(self):
        request = {"version": 1, "id": "test", "method": "status", "args": []}
        response = self.exchange(request)

        self.assertEqual(response["id"], "test")
        self.assertEqual(response["result"]["protocol"], 1)
        instance = response["result"]["instance"]
        self.assertEqual(len(instance), 32)

        for invalid in [
            dict(request, version=2),
            dict(request, method="destroy"),
            dict(request, args=["extra"]),
            dict(request, id=""),
        ]:
            self.assertIsNotNone(self.exchange(invalid)["error"])

        for data in [struct.pack("!I", 65537), self.frame(dict(request, unknown=True)), struct.pack("!I", 1) + b"x"]:
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(5)
                connection.connect(str(self.home / "control.sock"))
                connection.sendall(data)
                connection.shutdown(socket.SHUT_WR)

                self.assertEqual(connection.recv(1), b"")

        self.assertEqual(self.cli("request", "status").returncode, 0)
        self.assertNotEqual(self.cli("request", "workspace.create", "bad\nname", "/tmp", check=False).returncode, 0)
        self.assertNotEqual(self.cli("request", "workspace.create", "bad", "relative", check=False).returncode, 0)

    def test_restart_and_isolation(self):
        self.tmux("local.sock", "-f", "/dev/null", "new-session", "-d", "-s", "unrelated")
        created = self.cli("request", "workspace.create", "demo", "/tmp").stdout
        workspace_id = created.split("\t")[0]

        self.assertEqual(len(workspace_id), 32)
        self.assertNotEqual(self.cli("request", "workspace.create", "demo", "/tmp", check=False).returncode, 0)

        before = self.tmux("tmux.sock", "list-panes", "-a", "-F", "#{pane_pid}").stdout
        self.cli("ensure")
        self.assertNotEqual(self.cli("supervisor", check=False).returncode, 0)

        self.supervisor.terminate()
        self.supervisor.wait(timeout=5)
        self.assertEqual(self.tmux("tmux.sock", "list-panes", "-a", "-F", "#{pane_pid}").stdout, before)

        self.start_supervisor()

        self.assertIn(workspace_id, self.cli("request", "workspace.list").stdout)
        self.assertEqual(self.tmux("tmux.sock", "list-panes", "-a", "-F", "#{pane_pid}").stdout, before)
        self.tmux("local.sock", "has-session", "-t", "=unrelated")
        self.assertEqual(self.tmux("tmux.sock", "show-option", "-gv", "prefix").stdout.strip(), "C-a")

        self.tmux("tmux.sock", "kill-server")

        restored = json.loads(self.cli("request", "workspace.restore", workspace_id).stdout)
        self.assertEqual(restored["id"], workspace_id)
        self.assertIn("running", self.cli("request", "workspace.list").stdout)
        self.assertNotEqual(self.tmux("tmux.sock", "list-panes", "-a", "-F", "#{pane_pid}").stdout, before)

    def test_alias_quoting_and_attachment(self):
        self.load_plugin()
        self.assertEqual(self.tmux("local.sock", "show-option", "-gv", "prefix").stdout.strip(), "C-b")
        socket_alias = self.home / "socket-link"
        socket_alias.symlink_to(self.home / "local.sock")
        self.cli("client", "ensure", str(socket_alias))
        self.assertEqual(len([path for path in self.home.glob("c*")
                              if len(path.name) == 17 and path.is_socket()]), 1)

        self.tmux("local.sock", "machine", "start")
        marker = self.home / "INJECTED"
        name = f"a'b;$(touch {marker}) #{{pane_id}}"

        created = self.tmux("local.sock", "machine", "workspace", "create", name, "/tmp")

        self.assertIn(name, created.stdout)
        self.assertFalse(marker.exists())
        self.assertIn(name, self.cli("request", "workspace.list").stdout)

        self.tmux("local.sock", "machine", "workspace", "open", name)
        windows = self.tmux("local.sock", "list-windows", "-F", "#{window_id}").stdout
        self.tmux("local.sock", "machine", "workspace", "open", name)
        self.assertEqual(self.tmux("local.sock", "list-windows", "-F", "#{window_id}").stdout, windows)

        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            clients = self.tmux("tmux.sock", "list-clients", "-F", "#{client_session}").stdout
            if clients.strip():
                break
            time.sleep(0.05)

        self.assertIn("workspace-", clients)

        self.tmux("local.sock", "kill-server")

        self.assertIn("running", self.cli("request", "workspace.list").stdout)

    def test_existing_machine_alias_is_preserved(self):
        self.tmux("local.sock", "-f", "/dev/null", "new-session", "-d", "-s", "local")
        self.tmux("local.sock", "set-option", "-s", "command-alias[existing]", "machine=display-message -p original")

        result = self.tmux("local.sock", "run-shell", f"bash '{ROOT}/tmux-agents.tmux'", check=False)

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.tmux("local.sock", "machine").stdout.strip(), "original")
        self.assertEqual(
            self.tmux("local.sock", "show-option", "-sqv", "command-alias[tmux-agents-machine]").stdout.strip(), ""
        )

    def test_existing_prefix_bindings_are_preserved(self):
        self.load_plugin()
        self.assertIn("custom-M", self.tmux("local.sock", "list-keys", "-T", "prefix", "M").stdout)
        self.assertIn("custom-a", self.tmux("local.sock", "list-keys", "-T", "prefix", "a").stdout)

    def test_uncertain_creation_is_not_replayed(self):
        self.supervisor.terminate()
        self.supervisor.wait(timeout=5)
        (self.home / "control.sock").unlink()

        received = []
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(self.home / "control.sock"))
            listener.listen(1)
            listener.settimeout(5)

            def drop_response():
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(5)
                    header = connection.makefile("rb")
                    (size,) = struct.unpack("!I", header.read(4))
                    received.append(json.loads(header.read(size)))
                    header.close()

            thread = threading.Thread(target=drop_response)
            thread.start()
            result = self.cli("request", "workspace.create", "uncertain", "/tmp", check=False)
            thread.join(timeout=5)

        self.assertFalse(thread.is_alive())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outcome unknown", result.stderr)
        self.assertEqual(len(received), 1)
        self.assertEqual(received[0]["method"], "workspace.create")

    def test_detached_concurrent_startup(self):
        self.supervisor.terminate()
        self.supervisor.wait(timeout=5)

        children = [
            subprocess.Popen([BINARY, "ensure"], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            for _ in range(4)
        ]
        detached_pid = None

        try:
            for child in children:
                _, error = child.communicate(timeout=10)
                self.assertEqual(child.returncode, 0, error.decode())

            status = json.loads(self.cli("request", "status").stdout)
            detached_pid = status["pid"]

            self.assertNotEqual(detached_pid, os.getpid())
            self.assertEqual(self.cli("ensure").returncode, 0)
        finally:
            if detached_pid is None:
                status = self.cli("request", "status", check=False)
                if status.returncode == 0:
                    detached_pid = json.loads(status.stdout)["pid"]
            if detached_pid:
                os.kill(detached_pid, signal.SIGTERM)

    def test_interactive_picker_and_prefix(self):
        self.load_plugin()
        self.tmux("local.sock", "bind-key", "M", "run-shell", "-E",
                  "bash #{q:@tmux-agents-root}/scripts/machine.sh picker")
        self.tmux("local.sock", "bind-key", "a", "if-shell", "-F", "#{!=:#{@tmux-agents-workspace},}",
                  "send-keys C-a", "send-prefix")
        self.cli("request", "workspace.create", "interactive", "/tmp")

        pid, terminal = pty.fork()
        if pid == 0:
            environment = dict(self.env, TERM="xterm-256color")
            os.execvpe(
                "tmux", ["tmux", "-S", str(self.home / "local.sock"), "attach-session", "-t", "local"], environment
            )

        output = b""

        def wait_for(predicate):
            nonlocal output
            deadline = time.monotonic() + 5

            while time.monotonic() < deadline:
                if select.select([terminal], [], [], 0.05)[0]:
                    output += os.read(terminal, 65536)
                if predicate():
                    return

            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output)
            self.fail("interactive tmux check timed out: " + repr(plain[-3000:]))

        try:
            # Set a usable terminal size before opening a popup.
            import fcntl
            import termios

            fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
            wait_for(lambda: bool(self.tmux("local.sock", "list-clients", "-F", "#{client_name}").stdout.strip()))

            os.write(terminal, b"\x02M")
            wait_for(lambda: b"Workspace number" in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output))

            os.write(terminal, b"1\n")
            wait_for(lambda: "workspace-" in self.tmux("tmux.sock", "list-clients", "-F", "#{client_session}").stdout)

            # Attachment exists before the picker has closed; wait for its screen.
            wait_for(
                lambda: b"[workspace" in output
                and bool(self.tmux("local.sock", "show-option", "-wqv", "@tmux-agents-workspace").stdout.strip())
            )

            os.write(terminal, b"\x02ac")
            wait_for(lambda: len(self.tmux("tmux.sock", "list-windows", "-F", "#{window_id}").stdout.splitlines()) == 2)
        finally:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            os.close(terminal)

    def test_preserves_incompatible_metadata(self):
        self.supervisor.terminate()
        self.supervisor.wait(timeout=5)

        metadata = self.home / "metadata.json"
        for content in [b'{"version":99,"workspaces":[]}', b"not json"]:
            metadata.write_bytes(content)

            self.assertNotEqual(self.cli("supervisor", check=False).returncode, 0)
            self.assertEqual(metadata.read_bytes(), content)

    def test_private_directory_required(self):
        self.home.chmod(0o755)

        try:
            self.assertNotEqual(self.cli("request", "status", check=False).returncode, 0)
        finally:
            self.home.chmod(0o700)


if __name__ == "__main__":
    unittest.main()
