#!/usr/bin/env python3
"""Native bridge checks, without an SSH server. Run after cargo build --release."""

import json
import os
from pathlib import Path
import select
import struct
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/release/tmux-agents"


class Transport(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ta-bridge-", dir="/tmp")
        self.home = Path(self.temporary.name)
        self.env = dict(os.environ, TMUX_AGENTS_HOME=str(self.home))
        self.env.pop("TMUX", None)
        self.children = []
        self.supervisor = subprocess.Popen(
            [BINARY, "supervisor"], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE
        )
        self.children.append(self.supervisor)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            result = self.cli("request", "status")
            if result.returncode == 0:
                return
            if self.supervisor.poll() is not None:
                self.fail(self.supervisor.stderr.read().decode())
            time.sleep(0.05)
        self.fail("supervisor startup timed out")

    def tearDown(self):
        for child in reversed(self.children):
            if child.poll() is None:
                child.terminate()
            child.wait(timeout=5)
            for stream in (child.stdin, child.stdout, child.stderr):
                if stream is not None:
                    stream.close()
        self.temporary.cleanup()

    def cli(self, *args):
        return subprocess.run([BINARY, *args], env=self.env, capture_output=True, timeout=10)

    def bridge(self):
        child = subprocess.Popen(
            [BINARY, "bridge"], env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )
        self.children.append(child)
        return child

    def frame(self, request_id, method="status", args=None):
        data = json.dumps({"version": 1, "id": request_id, "method": method, "args": args or []}).encode()
        return struct.pack("!I", len(data)) + data

    def response(self, child):
        def exact(size):
            result = b""
            deadline = time.monotonic() + 5
            while len(result) < size:
                ready, _, _ = select.select([child.stdout], [], [], max(0, deadline - time.monotonic()))
                self.assertTrue(ready, "bridge response timed out")
                chunk = os.read(child.stdout.fileno(), size - len(result))
                self.assertTrue(chunk, "bridge response truncated")
                result += chunk
            return result

        (size,) = struct.unpack("!I", exact(4))
        self.assertGreater(size, 0)
        self.assertLessEqual(size, 65536)
        return json.loads(exact(size))

    def test_multiple_frames_and_stdin_closure_preserve_supervisor(self):
        child = self.bridge()
        child.stdin.write(self.frame("first") + self.frame("second"))
        child.stdin.flush()
        first = self.response(child)
        second = self.response(child)
        self.assertEqual(first["id"], "first")
        self.assertEqual(second["id"], "second")
        self.assertEqual(first["result"]["instance"], second["result"]["instance"])
        child.stdin.close()
        self.assertEqual(child.wait(timeout=5), 0)
        self.assertIsNone(self.supervisor.poll())
        status = json.loads(self.cli("request", "status").stdout)
        self.assertEqual(status["instance"], first["result"]["instance"])

    def test_socket_closure_exits_with_stdin_still_open(self):
        child = self.bridge()
        child.stdin.write(self.frame("connected"))
        child.stdin.flush()
        self.response(child)
        self.supervisor.terminate()
        self.supervisor.wait(timeout=5)
        # An async stdin blocking thread would leave this process hanging at runtime shutdown.
        self.assertEqual(child.wait(timeout=5), 0)

    def test_invalid_frame_disconnect_does_not_stop_supervisor(self):
        child = self.bridge()
        child.stdin.write(struct.pack("!I", 65537))
        child.stdin.flush()
        self.assertEqual(child.wait(timeout=5), 0)
        self.assertEqual(child.stdout.read(), b"")
        self.assertEqual(self.cli("request", "status").returncode, 0)

    def test_stdout_closure_does_not_stop_supervisor(self):
        child = self.bridge()
        child.stdout.close()
        child.stdin.write(self.frame("closed-output"))
        child.stdin.flush()
        child.wait(timeout=5)
        self.assertIsNone(self.supervisor.poll())
        self.assertEqual(self.cli("request", "status").returncode, 0)

    def test_terminal_rejects_non_ids(self):
        for invalid in ("name", "a" * 31, "a" * 33, "a" * 31 + ";"):
            result = self.cli("terminal", invalid)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"32 hexadecimal", result.stderr)


if __name__ == "__main__":
    unittest.main()
