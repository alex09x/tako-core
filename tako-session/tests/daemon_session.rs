/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Real sessions: the binary starts a daemon with a shell on a PTY, and the
//! tests talk to it over its socket and through the command line.

mod common;

use common::{Dir, Peer, process_gone, wait_until};
use std::time::Duration;
use tako_session::ipc::{GuardReply, Info, Tag, guard_payload};

fn info(dir: &Dir, name: &str) -> Info {
    let mut p = Peer::connect(&dir.socket(name));
    p.send(Tag::INFO, b"");
    Info::decode(&p.expect(Tag::INFO)).unwrap()
}

#[test]
fn bad_clients_are_dropped_and_the_session_carries_on() {
    use std::io::{Read, Write};
    let dir = Dir::new("bad");
    dir.create("b1");
    // A frame announced at 1 GiB: the daemon hangs up instead of buffering it.
    let mut huge = std::os::unix::net::UnixStream::connect(dir.socket("b1")).unwrap();
    huge.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut header = vec![Tag::INPUT.0];
    header.extend_from_slice(&(1u32 << 30).to_le_bytes());
    header.extend_from_slice(&[0, 0, 0]);
    // The daemon may hang up as soon as the header is in: a write that then
    // finds the socket closed is the drop itself, not a test failure
    // (macOS reports it as "not connected").
    let gone = |e: &std::io::Error| {
        matches!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
        )
    };
    let mut closed = false;
    for chunk in [&header[..], &[b'x'; 4096][..]] {
        match huge.write_all(chunk) {
            Ok(()) => {}
            Err(e) if gone(&e) => {
                closed = true;
                break;
            }
            Err(e) => panic!("write: {e}"),
        }
    }
    if !closed {
        // Kept open, the read would time out after 5 s: not closed.
        let mut buf = [0u8; 64];
        closed = match huge.read(&mut buf) {
            Ok(0) => true,
            Err(e) => gone(&e),
            Ok(_) => false,
        };
    }
    assert!(closed, "the daemon kept the connection");
    // A screen of zero or 65535 x 65535 cells is ignored.
    let mut p = Peer::connect(&dir.socket("b1"));
    p.send(Tag::RESIZE, &[0u8; 8]);
    p.send(Tag::RESIZE, &[0xff; 8]);
    p.send(Tag::INPUT, b"echo alive-$((3+4))\r");
    p.wait_output("alive-7");
}

#[test]
fn a_request_sent_just_before_hanging_up_is_not_lost() {
    use std::io::Write;
    use tako_session::ipc::encode_frame;
    let dir = Dir::new("hangup");
    dir.create("h1");
    let mut leader = Peer::connect(&dir.socket("h1"));
    leader.init(24, 80);
    leader.expect(Tag::RESIZE);
    let ppid = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &info(&dir, "h1").pid.to_string()])
        .output()
        .unwrap();
    let daemon: i32 = String::from_utf8_lossy(&ppid.stdout)
        .trim()
        .parse()
        .unwrap();
    // `attach` inside a session writes Switch and exits at once. The daemon
    // takes the connection in the same round as program output, which it
    // then fails to send there: the request must still be read.
    for round in 0..3 {
        leader.send(
            Tag::INPUT,
            format!("sleep 0.2; printf out-{round}\r").as_bytes(),
        );
        leader.wait_output(&format!("printf out-{round}"));
        // SAFETY: stopping and resuming the session's own daemon.
        unsafe { libc::kill(daemon, libc::SIGSTOP) };
        let name = format!("next-{round}");
        let mut request = std::os::unix::net::UnixStream::connect(dir.socket("h1")).unwrap();
        request
            .write_all(&encode_frame(Tag::SWITCH, name.as_bytes()))
            .unwrap();
        drop(request);
        std::thread::sleep(Duration::from_millis(500));
        // SAFETY: as above.
        unsafe { libc::kill(daemon, libc::SIGCONT) };
        let payload = String::from_utf8(leader.expect(Tag::SWITCH)).unwrap();
        assert_eq!(payload.split('\n').next(), Some(name.as_str()), "{payload}");
    }
}

#[test]
fn a_created_session_runs_a_shell_that_answers_input() {
    let dir = Dir::new("shell");
    dir.create("s1");
    let mut p = Peer::connect(&dir.socket("s1"));
    p.init(24, 80);
    assert_eq!(
        p.expect(Tag::RESIZE),
        b"",
        "the first terminal leads and is asked for its size"
    );
    p.send(Tag::INPUT, b"echo hello-$((40+2)) $ZMX_SESSION\r");
    p.wait_output("hello-42 s1");

    let i = info(&dir, "s1");
    assert!(i.pid > 0 && !process_gone(i.pid), "{i:?}");
    assert_eq!(i.clients_len, 1, "the asking client is not counted");
    let cwd = String::from_utf8(i.cwd).unwrap();
    assert!(
        cwd.starts_with("file://") && cwd.ends_with(dir.path.to_str().unwrap()),
        "{cwd}"
    );

    p.send(Tag::HISTORY, &[0]);
    let history = String::from_utf8(p.expect(Tag::HISTORY)).unwrap();
    assert!(history.contains("hello-42 s1"), "{history}");

    let list = dir.run(&["list"]);
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(
        text.contains("s1") && text.contains(&format!("pid={}", i.pid)),
        "{text}"
    );
}

