/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Reassembles frames from a byte stream that arrives in arbitrary pieces.

use super::header::{HEADER_LEN, Header, expected_length};

/// One complete frame borrowed from a `FrameBuffer`.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub header: Header,
    pub payload: &'a [u8],
}

/// Bytes received but not yet consumed as frames.
#[derive(Debug, Default)]
pub struct FrameBuffer {
    buf: Vec<u8>,
    head: usize,
}

impl FrameBuffer {
    pub fn new() -> FrameBuffer {
        FrameBuffer {
            buf: Vec::with_capacity(4096),
            head: 0,
        }
    }

    /// Adds received bytes, first dropping the frames already consumed.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.head > 0 {
            self.buf.drain(..self.head);
            self.head = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete frame, or None until one has fully arrived. The
    /// frame stays valid until the next call to `push` or `next_frame`. Not an
    /// `Iterator`: each frame borrows the buffer itself.
    pub fn next_frame(&mut self) -> Option<Frame<'_>> {
        let available = &self.buf[self.head..];
        let total = expected_length(available)?;
        if available.len() < total {
            return None;
        }
        let header = Header::decode(available)?;
        let start = self.head + HEADER_LEN;
        self.head += total;
        Some(Frame {
            header,
            payload: &self.buf[start..self.head],
        })
    }

    /// Bytes received and not yet returned as frames.
    pub fn unread(&self) -> &[u8] {
        &self.buf[self.head..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{Tag, encode_frame};

    #[test]
    fn frames_split_at_every_byte_reassemble() {
        let mut stream = encode_frame(Tag::LABEL_DATA, b"tako-gen=g1");
        stream.extend(encode_frame(Tag::INFO, b""));
        stream.extend(encode_frame(Tag::OUTPUT, b"$ "));
        let mut buf = FrameBuffer::new();
        let mut got = Vec::new();
        for byte in &stream {
            buf.push(std::slice::from_ref(byte));
            while let Some(frame) = buf.next_frame() {
                got.push((frame.header.tag, frame.payload.to_vec()));
            }
        }
        assert_eq!(
            got,
            [
                (Tag::LABEL_DATA, b"tako-gen=g1".to_vec()),
                (Tag::INFO, vec![]),
                (Tag::OUTPUT, b"$ ".to_vec())
            ]
        );
        assert!(buf.unread().is_empty());
    }

    #[test]
    fn a_partial_frame_stays_unread() {
        let mut buf = FrameBuffer::new();
        buf.push(&encode_frame(Tag::OUTPUT, b"hello")[..10]);
        assert!(buf.next_frame().is_none());
        assert_eq!(buf.unread().len(), 10);
    }

    #[test]
    fn unread_keeps_bytes_after_a_consumed_frame_at_a_boundary() {
        let mut buf = FrameBuffer::new();
        let mut stream = encode_frame(Tag::GUARD_STATUS, &[0]);
        stream.extend(encode_frame(Tag::OUTPUT, b"x"));
        buf.push(&stream);
        assert_eq!(buf.next_frame().unwrap().header.tag, Tag::GUARD_STATUS);
        assert_eq!(buf.unread(), encode_frame(Tag::OUTPUT, b"x"));
    }
}
