/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! This runtime against the one it replaces, in both directions: its client
//! with their daemon, their client with its daemon, and sessions the old
//! runtime started being taken over. Runs only when `TAKO_SESSION_REF_BIN`
//! names a reference binary (`TAKO_SESSION_REF_LEGACY_BIN` adds a daemon
//! from before guarded attach).

mod common;

use common::{Dir, Peer, process_gone, wait_until};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;
use tako_session::ipc::{Info, Tag};

fn reference(var: &str) -> Option<PathBuf> {
    let bin = std::env::var_os(var).map(PathBuf::from);
    if bin.is_none() {
        eprintln!("skipped: {var} is not set");
    }
    bin
}

fn run(dir: &Dir, bin: &Path, args: &[&str]) -> Output {
    dir.command(bin, args).output().unwrap()
}

fn create(dir: &Dir, bin: &Path, args: &[&str]) {
    let out = run(dir, bin, args);
    assert!(out.status.success(), "{args:?}: {out:?}");
    let name = args.last().unwrap();
    wait_until(Duration::from_secs(5), || {
        std::os::unix::net::UnixStream::connect(dir.socket(name)).is_ok()
    });
}

fn text(o: &Output) -> (i32, String, String) {
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Both clients print the same for the same command against the same
/// sessions.
fn same(dir: &Dir, theirs: &Path, args: &[&str]) {
    assert_eq!(
        text(&run(dir, &dir.bin, args)),
        text(&run(dir, theirs, args)),
        "{args:?}"
    );
}

fn pid(dir: &Dir, name: &str) -> i32 {
    let mut p = Peer::connect(&dir.socket(name));
    p.send(Tag::INFO, b"");
    Info::decode(&p.expect(Tag::INFO)).unwrap().pid
}

/// Commands both clients must agree on, against `name`.
fn agree_on_queries(dir: &Dir, theirs: &Path, name: &str) {
    for args in [
        vec!["list"],
        vec!["list", "--short"],
        vec!["get", name],
        vec!["get", name, "gen"],
        vec!["get", name, "nokey"],
    ] {
        same(dir, theirs, &args);
    }
    same(dir, theirs, &["attach", "--if-label", "gen=0", name]);
    same(dir, theirs, &["attach", "--create-only", name]);
    same(dir, theirs, &["kill", "--if-label", "gen=0", name]);
}

/// Session started by `creator`, driven by `driver`.
fn session_round_trip(creator: &Path, driver: &Path) {
    let dir = Dir::new("cross");
    create(
        &dir,
        creator,
        &[
            "attach",
            "--create-only",
            "--labels",
            "owner=tako gen=1",
            "x1",
        ],
    );
    let theirs = if dir.bin == creator { driver } else { creator };
    agree_on_queries(&dir, theirs, "x1");

    assert!(run(&dir, driver, &["set", "x1", "gen=2"]).status.success());
    assert_eq!(run(&dir, creator, &["get", "x1", "gen"]).stdout, b"2");
    let guarded = run(
        &dir,
        driver,
        &["attach", "--if-label", "gen=2", "--labels", "gen=3", "x1"],
    );
    assert!(guarded.status.success(), "{guarded:?}");
    assert_eq!(run(&dir, creator, &["get", "x1", "gen"]).stdout, b"3");

    // A terminal attached through the protocol sees the shell and its
    // earlier screen.
    let mut p = Peer::connect(&dir.socket("x1"));
    p.init(24, 80);
    p.send(Tag::INPUT, b"echo cross-$((6*7))\r");
    p.wait_output("cross-42");
    drop(p);
    let mut again = Peer::connect(&dir.socket("x1"));
    again.init(24, 80);
    again.wait_output("cross-42");
    again.send(Tag::HISTORY, &[0]);
    assert!(String::from_utf8_lossy(&again.expect(Tag::HISTORY)).contains("cross-42"));
    drop(again);

    let shell = pid(&dir, "x1");
    let killed = run(&dir, driver, &["kill", "--if-label", "gen=3", "x1"]);
    assert!(killed.status.success(), "{killed:?}");
    wait_until(Duration::from_secs(5), || process_gone(shell));
}

#[test]
fn this_client_drives_a_reference_daemon() {
    let Some(theirs) = reference("TAKO_SESSION_REF_BIN") else {
        return;
    };
    session_round_trip(&theirs, &common::binary());
}

#[test]
fn the_reference_client_drives_this_daemon() {
    let Some(theirs) = reference("TAKO_SESSION_REF_BIN") else {
        return;
    };
    session_round_trip(&common::binary(), &theirs);
}

fn run_with_stdin(dir: &Dir, bin: &Path, args: &[&str], stdin: &[u8]) -> Output {
    use std::io::Write;
    let mut child = dir
        .command(bin, args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

/// `run`, `wait`, `history`, `print-env`, `send`, `print` and `write` from
/// both clients against a task session of `daemon`.
fn task_commands(daemon: &Path, other: &Path) {
    let dir = Dir::new("task");
    let ours = dir.bin.clone();
    for (i, client) in [daemon, other].into_iter().enumerate() {
        let name = format!("t{i}");
        // Words are quoted for the shell, so `$` stays literal here.
        let ok = run(
            &dir,
            client,
            &["run", &name, "printf", "%s-%s\\n", "task", "ran"],
        );
        assert_eq!(ok.status.code(), Some(0), "{ok:?}");
        assert!(
            String::from_utf8_lossy(&ok.stdout).contains("task-ran"),
            "{ok:?}"
        );
        let failed = run(&dir, client, &["run", &name, "sh", "-c", "exit 3"]);
        assert_eq!(failed.status.code(), Some(3), "{failed:?}");
        let detached = run(&dir, client, &["run", &name, "-d", "true"]);
        assert_eq!(
            (detached.status.code(), detached.stdout.as_slice()),
            (Some(0), &b"command sent!\n"[..])
        );
        let waited = run(&dir, client, &["wait", &name]);
        assert!(
            String::from_utf8_lossy(&waited.stdout).contains("task(s) completed!"),
            "{waited:?}"
        );
    }
    create(&dir, daemon, &["attach", "--create-only", "h1"]);
    for args in [
        vec!["history", "h1"],
        vec!["print-env", "h1"],
        vec!["print-env", "-s", "h1"],
        vec!["print-env", "h1", "DISPLAY"],
    ] {
        same(&dir, other, &args);
    }
    // `print` draws at the cursor, where the shell's next output lands too:
    // watch it arrive rather than look for it in the history.
    let mut watcher = Peer::connect(&dir.socket("h1"));
    for client in [&ours, other] {
        assert!(
            run(&dir, client, &["send", "h1", "echo sent-$((1+1))\r"])
                .status
                .success()
        );
        assert!(
            run(&dir, client, &["print", "h1", "printed-text"])
                .status
                .success()
        );
    }
    watcher.wait_output("printed-text");
    drop(watcher);
    let target = dir.path.join("written.txt");
    let target = target.to_str().unwrap();
    for client in [&ours, other] {
        let _ = std::fs::remove_file(target);
        let w = run_with_stdin(
            &dir,
            client,
            &["write", "h1", target],
            b"line one\nline 'two'\n",
        );
        assert_eq!(
            String::from_utf8_lossy(&w.stdout),
            format!("file created {target}\n"),
            "{w:?}"
        );
        wait_until(Duration::from_secs(5), || {
            std::fs::read(target).ok().as_deref() == Some(&b"line one\nline 'two'\n"[..])
        });
    }
    let h = run(&dir, &ours, &["history", "h1"]);
    let text = String::from_utf8_lossy(&h.stdout);
    assert!(text.contains("sent-2"), "{text}");
    same(&dir, other, &["history", "h1"]);
}

#[test]
fn task_commands_agree_on_this_daemon() {
    let Some(theirs) = reference("TAKO_SESSION_REF_BIN") else {
        return;
    };
    task_commands(&common::binary(), &theirs);
}

#[test]
fn task_commands_agree_on_a_reference_daemon() {
    let Some(theirs) = reference("TAKO_SESSION_REF_BIN") else {
        return;
    };
    task_commands(&theirs, &common::binary());
}

#[test]
fn listing_mixed_sessions_matches() {
    let Some(theirs) = reference("TAKO_SESSION_REF_BIN") else {
        return;
    };
    let dir = Dir::new("mixed");
    create(
        &dir,
        &theirs,
        &["attach", "--create-only", "--labels", "a=1", "old"],
    );
    create(
        &dir,
        &dir.bin,
        &["attach", "--create-only", "--labels", "a=1", "new"],
    );
    same(&dir, &theirs, &["list"]);
    same(&dir, &theirs, &["list", "--short"]);
    for name in ["old", "new"] {
        let shell = pid(&dir, name);
        assert!(run(&dir, &dir.bin, &["kill", name]).status.success());
        wait_until(Duration::from_secs(5), || process_gone(shell));
    }
}

#[test]
fn a_daemon_without_guarded_attach_is_still_attached_safely() {
    let Some(legacy) = reference("TAKO_SESSION_REF_LEGACY_BIN") else {
        return;
    };
    let dir = Dir::new("legacy");
    // Old enough to know neither --create-only nor --labels.
    create(&dir, &legacy, &["attach", "g1"]);
    assert!(run(&dir, &legacy, &["set", "g1", "gen=1"]).status.success());
    let theirs = reference("TAKO_SESSION_REF_BIN").unwrap_or(legacy.clone());
    same(&dir, &theirs, &["attach", "--if-label", "gen=0", "g1"]);
    let ok = run(
        &dir,
        &dir.bin,
        &["attach", "--if-label", "gen=1", "--labels", "gen=2", "g1"],
    );
    assert!(ok.status.success(), "{ok:?}");
    assert_eq!(run(&dir, &legacy, &["get", "g1", "gen"]).stdout, b"2");
    same(&dir, &theirs, &["kill", "--if-label", "gen=0", "g1"]);
    let shell = pid(&dir, "g1");
    assert!(run(&dir, &dir.bin, &["kill", "g1"]).status.success());
    wait_until(Duration::from_secs(5), || process_gone(shell));
}
