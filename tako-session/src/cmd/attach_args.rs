/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The arguments of `attach`. Flags count only before the session name, so
//! everything after it belongs to the session's command.

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AttachArgs {
    /// "" when no session was named.
    pub session_name: String,
    /// Index of the command's first word.
    pub command_start: usize,
    /// `--labels "k=v ..."`: labels to apply once the session exists.
    pub labels: Option<String>,
    pub attach_only: bool,
    pub create_only: bool,
    pub if_label: Option<String>,
    pub want_help: bool,
    pub missing_labels_value: bool,
    pub missing_if_label_value: bool,
}

pub fn parse(argv: &[String]) -> AttachArgs {
    let mut p = AttachArgs::default();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        match arg {
            "--help" | "-h" => {
                p.want_help = true;
                return p;
            }
            "--attach-only" => p.attach_only = true,
            "--create-only" => p.create_only = true,
            "--if-label" => {
                p.attach_only = true;
                let Some(value) = argv.get(i + 1) else {
                    p.missing_if_label_value = true;
                    p.command_start = argv.len();
                    return p;
                };
                p.if_label = Some(value.clone());
                i += 1;
            }
            "--labels" => {
                let Some(value) = argv.get(i + 1) else {
                    p.missing_labels_value = true;
                    p.command_start = argv.len();
                    return p;
                };
                p.labels = Some(value.clone());
                i += 1;
            }
            _ => {
                if let Some(v) = arg.strip_prefix("--if-label=") {
                    p.if_label = Some(v.to_string());
                    p.attach_only = true;
                } else if let Some(v) = arg.strip_prefix("--labels=") {
                    p.labels = Some(v.to_string());
                } else {
                    p.session_name = arg.to_string();
                    i += 1;
                    break;
                }
            }
        }
        i += 1;
    }
    p.command_start = i;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn flags_before_the_name_and_the_command_after_it() {
        let a = args(&["--labels", "a=1", "--if-label=b=2", "s", "--labels", "x"]);
        let p = parse(&a);
        assert_eq!(p.session_name, "s");
        assert_eq!(p.labels.as_deref(), Some("a=1"));
        assert_eq!(p.if_label.as_deref(), Some("b=2"));
        assert!(p.attach_only && !p.create_only);
        assert_eq!(&a[p.command_start..], &args(&["--labels", "x"])[..]);
    }

    #[test]
    fn missing_values_and_help_stop_parsing() {
        assert!(parse(&args(&["--labels"])).missing_labels_value);
        let p = parse(&args(&["--create-only", "--if-label"]));
        assert!(p.missing_if_label_value && p.attach_only && p.create_only);
        assert!(parse(&args(&["--create-only", "-h", "s"])).want_help);
        let p = parse(&args(&[]));
        assert_eq!((p.session_name.as_str(), p.command_start), ("", 0));
    }
}
