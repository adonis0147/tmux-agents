"""Machine registration tests; SSH is stubbed and no network connections are made."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target" / "debug" / "tmux-agents"


class MachineTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ta-machines-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.bin = self.directory / "bin"
        self.bin.mkdir()
        self.log = self.directory / "ssh-arguments.json"
        self.environment = dict(
            os.environ,
            HOME=str(self.directory),
            XDG_CONFIG_HOME=str(self.directory / "config ' #literal"),
            PATH=str(self.bin) + os.pathsep + os.defpath,
            SSH_TEST_LOG=str(self.log),
            SSH_TEST_STDOUT="tmux 3.6\ntmux-agents 0.1.0\n",
            SSH_TEST_EXIT="0",
            SSH_TEST_EXECUTE_PROBE="",
        )
        self.write_executable(
            self.bin / "ssh",
            f"#!{sys.executable}\n"
            "import json, os, subprocess, sys\n"
            "with open(os.environ['SSH_TEST_LOG'], 'w') as log:\n"
            "    json.dump(sys.argv[1:], log)\n"
            "if os.environ.get('SSH_TEST_EXECUTE_PROBE'):\n"
            "    environment = dict(os.environ, PATH=os.environ['SSH_TEST_REMOTE_PATH'])\n"
            "    sys.exit(subprocess.run(['/bin/sh', '-c', sys.argv[-1]], env=environment).returncode)\n"
            "sys.stdout.write(os.environ['SSH_TEST_STDOUT'])\n"
            "sys.stderr.write(os.environ.get('SSH_TEST_STDERR', ''))\n"
            "sys.exit(int(os.environ['SSH_TEST_EXIT']))\n",
        )

    def write_executable(self, path, text):
        path.write_text(text)
        path.chmod(0o700)

    @property
    def registry(self):
        base = self.environment.get("XDG_CONFIG_HOME") or str(self.directory / ".config")
        return Path(base) / "tmux-agents" / "machines.json"

    def run_cli(self, *arguments):
        return subprocess.run(
            [str(BINARY), *arguments], env=self.environment, capture_output=True, text=True, timeout=10
        )

    def add(self, destination="dev@buildbox"):
        return self.run_cli("machine", "add", destination)

    def test_help_and_version_work_without_terminal(self):
        self.environment["TMUX"] = "/some/socket,123,0"
        self.assertIn("machine add", self.run_cli("--help").stdout)
        version = self.run_cli("--version")
        self.assertEqual(version.returncode, 0)
        self.assertEqual(version.stdout, "tmux-agents 0.1.0\n")

    def test_rejects_missing_or_extra_command_arguments(self):
        for arguments in [("machine",), ("machine", "add"), ("machine", "list"),
                          ("machine", "add", "dev@host", "extra")]:
            with self.subTest(arguments=arguments):
                result = self.run_cli(*arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("unexpected arguments", result.stderr)
                self.assertFalse(self.log.exists())

    def test_add_uses_literal_ssh_destination_and_private_registry(self):
        self.environment["TMUX"] = "/some/socket,123,0"
        result = self.add()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Added machine dev@buildbox", result.stdout)
        arguments = json.loads(self.log.read_text())
        self.assertEqual(
            arguments[:-1], ["-T", "-o", "ConnectTimeout=10", "-o", "ConnectionAttempts=1", "--", "dev@buildbox"]
        )
        self.assertIn("tmux -V && tmux-agents --version", arguments[-1])
        self.assertNotIn("dev@buildbox", arguments[-1])
        self.assertEqual(json.loads(self.registry.read_text()), {"version": 1, "machines": ["dev@buildbox"]})
        self.assertEqual(self.registry.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.registry.parent.stat().st_mode & 0o777, 0o700)

    def test_accepts_ssh_aliases_and_ipv6(self):
        destinations = ["build-box", "dev@build.example", "dev@2001:db8::1", "dev@fe80::1%eth0"]
        for destination in destinations:
            with self.subTest(destination=destination):
                result = self.add(destination)
                self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(self.registry.read_text())["machines"], destinations)

    def test_rejects_unsafe_or_malformed_destinations_before_ssh(self):
        destinations = [
            "", "-oProxyCommand=touch", "dev@-host", "@host", "user@", "a@b@host", "user name@host",
            "user@host\n", "user@host;touch marker", "user@$(touch marker)", "user@host:2222", "ssh://host",
            "user@host/path", "user@host'", "user@[::1]", "user@fe80::1%", "user@host\x1b",
        ]
        for destination in destinations:
            with self.subTest(destination=destination):
                result = self.add(destination)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("invalid SSH", result.stderr)
                self.assertFalse(self.log.exists())
                self.assertFalse(self.registry.exists())

    def test_missing_ssh_does_not_create_registry(self):
        (self.bin / "ssh").unlink()
        self.environment["PATH"] = str(self.bin)
        result = self.add()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot execute ssh", result.stderr)
        self.assertFalse(self.registry.exists())

    def test_ssh_failure_preserves_registry_and_exposes_diagnostic(self):
        self.assertEqual(self.add().returncode, 0)
        before = self.registry.read_bytes()
        self.environment.update(SSH_TEST_EXIT="255", SSH_TEST_STDERR="Permission denied (publickey).\n")
        result = self.add("dev@offline")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Permission denied", result.stderr)
        self.assertIn("SSH checks failed for dev@offline", result.stderr)
        self.assertEqual(self.registry.read_bytes(), before)

    def test_rejects_incompatible_or_unexpected_remote_versions(self):
        outputs = [
            "tmux 3.5a\ntmux-agents 0.1.0\n", "tmux next-3.7\ntmux-agents 0.1.0\n",
            "tmux 3.6\ntmux-agents 9.0.0\n", "banner\ntmux 3.6\ntmux-agents 0.1.0\n", "",
        ]
        for output in outputs:
            with self.subTest(output=output):
                self.environment["SSH_TEST_STDOUT"] = output
                result = self.add()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.registry.exists())

    def test_accepts_tmux_patch_suffix(self):
        self.environment["SSH_TEST_STDOUT"] = "tmux 3.6a\ntmux-agents 0.1.0\n"
        result = self.add()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_readding_is_idempotent_and_rechecks_ssh(self):
        self.assertEqual(self.add().returncode, 0)
        before = self.registry.read_bytes()
        self.log.unlink()
        result = self.add()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already registered", result.stdout)
        self.assertTrue(self.log.exists())
        self.assertEqual(self.registry.read_bytes(), before)

    def test_bad_registry_remains_unchanged(self):
        self.registry.parent.mkdir(parents=True)
        fixtures = [
            b"not json", b'{"version":99,"machines":[]}',
            b'{"version":1,"machines":["dev@host","dev@host"]}',
            b'{"version":1,"machines":["-oProxyCommand=evil"]}',
        ]
        for data in fixtures:
            with self.subTest(data=data):
                self.registry.write_bytes(data)
                result = self.add()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.registry.read_bytes(), data)

    def test_concurrent_additions_do_not_lose_entries(self):
        processes = [
            subprocess.Popen(
                [str(BINARY), "machine", "add", destination],
                env=self.environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            for destination in ["dev@first", "dev@second"]
        ]
        for process in processes:
            _, errors = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, errors)
        self.assertEqual(set(json.loads(self.registry.read_text())["machines"]), {"dev@first", "dev@second"})

    def test_configuration_path_fallback_and_validation(self):
        self.environment["XDG_CONFIG_HOME"] = "relative"
        result = self.add()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("XDG_CONFIG_HOME must be an absolute path", result.stderr)
        self.assertFalse(self.log.exists())
        self.environment.pop("XDG_CONFIG_HOME")
        result = self.add()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.directory / ".config/tmux-agents/machines.json").exists())

    def test_remote_probe_checks_real_shell_dependency_failures(self):
        remote = self.directory / "remote-bin"
        remote.mkdir()
        (remote / "sh").symlink_to("/bin/sh")
        self.environment.update(SSH_TEST_EXECUTE_PROBE="1", SSH_TEST_REMOTE_PATH=str(remote))
        result = self.add()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("remote tmux is not installed", result.stderr)
        self.assertFalse(self.registry.exists())
        self.write_executable(remote / "tmux", "#!/bin/sh\nprintf 'tmux 3.6\\n'\n")
        result = self.add()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("remote tmux-agents is not installed", result.stderr)
        self.assertFalse(self.registry.exists())
        self.write_executable(remote / "tmux-agents", "#!/bin/sh\nprintf 'tmux-agents 0.1.0\\n'\n")
        result = self.add()
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    subprocess.run(["cargo", "build"], cwd=ROOT, check=True)
    unittest.main()
