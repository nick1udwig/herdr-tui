"""Selection and clipboard as in Herdr: double-click word, triple-click line, selections
across history pages, program copies, and image paste into a remote terminal."""
import base64
import fcntl
import json
import os
import pathlib
import pty
import select
import shlex
import struct
import subprocess
import sys
import termios
import time

from fixture import Core

binary = pathlib.Path(os.environ.get(
    "TUI_TEST_BINARY", pathlib.Path(__file__).resolve().parents[1] / "target/debug/herder-tui"
)).resolve()
tools = pathlib.Path(os.environ.get("TMPDIR", "/tmp")) / f"tui-clip-{os.getpid()}"
tools.mkdir()
copied = tools / "copied.txt"
png = b"\x89PNG\r\n\x1a\n" + b"\0" * 32
(tools / "wl-copy").write_text(f"#!/bin/sh\ncat > {copied}\n")
(tools / "wl-paste").write_text("#!/bin/sh\n[ \"$2\" = image/png ] || exit 1\nprintf '\\211PNG\\r\\n\\032\\n'; head -c 32 /dev/zero\n")
for tool in ("wl-copy", "wl-paste"):
    (tools / tool).chmod(0o700)
env = {"WAYLAND_DISPLAY": "fixture", "PATH": f"{tools}:{os.environ['PATH']}"}
local = Core("selection", env)
for key in ("SSH_CONNECTION", "SSH_TTY", "VSCODE_IPC_HOOK_CLI", "WSL_DISTRO_NAME", "WSL_INTEROP"):
    local.env.pop(key, None)
