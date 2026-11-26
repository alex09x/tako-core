#!/usr/bin/env python3
# tako-session — session runtime for Tako
# Copyright (c) 2026 Alexander Panasenko
#
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/tako-core
# SPDX-License-Identifier: MIT
"""Records golden protocol fixtures from a reference session runtime binary.

A daemon is started in a private ZMX_DIR, its socket is moved aside, and a
relay takes its place. Every client command then talks to the daemon through
the relay, which stores each direction of each connection byte for byte.

    record-fixtures.py <runtime binary> <empty output dir>

Writes <out>/<scenario>.c2s and .s2c (raw bytes) and index.json, which lists
every frame (direction, tag, payload length, offset). Each scenario checks
the protocol outcome it expects before anything is written.
"""
import fcntl, json, os, pty, select, signal, socket, struct, subprocess, sys, tempfile
import termios, threading, time

HEADER = 8
TAG_OUTPUT = 1
TAG_INFO = 6
TAG_LABEL_DATA = 17
TAG_GUARD_STATUS = 24
# Typed into the attached shell; only the shell's own output contains MARKER.
COMMAND = b"echo $((6*7))x\r"
MARKER = b"42x\r\n"


def frames(data):
    """Splits a raw stream into (offset, tag, length) frames; a torn tail stays out."""
    out, pos = [], 0
    while pos + HEADER <= len(data):
        tag, length = data[pos], int.from_bytes(data[pos + 1:pos + 5], "little")
        if pos + HEADER + length > len(data):
            break
        out.append({"offset": pos, "tag": tag, "len": length})
        pos += HEADER + length
    return out


class Relay:
    """Listens on the session path and forwards each connection to the real socket."""

    def __init__(self, path, real):
        self.path, self.real = path, real
        self.lock = threading.Condition()
        self.conns, self.active, self.error = [], 0, None
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(path)
        self.server.listen(16)
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self):
        while True:
            try:
                client, _ = self.server.accept()
            except OSError:
                return
            upstream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                upstream.connect(self.real)
            except OSError as err:
                client.close()
                upstream.close()
                with self.lock:
                    self.error = err
                    self.lock.notify_all()
                continue
            record = {"c2s": bytearray(), "s2c": bytearray()}
            with self.lock:
                self.conns.append(record)
                self.active += 1
            threading.Thread(target=self._pump, args=(client, upstream, record), daemon=True).start()

    def _pump(self, client, upstream, record):
        ends = {client: (upstream, "c2s"), upstream: (client, "s2c")}
        while ends:
            readable, _, _ = select.select(list(ends), [], [], 0.5)
            for src in readable:
                dst, key = ends[src]
                try:
                    chunk = src.recv(65536)
                except OSError as err:
                    with self.lock:
                        self.error = err
                    chunk = b""
                if not chunk:
                    try:
                        dst.shutdown(socket.SHUT_WR)
                    except OSError:
                        pass
                    del ends[src]
                    continue
                # Only delivered bytes are recorded. A daemon that cannot take
                # a request is an error; a client gone after the deliberate
                # hangup just stops its direction.
                try:
                    dst.sendall(chunk)
                except OSError as err:
                    if key == "c2s":
                        with self.lock:
                            self.error = err
                    del ends[src]
                    continue
                with self.lock:
                    record[key] += chunk
        client.close()
        upstream.close()
        with self.lock:
            self.active -= 1
            self.lock.notify_all()

    def take(self, expected, timeout=10.0):
        """Waits for `expected` finished connections; returns immutable snapshots."""
        deadline = time.monotonic() + timeout
        with self.lock:
            while len(self.conns) < expected or self.active > 0:
                if self.error is not None:
                    break
                left = deadline - time.monotonic()
                if left <= 0:
                    raise RuntimeError(f"relay: {len(self.conns)}/{expected} connections, {self.active} open")
                self.lock.wait(left)
            if self.error is not None:
                raise RuntimeError(f"relay: a connection failed: {self.error}")
            taken = [{k: bytes(v) for k, v in c.items()} for c in self.conns]
            self.conns = []
        if len(taken) != expected:
            raise RuntimeError(f"relay: {len(taken)} connections, expected {expected}")
        return taken

    def close(self):
        self.server.close()
        os.unlink(self.path)


def run(binary, env, *args, ok=True, timeout=10):
    result = subprocess.run([binary, *args], env=env, capture_output=True, timeout=timeout,
                            cwd=FIXTURE_CWD)
    if ok and result.returncode != 0:
        raise RuntimeError(f"{' '.join(args)}: exit {result.returncode}: {result.stderr.decode()}")
    return result


