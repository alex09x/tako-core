/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The wire protocol between a session daemon and its clients: framed
//! messages over a Unix stream socket, documented in
//! `docs/session-protocol.md`. Every shape here is frozen: daemons started by
//! an older runtime keep running across an upgrade and must still be reached.

mod buffer;
mod header;
mod labels;
mod payload;
mod tag;

pub use buffer::{Frame, FrameBuffer};
pub use header::{HEADER_LEN, Header, append_frame, encode_frame, expected_length};
pub use labels::{
    LabelError, assert_label, label_pairs_match, label_value, labels_to_payload, parse_labels,
    validate_label_pairs,
};
pub use payload::{
    GuardReply, INFO_LEN, Info, LEGACY_RESIZE_LEN, MAX_CMD_LEN, MAX_CWD_LEN, RESIZE_LEN, Resize,
    append_size_frames, guard_payload,
};
pub use tag::Tag;

/// Bytes of `.Output` a guarded attach keeps while it waits for its reply,
/// so nothing the shell printed meanwhile is lost; past it the attach fails.
pub const MAX_HANDSHAKE_UNREAD_BYTES: usize = 4 * 1024 * 1024;
