# Session protocol

`tako-session` keeps a shell alive outside the app: one daemon per session
owns the PTY, and short-lived clients talk to it over a Unix stream socket.
This document is the wire contract. It is frozen: daemons started by an older
runtime keep running across an app upgrade, and the new client must still
reach them, while an old client must still reach a new daemon.

The protocol is the one of the session runtime Tako bundled before, version
0.8.1 with Tako's guarded extensions (protocol 3), so both can serve the
same sessions. The golden fixtures in `tako-session/tests/fixtures/p3/` were
recorded from that runtime and pin every byte described here.

## Where the socket lives

| Directory | Chosen when |
|-----------|-------------|
| `$ZMX_DIR` | set |
| `$XDG_RUNTIME_DIR/zmx` | `ZMX_DIR` unset, `XDG_RUNTIME_DIR` set |
| `${TMPDIR:-/tmp}/zmx-<uid>` | otherwise (trailing `/` of `TMPDIR` trimmed) |

A session named `NAME` listens on `<dir>/NAME`. The whole path must fit in
`sockaddr_un.sun_path` with its terminating NUL. Logs go to `$ZMX_DIR/logs`,
else `$XDG_STATE_HOME/zmx/logs`, else `$HOME/.local/state/zmx/logs`, else the
socket directory. `ZMX_DIR_MODE` and `ZMX_LOG_MODE` set the octal modes the
directories and log files are created with; Tako uses `0700` and `0600`.

## Frames

Every message is one frame: an 8-byte header and a payload.

| Bytes | Content |
|-------|---------|
| 0 | tag (u8) |
| 1-4 | payload length (u32, little-endian) |
| 5-7 | zero; readers ignore them |
| 8.. | payload |

A reader buffers bytes until a whole frame has arrived. The length is read
into a wide integer before it is added to the header size, so no length can
wrap. Frames are never interleaved; a peer may pipeline several requests
without waiting.

## Tags

Tags not listed are unknown. A daemon ignores a request with an unknown tag
and sends nothing back, which is how a newer client detects an older daemon
(see *Guarded requests*). Integers never change meaning: a retired tag is
reserved, not reused.

| Tag | Name | Direction | Payload |
|----:|------|-----------|---------|
| 0 | Input | client → daemon | bytes for the PTY, from an attached client |
| 1 | Output | daemon → client | bytes the PTY produced |
| 2 | Resize | both | client: a size (below); daemon: empty, after `Init` |
| 3 | Detach | client → daemon | empty: detach this client |
| 4 | DetachAll | client → daemon | empty: detach every client |
| 5 | Kill | client → daemon | empty: end the session |
| 6 | Info | both | request empty; reply the 552-byte record (below) |
| 7 | Init | client → daemon | a size; the client is attaching |
| 8 | History | both | request: optional format byte (0 plain, 1 vt, 2 html); reply: the screen and scrollback in that format |
| 9 | Run | client → daemon | a command line to type into the shell; reply `Ack` |
| 10 | Ack | daemon → client | empty |
| 11 | Switch | both | client: a session name; daemon (to the leading client): the name, or `name\ncwd` |
| 12 | Write | client → daemon | u32 path length (LE), path, file content; reply `Ack` |
| 13 | TaskComplete | daemon → client | one byte: the exit code of a `Run` command |
| 14 | LabelGet | client → daemon | empty; reply `LabelData` |
| 15 | LabelSet | client → daemon | labels to add or replace; reply `Ack` |
| 16 | LabelClear | client → daemon | empty; reply `Ack` |
| 17 | LabelData | daemon → client | the session's labels, sorted by key |
| 18 | Send | client → daemon | bytes for the PTY, from a detached client |
| 19 | EnvGet | client → daemon | empty; reply `EnvData` |
| 20 | EnvSet | client → daemon | the client's environment; reply `Ack` |
| 21 | EnvData | daemon → client | the leading client's environment |
| 22 | KillIfLabel | client → daemon | required labels; reply `GuardStatus` |
| 23 | AttachIfLabel | client → daemon | `required\nset` labels; reply `GuardStatus` |
| 24 | GuardStatus | daemon → client | one byte: 0 matched, 1 mismatch, 2 invalid |

### Sizes

`Init` and `Resize` carry rows, cols, xpixel and ypixel, each a u16 LE
(8 bytes). Daemons before 0.7.0 expect only rows and cols (4 bytes) and
drop the 8-byte form, so a client sends every size twice, 8-byte form first,
then its 4-byte prefix. Each daemon processes the length it knows and drops
the other.

### Info

A C struct, 552 bytes, zero-filled before it is set:

| Offset | Field |
|-------:|-------|
| 0 | clients_len, u64 |
| 8 | pid, i32 (the shell) |
| 12 | cmd_len, u16 |
| 14 | cwd_len, u16 |
| 16 | cmd, 256 bytes |
| 272 | cwd, 256 bytes |
| 528 | created_at, u64 |
| 536 | task_ended_at, u64 |
| 544 | task_exit_code, u8 |
| 545 | zero padding |

The size never changes; new data goes into new tags. A reader refuses a
record of any other length.

### Labels

A label list is `key=value` pairs separated by single spaces. Keys and
values use `A-Z a-z 0-9 . _ -`; keys `name`, `start_dir` and `cmd` are
reserved. `LabelData` lists the pairs sorted by key. Guarded requests
validate strictly: no leading, trailing or doubled spaces, no token without
`=`, no duplicate keys, and the required labels must be non-empty with
non-empty values.

## Guarded requests (protocol 3)

`KillIfLabel` ends the session only if every required label matches; the
daemon flushes `GuardStatus` (matched) before it exits. `AttachIfLabel`
attaches only if the required labels match, and then applies the second
half of its payload as new labels before it replies.

A client attaching with `--attach-only --if-label` pipelines `AttachIfLabel`
and `LabelGet`, then `Init` and `Resize`:

- a protocol-3 daemon answers `GuardStatus` first, then `LabelData`. Only
  `matched` continues; `mismatch` or `invalid` ends the attach at once;
- an older daemon ignores `AttachIfLabel` and answers only `LabelData`. The
  client checks the labels itself, asks again with a second `LabelGet` on the
  same socket right before attaching, and sends `LabelSet` (awaiting `Ack`)
  for the labels to set.

`Output` frames that arrive during the handshake are kept, up to 4 MiB, and
written once the attach completes; past that the attach fails. A guarded
kill without fallback treats a missing `GuardStatus` within its deadline as
a failure, so an old daemon is never killed on an unverified match.

## Recording fixtures

`tako-session/scripts/record-fixtures.py <runtime> <out>` starts a session in
a private directory, puts a relay on the session socket, runs each client
command through it and stores both directions of every connection
(`<scenario>.c2s`, `<scenario>.s2c`) with a frame index. The sessions run in
`/tmp/tako-session-fixture`, and the recording machine's host name in an
`Info` cwd URI is replaced with `fixture-host` (with `cwd_len` adjusted);
the recorder refuses to write anything that still names the machine, the
user or a private path, and a golden test checks the same.
