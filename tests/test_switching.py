"""PTY chooser tests and real SSH handoffs through an isolated loopback sshd."""

import errno
import json
import os
from pathlib import Path
import pty
import pwd
import select
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target" / "debug" / "tmux-agents"
TMUX = shutil.which("tmux")
SSH = shutil.which("ssh")
SSHD = shutil.which("sshd") or ("/usr/sbin/sshd" if Path("/usr/sbin/sshd").is_file() else None)
KEYGEN = shutil.which("ssh-keygen")


class Harness(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ta-switch-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.environments = []
        self.local = self.environment("local")
        self.clients = []
        self.output = bytearray()
        self.sshd = None
        self.ssh_log = self.root / "ssh.log"
        self.addCleanup(self.cleanup)

    def environment(self, name):
        home = self.root / name
        home.mkdir()
        temporary = home / "sockets"
        temporary.mkdir(mode=0o700)
        environment = dict(
            os.environ, HOME=str(home), XDG_STATE_HOME=str(home / "state"),
            XDG_CONFIG_HOME=str(home / "config"), TMUX_TMPDIR=str(temporary),
            SHELL="/bin/sh", TERM="xterm-256color",
        )
        environment.pop("TMUX", None)
        environment.pop("TMUX_PANE", None)
        self.environments.append(environment)
        return environment

    def cleanup(self):
        for environment in self.environments:
            self.tmux(environment, "kill-server", check=False)
        for process, terminal in self.clients:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            os.close(terminal)
        if self.sshd and self.sshd.poll() is None:
            self.sshd.terminate()
            self.sshd.wait(timeout=5)

    def tmux(self, environment, *arguments, check=True):
        return subprocess.run(
            [TMUX, "-L", "tmux-agents", *arguments], env=environment,
            capture_output=True, text=True, check=check, timeout=5,
        )

    def launch(self):
        master, slave = pty.openpty()
        process = subprocess.Popen(
            [str(BINARY)], env=self.local, stdin=slave, stdout=slave, stderr=slave,
            start_new_session=True,
        )
        os.close(slave)
        self.clients.append((process, master))
        return process, master

    def wait_for(self, predicate, message):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if predicate():
                return
            # Keep terminal buffers from blocking clients while polling server state.
            for _, terminal in self.clients:
                if select.select([terminal], [], [], 0)[0]:
                    try:
                        self.output.extend(os.read(terminal, 65536))
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
            time.sleep(0.025)
        self.fail(message + "\n" + self.output[-16000:].decode(errors="replace"))

    def client_info(self, environment):
        result = self.tmux(environment, "list-clients", "-F", "#{client_name}\t#{client_pid}", check=False)
        return [line.split("\t") for line in result.stdout.splitlines()] if result.returncode == 0 else []

    def wait_client(self, environment, count=1):
        def ready():
            clients = self.client_info(environment)
            return len(clients) == count and all(
                self.tmux(environment, "show-options", "-gv", f"@ta_route_{pid}", check=False).returncode == 0
                for _, pid in clients
            )
        self.wait_for(ready, "managed client did not attach")

    def request(self, environment, index, client=None):
        client, pid = client or self.client_info(environment)[0]
        path = self.tmux(environment, "display-message", "-p", "#{socket_path}").stdout.strip()
        return subprocess.run(
            [str(BINARY), "--internal-switch", path, pid, client, str(index)],
            env=environment, capture_output=True, text=True, timeout=5,
        )

    def cli(self, *arguments):
        return subprocess.run(
            [str(BINARY), *arguments], env=self.local, capture_output=True, text=True, timeout=10,
        )

    def detach(self, process, environment):
        self.tmux(environment, "detach-client", "-s", "main")
        self.assertEqual(process.wait(timeout=10), 0)

    def state(self, name):
        return Path(self.local["XDG_STATE_HOME"]) / "tmux-agents" / name


@unittest.skipUnless(TMUX, "tmux is required")
class ChooserTests(Harness):
    def test_prefix_W_opens_native_session_chooser(self):
        process, terminal = self.launch()
        self.wait_client(self.local)
        os.write(terminal, b"\x02W")
        # The native menu's explicit s shortcut selects tmux's session tree.
        time.sleep(0.15)
        os.write(terminal, b"s")
        self.wait_for(
            lambda: self.tmux(self.local, "display-message", "-p", "#{pane_in_mode}").stdout.strip() == "1",
            "native session chooser did not open",
        )
        os.write(terminal, b"q")
        self.detach(process, self.local)

    def test_switch_detaches_only_requesting_client(self):
        first, _ = self.launch()
        self.wait_client(self.local)
        first_client = self.client_info(self.local)[0]
        second, _ = self.launch()
        self.wait_client(self.local, 2)
        second_client = next(client for client in self.client_info(self.local) if client != first_client)
        result = self.request(self.local, 0, first_client)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.wait_for(
            lambda: len(self.client_info(self.local)) == 2
            and first_client not in self.client_info(self.local),
            "requesting client did not reattach",
        )
        self.assertIn(second_client, self.client_info(self.local))
        self.assertIsNone(first.poll())
        self.assertIsNone(second.poll())
        self.tmux(self.local, "detach-client", "-s", "main")
        self.assertEqual(first.wait(timeout=10), 0)
        self.assertEqual(second.wait(timeout=10), 0)

    def test_preserves_user_binding_and_accepts_native_override(self):
        process, _ = self.launch()
        self.wait_client(self.local)
        self.tmux(self.local, "bind-key", "-T", "prefix", "W", "display-message", "keep-my-key")
        self.tmux(self.local, "set-option", "-g", "@ta_workspace_key", "F12")
        self.detach(process, self.local)
        process, _ = self.launch()
        self.wait_client(self.local)
        self.assertIn("keep-my-key", self.tmux(self.local, "list-keys", "-T", "prefix", "W").stdout)
        self.assertIn("--internal-chooser", self.tmux(self.local, "list-keys", "-T", "prefix", "F12").stdout)

    def test_invalid_selection_does_not_detach_or_write_request(self):
        process, _ = self.launch()
        self.wait_client(self.local)
        original = self.client_info(self.local)
        result = self.request(self.local, 999)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.client_info(self.local), original)
        self.assertIsNone(process.poll())
        self.assertEqual(list(self.state(".").glob("handoff.*.json")), [])
        result = self.cli("--internal-take-handoff", "../../outside")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid connection token", result.stderr)

    def test_chooser_callbacks_keep_paths_literal(self):
        temporary = self.root / "sockets ' #{client_pid}"
        temporary.mkdir(mode=0o700)
        self.local["TMUX_TMPDIR"] = str(temporary)
        self.local["XDG_STATE_HOME"] = str(self.root / "state ' #{session_name}")
        registry = Path(self.local["XDG_CONFIG_HOME"]) / "tmux-agents" / "machines.json"
        registry.parent.mkdir(parents=True)
        registry.write_text(json.dumps({"version": 1, "machines": ["offline"]}))
        bin_directory = self.root / "bin"
        bin_directory.mkdir()
        attempted = self.root / "attempted"
        wrapper = bin_directory / "ssh"
        wrapper.write_text(f"#!/bin/sh\nprintf attempted > {shlex.quote(str(attempted))}\nexit 255\n")
        wrapper.chmod(0o700)
        self.local["PATH"] = str(bin_directory) + os.pathsep + os.defpath
        process, terminal = self.launch()
        self.wait_client(self.local)
        original = self.client_info(self.local)
        os.write(terminal, b"\x02W")
        time.sleep(0.15)
        os.write(terminal, b"\x1b[B\r")
        self.wait_for(attempted.exists, "literal-path menu action did not reach the SSH launcher")
        self.wait_for(
            lambda: bool(self.client_info(self.local)) and self.client_info(self.local) != original,
            "failed selection did not return local",
        )
        self.assertIsNone(process.poll())

    def test_unregistered_last_machine_falls_back_without_ssh(self):
        selection = self.state("selection.json")
        selection.parent.mkdir(parents=True)
        selection.write_text(json.dumps({"version": 1, "destination": {"kind": "remote", "address": "unknown"}}))
        process, _ = self.launch()
        self.wait_client(self.local)
        self.assertIsNone(process.poll())
        self.assertEqual(json.loads(selection.read_text())["destination"], {"kind": "local"})