def read_until(fd, wanted, seen, timeout):
    """Drains the pty into `seen` until `wanted` shows up or `timeout` passes."""
    deadline = time.monotonic() + timeout
    while wanted not in seen:
        left = deadline - time.monotonic()
        if left <= 0:
            return False
        readable, _, _ = select.select([fd], [], [], left)
        if readable:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                return False
            if not chunk:
                return False
            seen += chunk
    return True


def attach_on_pty(binary, env, args, command, marker, timeout=10.0):
    """Attaches on a 30x100 pty, waits for the prompt, types `command`, waits
    for `marker` in the output, then hangs the client up."""
    pid, fd = pty.fork()
    if pid == 0:
        try:
            os.chdir(FIXTURE_CWD)
            # Sized before exec, so the client reads 30x100 at once.
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
            os.execve(binary, [binary, *args], env)
        except BaseException as err:  # never unwind into the parent's cleanup
            os.write(2, f"attach child: {err}\n".encode())
        finally:
            os._exit(127)
    seen = bytearray()
    try:
        if not read_until(fd, b"$ ", seen, timeout):
            raise RuntimeError(f"attach: no prompt: {bytes(seen)!r}")
        os.write(fd, command)
        if not read_until(fd, marker, seen, timeout):
            raise RuntimeError(f"attach: no {marker!r}: {bytes(seen)!r}")
    finally:
        os.close(fd)
        os.kill(pid, signal.SIGHUP)
        deadline = time.monotonic() + 3
        while os.waitpid(pid, os.WNOHANG) == (0, 0):
            if time.monotonic() > deadline:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
                break
            time.sleep(0.05)


def whole(data):
    """Whether the indexed frames consume the stream to its last byte."""
    found = frames(data)
    end = found[-1]["offset"] + HEADER + found[-1]["len"] if found else 0
    return end == len(data)


def output(data):
    """The concatenated payloads of a stream's Output frames."""
    return b"".join(data[f["offset"] + HEADER:f["offset"] + HEADER + f["len"]]
                    for f in frames(data) if f["tag"] == TAG_OUTPUT)


FIXTURE_CWD = "/tmp/tako-session-fixture"
PLACEHOLDER_HOST = b"fixture-host"
INFO_LEN, INFO_CWD_LEN, INFO_CWD = 552, 14, 272


def scrub(data, host):
    """Replaces this machine's host name in each Info record's cwd URI with a
    placeholder (adjusting cwd_len), so fixtures name no real machine. Any
    other frame that carries the host name is refused."""
    out = bytearray()
    for f in frames(data):
        start, end = f["offset"], f["offset"] + HEADER + f["len"]
        frame = bytearray(data[start:end])
        if f["tag"] == TAG_INFO and f["len"] == INFO_LEN:
            info = frame[HEADER:]
            cwd_len = min(int.from_bytes(info[INFO_CWD_LEN:INFO_CWD_LEN + 2], "little"), 256)
            cwd = bytes(info[INFO_CWD:INFO_CWD + cwd_len]).replace(b"//" + host + b"/", b"//" + PLACEHOLDER_HOST + b"/")
            cwd = cwd[:256]
            info[INFO_CWD:INFO_CWD + 256] = cwd.ljust(256, b"\0")
            info[INFO_CWD_LEN:INFO_CWD_LEN + 2] = len(cwd).to_bytes(2, "little")
            frame[HEADER:] = info
        elif host in frame:
            raise RuntimeError(f"tag {f['tag']} carries the host name; refusing to record it")
        out += frame
    return bytes(out)


def private_markers(data):
    """Markers of this machine or its user that must never reach a fixture."""
    login = os.environ.get("USER") or os.environ.get("LOGNAME") or ""
    markers = [b"/Users/", b"/private/tmp/claude", b".local/", socket.gethostname().encode()]
    if login:
        # As a path component or in a mangled path, where it would leak; a
        # bare substring would also hit ordinary words such as label names.
        markers += [f"/{login}/".encode(), f"-{login}-".encode()]
    return [m.decode() for m in markers if m in data]


def payloads(data):
    """(tag, payload) for every frame of a stream."""
    return [(f["tag"], data[f["offset"] + HEADER:f["offset"] + HEADER + f["len"]]) for f in frames(data)]


def guard_reply(conn):
    """The GuardStatus byte the daemon sent first, or None."""
    data = conn["s2c"]
    first = frames(data)
    if not first or first[0]["tag"] != TAG_GUARD_STATUS or first[0]["len"] != 1:
        return None
    return data[HEADER]


