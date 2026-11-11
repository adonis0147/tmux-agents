#!/usr/bin/env python3
"""Persistent RPC and shell-only recovery checks. Run after cargo build --release."""

import json
import os
import re
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("TMUX_AGENTS_TEST_BINARY", ROOT / "target/release/tmux-agents"))


class Connection:
    def __init__(self, home):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(10)
        try:
            self.socket.connect(str(home / "control.sock"))
        except OSError:
            self.socket.close()
            raise
        self.sequence = 0

    def close(self):
        self.socket.close()

    def rpc(self, method, *args):
        self.sequence += 1
        request = {"version": 1, "id": str(self.sequence), "method": method, "args": list(args)}
        payload = json.dumps(request).encode()
        self.socket.sendall(struct.pack("!I", len(payload)) + payload)

        def exact(length):
            result = b""
            while len(result) < length:
                chunk = self.socket.recv(length - len(result))
                if not chunk:
                    raise AssertionError("truncated RPC response")
                result += chunk
            return result

        (length,) = struct.unpack("!I", exact(4))
        assert 0 < length <= 65536
        response = json.loads(exact(length))
        assert response["id"] == request["id"]
        if response["error"] is not None:
            raise AssertionError(response["error"])
        return response["result"]


class Recovery(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ta-recovery-", dir="/tmp")
        self.home = Path(self.temporary.name).resolve()
        self.env = dict(os.environ, TMUX_AGENTS_HOME=str(self.home))
        self.env.pop("TMUX", None)
        self.supervisor = None
        self.connections = []
        self.log = open(self.home / "supervisor.log", "ab")
        self.start_supervisor()

    def tearDown(self):
        for connection in self.connections:
            connection.close()
        self.stop_supervisor()
        self.tmux("kill-server", check=False)
        self.log.close()
        self.temporary.cleanup()

    def start_supervisor(self):
        self.supervisor = subprocess.Popen(
            [BINARY, "supervisor"], env=self.env, stdout=subprocess.DEVNULL, stderr=self.log
        )
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                connection = Connection(self.home)
                try:
                    connection.rpc("status")
                    return
                finally:
                    connection.close()
            except (OSError, AssertionError):
                if self.supervisor.poll() is not None:
                    self.fail("supervisor failed: " + (self.home / "supervisor.log").read_text())
                time.sleep(0.05)
        self.fail("supervisor startup timed out")

    def stop_supervisor(self):
        if self.supervisor and self.supervisor.poll() is None:
            self.supervisor.kill()
            self.supervisor.wait(timeout=5)

    def connect(self):
        connection = Connection(self.home)
        self.connections.append(connection)
        return connection

    def tmux(self, *args, check=True):
        return subprocess.run(
            ["tmux", "-S", str(self.home / "tmux.sock"), *args], env=self.env,
            text=True, capture_output=True, check=check, timeout=10
        )

    def wait_for(self, predicate):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                result = predicate()
                if result:
                    return result
            except (OSError, subprocess.CalledProcessError):
                pass
            time.sleep(0.1)
        self.fail("condition timed out: " + (self.home / "supervisor.log").read_text())

    def test_persistent_concurrent_connections(self):
        first, second = self.connect(), self.connect()
        instance = first.rpc("status")["instance"]
        # An incomplete frame on another client cannot block either established connection.
        stalled = socket.socket(socket.AF_UNIX)
        self.addCleanup(stalled.close)
        stalled.connect(str(self.home / "control.sock"))
        stalled.sendall(b"\x00")
        for _ in range(5):
            self.assertEqual(second.rpc("status")["instance"], instance)
            self.assertEqual(first.rpc("state.snapshot")["workspaces"], [])
        created = second.rpc("workspace.create", "demo", str(self.home))
        state = first.rpc("state.snapshot")
        self.assertEqual(state["instance"], instance)
        self.assertGreater(state["revision"], 0)
        self.assertEqual(state["workspaces"], [{
            "id": created["id"], "name": "demo", "running": True, "windows": 1, "panes": 1
        }])
        time.sleep(2.2)
        self.assertEqual(first.rpc("status")["instance"], second.rpc("status")["instance"])

    def test_layout_recovery_crash_and_no_replay(self):
        connection = self.connect()
        directories = [self.home / name for name in ["one", "two with spaces", "three", "four"]]
        for directory in directories:
            directory.mkdir()
        created = connection.rpc("workspace.create", "layout", str(directories[0]))
        session = "workspace-" + created["id"]
        self.tmux("set-option", "-w", "-t", session, "automatic-rename", "off")
        self.tmux("rename-window", "-t", session, "main window")
        first = self.tmux("list-panes", "-t", session, "-F", "#{pane_id}").stdout.strip()
        second = self.tmux("split-window", "-h", "-d", "-P", "-F", "#{pane_id}",
                           "-t", first, "-c", str(directories[1])).stdout.strip()
        self.tmux("split-window", "-v", "-d", "-t", second, "-c", str(directories[2]))
        self.tmux("new-window", "-d", "-t", session, "-n", "saved window", "-c", str(directories[3]))
        marker = self.home / "executions"
        # Deliberately an observed running command: restoration must not replay it.
        fixture = self.home / "fixture.sh"
        fixture.write_text('#!/bin/bash\nprintf "once\\n" >> "${1}"\nwhile :; do sleep 1; done\n')
        self.tmux("send-keys", "-t", first, f"bash '{fixture}' '{marker}'", "Enter")
        self.wait_for(lambda: marker.exists())
        connection.rpc("workspace.snapshot", "layout")
        self.tmux("swap-pane", "-d", "-s", first, "-t", second)
        snapshot = connection.rpc("workspace.snapshot", "layout")
        self.assertEqual([len(window["panes"]) for window in snapshot["windows"]], [3, 1])
        pids = self.tmux("list-panes", "-a", "-F", "#{pane_pid}").stdout
        instance = connection.rpc("status")["instance"]
        self.stop_supervisor()
        self.start_supervisor()
        connection = self.connect()
        self.assertNotEqual(connection.rpc("status")["instance"], instance)
        self.assertEqual(self.tmux("list-panes", "-a", "-F", "#{pane_pid}").stdout, pids)
        self.assertEqual(connection.rpc("workspace.snapshot", "layout"), snapshot)
        connection.rpc("workspace.restore", "layout")
        self.assertEqual(self.tmux("list-panes", "-a", "-F", "#{pane_pid}").stdout, pids)

        # Leave an unrelated session on the same dedicated server; never adopt or kill it.
        self.tmux("new-session", "-d", "-s", "unrelated")
        unrelated = self.tmux("list-panes", "-t", "unrelated", "-F", "#{pane_pid}").stdout
        self.tmux("kill-session", "-t", session)
        self.wait_for(lambda: self.tmux("has-session", "-t", session, check=False).returncode == 0)
        self.assertEqual(self.tmux("list-panes", "-t", "unrelated", "-F", "#{pane_pid}").stdout, unrelated)
        self.assert_saved_structure(snapshot, connection.rpc("workspace.snapshot", "layout"))

        # Actual server destruction, followed by automatic recreation without client resolve.
        self.tmux("kill-server")
        self.wait_for(lambda: self.tmux("has-session", "-t", session, check=False).returncode == 0)
        restored = connection.rpc("workspace.snapshot", created["id"])
        self.assert_saved_structure(snapshot, restored)
        self.assertEqual(marker.read_text(), "once\n")
        paths = {pane["directory"] for window in restored["windows"] for pane in window["panes"]}
        self.assertEqual(paths, {str(directory) for directory in directories})

    def assert_saved_structure(self, before, after):
        self.assertEqual(before["id"], after["id"])
        self.assertEqual([w["id"] for w in before["windows"]], [w["id"] for w in after["windows"]])
        for old, new in zip(before["windows"], after["windows"]):
            self.assertEqual(old["name"], new["name"])
            self.assertEqual([(p["id"], p["directory"]) for p in old["panes"]],
                             [(p["id"], p["directory"]) for p in new["panes"]])
            # Geometry must survive, while runtime pane numbers and checksum may change.
            def geometry(layout):
                return re.sub(r"(\d+x\d+,\d+,\d+),\d+", r"\1,P", layout[5:])

            self.assertEqual(geometry(old["layout"]), geometry(new["layout"]))

    def test_polling_shrink_backup_and_dead_panes(self):
        connection = self.connect()
        created = connection.rpc("workspace.create", "polling", str(self.home))
        session = "workspace-" + created["id"]
        self.tmux("set-option", "-w", "-t", session, "automatic-rename", "off")
        self.tmux("rename-window", "-t", session, "polling")
        pane = self.tmux("split-window", "-d", "-P", "-F", "#{pane_id}",
                         "-t", session, "-c", str(self.home)).stdout.strip()
        path = self.home / "metadata.json"

        def saved_panes():
            return json.loads(path.read_bytes())["workspaces"][0]["windows"][0]["panes"]

        self.wait_for(lambda: len(saved_panes()) == 2)
        # Exiting shells remain in the layout and cannot destroy the last useful structure.
        self.tmux("send-keys", "-t", pane, "exit", "Enter")
        self.wait_for(lambda: self.tmux("display-message", "-p", "-t", pane, "#{pane_dead}").stdout.strip() == "1")
        self.assertEqual(len(connection.rpc("workspace.snapshot", "polling")["windows"][0]["panes"]), 2)
        self.tmux("kill-pane", "-t", pane)
        self.wait_for(lambda: len(saved_panes()) == 1)
        backup = json.loads((self.home / "metadata.previous.json").read_bytes())
        self.assertEqual(len(backup["workspaces"][0]["windows"][0]["panes"]), 2)
        modification = path.stat().st_mtime_ns
        time.sleep(1.2)
        self.assertEqual(path.stat().st_mtime_ns, modification)

    def test_unsupported_window_name_preserves_snapshot(self):
        connection = self.connect()
        workspace = connection.rpc("workspace.create", "safe", str(self.home))
        connection.rpc("workspace.snapshot", workspace["id"])
        saved = (self.home / "metadata.json").read_bytes()
        self.tmux("rename-window", "-t", f"workspace-{workspace['id']}:0", r"\;")
        with self.assertRaisesRegex(AssertionError, "invalid window name"):
            connection.rpc("workspace.snapshot", workspace["id"])
        time.sleep(0.7)
        self.assertEqual((self.home / "metadata.json").read_bytes(), saved)

    def test_incompatible_and_malformed_state_are_not_overwritten(self):
        connection = self.connect()
        connection.rpc("workspace.create", "valid", str(self.home))
        self.stop_supervisor()
        path = self.home / "metadata.json"
        valid = json.loads(path.read_bytes())
        malformed = json.loads(json.dumps(valid))
        malformed["workspaces"][0]["windows"][0]["layout"] = "0000,80x24,0,0,0"
        for data in [dict(valid, version=999), malformed, {"version": 2, "workspaces": "wrong"}]:
            content = json.dumps(data).encode()
            path.write_bytes(content)
            result = subprocess.run([BINARY, "supervisor"], env=self.env, capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(path.read_bytes(), content)

    def test_unrelated_collision_and_incomplete_restore_preserve_snapshot(self):
        connection = self.connect()
        created = connection.rpc("workspace.create", "protected", str(self.home))
        session = "workspace-" + created["id"]
        self.stop_supervisor()
        self.tmux("kill-server")
        self.tmux("-f", "/dev/null", "new-session", "-d", "-s", session, "-c", str(self.home))
        pids = self.tmux("list-panes", "-t", session, "-F", "#{pane_pid}").stdout
        path = self.home / "metadata.json"
        saved = path.read_bytes()
        self.start_supervisor()
        connection = self.connect()
        with self.assertRaisesRegex(AssertionError, "unrelated tmux session"):
            connection.rpc("workspace.restore", "protected")
        self.assertEqual(self.tmux("list-panes", "-t", session, "-F", "#{pane_pid}").stdout, pids)
        self.assertEqual(path.read_bytes(), saved)
        self.stop_supervisor()
        self.tmux("set-option", "-t", session, "@tmux_agents_workspace_id", created["id"])
        self.tmux("set-option", "-t", session, "@tmux_agents_restoring", "1")
        self.start_supervisor()
        connection = self.connect()
        with self.assertRaisesRegex(AssertionError, "incomplete restore"):
            connection.rpc("workspace.snapshot", "protected")
        self.assertEqual(path.read_bytes(), saved)
        self.assertEqual(self.tmux("list-panes", "-t", session, "-F", "#{pane_pid}").stdout, pids)

    def test_v1_migration_and_missing_directory_protection(self):
        self.stop_supervisor()
        directory = self.home / "legacy"
        directory.mkdir()
        legacy = {"version": 1, "workspaces": [{
            "id": "1234567890abcdef1234567890abcdef", "name": "legacy", "directory": str(directory)
        }]}
        content = json.dumps(legacy).encode()
        path = self.home / "metadata.json"
        path.write_bytes(content)
        self.start_supervisor()
        connection = self.connect()
        snapshot = connection.rpc("workspace.snapshot", "legacy")
        self.assertEqual((self.home / "metadata.v1.backup.json").read_bytes(), content)
        self.assertEqual(len(snapshot["windows"]), 1)
        self.assertEqual(snapshot["windows"][0]["panes"][0]["directory"], str(directory))
        self.stop_supervisor()
        self.tmux("kill-server")
        directory.rmdir()
        saved = path.read_bytes()
        self.start_supervisor()
        connection = self.connect()
        with self.assertRaisesRegex(AssertionError, "directory does not exist"):
            connection.rpc("workspace.restore", "legacy")
        time.sleep(0.7)
        self.assertEqual(path.read_bytes(), saved)
        directory.mkdir()
        self.wait_for(lambda: self.tmux("has-session", "-t", "workspace-" + snapshot["id"],
                                       check=False).returncode == 0)


if __name__ == "__main__":
    unittest.main()