@unittest.skipUnless(TMUX and SSH and SSHD and KEYGEN, "tmux, OpenSSH, sshd and ssh-keygen are required")
class SshSwitchingTests(Harness):
    def setUp(self):
        super().setUp()
        self.remote = self.environment("remote")
        key = self.root / "client-key"
        host_key = self.root / "host-key"
        for path in [key, host_key]:
            subprocess.run([KEYGEN, "-q", "-t", "ed25519", "-N", "", "-f", str(path)], check=True, timeout=5)
        authorized = self.root / "authorized_keys"
        authorized.write_text(key.with_suffix(".pub").read_text())
        authorized.chmod(0o600)
        remote_bin = Path(self.remote["HOME"]) / "bin"
        remote_bin.mkdir()
        (remote_bin / "tmux-agents").symlink_to(BINARY)
        script = self.root / "remote.sh"
        variables = ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "TMUX_TMPDIR", "SHELL"]
        exports = {name: self.remote[name] for name in variables}
        exports["PATH"] = str(remote_bin) + os.pathsep + os.defpath
        script.write_text(
            "#!/bin/sh\nunset TMUX TMUX_PANE\n"
            + "\n".join(f"export {name}={shlex.quote(value)}" for name, value in exports.items())
            + '\nexec /bin/sh -c "${SSH_ORIGINAL_COMMAND}"\n'
        )
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        configuration = self.root / "sshd.conf"
        configuration.write_text(
            f"Port {port}\nListenAddress 127.0.0.1\nHostKey {host_key}\n"
            f"AuthorizedKeysFile {authorized}\nPidFile {self.root / 'sshd.pid'}\n"
            "StrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n"
            f"AllowUsers {pwd.getpwuid(os.getuid()).pw_name}\nForceCommand /bin/sh {script}\n"
        )
        log = self.root / "sshd.log"
        with log.open("wb") as errors:
            self.sshd = subprocess.Popen([SSHD, "-D", "-e", "-f", str(configuration)], stderr=errors)
        time.sleep(0.1)
        if self.sshd.poll() is not None:
            self.skipTest(f"cannot start an unprivileged sshd: {log.read_text()}")
        known_hosts = self.root / "known_hosts"
        known_hosts.write_text(f"[127.0.0.1]:{port} " + host_key.with_suffix(".pub").read_text())
        config = self.root / "ssh.conf"
        config.write_text(
            f"Host loopback loopback-two\n  HostName 127.0.0.1\n  Port {port}\n"
            f"  User {pwd.getpwuid(os.getuid()).pw_name}\n  IdentityFile {key}\n"
            f"  UserKnownHostsFile {known_hosts}\n  GlobalKnownHostsFile /dev/null\n"
            "  IdentitiesOnly yes\n  StrictHostKeyChecking yes\n  BatchMode yes\n"
        )
        local_bin = self.root / "local-bin"
        local_bin.mkdir()
        wrapper = local_bin / "ssh"
        wrapper.write_text(
            f"#!{sys.executable}\nimport json, os, sys\n"
            f"with open({str(self.ssh_log)!r}, 'a') as log:\n"
            "    log.write(json.dumps({'pid': os.getpid(), 'arguments': sys.argv[1:]}) + '\\n')\n"
            f"os.execv({SSH!r}, [{SSH!r}, '-F', {str(config)!r}, *sys.argv[1:]])\n"
        )
        wrapper.chmod(0o700)
        self.local["PATH"] = str(local_bin) + os.pathsep + os.defpath
        registration = self.cli("machine", "add", "loopback")
        self.assertEqual(registration.returncode, 0, registration.stderr + log.read_text())

    def test_menu_handoff_and_remote_resume_preserve_processes_without_nesting(self):
        process, terminal = self.launch()
        self.wait_client(self.local)
        self.tmux(self.local, "new-session", "-d", "-s", "project")
        self.tmux(self.local, "switch-client", "-t", "project")
        local_pids = self.tmux(self.local, "list-panes", "-a", "-F", "#{pane_pid}").stdout
        os.write(terminal, b"\x02W")
        time.sleep(0.15)
        os.write(terminal, b"\x1b[B\r")
        self.wait_client(self.remote)
        self.assertEqual(self.client_info(self.local), [])
        self.assertIsNone(process.poll())
        self.tmux(self.remote, "split-window", "-h")
        remote_pids = self.tmux(self.remote, "list-panes", "-a", "-F", "#{pane_pid}").stdout
        os.write(terminal, b"\x02W")
        time.sleep(0.15)
        os.write(terminal, b"\x1b[B\r")
        self.wait_client(self.local)
        self.assertEqual(self.client_info(self.remote), [])
        self.assertEqual(self.tmux(self.local, "list-clients", "-F", "#{client_session}").stdout.strip(), "project")
        self.assertEqual(self.tmux(self.local, "list-panes", "-a", "-F", "#{pane_pid}").stdout, local_pids)
        self.assertEqual(self.tmux(self.remote, "list-panes", "-a", "-F", "#{pane_pid}").stdout, remote_pids)
        result = self.request(self.local, 1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.wait_client(self.remote)
        self.detach(process, self.remote)
        process, _ = self.launch()
        self.wait_client(self.remote)
        self.assertEqual(self.client_info(self.local), [])
        self.assertEqual(self.tmux(self.remote, "list-panes", "-a", "-F", "#{pane_pid}").stdout, remote_pids)
        invocations = [json.loads(line)["arguments"] for line in self.ssh_log.read_text().splitlines()]
        self.assertTrue(any(
            "BatchMode=yes" in arguments and "ConnectTimeout=3" in arguments for arguments in invocations
        ))

    def test_registration_while_local_is_open_appears_in_chooser(self):
        process, terminal = self.launch()
        self.wait_client(self.local)
        registration = self.cli("machine", "add", "loopback-two")
        self.assertEqual(registration.returncode, 0, registration.stderr)
        os.write(terminal, b"\x02W")
        time.sleep(0.15)
        os.write(terminal, b"\x1b[B\x1b[B\r")
        self.wait_client(self.remote)
        last = json.loads(self.ssh_log.read_text().splitlines()[-1])["arguments"]
        self.assertEqual(last[last.index("--") + 1], "loopback-two")
        self.assertIsNone(process.poll())

    def test_remote_to_remote_switch_uses_local_ssh(self):
        registration = self.cli("machine", "add", "loopback-two")
        self.assertEqual(registration.returncode, 0, registration.stderr)
        process, _ = self.launch()
        self.wait_client(self.local)
        self.assertEqual(self.request(self.local, 1).returncode, 0)
        self.wait_client(self.remote)
        original = self.client_info(self.remote)
        pids = self.tmux(self.remote, "list-panes", "-a", "-F", "#{pane_pid}").stdout
        self.assertEqual(self.request(self.remote, 2).returncode, 0)
        self.wait_for(
            lambda: bool(self.client_info(self.remote)) and self.client_info(self.remote) != original,
            "second remote destination did not attach",
        )
        self.wait_client(self.remote)
        last = json.loads(self.ssh_log.read_text().splitlines()[-1])["arguments"]
        self.assertEqual(last[last.index("--") + 1], "loopback-two")
        self.assertEqual(self.client_info(self.local), [])
        self.assertEqual(self.tmux(self.remote, "list-panes", "-a", "-F", "#{pane_pid}").stdout, pids)
        self.assertIsNone(process.poll())

    def test_unselected_offline_machine_is_not_contacted_on_startup(self):
        previous = self.ssh_log.read_bytes()
        self.sshd.terminate()
        self.sshd.wait(timeout=5)
        process, _ = self.launch()
        self.wait_client(self.local)
        self.assertEqual(self.ssh_log.read_bytes(), previous)
        self.assertIsNone(process.poll())

    def test_saved_offline_machine_falls_back_to_local(self):
        process, _ = self.launch()
        self.wait_client(self.local)
        self.assertEqual(self.request(self.local, 1).returncode, 0)
        self.wait_client(self.remote)
        self.detach(process, self.remote)
        self.sshd.terminate()
        self.sshd.wait(timeout=5)
        process, _ = self.launch()
        self.wait_client(self.local)
        self.assertIsNone(process.poll())
        self.assertEqual(json.loads(self.state("selection.json").read_text())["destination"], {"kind": "local"})

    def test_connection_loss_returns_local_and_keeps_remote_processes(self):
        process, _ = self.launch()
        self.wait_client(self.local)
        self.assertEqual(self.request(self.local, 1).returncode, 0)
        self.wait_client(self.remote)
        remote_pids = self.tmux(self.remote, "list-panes", "-a", "-F", "#{pane_pid}").stdout
        invocation = json.loads(self.ssh_log.read_text().splitlines()[-1])
        os.kill(invocation["pid"], signal.SIGKILL)
        self.wait_client(self.local)
        self.wait_for(lambda: not self.client_info(self.remote), "disconnected remote client did not detach")
        self.assertIsNone(process.poll())
        self.assertEqual(self.tmux(self.remote, "list-panes", "-a", "-F", "#{pane_pid}").stdout, remote_pids)


if __name__ == "__main__":
    subprocess.run(["cargo", "build"], cwd=ROOT, check=True)
    unittest.main()
