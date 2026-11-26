/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Fixed-shape payloads: the window size of `Init`/`Resize`, the `Info`
//! record and the `GuardStatus` reply. All integers are little-endian.

use super::header::append_frame;
use super::tag::Tag;

/// `Init`/`Resize` payload: rows, cols, xpixel, ypixel as u16 each.
pub const RESIZE_LEN: usize = 8;
/// The rows+cols prefix daemons before 0.7.0 expect. Clients send both forms,
/// and each daemon drops the length it does not expect.
pub const LEGACY_RESIZE_LEN: usize = 4;
/// Size of the frozen `Info` record.
pub const INFO_LEN: usize = 552;
pub const MAX_CMD_LEN: usize = 256;
pub const MAX_CWD_LEN: usize = 256;

/// A terminal size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resize {
    pub rows: u16,
    pub cols: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

impl Resize {
    pub fn encode(self) -> [u8; RESIZE_LEN] {
        let mut out = [0u8; RESIZE_LEN];
        for (i, v) in [self.rows, self.cols, self.xpixel, self.ypixel]
            .into_iter()
            .enumerate()
        {
            out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Decodes the current 8-byte form only; the 4-byte form is the one a
    /// current daemon ignores.
    pub fn decode(payload: &[u8]) -> Option<Resize> {
        if payload.len() != RESIZE_LEN {
            return None;
        }
        let at = |i: usize| u16::from_le_bytes([payload[i], payload[i + 1]]);
        Some(Resize {
            rows: at(0),
            cols: at(2),
            xpixel: at(4),
            ypixel: at(6),
        })
    }
}

/// Appends `tag` (`Init` or `Resize`) twice: the current form, then the
/// legacy rows+cols prefix.
pub fn append_size_frames(out: &mut Vec<u8>, tag: Tag, size: Resize) {
    let bytes = size.encode();
    append_frame(out, tag, &bytes);
    append_frame(out, tag, &bytes[..LEGACY_RESIZE_LEN]);
}

/// The `Info` record, a C struct on the wire: clients_len u64 @0, pid i32
/// @8, cmd_len u16 @12, cwd_len u16 @14, cmd [256] @16, cwd [256] @272,
/// created_at u64 @528, task_ended_at u64 @536, task_exit_code u8 @544,
/// zero padding to 552.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Info {
    pub clients_len: u64,
    pub pid: i32,
    pub cmd: Vec<u8>,
    pub cwd: Vec<u8>,
    pub created_at: u64,
    pub task_ended_at: u64,
    pub task_exit_code: u8,
}

impl Info {
    pub fn encode(&self) -> [u8; INFO_LEN] {
        let mut out = [0u8; INFO_LEN];
        let cmd = &self.cmd[..self.cmd.len().min(MAX_CMD_LEN)];
        let cwd = &self.cwd[..self.cwd.len().min(MAX_CWD_LEN)];
        out[0..8].copy_from_slice(&self.clients_len.to_le_bytes());
        out[8..12].copy_from_slice(&self.pid.to_le_bytes());
        out[12..14].copy_from_slice(&(cmd.len() as u16).to_le_bytes());
        out[14..16].copy_from_slice(&(cwd.len() as u16).to_le_bytes());
        out[16..16 + cmd.len()].copy_from_slice(cmd);
        out[272..272 + cwd.len()].copy_from_slice(cwd);
        out[528..536].copy_from_slice(&self.created_at.to_le_bytes());
        out[536..544].copy_from_slice(&self.task_ended_at.to_le_bytes());
        out[544] = self.task_exit_code;
        out
    }

    /// Decodes a record of exactly `INFO_LEN` bytes. Lengths past the
    /// arrays are clamped, as a reader of the C struct would.
    pub fn decode(payload: &[u8]) -> Option<Info> {
        if payload.len() != INFO_LEN {
            return None;
        }
        let u64_at = |i: usize| u64::from_le_bytes(payload[i..i + 8].try_into().unwrap());
        let u16_at = |i: usize| u16::from_le_bytes([payload[i], payload[i + 1]]) as usize;
        let cmd_len = u16_at(12).min(MAX_CMD_LEN);
        let cwd_len = u16_at(14).min(MAX_CWD_LEN);
        Some(Info {
            clients_len: u64_at(0),
            pid: i32::from_le_bytes(payload[8..12].try_into().unwrap()),
            cmd: payload[16..16 + cmd_len].to_vec(),
            cwd: payload[272..272 + cwd_len].to_vec(),
            created_at: u64_at(528),
            task_ended_at: u64_at(536),
            task_exit_code: payload[544],
        })
    }
}

/// The daemon's answer to `KillIfLabel` and `AttachIfLabel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardReply {
    Matched,
    Mismatch,
    Invalid,
    /// A value this build does not know; callers treat it as `Invalid`.
    Unknown(u8),
}

