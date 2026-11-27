/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The attach client on a real pseudo-terminal, as a terminal app runs it:
//! `attach --labels k=v <name> <program>`, typing, resizing, detaching, the
//! app going away, and coming back to the same screen.

mod common;

use common::pty::PtyClient;
use common::{Dir, wait_until};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

fn attach(dir: &Dir, args: &[&str], extra_env: &[(&str, &str)]) -> PtyClient {
    let mut cmd = dir.command(&dir.bin, args);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    PtyClient::spawn(cmd, 24, 80)
}

fn alive(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

#[test]
fn a_terminal_app_starts_types_resizes_detaches_and_comes_back() {
    let dir = Dir::new("pty");
    let mut client = attach(
        &dir,
        &["attach", "--labels", "tako-gen=g1", "p1", "/bin/sh"],
        &[],
    );
    client.wait_for("session \"p1\" created");
    client.write(b"echo typed-$((6*7))\r");
    client.wait_for("typed-42");
    assert_eq!(dir.run(&["get", "p1"]).stdout, b"tako-gen=g1");

    // The window changes size: the program sees it.
    client.resize(30, 100);
    std::thread::sleep(Duration::from_millis(100));
    client.write(b"stty size\r");
    client.wait_for("30 100");

    // ctrl+\ detaches this client; the session keeps running.
    client.write(b"\x1c");
    assert_eq!(client.exit_within(Duration::from_secs(5)), Some(0));
    assert!(alive(&dir.socket("p1")));

    // Coming back shows the same screen before anything new is typed.
    let mut again = attach(
        &dir,
        &["attach", "--labels", "tako-attempt=a2", "p1", "/bin/false"],
        &[],
    );
    again.wait_for("typed-42");
    assert_eq!(
        dir.run(&["get", "p1"]).stdout,
        b"tako-attempt=a2 tako-gen=g1"
    );
    again.write(b"echo still-$((1+1))\r");
    again.wait_for("still-2");
}

#[test]
fn the_app_going_away_leaves_the_session_running() {
    let dir = Dir::new("ptygone");
    let mut client = attach(
        &dir,
        &["attach", "p2", "/bin/sh"],
        &[("ZMX_NO_DETACH_KEY", "1")],
    );
    client.wait_for("session \"p2\" created");
    client.write(b"echo up\r");
    client.wait_for("up");
    // With the detach key off, ctrl+\ goes to the program.
    client.write(b"\x1c");
    assert_eq!(
        client.exit_within(Duration::from_millis(500)),
        None,
        "ctrl+\\ must not detach"
    );
    // The terminal closes: the client ends, the session stays.
    client.hang_up();
    assert!(
        client.exit_within(Duration::from_secs(5)).is_some(),
        "client outlived its terminal"
    );
    wait_until(Duration::from_secs(1), || alive(&dir.socket("p2")));
}
