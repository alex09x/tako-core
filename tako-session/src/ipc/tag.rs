/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Message tags. A tag is any byte: values this build does not know arrive
//! from newer peers and are ignored, never rejected, so an old daemon stays
//! reachable by a newer client.

/// A message tag as it travels on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tag(pub u8);

impl Tag {
    pub const INPUT: Tag = Tag(0);
    pub const OUTPUT: Tag = Tag(1);
    pub const RESIZE: Tag = Tag(2);
    pub const DETACH: Tag = Tag(3);
    pub const DETACH_ALL: Tag = Tag(4);
    pub const KILL: Tag = Tag(5);
    pub const INFO: Tag = Tag(6);
    pub const INIT: Tag = Tag(7);
    pub const HISTORY: Tag = Tag(8);
    pub const RUN: Tag = Tag(9);
    pub const ACK: Tag = Tag(10);
    pub const SWITCH: Tag = Tag(11);
    pub const WRITE: Tag = Tag(12);
    pub const TASK_COMPLETE: Tag = Tag(13);
    pub const LABEL_GET: Tag = Tag(14);
    pub const LABEL_SET: Tag = Tag(15);
    pub const LABEL_CLEAR: Tag = Tag(16);
    pub const LABEL_DATA: Tag = Tag(17);
    pub const SEND: Tag = Tag(18);
    pub const ENV_GET: Tag = Tag(19);
    pub const ENV_SET: Tag = Tag(20);
    pub const ENV_DATA: Tag = Tag(21);
    /// Protocol 3: end the session only when its labels match.
    pub const KILL_IF_LABEL: Tag = Tag(22);
    /// Protocol 3: attach only when the labels match, then set new ones.
    pub const ATTACH_IF_LABEL: Tag = Tag(23);
    /// Protocol 3: the daemon's one-byte answer to the two guarded requests.
    pub const GUARD_STATUS: Tag = Tag(24);

    /// The tag's name, or None for a value this build does not know.
    pub fn name(self) -> Option<&'static str> {
        const NAMES: [&str; 25] = [
            "Input",
            "Output",
            "Resize",
            "Detach",
            "DetachAll",
            "Kill",
            "Info",
            "Init",
            "History",
            "Run",
            "Ack",
            "Switch",
            "Write",
            "TaskComplete",
            "LabelGet",
            "LabelSet",
            "LabelClear",
            "LabelData",
            "Send",
            "EnvGet",
            "EnvSet",
            "EnvData",
            "KillIfLabel",
            "AttachIfLabel",
            "GuardStatus",
        ];
        NAMES.get(self.0 as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::Tag;

    #[test]
    fn wire_values_are_frozen() {
        let frozen = [
            (Tag::INPUT, 0),
            (Tag::OUTPUT, 1),
            (Tag::RESIZE, 2),
            (Tag::DETACH, 3),
            (Tag::DETACH_ALL, 4),
            (Tag::KILL, 5),
            (Tag::INFO, 6),
            (Tag::INIT, 7),
            (Tag::HISTORY, 8),
            (Tag::RUN, 9),
            (Tag::ACK, 10),
            (Tag::SWITCH, 11),
            (Tag::WRITE, 12),
            (Tag::TASK_COMPLETE, 13),
            (Tag::LABEL_GET, 14),
            (Tag::LABEL_SET, 15),
            (Tag::LABEL_CLEAR, 16),
            (Tag::LABEL_DATA, 17),
            (Tag::SEND, 18),
            (Tag::ENV_GET, 19),
            (Tag::ENV_SET, 20),
            (Tag::ENV_DATA, 21),
            (Tag::KILL_IF_LABEL, 22),
            (Tag::ATTACH_IF_LABEL, 23),
            (Tag::GUARD_STATUS, 24),
        ];
        for (tag, value) in frozen {
            assert_eq!(tag.0, value, "{:?}", tag.name());
        }
    }

    #[test]
    fn unknown_tags_have_no_name_but_are_representable() {
        assert_eq!(Tag(24).name(), Some("GuardStatus"));
        assert_eq!(Tag(25).name(), None);
        assert_eq!(Tag(255).name(), None);
    }
}