impl GuardReply {
    pub fn byte(self) -> u8 {
        match self {
            GuardReply::Matched => 0,
            GuardReply::Mismatch => 1,
            GuardReply::Invalid => 2,
            GuardReply::Unknown(b) => b,
        }
    }

    /// Decodes a `GuardStatus` payload, which is exactly one byte.
    pub fn decode(payload: &[u8]) -> Option<GuardReply> {
        match payload {
            [0] => Some(GuardReply::Matched),
            [1] => Some(GuardReply::Mismatch),
            [2] => Some(GuardReply::Invalid),
            [b] => Some(GuardReply::Unknown(*b)),
            _ => None,
        }
    }
}

/// The `AttachIfLabel` payload: the labels that must match, a newline, then
/// the labels to set (possibly empty).
pub fn guard_payload(required: &str, set: &str) -> Vec<u8> {
    format!("{required}\n{set}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_is_four_le_u16s() {
        let size = Resize {
            rows: 45,
            cols: 170,
            xpixel: 900,
            ypixel: 1800,
        };
        assert_eq!(size.encode(), [45, 0, 170, 0, 0x84, 3, 0x08, 7]);
        assert_eq!(Resize::decode(&size.encode()), Some(size));
        assert_eq!(Resize::decode(&size.encode()[..4]), None);
    }

    #[test]
    fn size_frames_are_current_then_legacy_prefix() {
        let mut out = Vec::new();
        append_size_frames(
            &mut out,
            Tag::INIT,
            Resize {
                rows: 30,
                cols: 100,
                xpixel: 0,
                ypixel: 0,
            },
        );
        assert_eq!(
            out,
            [
                7, 8, 0, 0, 0, 0, 0, 0, 30, 0, 100, 0, 0, 0, 0, 0, 7, 4, 0, 0, 0, 0, 0, 0, 30, 0,
                100, 0
            ]
        );
    }

    #[test]
    fn info_round_trips_and_pads_with_zeros() {
        let info = Info {
            clients_len: 3,
            pid: 999,
            cmd: b"/bin/sh".to_vec(),
            cwd: b"/tmp".to_vec(),
            created_at: 1_700_000_000,
            task_ended_at: 0,
            task_exit_code: 7,
        };
        let bytes = info.encode();
        assert_eq!(bytes.len(), 552);
        assert_eq!(&bytes[12..16], &[7, 0, 4, 0]);
        assert!(bytes[545..].iter().all(|&b| b == 0), "tail padding is zero");
        assert_eq!(Info::decode(&bytes), Some(info));
        assert_eq!(Info::decode(&bytes[..551]), None);
    }

    #[test]
    fn info_clamps_oversized_lengths() {
        let mut bytes = [0u8; INFO_LEN];
        bytes[12..14].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(Info::decode(&bytes).unwrap().cmd.len(), MAX_CMD_LEN);
    }

    #[test]
    fn guard_reply_is_one_byte_and_unknown_values_survive() {
        assert_eq!(GuardReply::decode(&[0]), Some(GuardReply::Matched));
        assert_eq!(GuardReply::decode(&[2]), Some(GuardReply::Invalid));
        assert_eq!(GuardReply::decode(&[9]), Some(GuardReply::Unknown(9)));
        assert_eq!(GuardReply::decode(&[]), None);
        assert_eq!(GuardReply::decode(&[0, 0]), None);
    }

    #[test]
    fn guard_payload_joins_with_a_newline() {
        assert_eq!(guard_payload("tako-gen=g1", ""), b"tako-gen=g1\n");
        assert_eq!(guard_payload("a=1", "b=2"), b"a=1\nb=2");
    }
}
