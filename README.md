# tako-core

The terminal engine behind [Tako](https://github.com/alex09x/tako): a Rust VT core and a Metal renderer for Swift, usable on their own in any macOS or iOS app.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

## What is here

- **`tako_core`** (Rust, the root crate):
  - VT100, xterm and modern escape sequences, screen and scrollback with reflow;
  - Kitty keyboard protocol and Kitty graphics (RGB, RGBA and PNG over direct transmission, query replies, placement sizing);
  - OSC 133 shell command records;
  - checkpoints: a screen saved and restored byte for byte;
  - an optional PTY runner (`features = ["pty"]`) and SSH client (`features = ["ssh"]`);
  - Swift bindings through UniFFI.
- **`TakoCoreUI`** (Swift package at the root): `TakoTerminalNSView` for AppKit and `TakoTerminalView` for UIKit, drawn with Metal.

Integration guide: [docs/takocore.md](docs/takocore.md). Escape sequence policy: [docs/escape-sequence-policy.md](docs/escape-sequence-policy.md).

## Use it

Swift Package Manager:

```swift
.package(url: "https://github.com/alex09x/tako-core", from: "0.2.0"),
```

Then depend on the `TakoCoreUI` product. The package downloads `TakoCore.xcframework` from the matching release and checks its checksum.

Rust:

```toml
tako-core = { git = "https://github.com/alex09x/tako-core", tag = "core-v0.2.0" }
```

## Develop

| What | Command |
| --- | --- |
| Engine tests | `mitts test --features pty,ssh` (or `cargo test --features pty,ssh`) |
| Lints | `cargo fmt --all --check`, `mitts clippy --all-targets --features pty,ssh -- -D warnings` |
| TakoCoreUI tests on macOS | `scripts/swift-test.sh` |
| The xcframework | `scripts/build-xcframework.sh` (`--macos-only` for the host slice) |
| A release | `scripts/package-xcframework.sh core-vX.Y.Z`, then attach `target/TakoCore.xcframework.zip` to the release |

CI runs the engine tests on macOS and Linux, the lints, the TakoCoreUI tests, and an iOS build of the package on every pull request.

## License

MIT, see [LICENSE](LICENSE). Third-party material is listed in [NOTICE.md](NOTICE.md).
