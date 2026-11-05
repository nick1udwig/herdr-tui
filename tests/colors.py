"""Core streams retain host color indices in both workspace and single-terminal views."""
import fcntl
import json
import os
import pathlib
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

from fixture import Core

binary = pathlib.Path(os.environ.get(
    "TUI_TEST_BINARY", pathlib.Path(__file__).resolve().parents[1] / "target/debug/herder-tui"
)).resolve()
core = Core("colors")
core.env.pop("NO_COLOR", None)
process = master = slave = None
try:
    created = json.loads(subprocess.check_output([
        str(binary), "do", "space.new", json.dumps({
            "name": "Colors", "folder": str(core.root),
            "argv": ["sh", "-c", (
                "printf '\\033[31;44mPALETTE_RED\\033[0m "
                "\\033[38;5;202mPALETTE_202\\033[0m "
                "\\033[38;2;1;2;3mTRUECOLOR\\033[0m'; cat"
            )],
        }),
    ], env=core.env, timeout=15))
    address = created["terminal"]["address"]
    for target in ([], [address]):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        original = termios.tcgetattr(slave)
        process = subprocess.Popen(
            [str(binary), "--instance", "colors", *target], env=core.env,
            stdin=slave, stdout=slave, stderr=slave,
        )
        output = bytearray()

        def wait_for(marker):
            deadline = time.monotonic() + 10
            while marker not in output and time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    output.extend(os.read(master, 65536))
            assert marker in output, bytes(output[-4000:])

        wait_for(b"TRUECOLOR")
        for color in (b"38;5;1", b"48;5;4", b"38;5;202", b"38;2;1;2;3"):
            assert re.search(
                rb"\x1b\[(?:\d+;)*" + color + rb"(?:;\d+)*m", output
            ), (color, bytes(output))
        os.write(master, b"printf INPUT_OK\r")
        wait_for(b"INPUT_OK")
        os.write(master, b"\x11" if target else b"\x02q")
        assert process.wait(timeout=5) == 0
        assert termios.tcgetattr(slave) == original
        assert core.rpc("terminal.get", {"terminal": address})["state"] == "live"
        os.close(master)
        os.close(slave)
        master = slave = None
    print("PASS: core repaint preserves host palette and RGB in both views; input, detach and terminal restoration work")

    query = r"""
import json, os, re, sys, termios, tty
old = termios.tcgetattr(0)
try:
    tty.setraw(0)
    print("QUERY_READY_" + sys.argv[1], flush=True)
    os.read(0, 1)
    os.write(1, b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\")
    replies = b""
    while len(re.findall(rb"\x1b\](?:10|11);rgb:([0-9a-f/]+)(?:\x07|\x1b\\)", replies)) < 2:
        replies += os.read(0, 256)
    colors = {}
    for code, rgb in re.findall(rb"\x1b\](10|11);rgb:([0-9a-f/]+)", replies):
        colors[code.decode()] = [int(part[:2], 16) for part in rgb.split(b"/")]
    bg = colors["11"]
    light = sum(c * w for c, w in zip(bg, (.299, .587, .114))) >= 128
    alpha, top = (.04, 0) if light else (.12, 255)
    prompt = [int(top * alpha + c * (1 - alpha)) for c in bg]
    print("\r\nCOLOR_REPORT_" + sys.argv[1] + "=" + json.dumps([colors["10"], bg], separators=(",", ":")), flush=True)
    print("\x1b[48;2;%d;%d;%dmPROMPT_COLOR\x1b[0m" % tuple(prompt), flush=True)
    while True:
        os.read(0, 256)
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
"""
    for background in ((0, 0, 0), (240, 240, 240), None):
        for target_workspace, interrupt_probe in ((True, False), (False, False), (True, True), (False, True)):
            if interrupt_probe and background is not None:
                continue
            core.close()
            core = Core("colors")
            core.env.pop("NO_COLOR", None)
            case = f"{background[0] if background is not None else 'none'}_{int(target_workspace)}"
            created = json.loads(subprocess.check_output([
                str(binary), "do", "space.new", json.dumps({
                    "name": "Background", "folder": str(core.root),
                    "argv": [sys.executable, "-u", "-c", query, case],
                }),
            ], env=core.env, timeout=15))
            address = created["terminal"]["address"]
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
            original = termios.tcgetattr(slave)
            process = subprocess.Popen(
                [str(binary), "--instance", "colors", *([] if target_workspace else [address])],
                env=core.env, stdin=slave, stdout=slave, stderr=slave,
            )
            output = bytearray()
            answered = set()

            def wait_for(marker):
                deadline = time.monotonic() + 10
                while marker not in output and time.monotonic() < deadline:
                    if not select.select([master], [], [], .05)[0]:
                        continue
                    output.extend(os.read(master, 65536))
                    if background is None:
                        continue
                    for code, rgb in ((10, (216, 222, 233)), (11, background)):
                        prefix = b"\x1b]" + str(code).encode() + b";?"
                        if prefix in output and code not in answered:
                            components = "/".join(f"{c:02x}{c:02x}" for c in rgb)
                            os.write(master, f"\x1b]{code};rgb:{components}\x1b\\".encode())
                            answered.add(code)
                    if b"\x1b[c" in output and "da" not in answered:
                        os.write(master, b"\x1b[?62;22c")
                        answered.add("da")
                assert marker in output, bytes(output[-4000:])

            if interrupt_probe:
                wait_for(b"\x1b]11;?")
                process.send_signal(signal.SIGINT)
            else:
                wait_for(("QUERY_READY_" + case).encode())
                os.write(master, b"?")
                fg, bg = ((216, 222, 233), background) if background is not None else (
                    (255, 255, 255), (40, 44, 52),
                )
                report = ("COLOR_REPORT_" + case + "=" + json.dumps([list(fg), list(bg)], separators=(",", ":"))).encode()
                wait_for(report)
                wait_for(b"PROMPT_COLOR")
                alpha, top = (.04, 0) if bg[0] >= 128 else (.12, 255)
                prompt = [int(top * alpha + c * (1 - alpha)) for c in bg]
                color = ("48;2;%d;%d;%d" % tuple(prompt)).encode()
                assert re.search(rb"\x1b\[(?:\d+;)*" + color + rb"(?:;\d+)*m", output), bytes(output)
                os.write(master, b"\x02q" if target_workspace else b"\x11")
            assert process.wait(timeout=5) == 0
            assert termios.tcgetattr(slave) == original
            assert core.rpc("terminal.get", {"terminal": address})["state"] == "live"
            os.close(master)
            os.close(slave)
            master = slave = None
    print("PASS: host foreground/background reach core queries in both views; dark, light and unanswered hosts render derived prompt colors; SIGINT during probing restores host settings")
finally:
    if process and process.poll() is None:
        process.kill()
        process.wait()
    if master is not None:
        os.close(master)
        os.close(slave)
    core.close()