def main():
    if len(sys.argv) != 3:
        sys.exit("usage: record-fixtures.py <runtime binary> <empty output dir>")
    binary, out_dir = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    os.makedirs(out_dir, exist_ok=True)
    if os.listdir(out_dir):
        sys.exit(f"{out_dir} is not empty; record into an empty directory")
    if os.path.exists(FIXTURE_CWD):
        sys.exit(f"{FIXTURE_CWD} exists; remove it first (it is the sessions' working directory)")
    os.makedirs(FIXTURE_CWD)
    try:
        with tempfile.TemporaryDirectory(prefix="ts-fx-", dir="/tmp") as root:
            record(binary, out_dir, root)
    finally:
        os.rmdir(FIXTURE_CWD)


def record(binary, out_dir, root):
    home = os.path.join(root, "home")
    os.makedirs(home)
    env = {"PATH": "/usr/bin:/bin", "HOME": home, "SHELL": "/bin/sh", "TERM": "xterm-256color",
           "ZMX_DIR": os.path.join(root, "z"), "ZMX_NO_DETACH_KEY": "1", "ZMX_TRACK_ENV": "",
           "HISTFILE": "/dev/null", "PS1": "$ ", "ENV": "", "LANG": "C", "TMPDIR": root}
    name = "s1"
    sock = os.path.join(env["ZMX_DIR"], name)
    scenarios = {}

    version = run(binary, env, "version").stdout.decode().split("\n")[0]
    relay, killed = None, False
    try:
        run(binary, env, "attach", "--create-only", "--labels", "tako-gen=g1", name, "/bin/sh")
        deadline = time.monotonic() + 5
        while not os.path.exists(sock):
            if time.monotonic() > deadline:
                raise RuntimeError("the session socket never appeared")
            time.sleep(0.05)
        os.rename(sock, sock + ".real")
        relay = Relay(sock, sock + ".real")
        run(binary, env, "list")
        scenarios["list"] = relay.take(1)[0]
        run(binary, env, "get", name)
        scenarios["get"] = relay.take(1)[0]
        run(binary, env, "kill", "--if-label", "tako-gen=nope", name, ok=False)
        scenarios["kill-if-label-mismatch"] = relay.take(1)[0]
        attach_on_pty(binary, env, ["attach", "--attach-only", "--if-label", "tako-gen=g1",
                                    "--labels", "tako-attempt=a1", name], COMMAND, MARKER)
        scenarios["attach-if-label"] = relay.take(1)[0]
        run(binary, env, "kill", "--if-label", "tako-gen=g1", name)
        scenarios["kill-if-label-match"] = relay.take(1)[0]
        killed = True
    finally:
        if relay is not None:
            try:
                relay.close()
            except OSError:
                pass
        if not killed:
            if os.path.exists(sock + ".real"):
                os.replace(sock + ".real", sock)
            run(binary, env, "kill", name, ok=False)

    checks = {
        "kill-if-label-mismatch": guard_reply(scenarios["kill-if-label-mismatch"]) == 1,
        "kill-if-label-match": guard_reply(scenarios["kill-if-label-match"]) == 0,
        "attach-if-label": guard_reply(scenarios["attach-if-label"]) == 0
        and payloads(scenarios["attach-if-label"]["s2c"])[1:2]
        == [(TAG_LABEL_DATA, b"tako-attempt=a1 tako-gen=g1")]
        and MARKER in output(scenarios["attach-if-label"]["s2c"]),
        "list": [(f["tag"], f["len"]) for f in frames(scenarios["list"]["s2c"])][:1] == [(TAG_INFO, 552)]
        and payloads(scenarios["list"]["s2c"])[1:] == [(TAG_LABEL_DATA, b"tako-gen=g1")],
        "get": payloads(scenarios["get"]["s2c"]) == [(TAG_LABEL_DATA, b"tako-gen=g1")],
    }
    for key, conn in scenarios.items():
        for direction in ("c2s", "s2c"):
            checks[f"{key}.{direction} whole frames"] = whole(conn[direction])
    failed = [k for k, ok in checks.items() if not ok]
    if failed:
        raise RuntimeError(f"unexpected protocol outcome: {failed}")

    host = socket.gethostname().encode()
    for key, conn in scenarios.items():
        for direction in ("c2s", "s2c"):
            conn[direction] = scrub(conn[direction], host)
            leaked = private_markers(conn[direction])
            if leaked:
                raise RuntimeError(f"{key}.{direction} still holds private data {leaked}; not writing")

    for key, conn in scenarios.items():
        for direction in ("c2s", "s2c"):
            with open(os.path.join(out_dir, f"{key}.{direction}"), "wb") as f:
                f.write(conn[direction])
    with open(os.path.join(out_dir, "index.json"), "w") as f:
        json.dump({"binary_version": version,
                   "scenarios": {k: {d: frames(c[d]) for d in ("c2s", "s2c")} for k, c in scenarios.items()}},
                  f, indent=1, sort_keys=True)
    print("\n".join(scenarios))


if __name__ == "__main__":
    main()
