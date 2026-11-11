#!/usr/bin/env python3
"""Native-pipe/PTY SSH stand-in used only by the isolated control integration tests."""
import json
import os
from pathlib import Path
import pty
import selectors
import shlex
import signal
import struct
import subprocess
import sys
import termios
import time
import tty


def main():
    root = Path(os.environ["TEST_CONTROL_ROOT"])
    arguments = sys.argv[1:]
    if "-O" in arguments:
        host = arguments[-1]
        if arguments[arguments.index("-O") + 1] == "check":
            return 1 if any((root / f"{kind}-{host}").exists() for kind in ["auth", "hostkey"]) else 0
        return 0

    if "-MNf" in arguments:
        host = arguments[-1]
        if (root / f"hostkey-{host}").exists():
            print("Host key verification failed.", file=sys.stderr)
            return 255
        (root / f"auth-{host}").unlink(missing_ok=True)
        return 0

    host, command = arguments[-2:]
    homes = json.loads((root / "homes.json").read_text())
    home = Path(homes[host])
    parts = shlex.split(command)
    assert parts[:2] == ["bash", "-c"]
    mode = parts[5]
    assert mode in ["bridge", "terminal"]
    assert "BatchMode=yes" in arguments and "StrictHostKeyChecking=yes" in arguments
    log(root, {"host": host, "mode": mode, "pid": os.getpid(), "command": command, "arguments": arguments})

    if (root / f"offline-{host}").exists():
        print("Connection reset by peer", file=sys.stderr)
        return 255
    if (root / f"auth-{host}").exists():
        print("Permission denied (publickey,password).", file=sys.stderr)
        return 255
    if (root / f"hostkey-{host}").exists():
        print("Host key verification failed.", file=sys.stderr)
        return 255

    environment = dict(os.environ, HOME=str(home), TERM="xterm-256color")
    for name in ["TMUX", "TMUX_PANE", "TMUX_AGENTS_HOME", "TMUX_AGENTS_BIN"]:
        environment.pop(name, None)
    if mode == "terminal":
        return terminal(command, environment, home)
    return bridge(command, environment, home, root, host)


def log(root, value):
    with (root / "ssh.jsonl").open("a") as output:
        output.write(json.dumps(value) + "\n")


def bridge(command, environment, home, root, host):
    child = subprocess.Popen(
        ["bash", "-c", command], cwd=home, env=environment,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
    )
    selector = selectors.DefaultSelector()
    selector.register(sys.stdin.buffer, selectors.EVENT_READ, "input")
    selector.register(child.stdout, selectors.EVENT_READ, "output")
    selector.register(child.stderr, selectors.EVENT_READ, "error")
    incoming = bytearray()
    outgoing = bytearray()
    dropped_id = None
    result = None
    try:
        while result is None:
            for key, _ in selector.select(0.1):
                data = os.read(key.fd, 8192)
                if not data:
                    selector.unregister(key.fileobj)
                    if key.data == "input":
                        child.stdin.close()
                    elif key.data == "output":
                        result = child.wait(timeout=5)
                        break
                    continue

                if key.data == "input":
                    incoming.extend(data)
                    for value in frames(incoming):
                        log(root, {"host": host, "method": value["method"], "id": value["id"]})
                        marker = root / f"drop-create-{host}"
                        if value["method"] == "workspace.create" and marker.exists():
                            marker.unlink()
                            dropped_id = value["id"]
                    child.stdin.write(data)
                    child.stdin.flush()
                elif key.data == "output":
                    outgoing.extend(data)
                    for value, raw in frames(outgoing, include_bytes=True):
                        if value["id"] == dropped_id:
                            result = 255
                            break
                        sys.stdout.buffer.write(raw)
                        sys.stdout.buffer.flush()
                else:
                    sys.stderr.buffer.write(data)
                    sys.stderr.buffer.flush()
    finally:
        selector.close()
        for stream in [child.stdin, child.stdout, child.stderr]:
            if not stream.closed:
                stream.close()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=3)
    return result


def frames(buffer, include_bytes=False):
    values = []
    while len(buffer) >= 4:
        size, = struct.unpack("!I", buffer[:4])
        assert 0 < size <= 65536
        if len(buffer) < size + 4:
            break
        raw = bytes(buffer[:size + 4])
        del buffer[:size + 4]
        value = json.loads(raw[4:])
        values.append((value, raw) if include_bytes else value)
    return values


def terminal(command, environment, home):
    saved = termios.tcgetattr(sys.stdin.fileno())
    tty.setraw(sys.stdin.fileno())
    child, remote = pty.fork()
    if child == 0:
        os.chdir(home)
        os.execvpe("bash", ["bash", "-c", command], environment)

    import fcntl
    size = fcntl.ioctl(sys.stdin.fileno(), termios.TIOCGWINSZ, b"\0" * 8)
    fcntl.ioctl(remote, termios.TIOCSWINSZ, size)
    selector = selectors.DefaultSelector()
    selector.register(sys.stdin, selectors.EVENT_READ, "input")
    selector.register(remote, selectors.EVENT_READ, "output")
    result = 255
    try:
        running = True
        while running:
            for key, _ in selector.select(0.1):
                try:
                    data = os.read(key.fd, 8192)
                except OSError:
                    data = b""
                if not data:
                    running = False
                    break
                if key.data == "input":
                    os.write(remote, data)
                else:
                    sys.stdout.buffer.write(data)
                    sys.stdout.buffer.flush()
    finally:
        selector.close()
        os.close(remote)
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSAFLUSH, saved)
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            waited, status = os.waitpid(child, os.WNOHANG)
            if waited:
                result = os.waitstatus_to_exitcode(status)
                child = None
                break
            time.sleep(0.05)
        if child is not None:
            os.killpg(child, signal.SIGKILL)
            os.waitpid(child, 0)
    return result


if __name__ == "__main__":
    sys.exit(main())
