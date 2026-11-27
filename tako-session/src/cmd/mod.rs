/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The command-line commands, one module each.

pub mod attach;
pub mod attach_args;
pub mod detach;
pub mod error;
pub mod history;
pub mod kill;
pub mod labels;
pub mod list;
pub mod run;
pub mod send;
pub mod tail;
pub mod tasks;
pub mod wait;

pub use error::{CliError, CliResult};
