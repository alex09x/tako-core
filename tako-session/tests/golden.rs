/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Golden fixtures: byte streams recorded between the reference runtime's
//! client and daemon (protocol 3), by `scripts/record-fixtures.py`. The codec
//! must produce the clients' bytes exactly and read the daemons' bytes back
//! into the same frames.

use tako_session::ipc::{
    FrameBuffer, GuardReply, HEADER_LEN, Info, Resize, Tag, append_frame, append_size_frames,
    encode_frame, guard_payload,
};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/p3/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Every frame of a stream, which must be consumed to its last byte.
fn frames(stream: &[u8]) -> Vec<(Tag, Vec<u8>)> {
    let mut buf = FrameBuffer::new();
    buf.push(stream);
    let mut out = Vec::new();
    while let Some(frame) = buf.next_frame() {
        out.push((frame.header.tag, frame.payload.to_vec()));
    }
    assert!(
        buf.unread().is_empty(),
        "{} bytes left over",
        buf.unread().len()
    );
    out
}

const STREAMS: [&str; 10] = [
    "list.c2s",
    "list.s2c",
    "get.c2s",
    "get.s2c",
    "kill-if-label-mismatch.c2s",
    "kill-if-label-mismatch.s2c",
    "kill-if-label-match.c2s",
    "kill-if-label-match.s2c",
    "attach-if-label.c2s",
    "attach-if-label.s2c",
];

#[test]
fn every_recorded_stream_re_encodes_byte_for_byte() {
    for name in STREAMS {
        let stream = fixture(name);
        let mut again = Vec::new();
        for (tag, payload) in frames(&stream) {
            append_frame(&mut again, tag, &payload);
        }
        assert_eq!(again, stream, "{name}");
    }
}

/// Fixtures are public: they must not name the machine, the user or a
/// private path they were recorded on.
#[test]
fn fixtures_hold_no_private_data() {
    let mut markers = vec![
        "/Users/".to_string(),
        "/home/".to_string(),
        "/private/tmp/claude".to_string(),
        ".local/".to_string(),
    ];
    for var in ["USER", "LOGNAME"] {
        // As a path component or in a mangled path, where a login leaks; a
        // bare substring would also hit ordinary words such as label names.
        if let Ok(login) = std::env::var(var)
            && !login.is_empty()
        {
            markers.push(format!("/{login}/"));
            markers.push(format!("-{login}-"));
        }
    }
    let dir = format!("{}/tests/fixtures/p3", env!("CARGO_MANIFEST_DIR"));
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        for marker in &markers {
            assert!(
                !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
                "{} contains {marker:?}",
                path.display()
            );
        }
    }
}

#[test]
fn every_recorded_header_has_zero_tail_bytes() {
    for name in STREAMS {
        let stream = fixture(name);
        let mut at = 0;
        for (_, payload) in frames(&stream) {
            assert_eq!(&stream[at + 5..at + HEADER_LEN], &[0, 0, 0], "{name} @{at}");
            at += HEADER_LEN + payload.len();
        }
    }
}

#[test]
fn list_asks_for_info_and_labels_and_info_round_trips_exactly() {
    let mut request = encode_frame(Tag::INFO, b"");
    request.extend(encode_frame(Tag::LABEL_GET, b""));
    assert_eq!(fixture("list.c2s"), request);

    let reply = frames(&fixture("list.s2c"));
    assert_eq!(reply.len(), 2);
    let (tag, raw) = &reply[0];
    assert_eq!(*tag, Tag::INFO);
    let info = Info::decode(raw).expect("a 552-byte Info");
    assert_eq!(
        info.encode().as_slice(),
        raw.as_slice(),
        "decode + encode keeps every byte"
    );
    assert_eq!(info.cmd, b"/bin/sh");
    assert!(info.pid > 0 && info.created_at > 0);
    assert_eq!(reply[1], (Tag::LABEL_DATA, b"tako-gen=g1".to_vec()));
}

#[test]
fn get_reads_labels() {
    assert_eq!(fixture("get.c2s"), encode_frame(Tag::LABEL_GET, b""));
    assert_eq!(
        frames(&fixture("get.s2c")),
        [(Tag::LABEL_DATA, b"tako-gen=g1".to_vec())]
    );
}

#[test]
fn guarded_kill_sends_the_labels_and_gets_one_byte() {
    assert_eq!(
        fixture("kill-if-label-mismatch.c2s"),
        encode_frame(Tag::KILL_IF_LABEL, b"tako-gen=nope")
    );
    assert_eq!(
        frames(&fixture("kill-if-label-mismatch.s2c")),
        [(Tag::GUARD_STATUS, vec![1])]
    );
    assert_eq!(
        fixture("kill-if-label-match.c2s"),
        encode_frame(Tag::KILL_IF_LABEL, b"tako-gen=g1")
    );
    let reply = frames(&fixture("kill-if-label-match.s2c"));
    assert_eq!(GuardReply::decode(&reply[0].1), Some(GuardReply::Matched));
}

#[test]
fn guarded_attach_request_matches_the_reference_client() {
    let size = Resize {
        rows: 30,
        cols: 100,
        xpixel: 0,
        ypixel: 0,
    };
    let mut request = encode_frame(
        Tag::ATTACH_IF_LABEL,
        &guard_payload("tako-gen=g1", "tako-attempt=a1"),
    );
    append_frame(&mut request, Tag::LABEL_GET, b"");
    append_size_frames(&mut request, Tag::INIT, size);
    append_size_frames(&mut request, Tag::RESIZE, size);
    append_frame(&mut request, Tag::INPUT, b"echo $((6*7))x\r");
    assert_eq!(fixture("attach-if-label.c2s"), request);
}

#[test]
fn guarded_attach_reply_is_guard_then_labels_then_output() {
    let reply = frames(&fixture("attach-if-label.s2c"));
    assert_eq!(reply[0], (Tag::GUARD_STATUS, vec![0]));
    assert_eq!(
        reply[1],
        (Tag::LABEL_DATA, b"tako-attempt=a1 tako-gen=g1".to_vec())
    );
    assert_eq!(
        reply[2],
        (Tag::RESIZE, vec![]),
        "the daemon's empty Resize after Init"
    );
    assert!(reply[3..].iter().all(|(tag, _)| *tag == Tag::OUTPUT));
    let output: Vec<u8> = reply[3..].iter().flat_map(|(_, p)| p.clone()).collect();
    assert!(
        String::from_utf8_lossy(&output).contains("42x\r\n"),
        "the shell ran the typed command"
    );
}
