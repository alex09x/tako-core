/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The client side of the protocol: connections, short exchanges, the
//! guarded attach handshake and the attached loop.

pub mod conn;
pub mod handshake;
pub mod probe;
pub mod session_loop;

pub use conn::{Conn, ProbeError, monotonic_millis};
pub use handshake::{HandshakeError, Hooks, guarded_attach};
pub use probe::{
    Probe, await_guard_status, await_tag, await_tag_until, keep_output, probe_session,
    round_trip_for_tag,
};
pub use session_loop::LoopEnd;