#[test]
fn kill_ends_the_shell_and_closes_the_socket() {
    let dir = Dir::new("kill");
    dir.create("k1");
    let pid = info(&dir, "k1").pid;
    let out = dir.run(&["kill", "k1"]);
    assert!(out.status.success(), "{out:?}");
    wait_until(Duration::from_secs(5), || process_gone(pid));
    wait_until(Duration::from_secs(5), || {
        std::os::unix::net::UnixStream::connect(dir.socket("k1")).is_err()
    });
}

#[test]
fn exiting_the_shell_ends_the_session() {
    let dir = Dir::new("exit");
    dir.create("e1");
    let mut p = Peer::connect(&dir.socket("e1"));
    p.init(24, 80);
    let pid = info(&dir, "e1").pid;
    p.send(Tag::INPUT, b"exit\r");
    wait_until(Duration::from_secs(5), || process_gone(pid));
    wait_until(Duration::from_secs(5), || {
        std::os::unix::net::UnixStream::connect(dir.socket("e1")).is_err()
    });
}

#[test]
fn a_second_attach_gets_the_screen_back() {
    let dir = Dir::new("replay");
    dir.create("r1");
    let mut first = Peer::connect(&dir.socket("r1"));
    first.init(24, 80);
    first.send(Tag::INPUT, b"printf 'marker-%s\\n' replayed\r");
    first.wait_output("marker-replayed");
    first.send(Tag::DETACH, b"");
    drop(first);

    let mut second = Peer::connect(&dir.socket("r1"));
    second.init(24, 80);
    second.wait_output("marker-replayed");
}

#[test]
fn labels_set_get_clear_and_guard() {
    let dir = Dir::new("labels");
    let out = dir.run(&[
        "attach",
        "--create-only",
        "--labels",
        "owner=tako gen=1",
        "l1",
    ]);
    // With labels the creator attaches through the guard; stdin is empty,
    // so it detaches at once.
    assert!(out.status.success(), "{out:?}");
    assert_eq!(dir.run(&["get", "l1"]).stdout, b"gen=1 owner=tako");
    assert!(dir.run(&["set", "l1", "gen=2"]).status.success());
    assert_eq!(dir.run(&["get", "l1", "gen"]).stdout, b"2");

    let mut p = Peer::connect(&dir.socket("l1"));
    p.send(Tag::ATTACH_IF_LABEL, &guard_payload("gen=1", ""));
    assert_eq!(p.expect(Tag::GUARD_STATUS), [GuardReply::Mismatch.byte()]);
    p.send(Tag::ATTACH_IF_LABEL, &guard_payload("gen=2", "gen=3"));
    assert_eq!(p.expect(Tag::GUARD_STATUS), [GuardReply::Matched.byte()]);
    p.send(Tag::ATTACH_IF_LABEL, &guard_payload("gen=", ""));
    assert_eq!(p.expect(Tag::GUARD_STATUS), [GuardReply::Invalid.byte()]);
    assert_eq!(dir.run(&["get", "l1", "gen"]).stdout, b"3");

    let mismatch = dir.run(&["kill", "--if-label", "gen=9", "l1"]);
    assert!(!mismatch.status.success());
    let pid = info(&dir, "l1").pid;
    assert!(
        dir.run(&["kill", "--if-label", "gen=3", "l1"])
            .status
            .success()
    );
    wait_until(Duration::from_secs(5), || process_gone(pid));

    assert!(dir.run(&["attach", "--create-only", "l2"]).status.success());
    assert!(dir.run(&["clear", "l2"]).status.success());
    assert_eq!(dir.run(&["get", "l2"]).stdout, b"");
}

#[test]
fn create_only_refuses_an_existing_session_and_attach_only_a_missing_one() {
    let dir = Dir::new("modes");
    dir.create("m1");
    let again = dir.run(&["attach", "--create-only", "m1"]);
    assert_eq!(
        String::from_utf8_lossy(&again.stderr),
        "error: session \"m1\" already exists\n"
    );
    let missing = dir.run(&["attach", "--if-label", "a=1", "nosuch"]);
    assert_eq!(
        String::from_utf8_lossy(&missing.stderr),
        "error: session \"nosuch\" not found\n"
    );
    let mismatch = dir.run(&["attach", "--if-label", "a=1", "m1"]);
    assert_eq!(
        String::from_utf8_lossy(&mismatch.stderr),
        "error: session \"m1\" label mismatch\n"
    );
}

#[test]
fn a_command_runs_instead_of_the_shell_and_shows_in_info() {
    let dir = Dir::new("cmd");
    let out = dir.run(&[
        "attach",
        "--create-only",
        "c1",
        "sh",
        "-c",
        "echo from-cmd; sleep 30",
    ]);
    assert!(out.status.success(), "{out:?}");
    wait_until(Duration::from_secs(5), || {
        std::os::unix::net::UnixStream::connect(dir.socket("c1")).is_ok()
    });
    let i = info(&dir, "c1");
    assert_eq!(
        String::from_utf8(i.cmd).unwrap(),
        "sh -c 'echo from-cmd; sleep 30'"
    );
    let mut p = Peer::connect(&dir.socket("c1"));
    p.send(Tag::HISTORY, &[0]);
    let start = std::time::Instant::now();
    loop {
        let h = String::from_utf8(p.expect(Tag::HISTORY)).unwrap();
        if h.contains("from-cmd") {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{h}");
        std::thread::sleep(Duration::from_millis(50));
        p.send(Tag::HISTORY, &[0]);
    }
}
