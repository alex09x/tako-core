/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The 8-byte frame header: byte 0 the tag, bytes 1-4 the payload length
//! (u32, little-endian), bytes 5-7 zero. Readers ignore bytes 5-7.

use super::tag::Tag;

/// Size of a frame header on the wire.
pub const HEADER_LEN: usize = 8;

/// A decoded frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub tag: Tag,
    pub len: u32,
}

impl Header {
    /// The header's wire bytes.
    pub fn encode(self) -> [u8; HEADER_LEN] {
        let len = self.len.to_le_bytes();
        [self.tag.0, len[0], len[1], len[2], len[3], 0, 0, 0]
    }

    /// Reads a header from the first eight bytes of `bytes`, if there are eight.
    pub fn decode(bytes: &[u8]) -> Option<Header> {
        let head: &[u8; HEADER_LEN] = bytes.get(..HEADER_LEN)?.try_into().ok()?;
        Some(Header {
            tag: Tag(head[0]),
            len: u32::from_le_bytes([head[1], head[2], head[3], head[4]]),
        })
    }
}

/// Header plus payload length of the frame at the start of `data`, or None
/// until a whole header has arrived. Saturates instead of wrapping: on a
/// target where the sum does not fit in `usize`, the frame reads as
/// `usize::MAX` bytes long and never completes.
pub fn expected_length(data: &[u8]) -> Option<usize> {
    Header::decode(data).map(|h| {
        usize::try_from(h.len)
            .ok()
            .and_then(|len| len.checked_add(HEADER_LEN))
            .unwrap_or(usize::MAX)
    })
}

/// Appends one frame to `out`.
///
/// # Panics
/// When `payload` is longer than `u32::MAX` bytes, which no message is.
pub fn append_frame(out: &mut Vec<u8>, tag: Tag, payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("frame payload fits in u32");
    out.reserve(HEADER_LEN + payload.len());
    out.extend_from_slice(&Header { tag, len }.encode());
    out.extend_from_slice(payload);
}

/// One frame's wire bytes.
pub fn encode_frame(tag: Tag, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    append_frame(&mut out, tag, payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_is_tag_then_le_length_then_zeros() {
        let bytes = Header {
            tag: Tag::LABEL_DATA,
            len: 0x0102_0304,
        }
        .encode();
        assert_eq!(bytes, [17, 4, 3, 2, 1, 0, 0, 0]);
        assert_eq!(
            Header::decode(&bytes),
            Some(Header {
                tag: Tag::LABEL_DATA,
                len: 0x0102_0304
            })
        );
    }

    #[test]
    fn decode_ignores_the_tail_bytes() {
        let header = Header::decode(&[1, 5, 0, 0, 0, 0xAA, 0xBB, 0xCC]).unwrap();
        assert_eq!(
            header,
            Header {
                tag: Tag::OUTPUT,
                len: 5
            }
        );
    }

    #[test]
    fn a_short_header_is_not_decoded() {
        assert_eq!(Header::decode(&[1, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(expected_length(&[0; 7]), None);
    }

    #[test]
    fn expected_length_does_not_wrap_near_u32_max() {
        let header = Header {
            tag: Tag::OUTPUT,
            len: u32::MAX,
        }
        .encode();
        let expected = (HEADER_LEN as u64 + u64::from(u32::MAX))
            .try_into()
            .unwrap_or(usize::MAX);
        assert_eq!(expected_length(&header), Some(expected));
        assert!(
            expected_length(&header).unwrap() > HEADER_LEN,
            "never wraps below the header"
        );
    }

    #[test]
    fn an_empty_payload_is_a_bare_header() {
        assert_eq!(encode_frame(Tag::INFO, b""), [6, 0, 0, 0, 0, 0, 0, 0]);
    }
}