remote = Core("selection-remote")
process = master = slave = None
log = bytearray()
try:
    via = local.root / "remote-command"
    via.write_text("#!/bin/sh\n" + "".join(f"export {k}={shlex.quote(remote.env[k])}\n" for k in ("HC_SESSION", "XDG_RUNTIME_DIR", "XDG_STATE_HOME", "XDG_CONFIG_HOME")) + 'exec sh -c "$1"\n')
    via.chmod(0o700)
    subprocess.run(["hc", "machine", "add", "--via", str(via), "--name", "far", "--session", remote.env["HC_SESSION"]], env=local.env, check=True, capture_output=True, timeout=30)

    def do(command, args, machine=None):
        extra = ["--machine", machine] if machine else []
        return json.loads(subprocess.check_output([str(binary), *extra, "do", command, json.dumps(args)], env=local.env, timeout=20))

    words = do("space.new", {"name": "Words", "argv": ["sh", "-c", "printf 'alpha beta-gamma delta\\n'; sleep 180"]})
    pages = do("tab.new", {"space": words["space"], "argv": ["sh", "-c", "i=1; while [ $i -le 60 ]; do echo page$i; i=$((i+1)); done; sleep 180"]})
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 100, 0, 0))
    process = subprocess.Popen([str(binary), "--instance", "selection"], env=local.env, stdin=slave, stdout=slave, stderr=slave)

    def read_for(seconds=.1):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if select.select([master], [], [], .02)[0]:
                log.extend(os.read(master, 65536))

    def view(command, args, instance="selection"):
        out = subprocess.run([str(binary), "--instance", instance, "view", command, json.dumps(args)], env=local.env, capture_output=True, timeout=5)
        return json.loads(out.stdout) if out.returncode == 0 else None

    def clipboard(expected_change, what):
        end = time.monotonic() + 8
        while time.monotonic() < end:
            read_for(.1)
            if copied.exists() and expected_change(copied.read_text()):
                return copied.read_text()
        raise AssertionError((what, copied.read_text() if copied.exists() else None))

    def click(x, y, times=1):
        for _ in range(times):
            os.write(master, f"\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m".encode())
            time.sleep(.05)

    read_for(1.5)
    view("pane.focus", {"id": words["pane"]})
    read_for(.5)
    # Content starts after the 26-column sidebar and the tab row: column 27, row 2.
    click(33, 2, 2)
    clipboard(lambda t: t == "beta-gamma", "double-click word")
    copied.unlink()
    time.sleep(.5)
    click(30, 2, 3)
    clipboard(lambda t: t == "alpha beta-gamma delta", "triple-click line")
    copied.unlink()

    # Copy mode: select upward past the top of the screen; history scrolls under it.
    view("pane.focus", {"id": pages["pane"]})
    read_for(.5)
    os.write(master, b"\x02[")
    read_for(.3)
    os.write(master, b"v" + b"k" * 30 + b"0")
    read_for(.5)
    os.write(master, b"y")
    text = clipboard(lambda t: "page60" in t, "selection across pages")
    lines = text.splitlines()
    assert len(lines) > 20 and lines[-1] == "page60", lines

    # Image paste: ctrl+v in a terminal on another machine puts the image there and pastes its path.
    remote_pane = do("space.new", {"name": "Far", "folder": str(remote.root), "argv": ["sh", "-c", "read path; cp \"$path\" " + str(remote.root / "received.png") + "; sleep 180"]}, machine="far")
    view("machine", {"machine": "far"})
    read_for(1)
    view("pane.focus", {"id": remote_pane["pane"]})
    read_for(.5)
    os.write(master, b"\x16")
    read_for(1)
    os.write(master, b"\r")
    end = time.monotonic() + 10
    while not (remote.root / "received.png").exists() and time.monotonic() < end:
        read_for(.2)
    assert (remote.root / "received.png").read_bytes() == png
    os.write(master, b"\x02q")
    assert process.wait(timeout=5) == 0
    os.close(master)
    os.close(slave)
    master = slave = None
    print("PASS: double-click word, triple-click line, copy-mode selection across history pages, native clipboard writer, image paste to a remote terminal")

    # A program's copies must reach the host clipboard through either viewer.
    text = "Codex \u590d\u5236\nsecond line"
    large = "\u4e2d" * 6000 + "\nlast line"
    requests = [
        (b"\x1b]52;c;" + base64.b64encode(text.encode()) + b"\x07", text),
        (b"\x1b]52;c;" + base64.b64encode(large.encode()) + b"\x1b\\", large),
        (b"\x1b]52;c;%%%\x07", None),
        (b"\x1b]52;c;/w==\x07", None),
        (b"\x1b]52;c;?\x07", None),
        (b"\x1b]52;p;cHJpbWFyeQ==\x07", None),
        (b"\x1b]52;c;\x07", ""),
    ]
    program = (
        "import os, time, tty\n"
        "tty.setraw(0)\n"
        "os.write(1, b'COPY_READY')\n"
        f"for i, sequence in enumerate({[sequence for sequence, _ in requests]!r}):\n"
        "    os.read(0, 1)\n"
        "    for chunk in (sequence[:5], sequence[5:-1], sequence[-1:]):\n"
        "        os.write(1, chunk)\n"
        "        time.sleep(.02)\n"
        "    os.write(1, ('\\r\\nCOPY_DONE_%d\\r\\n' % i).encode())\n"
        "while True:\n"
        "    os.read(0, 1)\n"
    )
    for ssh in (False, True):
        for single in (False, True):
            created = do("space.new", {
                "name": "Program copies", "folder": str(local.root),
                "argv": [sys.executable, "-u", "-c", program],
            })
            viewer_env = dict(local.env)
            if ssh:
                viewer_env["SSH_TTY"] = "/dev/pts/fixture"
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 100, 0, 0))
            original = termios.tcgetattr(slave)
            target = [created["terminal"]["address"]] if single else []
            process = subprocess.Popen(
                [str(binary), "--instance", "program-copy", *target], env=viewer_env,
                stdin=slave, stdout=slave, stderr=slave,
            )
            log.clear()
            if not single:
                end = time.monotonic() + 10
                while view("pane.focus", {"id": created["pane"]}, "program-copy") is None:
                    assert time.monotonic() < end, "program-copy viewer did not start"
                    read_for(.05)

            def wait_for(marker):
                end = time.monotonic() + 10
                while marker not in log and time.monotonic() < end:
                    read_for(.05)
                assert marker in log, (ssh, single, marker, bytes(log[-2000:]))

            wait_for(b"COPY_READY")
            for i, (_, expected) in enumerate(requests):
                copied.unlink(missing_ok=True)
                log.clear()
                os.write(master, b"x")
                wait_for(f"COPY_DONE_{i}".encode())
                if expected is None:
                    assert not copied.exists(), (ssh, single, i)
                    assert b"\x1b]52;" not in log, (ssh, single, i)
                elif ssh:
                    assert not copied.exists(), "SSH copy used the remote desktop clipboard"
                    sequence = b"\x1b]52;c;" + base64.b64encode(expected.encode()) + b"\x07"
                    assert sequence in log, (ssh, single, i, bytes(log[-2000:]))
                else:
                    assert copied.exists(), (ssh, single, i, "program copy was lost")
                    assert copied.read_text() == expected, (ssh, single, i)
                    assert b"\x1b]52;" not in log, (ssh, single, i)
            os.write(master, b"\x11" if single else b"\x02q")
            assert process.wait(timeout=5) == 0
            assert termios.tcgetattr(slave) == original
            os.close(master)
            os.close(slave)
            master = slave = None
    print("PASS: program copies use native helpers or SSH OSC 52 in both views; split Unicode, large copies, malformed data, reads, primary selection and clear")
finally:
    if process and process.poll() is None:
        process.kill()
        process.wait()
    if master is not None:
        os.close(master)
        os.close(slave)
    local.close()
    remote.close()
    for tool in tools.iterdir():
        tool.unlink()
    tools.rmdir()
