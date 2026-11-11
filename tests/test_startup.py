"""Real-terminal startup checks using only Python's standard library."""

import errno
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target" / "debug" / "tmux-agents"
TMUX = shutil.which("tmux")


class StartupTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ta-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.environment = os.environ.copy()
        self.environment.pop("TMUX", None)
        self.environment.pop("TMUX_PANE", None)
        self.environment.update(
            HOME=str(self.directory),
            XDG_STATE_HOME=str(self.directory / "state"),
            TMUX_TMPDIR=str(self.directory),
            TERM="xterm-256color",
            SHELL="/bin/sh",
        )
        self.clients = []
        self.addCleanup(self.cleanup_clients)

    def cleanup_clients(self):
        if TMUX:
            self.tmux("kill-server", check=False)
        for process, terminal in self.clients:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
            os.close(terminal)

    def tmux(self, *arguments, check=True):
        return subprocess.run(
            [TMUX, "-L", "tmux-agents", *arguments],
            env=self.environment,
            capture_output=True,
            text=True,
            check=check,
            timeout=5,
        )

    def launch(self, environment=None):
        master, slave = pty.openpty()
        process = subprocess.Popen(
            [str(BINARY)],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            cwd=self.directory,
            env=environment or self.environment,
            start_new_session=True,
        )
        os.close(slave)
        self.clients.append((process, master))
        return process, master

    def wait_for_client(self, process, terminal, session="main"):
        deadline = time.monotonic() + 5
        output = bytearray()
        while time.monotonic() < deadline:
            if select.select([terminal], [], [], 0.05)[0]:
                try:
                    output.extend(os.read(terminal, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
            if process.poll() is not None:
                self.fail(f"client exited early: {output.decode(errors='replace')}")
            clients = self.tmux("list-clients", "-F", "#{client_session}", check=False)
            if clients.returncode == 0 and clients.stdout.strip() == session:
                return
        self.fail(f"client did not attach: {output.decode(errors='replace')}")

    @property
    def snapshot_path(self):
        return Path(self.environment["XDG_STATE_HOME"]) / "tmux-agents" / "workspace.json"

    def wait_for_snapshot(self, predicate):
        deadline = time.monotonic() + 8
        snapshot = None
        while time.monotonic() < deadline:
            try:
                snapshot = json.loads(self.snapshot_path.read_text())
                if predicate(snapshot):
                    return snapshot
            except FileNotFoundError:
                pass
            time.sleep(0.05)
        self.fail(f"snapshot did not match expected state: {snapshot}")

    def detach(self, process, session="main"):
        self.tmux("detach-client", "-s", session)
        self.assertEqual(process.wait(timeout=8), 0)

    def run_cli(self, *arguments, environment=None):
        return subprocess.run(
            [str(BINARY), *arguments],
            env=environment or self.environment,
            capture_output=True,
            text=True,
            timeout=5,
        )

    def test_help(self):
        result = self.run_cli("--help")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Usage: tmux-agents", result.stdout)

    def test_rejects_extra_commands(self):
        result = self.run_cli("start")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected arguments", result.stderr)

    def test_rejects_noninteractive_startup(self):
        result = self.run_cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("interactive terminal", result.stderr)

    def test_rejects_nested_tmux(self):
        environment = dict(self.environment, TMUX="/some/socket,123,0")
        result = self.run_cli(environment=environment)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("already inside tmux", result.stderr)

    def test_reports_missing_tmux(self):
        environment = dict(self.environment, PATH=str(self.directory))
        process, terminal = self.launch(environment)
        self.assertNotEqual(process.wait(timeout=5), 0)
        self.assertIn(b"cannot execute tmux", os.read(terminal, 65536))

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_startup_and_reconnection_preserve_live_workspace(self):
        # A second server stands in for the user's ordinary tmux sessions.
        ordinary = subprocess.run(
            [TMUX, "-L", "ordinary", "-f", "/dev/null", "new-session", "-d", "-s", "untouched"],
            env=self.environment,
            capture_output=True,
            check=True,
            timeout=5,
        )
        self.assertEqual(ordinary.returncode, 0)
        self.addCleanup(
            subprocess.run,
            [TMUX, "-L", "ordinary", "kill-server"],
            env=self.environment,
            capture_output=True,
            timeout=5,
        )

        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.assertEqual(self.tmux("list-sessions", "-F", "#{session_name}").stdout.strip(), "main")
        self.assertEqual(
            self.tmux("display-message", "-p", "-t", "main:", "#{pane_current_path}").stdout.strip(),
            str(self.directory),
        )

        self.tmux("split-window", "-h", "-t", "main:")
        self.tmux("new-window", "-t", "main:", "-n", "second")
        self.tmux("split-window", "-v", "-t", "main:second")
        self.tmux("select-pane", "-t", "main:second", "-U")
        layout_format = "#{window_id} #{window_layout} #{window_active}"
        layouts = self.tmux("list-windows", "-t", "main", "-F", layout_format).stdout
        active_pane = self.tmux("display-message", "-p", "-t", "main:", "#{pane_id}").stdout
        pane_pids = self.tmux("list-panes", "-s", "-t", "main", "-F", "#{pane_pid}").stdout

        self.tmux("detach-client", "-s", "main")
        self.assertEqual(process.wait(timeout=5), 0)
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)

        self.assertEqual(self.tmux("list-windows", "-t", "main", "-F", layout_format).stdout, layouts)
        self.assertEqual(self.tmux("display-message", "-p", "-t", "main:", "#{pane_id}").stdout, active_pane)
        self.assertEqual(self.tmux("list-panes", "-s", "-t", "main", "-F", "#{pane_pid}").stdout, pane_pids)
        ordinary = subprocess.run(
            [TMUX, "-L", "ordinary", "list-sessions", "-F", "#{session_name}"],
            env=self.environment,
            capture_output=True,
            text=True,
            check=True,
            timeout=5,
        )
        self.assertEqual(ordinary.stdout.strip(), "untouched")

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_native_changes_and_last_session_are_saved(self):
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.tmux("new-session", "-d", "-s", "project")
        self.tmux("switch-client", "-t", "project")
        self.wait_for_client(process, terminal, "project")
        self.tmux("new-window", "-t", "project:7", "-n", "work ' notes\nsecond line")
        self.tmux("split-window", "-h", "-t", "project:7")
        snapshot = self.wait_for_snapshot(
            lambda state: state["active_session"] == "project"
            and any(
                session["name"] == "project" and session["active_window"] == 7
                and len(session["windows"][-1]["directories"]) == 2
                for session in state["sessions"]
            )
        )
        project = next(session for session in snapshot["sessions"] if session["name"] == "project")
        self.assertEqual(project["windows"][-1]["name"], "work ' notes\nsecond line")
        self.detach(process, "project")
        process, terminal = self.launch()
        self.wait_for_client(process, terminal, "project")
        self.assertEqual(self.tmux("display-message", "-p", "#{window_index}").stdout.strip(), "7")
        self.assertEqual(self.snapshot_path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.snapshot_path.parent.stat().st_mode & 0o777, 0o700)

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_rebuilds_sessions_layout_directories_selection_and_zoom_without_replaying_commands(self):
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        directory = self.directory / "source ' #{session_name}\nfiles"
        directory.mkdir()
        self.tmux("move-window", "-s", "main:0", "-t", "main:5")
        self.tmux("new-window", "-t", "main:9", "-n", "coding ' window")
        self.tmux("split-window", "-h", "-t", "main:9", "-c", str(directory).replace("#", "##"))
        self.tmux("split-window", "-v", "-t", "main:9")
        self.tmux("select-layout", "-t", "main:9", "main-vertical")
        self.tmux("select-pane", "-t", "main:9.1")
        geometry_format = "#{pane_left} #{pane_top} #{pane_width} #{pane_height}"
        geometry = self.tmux("list-panes", "-t", "main:9", "-F", geometry_format).stdout
        self.tmux("resize-pane", "-Z", "-t", "main:9")
        self.tmux("new-session", "-d", "-s", "project")
        self.tmux("switch-client", "-t", "project")
        self.wait_for_client(process, terminal, "project")
        # cd alone has no tmux hook; the normal detach must save its final cwd.
        self.tmux("send-keys", "-t", "project:", f"cd {shlex.quote(str(directory))}", "Enter")
        deadline = time.monotonic() + 5
        while True:
            path = self.tmux("display-message", "-p", "-t", "project:", "#{pane_current_path}").stdout[:-1]
            if path == str(directory):
                break
            if time.monotonic() > deadline:
                self.fail("shell did not change directory")
            time.sleep(0.05)
        self.detach(process, "project")
        saved = json.loads(self.snapshot_path.read_text())
        self.assertEqual(saved["active_session"], "project")
        self.assertEqual(
            next(session for session in saved["sessions"] if session["name"] == "project")["windows"][0]["directories"],
            [str(directory)],
        )
        self.tmux("kill-server")
        # A configured default-command must not run during reconstruction.
        sentinel = self.directory / "must-not-run"
        (self.directory / ".tmux.conf").write_text(
            f'set-option -g default-command "touch {sentinel}; exec /bin/sh"\n'
        )
        process, terminal = self.launch()
        self.wait_for_client(process, terminal, "project")
        self.assertIn(str(sentinel), self.tmux("show-options", "-gv", "default-command").stdout)
        self.assertFalse(sentinel.exists())
        self.assertEqual(self.tmux("list-sessions", "-F", "#{session_name}").stdout.splitlines(), ["main", "project"])
        self.assertEqual(
            self.tmux("list-windows", "-t", "main", "-F", "#{window_index}").stdout.splitlines(), ["5", "9"]
        )
        self.assertEqual(self.tmux("display-message", "-p", "-t", "main:", "#{window_index}").stdout.strip(), "9")
        self.assertEqual(self.tmux("display-message", "-p", "-t", "main:9", "#{pane_index}").stdout.strip(), "1")
        self.assertEqual(
            self.tmux("display-message", "-p", "-t", "main:9", "#{window_zoomed_flag}").stdout.strip(), "1"
        )
        self.tmux("resize-pane", "-Z", "-t", "main:9")
        self.assertEqual(self.tmux("list-panes", "-t", "main:9", "-F", geometry_format).stdout, geometry)
        self.assertEqual(
            self.tmux("display-message", "-p", "-t", "project:", "#{pane_current_path}").stdout[:-1],
            str(directory),
        )
        self.assertEqual(
            self.tmux("list-panes", "-a", "-F", "#{pane_start_command}").stdout.splitlines(), ["/bin/sh -l"] * 5
        )

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_linked_windows_keep_session_specific_indices(self):
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.tmux("new-session", "-d", "-s", "other")
        self.tmux("link-window", "-s", "main:0", "-t", "other:7")
        self.tmux("select-window", "-t", "other:7")
        self.tmux("switch-client", "-t", "other")
        self.wait_for_client(process, terminal, "other")
        snapshot = self.wait_for_snapshot(
            lambda state: state["active_session"] == "other" and len(state["sessions"]) == 2
            and state["sessions"][1]["active_window"] == 7
        )
        self.assertEqual([window["index"] for window in snapshot["sessions"][1]["windows"]], [0, 7])
        self.detach(process, "other")
        self.tmux("kill-server")
        process, terminal = self.launch()
        self.wait_for_client(process, terminal, "other")
        self.assertEqual(
            self.tmux("list-windows", "-t", "other", "-F", "#{window_index}").stdout.splitlines(), ["0", "7"]
        )
        self.assertEqual(self.tmux("display-message", "-p", "-t", "other:", "#{window_index}").stdout.strip(), "7")
        self.assertNotEqual(
            self.tmux("display-message", "-p", "-t", "main:0", "#{window_id}").stdout,
            self.tmux("display-message", "-p", "-t", "other:7", "#{window_id}").stdout,
        )

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_snapshot_names_are_literal_not_tmux_formats(self):
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.detach(process)
        self.tmux("kill-server")
        state = json.loads(self.snapshot_path.read_text())
        name = "project #{session_name}"
        sentinel = self.directory / "must-not-run"
        window = f"notes #(touch {sentinel}) #{{session_name}}"
        state["active_session"] = name
        state["sessions"][0]["name"] = name
        state["sessions"][0]["windows"][0]["name"] = window
        self.snapshot_path.write_text(json.dumps(state))
        process, terminal = self.launch()
        self.wait_for_client(process, terminal, name)
        self.assertEqual(self.tmux("list-windows", "-a", "-F", "#{window_name}").stdout[:-1], window)
        self.assertFalse(sentinel.exists())

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_exited_panes_restore_as_shells(self):
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.tmux("set-option", "-w", "-t", "main:", "remain-on-exit", "on")
        self.tmux("send-keys", "-t", "main:", "exit", "Enter")
        deadline = time.monotonic() + 5
        while self.tmux("display-message", "-p", "-t", "main:", "#{pane_dead}").stdout.strip() != "1":
            if time.monotonic() > deadline:
                self.fail("pane did not exit")
            time.sleep(0.05)
        self.detach(process)
        self.tmux("kill-server")
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.assertEqual(self.tmux("display-message", "-p", "-t", "main:", "#{pane_dead}").stdout.strip(), "0")

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_hook_paths_remain_literal(self):
        self.environment["XDG_STATE_HOME"] = str(self.directory / "state ' #{session_name}\n$literal")
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.tmux("split-window", "-h", "-t", "main:")
        self.wait_for_snapshot(lambda state: len(state["sessions"][0]["windows"][0]["directories"]) == 2)

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_missing_directory_falls_back_to_launch_directory(self):
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        removed = self.directory / "removed"
        removed.mkdir()
        self.tmux("new-window", "-t", "main:", "-c", str(removed))
        self.detach(process)
        self.tmux("kill-server")
        removed.rmdir()
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.assertEqual(
            self.tmux("display-message", "-p", "-t", "main:", "#{pane_current_path}").stdout.strip(),
            str(self.directory),
        )

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_bad_snapshot_is_preserved_without_creating_workspace(self):
        self.snapshot_path.parent.mkdir(parents=True)
        for data in [b"not json", b'{"version":99,"active_session":"main","sessions":[]}',
                     b'{"version":1,"active_session":"missing","sessions":[]}']:
            with self.subTest(data=data):
                self.snapshot_path.write_bytes(data)
                process, terminal = self.launch()
                self.assertNotEqual(process.wait(timeout=5), 0)
                self.assertIn(b"snapshot", os.read(terminal, 65536))
                self.assertEqual(self.snapshot_path.read_bytes(), data)
                self.assertNotEqual(self.tmux("has-session", check=False).returncode, 0)

    @unittest.skipUnless(TMUX, "tmux is required for live-session tests")
    def test_failed_layout_restore_rolls_back_and_preserves_snapshot(self):
        process, terminal = self.launch()
        self.wait_for_client(process, terminal)
        self.detach(process)
        self.tmux("kill-server")
        snapshot = json.loads(self.snapshot_path.read_text())
        snapshot["sessions"][0]["windows"][0]["layout"] = "invalid-layout"
        data = json.dumps(snapshot).encode()
        self.snapshot_path.write_bytes(data)
        process, terminal = self.launch()
        self.assertNotEqual(process.wait(timeout=5), 0)
        self.assertIn(b"select-layout", os.read(terminal, 65536))
        self.assertEqual(self.snapshot_path.read_bytes(), data)
        self.assertNotEqual(self.tmux("has-session", check=False).returncode, 0)


if __name__ == "__main__":
    subprocess.run(["cargo", "build"], cwd=ROOT, check=True)
    unittest.main()
