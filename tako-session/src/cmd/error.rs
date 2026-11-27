/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! How a command fails: always exit status 1, with one of three outputs.

use std::fmt;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliError {
    /// Printed as `error: <message>` on standard error.
    Message(String),
    /// Printed on standard error exactly as given (it carries its own prefix).
    Line(String),
    /// Nothing printed: an error passed up from deep inside a command.
    Silent,
    /// Nothing more printed; the process exits with this status (a task's
    /// exit code, or `wait`'s 2).
    Exit(i32),
}

pub type CliResult = Result<(), CliError>;

impl CliError {
    pub fn msg(message: impl Into<String>) -> CliError {
        CliError::Message(message.into())
    }

    /// The text written to standard error, if any.
    pub fn stderr_text(&self) -> String {
        match self {
            CliError::Message(m) => format!("error: {m}\n"),
            CliError::Line(l) => format!("{l}\n"),
            CliError::Silent | CliError::Exit(_) => String::new(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.stderr_text().trim_end())
    }
}

/// The error for a session name that does not fit in a socket path.
pub fn name_too_long(name: &str, dir: &Path) -> CliError {
    CliError::Line(crate::socket::name_too_long_message(name, dir))
}

/// A full session name, or the error the CLI gives for a bad one: an
/// invalid or missing name is passed up and prints nothing.
pub fn session_name(env: &dyn crate::cfg::Env, raw: &str) -> Result<String, CliError> {
    crate::socket::session_name(env, raw).map_err(|_| CliError::Silent)
}

/// A name where `.` and absence mean `ZMX_SESSION`; `.` outside a session
/// prints its own explanation.
pub fn resolve_session(env: &dyn crate::cfg::Env, raw: Option<&str>) -> Result<String, CliError> {
    match crate::socket::resolve_session_or_env(env, raw) {
        Ok(name) => Ok(name),
        Err(_) if raw == Some(".") => Err(CliError::Line(
            "error: \".\" requires ZMX_SESSION (are you inside a zmx session?)".into(),
        )),
        Err(_) => Err(CliError::Silent),
    }
}
