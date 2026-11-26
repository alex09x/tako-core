/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The session runtime Tako keeps shells alive with across a relaunch.
//!
//! So far this crate provides the wire protocol between a session daemon and
//! its clients (`ipc`): frames, tags, payloads and labels, as documented in
//! `docs/session-protocol.md`. The daemon and the client commands build on it.

pub mod ipc;
