#!/usr/bin/env python3
"""Offline installer checks: fake GitHub downloads, real SHA-256 verification."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class Installer(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ta install ")
        self.root = Path(self.temporary.name).resolve()
        (self.root / "scripts").mkdir()
        shutil.copyfile(ROOT / "scripts/install.sh", self.root / "scripts/install.sh")
        shutil.copyfile(ROOT / "tmux-agents.tmux", self.root / "tmux-agents.tmux")
        (self.root / "RELEASE_TAG").write_text('pre-0.1\n')
        self.payload = b"#!/bin/sh\nprintf 'not launched\\n'\n"
        (self.root / "payload").write_bytes(self.payload)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.env = dict(os.environ, PATH=f"{self.tools}:{os.environ['PATH']}", TEST_ROOT=str(self.root), TEST_OS="Darwin", TEST_ARCH="arm64", EXPECTED_TARGET="aarch64-apple-darwin", EXPECTED_TAG="pre-0.1")
        self.env.pop("TMUX_AGENTS_BIN", None)
        self.make_tool("uname", """
import os, sys
print(os.environ['TEST_OS'] if sys.argv[1] == '-s' else os.environ['TEST_ARCH'])
""")
        self.make_tool("curl", """
import hashlib, json, os, pathlib, sys, time
root = pathlib.Path(os.environ['TEST_ROOT'])
with (root / 'downloads').open('a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
url = sys.argv[-1]
tag = os.environ['EXPECTED_TAG']
asset = 'tmux-agents-' + tag + '-' + os.environ['EXPECTED_TARGET']
base = 'https://github.com/adonis0147/tmux-agents/releases/download/' + tag
assert url in [base + '/' + asset, base + '/SHA256SUMS'], url
assert sys.argv[sys.argv.index('--proto') + 1] == '=https'
assert sys.argv[sys.argv.index('--proto-redir') + 1] == '=https'
output = pathlib.Path(sys.argv[sys.argv.index('--output') + 1])
time.sleep(float(os.environ.get('TEST_DELAY', '0')))
if os.environ.get('FAIL_DOWNLOAD') == ('checksum' if url.endswith('/SHA256SUMS') else 'binary'):
    output.write_bytes(b'partial download')
    sys.exit(22)
payload = (root / 'payload').read_bytes()
if url.endswith('/SHA256SUMS'):
    digest = hashlib.sha256(payload).hexdigest()
    mode = os.environ.get('CHECKSUM_MODE', '')
    if mode == 'mismatch': digest = '0' * 64
    if mode == 'invalid_hash': digest = 'z' * 64
    if mode == 'short_hash': digest = 'abc'
    targets = ['x86_64-apple-darwin', 'aarch64-apple-darwin', 'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl']
    names = ['tmux-agents-' + tag + '-' + target for target in targets]
    if mode == 'wrong_name': names[names.index(asset)] = 'other-file'
    checksum = ''.join(digest + '  ' + name + '\\n' for name in names)
    if mode == 'malformed': checksum = 'not a checksum\\n'
    if mode == 'extra_line': checksum += digest + '  ' + asset + '\\n'
    output.write_text(checksum)
else:
    output.write_bytes(payload)
""")
        self.make_tool("tmux", """
import json, os, pathlib, sys
with (pathlib.Path(os.environ['TEST_ROOT']) / 'tmux-calls').open('a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
if sys.argv[1:] == ['-V']: print('tmux 3.8')
""")

    def tearDown(self):
        self.temporary.cleanup()

    def make_tool(self, name, body):
        path = self.tools / name
        path.write_text(f"#!{sys.executable}\n" + body)
        path.chmod(0o700)

    def install(self, check=True, **environment):
        return subprocess.run(["sh", self.root / "scripts/install.sh"], env=dict(self.env, **environment), capture_output=True, text=True, timeout=10, check=check)

    def binary(self, target=None):
        return self.root / "bin/pre-0.1" / (target or self.env["EXPECTED_TARGET"]) / "tmux-agents"

    def downloads(self):
        path = self.root / "downloads"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def assert_no_staging_files(self):
        self.assertEqual(list(self.root.glob("bin/*/*/.install.*")), [])

    def test_platform_mapping_and_verified_install(self):
        for system, architecture, target in [
            ("Darwin", "x86_64", "x86_64-apple-darwin"),
            ("Darwin", "arm64", "aarch64-apple-darwin"),
            ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
            ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
        ]:
            with self.subTest(system=system, architecture=architecture):
                result = self.install(TEST_OS=system, TEST_ARCH=architecture, EXPECTED_TARGET=target)
                binary = self.binary(target)
                self.assertEqual(result.stdout.strip(), str(binary))
                self.assertEqual(binary.read_bytes(), self.payload)
                self.assertEqual(binary.stat().st_mode & 0o777, 0o700)
        self.assertEqual(len(self.downloads()), 8)
        self.assert_no_staging_files()

    def test_v_prefixed_tags_remain_supported(self):
        (self.root / "RELEASE_TAG").write_text("v0.1.0\n")
        result = self.install(EXPECTED_TAG="v0.1.0")
        binary = self.root / "bin/v0.1.0" / self.env["EXPECTED_TARGET"] / "tmux-agents"
        self.assertEqual(result.stdout.strip(), str(binary))
        self.assertEqual(binary.read_bytes(), self.payload)

    def test_cached_binary_is_reused_offline(self):
        self.install()
        binary = self.binary()
        before = binary.stat().st_mtime_ns
        result = self.install(FAIL_DOWNLOAD="binary")
        self.assertEqual(result.stdout.strip(), str(binary))
        self.assertEqual(binary.stat().st_mtime_ns, before)
        self.assertEqual(len(self.downloads()), 2)

    def test_explicit_override_skips_installation(self):
        override = self.root / "custom executable"
        override.write_bytes(self.payload)
        override.chmod(0o700)
        self.assertEqual(self.install(TMUX_AGENTS_BIN=str(override), TEST_OS="unsupported").stdout.strip(), str(override))
        self.assertEqual(self.downloads(), [])
        self.assertFalse((self.root / "bin").exists())
        result = self.install(TMUX_AGENTS_BIN=str(self.root / "missing"), check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("TMUX_AGENTS_BIN", result.stderr)
        self.assertNotEqual(self.install(TMUX_AGENTS_BIN="relative/path", check=False).returncode, 0)

    def test_download_failure_preserves_existing_file(self):
        binary = self.binary()
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"previous non-executable file")
        binary.chmod(0o600)
        result = self.install(FAIL_DOWNLOAD="binary", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("download failed", result.stderr)
        self.assertEqual(binary.read_bytes(), b"previous non-executable file")
        self.assert_no_staging_files()

    def test_rejects_invalid_checksums(self):
        for mode in ["mismatch", "wrong_name", "malformed", "extra_line", "invalid_hash", "short_hash"]:
            with self.subTest(mode=mode):
                result = self.install(CHECKSUM_MODE=mode, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.binary().exists())
                self.assert_no_staging_files()

    def test_shasum_fallback(self):
        portable = self.root / "portable tools"
        portable.mkdir()
        for command in ["sh", "awk", "grep", "dirname", "mkdir", "mktemp", "chmod", "mv", "rm", "shasum"]:
            executable = shutil.which(command)
            self.assertIsNotNone(executable, f"test prerequisite: {command}")
            (portable / command).symlink_to(executable)
        result = self.install(PATH=f"{self.tools}:{portable}")
        self.assertEqual(result.stdout.strip(), str(self.binary()))
        self.assertEqual(self.binary().read_bytes(), self.payload)

    def test_invalid_tag_fails_before_download(self):
        for tag in ["../../invalid\n", "\n", "pre-0.1\nextra\n", "pre-0.1 with spaces\n"]:
            with self.subTest(tag=tag):
                (self.root / "RELEASE_TAG").write_text(tag)
                result = self.install(check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("RELEASE_TAG", result.stderr)
                self.assertEqual(self.downloads(), [])

    def test_unsupported_platform_fails_before_download(self):
        result = self.install(TEST_OS="FreeBSD", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported platform", result.stderr)
        self.assertEqual(self.downloads(), [])

    def test_concurrent_installation_is_atomic(self):
        environment = dict(self.env, TEST_DELAY="0.05")
        processes = [subprocess.Popen(["sh", self.root / "scripts/install.sh"], env=environment, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(4)]
        results = [(process, *process.communicate(timeout=10)) for process in processes]
        for process, output, error in results:
            self.assertEqual(process.returncode, 0, error)
            self.assertEqual(output.strip(), str(self.binary()))
        self.assertEqual(self.binary().read_bytes(), self.payload)
        self.assert_no_staging_files()

    def test_plugin_installs_before_registering_client(self):
        result = subprocess.run(["sh", self.root / "tmux-agents.tmux"], env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "tmux-calls").read_text().splitlines()]
        self.assertIn(["set-option", "-g", "@tmux-agents-bin", str(self.binary())], calls)
        self.assertTrue(any(call[0] == "bind-key" for call in calls))
        self.assertEqual(self.binary().read_bytes(), self.payload)

    def test_failed_install_does_not_register_client(self):
        result = subprocess.run(["sh", self.root / "tmux-agents.tmux"], env=dict(self.env, FAIL_DOWNLOAD="checksum"), capture_output=True, text=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        calls = [json.loads(line) for line in (self.root / "tmux-calls").read_text().splitlines()]
        self.assertFalse(any(call[0] == "bind-key" for call in calls))
        self.assertFalse(any(call[:2] == ["set-option", "-s"] for call in calls))
        self.assertFalse(self.binary().exists())


if __name__ == "__main__":
    unittest.main()
