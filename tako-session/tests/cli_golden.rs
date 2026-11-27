/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Exit status, standard output and standard error of command lines that
//! need no live session, as the reference runtime gives them (recorded with
//! an empty ZMX_DIR; `{dir}` stands for it).

use std::ffi::OsString;
use std::path::PathBuf;
use tako_session::cli;

const CASES: &[(&[&str], i32, &str, &str)] = &[
    (
        &["kill", "nosuch"],
        1,
        "",
        "error: failed to kill session=nosuch: SessionNotFound\n",
    ),
    (
        &["get", "nosuch"],
        1,
        "",
        "error: session \"nosuch\" not found or unresponsive\n",
    ),
    (
        &["get", "x", "k"],
        1,
        "",
        "error: session \"x\" not found or unresponsive\n",
    ),
    (
        &["set", "nosuch", "a=b"],
        1,
        "",
        "error: session \"nosuch\" not found or unresponsive\n",
    ),
    (
        &["clear", "nosuch"],
        1,
        "",
        "error: session \"nosuch\" not found or unresponsive\n",
    ),
    (
        &["kill", "--if-label", "tako-gen=g", "nosuch"],
        1,
        "",
        "error: failed to kill session=nosuch: SessionNotFound\n",
    ),
    (
        &["kill", "--if-label", "tako-gen=", "nosuch"],
        1,
        "",
        "error: --if-label requires non-empty values\n",
    ),
    (
        &["kill", "--force", "--if-label", "a=b", "x"],
        1,
        "",
        "error: --force cannot be combined with --if-label\n",
    ),
    (
        &["kill", "--if-label", "a=b", "x*"],
        1,
        "",
        "error: --if-label requires a single exact session name\n",
    ),
    (
        &["kill", "--if-label"],
        1,
        "",
        "error: --if-label requires \"key=value ...\"\n",
    ),
    (&["kill"], 1, "", "error: session name required\n"),
    (&["kill", "a/b"], 1, "", ""),
    (&["get", "a/b"], 1, "", ""),
    (
        &["get"],
        1,
        "",
        "error: session name required (or run inside a zmx session)\n",
    ),
    (
        &["get", "."],
        1,
        "",
        "error: \".\" requires ZMX_SESSION (are you inside a zmx session?)\n",
    ),
    (&["list"], 0, "", "no sessions found in {dir}\n"),
    (&[], 0, "", "no sessions found in {dir}\n"),
    (&["list", "--short"], 0, "", ""),
    (
        &["set", "x", "bad/key=1"],
        1,
        "",
        "error: key-value kvs can only contain [a-z, A-Z, 0-9, -_.] characters: key=[bad/key]\n",
    ),
    (
        &["set", "x", "name=1"],
        1,
        "",
        "error: \"name\" is a read-only built-in field\n",
    ),
    (
        &["detach"],
        1,
        "",
        "error: not inside a zmx session (ZMX_SESSION not set)\n",
    ),
    (
        &["frobnicate"],
        1,
        "",
        "error: unknown command \"frobnicate\"\n",
    ),
    (&["run"], 1, "", ""),
    (&["send"], 1, "", "error: session name required\n"),
    (
        &["send", "x", "hi"],
        1,
        "",
        "session x is unresponsive (Unexpected)\ndaemon may be busy: try again\n",
    ),
    (
        &["print", "x", "hi"],
        1,
        "",
        "session x is unresponsive (Unexpected)\ndaemon may be busy: try again\n",
    ),
    (&["write"], 1, "", "error: session name required\n"),
    (&["write", "x"], 1, "", "error: file path required\n"),
    (&["wait"], 1, "", "error: session name required\n"),
    (&["tail"], 1, "", "error: session name required\n"),
    (
        &["tail", "nosuch"],
        1,
        "",
        "error: cannot connect to session \"nosuch\": FileNotFound\n",
    ),
    (
        &["tail", "no*"],
        1,
        "",
        "error: no matching sessions found\n",
    ),
    (
        &["history"],
        1,
        "",
        "error: session name required (or run inside a zmx session)\n",
    ),
    (
        &["hi", "nosuch", "--vt"],
        1,
        "",
        "error: session \"nosuch\" does not exist\n",
    ),
    (
        &["print-env"],
        1,
        "",
        "error: session name required (or run inside a zmx session)\n",
    ),
    (
        &["print-env", "-s", "nosuch", "K"],
        1,
        "",
        "error: could not communicate with session \"nosuch\" err=Unexpected\n",
    ),
    (
        &["completions"],
        1,
        "",
        "error: completions requires a shell argument (bash, zsh, fish, nu)\n",
    ),
    (
        &["completions", "foo"],
        1,
        "",
        "error: unknown shell \"foo\" (valid: bash, zsh, fish, nu)\n",
    ),
    (
        &["attach", "--labels"],
        1,
        "",
        "error: --labels requires \"key=value ...\"\n",
    ),
    (
        &["attach", "--if-label"],
        1,
        "",
        "error: --if-label requires \"key=value ...\"\n",
    ),
    (
        &["attach", "--attach-only", "x"],
        1,
        "",
        "error: --attach-only requires --if-label \"key=value ...\"\n",
    ),
    (
        &["attach", "--create-only", "--if-label", "a=1", "x"],
        1,
        "",
        "error: --create-only cannot be combined with --attach-only or --if-label\n",
    ),
    (
        &["attach", "--if-label", "a=", "x"],
        1,
        "",
        "error: --if-label requires non-empty values\n",
    ),
    (
        &["attach", "--create-only"],
        1,
        "",
        "error: session name required\n",
    ),
    (
        &["attach", "--if-label", "a=1", "x", "cmd"],
        1,
        "",
        "error: --attach-only does not accept a command\n",
    ),
    (
        &["attach", "--create-only", "--labels", "a=", "x"],
        1,
        "",
        "error: --if-label requires non-empty values\n",
    ),
    (
        &["attach", "--if-label", "a=1", "nosuch"],
        1,
        "",
        "error: session \"nosuch\" not found\n",
    ),
];

fn zmx_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ts-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn run(dir: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let zdir = OsString::from(dir);
    let env = move |key: &str| (key == "ZMX_DIR").then(|| zdir.clone());
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = cli::run(&args, &env, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn command_lines_without_a_session_match_the_reference() {
    let dir = zmx_dir("golden");
    for (args, status, out, err) in CASES {
        let expected_err = err.replace("{dir}", &dir.display().to_string());
        assert_eq!(
            run(&dir, args),
            (*status, out.to_string(), expected_err),
            "{args:?}"
        );
    }
    assert!(
        dir.join("logs/zmx.log").exists(),
        "the client logs to zmx.log in the log directory"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn help_is_the_reference_text_and_version_keeps_its_layout() {
    let dir = zmx_dir("help");
    for args in [
        &["help"][..],
        &["h"],
        &["-h"],
        &["kill", "--help"],
        &["list", "-h"],
    ] {
        let (status, out, err) = run(&dir, args);
        assert_eq!((status, err.as_str()), (0, ""), "{args:?}");
        assert_eq!(out, cli::HELP, "{args:?}");
    }
    assert!(cli::HELP.starts_with("zmx - session persistence for terminal processes\n"));
    let (status, out, _) = run(&dir, &["version"]);
    assert_eq!(status, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "zmx\t\t0.8.1");
    assert_eq!(lines[2], format!("socket_dir\t{}", dir.display()));
    assert_eq!(lines[3], format!("log_dir\t\t{}/logs", dir.display()));
    std::fs::remove_dir_all(&dir).unwrap();
}
