# tako-session

The session runtime for Tako: shells and programs that keep running when the terminal window closes or Tako relaunches, and come back with their screen intact.

One small daemon per session owns the PTY and keeps the screen in a [tako-core](..) terminal. Clients attach to it, or query and drive it, over a Unix socket. The command line, socket layout, environment variables and wire protocol are those of the runtime described in [NOTICE](NOTICE). Either runtime's client works with the other's daemon, so a binary can replace the other while sessions are running.

## Build and install

```sh
mitts build --release -p tako-session      # -> target/release/tako-session
install -m 755 target/release/tako-session ~/.local/bin/
```

It needs only libc. It builds and is tested on macOS (arm64) and Linux (x86_64).

## Use

```sh
tako-session attach dev                    # attach, creating the session (a login $SHELL) if needed
tako-session attach dev vim notes.md       # a program instead of the shell
tako-session attach --labels "owner=me" build
ctrl+\                                     # detach this client; the session keeps running
tako-session list                          # sessions, with pid, clients, cwd, command, labels
tako-session kill dev
```

| command | what it does |
|---|---|
| `attach [--labels kv] <name> [command...]` | Attach, creating the session if needed. |
| `attach --create-only [--labels kv] <name> [command...]` | Create only; fail if the session exists. |
| `attach --if-label kv [--labels kv] <name>` | Attach only to a running session whose labels match, then apply `--labels`. |
| `run <name> [-d] [command...]` | Type a command into the session's shell, print its output, exit with its status. |
| `send <name> <text...>` | Raw input to the session (stdin when no text). |
| `print <name> <text...>` | Text onto the session's screen; the program sees nothing. |
| `write <name> <path>` | Stdin becomes `path` inside the session (works over SSH). |
| `wait <name>...` / `tail <name>...` | Wait for `run -d` tasks / follow output as plain text. |
| `history <name> [--vt\|--html]` | Scrollback and screen as text, VT or HTML. |
| `list [--short]`, `get <name> [key]`, `set <name> k=v...`, `clear <name>` | Sessions and their labels. |
| `kill <name>... [--force]`, `kill --if-label kv <name>` | End sessions (only on matching labels). |
| `detach`, `print-env [-s] <name> [key]`, `completions <shell>`, `version`, `help` | |

`tako-session help` has the details and examples. Names ending in `*` match prefixes in `kill`, `wait` and `tail`. Inside a session, `attach <other>` switches the attached client to the other session.

## Environment

| variable | effect |
|---|---|
| `ZMX_DIR` | Directory of the session sockets; logs go to its `logs/`. Default: `$XDG_RUNTIME_DIR/zmx`, else `${TMPDIR:-/tmp}/zmx-<uid>`. |
| `ZMX_SESSION` | Set inside a session to its name; `.` means this session. |
| `ZMX_SESSION_PREFIX` | Prefix added to every session name. |
| `ZMX_NO_DETACH_KEY` | Turns ctrl+\ off; the key goes to the program. |
| `ZMX_TRACK_ENV` | Comma-separated variables a client hands to the session (`print-env`). |
| `ZMX_DIR_MODE`, `ZMX_LOG_MODE` | Octal modes of new directories (750) and logs (640). |

## How it behaves

- **Coming back:** a client that attaches to a running session gets its screen as it is: history, styles, modes, cursor, title, working directory, palette. If a full-screen program is running, the shell screen behind it comes back too.
- **No terminal attached:** the daemon answers terminal queries (DA1/DA2), so shells do not stall.
- **Several clients:** one client leads. It sets the size, and its keystrokes go to the program. Typing in another client makes that one the leader.
- **Ending a session:** the program gets SIGHUP, and SIGKILL half a second later. So does whatever runs in its foreground.
- **Limits:** a client that announces a frame over 256 MiB is dropped. A screen size of 0, or of more than 4M cells, is ignored.
- **`history`:** prints exactly what the original runtime prints, in all three formats.

The wire protocol is in [docs/session-protocol.md](../docs/session-protocol.md).

## Tests and benchmarks

```sh
mitts test -p tako-session
TAKO_SESSION_REF_BIN=<old runtime> TAKO_SESSION_REF_LEGACY_BIN=<older> mitts test -p tako-session --test cross_matrix
mitts bench -p tako-session                # TAKO_TEST_NEW_ZMX=<old runtime> adds it side by side
```

The tests cover:
- real sessions over the socket and the command line;
- the client on a real PTY (typing, resize, detach, the terminal closing, switching sessions);
- 48 command lines against the original runtime's output;
- with the reference binaries, both directions of client and daemon.

The benchmarks measure attach latency (empty, and with 10k lines of history), output throughput to 1 and 4 clients, keystroke round trip, the frame codec, and idle CPU and memory.

## License

MIT, see [LICENSE](LICENSE). It implements another project's protocol and command line; that project's notice is in [NOTICE](NOTICE) and [LICENSE-zmx](LICENSE-zmx).
