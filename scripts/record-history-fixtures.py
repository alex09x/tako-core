#!/usr/bin/env python3
# tako — Terminal emulator
# Copyright (c) 2026 Alexander Panasenko
#
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/tako
# SPDX-License-Identifier: MIT
"""Records what a reference session runtime prints for `history` (plain, VT,
HTML) after a terminal of a known size receives known bytes.

    python3 -I scripts/record-history-fixtures.py <runtime binary> tests/fixtures/history

Each case's bytes reach the daemon's terminal as `print` output from a
session running `sleep`, so no shell writes anything. Writes <case>.in and
<case>.{plain,vt,html} for tests/history_golden.rs.
"""
import os, socket, struct, subprocess, sys, tempfile, time

COLS, ROWS = 40, 10
CASES = {
    "styles": b"\n\n\x1b[1mB\x1b[0m \x1b[2mD\x1b[0m \x1b[3mI\x1b[0m \x1b[4mU\x1b[0m \x1b[4:3mC\x1b[0m \x1b[5mK\x1b[0m"
              b" \x1b[7mR\x1b[0m \x1b[8mH\x1b[0m \x1b[9mS\x1b[0m \x1b[53mO\x1b[0m\r\n\x1b[31mf1\x1b[0m \x1b[91mf9\x1b[0m"
              b" \x1b[38;5;100mf100\x1b[0m \x1b[38;2;1;2;3mrgb\x1b[0m \x1b[41mb1\x1b[0m \x1b[48;5;200mb200\x1b[0m"
              b" \x1b[48;2;250;5;6mbrgb\x1b[0m \x1b[58;5;3;4mul\x1b[0m <&\"x'>\r\n   lead  \r\n\r\n\r\nend",
    "wrap": b"0123456789" * 6 + b"\r\ntail  \r\n" + b"x" * 40,
    "scrollback": b"".join(b"line %d \x1b[1;3%dm\xc3\xa9\xe6\xbc\xa2\x1b[0m\r\n" % (i, i % 8) for i in range(1, 31))
                  + b"\x1b(B\x1b[m\x1b(0lqk\x1b(B\x1b]4;1;rgb:12/34/56\x1b\\",
    "alternate": b"main\r\n\x1b]7;file://somehost/tmp/a%20b\x1b\\\x1b[?2004h\x1b[?1h\x1b[?1006h\x1b[?1002h\x1b[4h"
                 b"\x1b[?1049h\x1b[2;5r\x1b[?25l\x1b[5;1H\x1b[38;2;9;8;7mALT\x1b[44m\x1b[10;31H0123456789\x1b[>5u",
    "erase-bg": b"\x1b[42mgreen line\x1b[K\x1b[0m\r\n\x1b[44m\x1b[2K\x1b[0m\r\nafter",
}


def frame(tag, payload=b""):
    return bytes([tag]) + struct.pack("<I", len(payload)) + b"\0\0\0" + payload


def history(s, fmt):
    """Asks on the connection that sent the output: the daemon handles one
    connection's requests in order, so the reply covers everything sent."""
    s.sendall(frame(8, bytes([fmt])))
    buf = b""
    while True:
        chunk = s.recv(65536)
        if not chunk:
            raise RuntimeError("the runtime closed the connection before answering")
        buf += chunk
        while len(buf) >= 8:
            n = struct.unpack("<I", buf[1:5])[0]
            if len(buf) < 8 + n:
                break
            tag, payload, buf = buf[0], buf[8:8 + n], buf[8 + n:]
            if tag == 8:
                return payload


def record(runtime, data):
    """The plain, VT and HTML history after `data`, from a fresh session."""
    d = tempfile.mkdtemp(prefix="hf", dir="/tmp")
    env = {"ZMX_DIR": d, "HOME": d, "PATH": "/usr/bin:/bin", "SHELL": "/bin/sh"}
    s = None
    try:
        subprocess.run([runtime, "attach", "--create-only", "h", "sleep", "600"], env=env,
                       stdin=subprocess.DEVNULL, capture_output=True, check=True, timeout=10)
        time.sleep(0.3)
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(5)
        s.connect(os.path.join(d, "h"))
        s.sendall(frame(7, struct.pack("<HHHH", ROWS, COLS, 0, 0)))  # Init: leads, sets size
        s.sendall(frame(1, data))  # Output: straight into the daemon's terminal
        return {ext: history(s, fmt) for fmt, ext in ((0, "plain"), (1, "vt"), (2, "html"))}
    finally:
        # Best effort: a failure above must stay the error that is reported.
        if s is not None:
            try:
                s.sendall(frame(5))  # Kill
                time.sleep(0.7)
            except OSError:
                pass
            s.close()
        subprocess.run(["rm", "-rf", d])


def main(runtime, out):
    os.makedirs(out, exist_ok=True)
    for name, data in CASES.items():
        recorded = record(runtime, data)  # every format first: no half-written case
        with open(os.path.join(out, name + ".in"), "wb") as f:
            f.write(data)
        for ext, text in recorded.items():
            with open(os.path.join(out, f"{name}.{ext}"), "wb") as f:
                f.write(text)
        print("recorded", name)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
