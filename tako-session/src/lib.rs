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
//! A daemon per session (`daemon`) owns the PTY and keeps the screen in a
//! `tako_core` terminal; clients (`client`, `cmd`) attach to it or query it
//! over a Unix socket in the wire protocol of `ipc`, as documented in
//! `docs/session-protocol.md`.

pub mod ansi;
pub mod cfg;
pub mod cli;
pub mod client;
pub mod cmd;
pub mod daemon;
pub mod input;
pub mod ipc;
pub mod keys;
pub mod log;
pub mod osc7;
pub mod pty_text;
pub mod signal;
pub mod socket;
pub mod term;
