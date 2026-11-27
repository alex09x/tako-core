/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Client commands against a stand-in daemon that answers with the reference
//! daemon's recorded bytes, and checks the client's request bytes.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use tako_session::cmd::list;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/p3/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ts-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Serves one connection: reads the request bytes until `request_len` have
/// arrived, answers with `reply`, then hangs up. Returns what was read.
fn serve_once(path: &Path, request_len: usize, reply: Vec<u8>) -> JoinHandle<Vec<u8>> {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut got = vec![0u8; request_len];
        stream.read_exact(&mut got).unwrap();
        stream.write_all(&reply).unwrap();
        got
    })
}

#[test]
fn list_probes_with_the_reference_request_and_prints_the_reference_reply() {
    let dir = temp_dir("list");
    let request = fixture("list.c2s");
    let daemon = serve_once(&dir.join("s1"), request.len(), fixture("list.s2c"));

    let (mut out, mut err) = (Vec::new(), Vec::new());
    list::run(&dir, false, "", &mut out, &mut err).unwrap();
    assert_eq!(
        daemon.join().unwrap(),
        request,
        "the probe sends Info then LabelGet"
    );

    let line = String::from_utf8(out).unwrap();
    assert!(
        line.starts_with("  name=s1\tpid=") && line.contains("\tclients=0\tcreated="),
        "{line}"
    );
    assert!(
        line.ends_with(
            "\tcwd=file://fixture-host/private/tmp/tako-session-fixture\tcmd=/bin/sh\ttako-gen=g1\n"
        ),
        "{line}"
    );
    assert!(err.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn list_skips_a_refused_socket_without_deleting_it_and_says_when_empty() {
    let dir = temp_dir("refused");
    drop(UnixListener::bind(dir.join("dead")).unwrap());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    list::run(&dir, false, "", &mut out, &mut err).unwrap();
    assert!(out.is_empty());
    assert_eq!(
        String::from_utf8(err).unwrap(),
        format!("no sessions found in {}\n", dir.display())
    );
    assert!(
        dir.join("dead").exists(),
        "a listing never deletes a socket"
    );
    let (mut out, mut err) = (Vec::new(), Vec::new());
    list::run(&dir, true, "", &mut out, &mut err).unwrap();
    assert!(
        out.is_empty() && err.is_empty(),
        "--short is silent when empty"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

fn connect(path: &Path) -> tako_session::client::Conn {
    tako_session::client::Conn::from_stream(std::os::unix::net::UnixStream::connect(path).unwrap())
}

#[test]
fn guarded_attach_against_the_reference_reply_keeps_what_follows_the_labels() {
    use tako_session::client::{Hooks, guarded_attach};
    use tako_session::ipc::{FrameBuffer, Tag, encode_frame, guard_payload};
    let dir = temp_dir("attach");
    let mut request = encode_frame(
        Tag::ATTACH_IF_LABEL,
        &guard_payload("tako-gen=g1", "tako-attempt=a1"),
    );
    request.extend(encode_frame(Tag::LABEL_GET, b""));
    let reply = fixture("attach-if-label.s2c");
    let daemon = serve_once(&dir.join("s1"), request.len(), reply.clone());

    let mut conn = connect(&dir.join("s1"));
    let unread = guarded_attach(
        &mut conn,
        "tako-gen=g1",
        Some("tako-attempt=a1"),
        true,
        1000,
        &Hooks::default(),
    )
    .unwrap();
    assert_eq!(
        daemon.join().unwrap(),
        request,
        "the reference client's first two frames"
    );

    let mut skip = FrameBuffer::new();
    skip.push(&reply);
    assert_eq!(skip.next_frame().unwrap().header.tag, Tag::GUARD_STATUS);
    assert_eq!(skip.next_frame().unwrap().header.tag, Tag::LABEL_DATA);
    assert_eq!(
        unread,
        skip.unread(),
        "the Resize and Output frames go to the attached loop"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A daemon from before protocol 3: ignores AttachIfLabel, answers each
/// LabelGet with `labels`, acknowledges LabelSet. Returns the request tags.
fn legacy_daemon(path: &Path, labels: &'static [u8]) -> JoinHandle<Vec<(u8, Vec<u8>)>> {
    use tako_session::ipc::{FrameBuffer, Tag, encode_frame};
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (mut buf, mut seen, mut tmp) = (FrameBuffer::new(), Vec::new(), [0u8; 4096]);
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                return seen;
            }
            buf.push(&tmp[..n]);
            while let Some(frame) = buf.next_frame() {
                seen.push((frame.header.tag.0, frame.payload.to_vec()));
                let answer = match frame.header.tag {
                    Tag::LABEL_GET => encode_frame(Tag::LABEL_DATA, labels),
                    Tag::LABEL_SET => encode_frame(Tag::ACK, b""),
                    _ => continue,
                };
                stream.write_all(&answer).unwrap();
            }
        }
    })
}

#[test]
fn guarded_attach_falls_back_for_an_old_daemon_and_checks_twice() {
    use tako_session::client::{Hooks, guarded_attach};
    let dir = temp_dir("legacy");
    let daemon = legacy_daemon(&dir.join("old"), b"tako-gen=g1");
    let mut conn = connect(&dir.join("old"));
    let unread = guarded_attach(
        &mut conn,
        "tako-gen=g1",
        Some("tako-attempt=a1"),
        true,
        1000,
        &Hooks::default(),
    )
    .unwrap();
    assert!(unread.is_empty());
    drop(conn);
    let tags: Vec<u8> = daemon.join().unwrap().into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        tags,
        [23, 14, 14, 15],
        "AttachIfLabel, LabelGet, a second LabelGet, then LabelSet"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn guarded_attach_refuses_an_old_daemon_with_other_labels() {
    use tako_session::client::{HandshakeError, Hooks, guarded_attach};
    let dir = temp_dir("legacy-mismatch");
    let daemon = legacy_daemon(&dir.join("old"), b"tako-gen=g2");
    let mut conn = connect(&dir.join("old"));
    let err = guarded_attach(
        &mut conn,
        "tako-gen=g1",
        Some("tako-attempt=a1"),
        true,
        1000,
        &Hooks::default(),
    );
    assert_eq!(err, Err(HandshakeError::LabelMismatch));
    drop(conn);
    let tags: Vec<u8> = daemon.join().unwrap().into_iter().map(|(t, _)| t).collect();
    assert_eq!(tags, [23, 14], "no LabelSet on a mismatch");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn list_reports_a_silent_daemon_as_unreachable() {
    let dir = temp_dir("silent");
    let listener = UnixListener::bind(dir.join("mute")).unwrap();
    let hold = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        drop(stream);
    });
    let (mut out, mut err) = (Vec::new(), Vec::new());
    list::run(&dir, false, "mute", &mut out, &mut err).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "→ name=mute\terr=Timeout\tstatus=unreachable\n"
    );
    hold.join().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
