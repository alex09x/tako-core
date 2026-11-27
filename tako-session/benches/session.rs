/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! What a session costs: attach latency, output throughput to 1 and 4
//! clients, keystroke round trip, the frame codec, and idle daemons. Each
//! case runs against this build and, when `TAKO_TEST_NEW_ZMX` names the old
//! runtime, against that one too, side by side.
//!
//! `mitts bench -p tako-session`. `TAKO_SESSION_BENCH_QUICK=1` runs fewer
//! rounds, `TAKO_SESSION_BENCH_TRIALS=n` sets the interleaved trials (best
//! one counts), `TAKO_SESSION_BENCH_GATE=1` fails on generous bounds (see
//! GATES). `TAKO_SESSION_BIN` points a bench binary copied to another machine
//! at its runtime binary.

mod support;

use std::time::{Duration, Instant};
use support::{Peer, Runtime, Sessions, process_cost, runtimes};
use tako_session::ipc::{FrameBuffer, Tag, append_frame};

/// A measured value; `higher` says which direction is better.
struct Metric {
    name: &'static str,
    unit: &'static str,
    higher: bool,
    values: Vec<Option<f64>>,
}

fn quick() -> bool {
    std::env::var_os("TAKO_SESSION_BENCH_QUICK").is_some()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn percentile(mut v: Vec<f64>, p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p) as usize]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Connect + Init until the daemon answers (its request for our size).
fn attach_empty(rt: &Runtime) -> f64 {
    let s = Sessions::new(rt);
    s.start("a", &["sleep", "100000"]);
    let rounds = if quick() { 30 } else { 200 };
    let times = (0..rounds)
        .map(|_| {
            let start = Instant::now();
            let mut p = s.connect("a");
            p.init(24, 80);
            p.expect(Tag::RESIZE);
            ms(start.elapsed())
        })
        .collect();
    median(times)
}

/// Connect + Init until the restored screen of 10k lines of history arrives.
fn attach_scrollback(rt: &Runtime) -> f64 {
    let s = Sessions::new(rt);
    s.start("h", &["sleep", "100000"]);
    {
        let mut first = s.connect("h");
        first.init(24, 80);
        let lines: Vec<u8> = (0..10_000)
            .flat_map(|i| {
                format!(
                    "\x1b[3{}mline {i:05}\x1b[0m the quick brown fox jumps over the lazy dog\r\n",
                    i % 8
                )
                .into_bytes()
            })
            .collect();
        first.send(Tag::OUTPUT, &lines);
        // Requests on one connection are handled in order: the history is in.
        first.send(Tag::INFO, b"");
        first.expect(Tag::INFO);
    }
    let rounds = if quick() { 5 } else { 30 };
    let times = (0..rounds)
        .map(|_| {
            let start = Instant::now();
            let mut p = s.connect("h");
            p.init(24, 80);
            p.expect(Tag::OUTPUT);
            ms(start.elapsed())
        })
        .collect();
    median(times)
}

/// MB/s from the program's PTY to `clients` attached clients, timed on the
/// slowest.
fn output(rt: &Runtime, clients: usize) -> f64 {
    let total: usize = if quick() { 16 << 20 } else { 64 << 20 };
    let s = Sessions::new(rt);
    let line = "0123456789abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRS";
    let script = format!("read go; yes {line} | head -c {total}; exec sleep 100000");
    s.start("o", &["sh", "-c", &script]);
    let mut peers: Vec<Peer> = (0..clients)
        .map(|_| {
            let mut p = s.connect("o");
            p.init(24, 80);
            p
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    let start = Instant::now();
    peers[0].send(Tag::INPUT, b"\r");
    let readers: Vec<_> = peers
        .drain(..)
        .map(|mut p| std::thread::spawn(move || p.read_output(total)))
        .collect();
    for r in readers {
        r.join().unwrap();
    }
    total as f64 / 1e6 / start.elapsed().as_secs_f64()
}

/// Keystroke to echo through the daemon (`cat` on a raw PTY): median, p99 µs.
fn input_round_trip(rt: &Runtime) -> (f64, f64) {
    let s = Sessions::new(rt);
    s.start("i", &["sh", "-c", "stty raw -echo; exec cat"]);
    let mut p = s.connect("i");
    p.init(24, 80);
    std::thread::sleep(Duration::from_millis(300));
    let rounds = if quick() { 300 } else { 2000 };
    let mut times = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let start = Instant::now();
        p.send(Tag::INPUT, b"x");
        while !p.expect(Tag::OUTPUT).contains(&b'x') {}
        times.push(start.elapsed().as_secs_f64() * 1e6);
    }
    (median(times.clone()), percentile(times, 0.99))
}

/// Encode and decode MB/s for 4 KiB `Output` frames.
fn codec() -> (f64, f64) {
    let payload = vec![b'x'; 4096];
    let frames = if quick() { 4096 } else { 32768 };
    let mut wire = Vec::with_capacity(frames * 4104);
    let start = Instant::now();
    for _ in 0..frames {
        append_frame(&mut wire, Tag::OUTPUT, &payload);
    }
    let encode = wire.len() as f64 / 1e6 / start.elapsed().as_secs_f64();
    let start = Instant::now();
    let mut buf = FrameBuffer::new();
    let mut seen = 0usize;
    for chunk in wire.chunks(65536) {
        buf.push(chunk);
        while let Some(f) = buf.next_frame() {
            seen += f.payload.len();
        }
    }
    assert_eq!(seen, frames * 4096);
    (
        encode,
        wire.len() as f64 / 1e6 / start.elapsed().as_secs_f64(),
    )
}

/// CPU ms per daemon over 3 s and RSS MiB per daemon, with `n` idle sessions.
fn idle(rt: &Runtime, n: usize) -> (f64, f64) {
    let s = Sessions::new(rt);
    for i in 0..n {
        s.start(&format!("s{i}"), &["sleep", "100000"]);
    }
    std::thread::sleep(Duration::from_secs(1));
    let pids: Vec<i32> = (0..n).map(|i| s.daemon_pid(&format!("s{i}"))).collect();
    let before: Vec<f64> = pids.iter().map(|&p| process_cost(p).0).collect();
    std::thread::sleep(Duration::from_secs(3));
    let after: Vec<(f64, u64)> = pids.iter().map(|&p| process_cost(p)).collect();
    let cpu: f64 = after
        .iter()
        .zip(&before)
        .map(|((a, _), b)| a - b)
        .sum::<f64>()
        / n as f64;
    let rss = after.iter().map(|(_, r)| *r as f64).sum::<f64>() / n as f64 / 1024.0;
    (cpu, rss)
}

/// Generous bounds that hold on any machine we test on; a broken build or a
/// busy loop misses them by far.
const GATES: &[(&str, f64, f64)] = &[
    // (metric, worst acceptable value, worst acceptable ratio to the old runtime)
    ("attach, empty session", 50.0, 0.25),
    ("attach, 10k lines of history", 1000.0, 0.25),
    ("output, 1 client", 2.0, 0.25),
    ("output, 4 clients", 1.0, 0.25),
    ("input round trip, median", 20000.0, 0.25),
    ("codec encode", 200.0, 0.0),
    ("codec decode", 200.0, 0.0),
    ("idle CPU per daemon (3 s)", 30.0, 0.0),
    ("RSS per daemon", 64.0, 0.0),
];

/// Trials per case. They alternate between the runtimes, so load on the
/// machine falls on both alike, and the best trial counts: noise only ever
/// makes a run slower.
fn trials() -> usize {
    std::env::var("TAKO_SESSION_BENCH_TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if quick() { 1 } else { 3 })
}

/// `f` for each runtime over interleaved trials: each runtime's best.
fn best<T: Copy>(
    rts: &[Runtime],
    f: &dyn Fn(&Runtime) -> T,
    better: &dyn Fn(T, T) -> bool,
) -> Vec<T> {
    let mut out: Vec<Option<T>> = vec![None; rts.len()];
    for _ in 0..trials() {
        for (i, rt) in rts.iter().enumerate() {
            let v = f(rt);
            if out[i].is_none_or(|b| better(v, b)) {
                out[i] = Some(v);
            }
        }
    }
    out.into_iter().map(Option::unwrap).collect()
}

/// `TAKO_SESSION_BENCH_PROFILE=<case>`: run one case over and over for
/// `TAKO_SESSION_BENCH_SECONDS` (default 20) against one runtime
/// (`TAKO_SESSION_BENCH_RUNTIME`, default rust), for a profiler to record.
fn profile(rts: &[Runtime], case: &str) {
    let name = std::env::var("TAKO_SESSION_BENCH_RUNTIME").unwrap_or_else(|_| "rust".into());
    let rt = rts
        .iter()
        .find(|r| r.name == name)
        .expect("no such runtime");
    let secs = std::env::var("TAKO_SESSION_BENCH_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let s = Sessions::new(rt);
    // One daemon for the whole run, so a profiler can follow its pid.
    let (session, program): (&str, Vec<&str>) = match case {
        "attach" | "idle" => ("p", vec!["sleep", "100000"]),
        "output1" | "output4" => ("p", vec!["sh", "-c", OUTPUT_LOOP]),
        "input" => ("p", vec!["sh", "-c", "stty raw -echo; exec cat"]),
        other => panic!("unknown case {other}"),
    };
    s.start(session, &program);
    if case == "attach" {
        fill_history(&s, session);
    }
    let clients = if case == "output4" { 4 } else { 1 };
    let mut peers: Vec<Peer> = (0..clients).map(|_| s.connect(session)).collect();
    for p in &mut peers {
        p.init(24, 80);
    }
    std::thread::sleep(Duration::from_millis(300));
    eprintln!(
        "daemon pid {} ({case} on {name}, {secs} s)",
        s.daemon_pid(session)
    );
    // Gives the profiler time to attach before the work starts.
    std::thread::sleep(Duration::from_secs(2));
    let until = Instant::now() + Duration::from_secs(secs);
    let mut rounds = 0u64;
    while Instant::now() < until {
        match case {
            "attach" => {
                let mut p = s.connect(session);
                p.init(24, 80);
                p.expect(Tag::OUTPUT);
            }
            "output1" | "output4" => {
                peers[0].send(Tag::INPUT, b"\r");
                let readers: Vec<_> = peers
                    .drain(..)
                    .map(|mut p| {
                        std::thread::spawn(move || {
                            p.read_output(OUTPUT_ROUND);
                            p
                        })
                    })
                    .collect();
                peers = readers.into_iter().map(|r| r.join().unwrap()).collect();
            }
            "input" => {
                peers[0].send(Tag::INPUT, b"x");
                while !peers[0].expect(Tag::OUTPUT).contains(&b'x') {}
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
        rounds += 1;
    }
    eprintln!("{rounds} rounds");
}

/// Bytes the output loop writes per `\r` it reads.
const OUTPUT_ROUND: usize = 8 << 20;
const OUTPUT_LOOP: &str = "while read go; do yes 0123456789abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRS | head -c 8388608; done";

/// 10k lines of coloured history, then a barrier on the same connection.
fn fill_history(s: &Sessions, session: &str) {
    let mut first = s.connect(session);
    first.init(24, 80);
    let lines: Vec<u8> = (0..10_000)
        .flat_map(|i| {
            format!(
                "\x1b[3{}mline {i:05}\x1b[0m the quick brown fox jumps over the lazy dog\r\n",
                i % 8
            )
            .into_bytes()
        })
        .collect();
    first.send(Tag::OUTPUT, &lines);
    first.send(Tag::INFO, b"");
    first.expect(Tag::INFO);
}

fn main() {
    let rts = runtimes();
    if let Ok(case) = std::env::var("TAKO_SESSION_BENCH_PROFILE") {
        return profile(&rts, &case);
    }
    let mut metrics: Vec<Metric> = Vec::new();
    let mut add = |name, unit, higher, values| {
        metrics.push(Metric {
            name,
            unit,
            higher,
            values,
        })
    };
    let low = |f: &dyn Fn(&Runtime) -> f64| {
        best(&rts, f, &|a, b| a < b)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>()
    };
    let high = |f: &dyn Fn(&Runtime) -> f64| {
        best(&rts, f, &|a, b| a > b)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>()
    };
    add("attach, empty session", "ms", false, low(&attach_empty));
    add(
        "attach, 10k lines of history",
        "ms",
        false,
        low(&attach_scrollback),
    );
    add("output, 1 client", "MB/s", true, high(&|rt| output(rt, 1)));
    add("output, 4 clients", "MB/s", true, high(&|rt| output(rt, 4)));
    let rtt = best(&rts, &input_round_trip, &|a: (f64, f64), b| a.0 < b.0);
    add(
        "input round trip, median",
        "µs",
        false,
        rtt.iter().map(|r| Some(r.0)).collect(),
    );
    add(
        "input round trip, p99",
        "µs",
        false,
        rtt.iter().map(|r| Some(r.1)).collect(),
    );
    let (enc, dec) = codec();
    let only_rust = |v: f64| {
        (0..rts.len())
            .map(|i| (i == 0).then_some(v))
            .collect::<Vec<_>>()
    };
    add("codec encode", "MB/s", true, only_rust(enc));
    add("codec decode", "MB/s", true, only_rust(dec));
    let idle_n = 20;
    let costs: Vec<(f64, f64)> = rts.iter().map(|rt| idle(rt, idle_n)).collect();
    add(
        "idle CPU per daemon (3 s)",
        "ms",
        false,
        costs.iter().map(|c| Some(c.0)).collect(),
    );
    add(
        "RSS per daemon",
        "MiB",
        false,
        costs.iter().map(|c| Some(c.1)).collect(),
    );
    report(&rts, &metrics);
    if std::env::var_os("TAKO_SESSION_BENCH_GATE").is_some() && !gate(&metrics) {
        std::process::exit(1);
    }
}

/// How many times better this build is than the old one (>1 is better).
fn speedup(m: &Metric) -> Option<f64> {
    match (
        m.values.first().copied().flatten(),
        m.values.get(1).copied().flatten(),
    ) {
        (Some(ours), Some(theirs)) if ours > 0.0 && theirs > 0.0 => Some(if m.higher {
            ours / theirs
        } else {
            theirs / ours
        }),
        _ => None,
    }
}

fn report(rts: &[Runtime], metrics: &[Metric]) {
    let names: Vec<&str> = rts.iter().map(|r| r.name).collect();
    println!("\n| case | unit | {} | rust vs zig |", names.join(" | "));
    println!("|---|---|{}---|", "---:|".repeat(names.len()));
    for m in metrics {
        let cells: Vec<String> = m
            .values
            .iter()
            .map(|v| v.map_or("—".into(), |x| format!("{x:.2}")))
            .collect();
        let ratio = speedup(m).map_or("—".into(), |r| format!("{r:.2}×"));
        println!(
            "| {} | {} | {} | {ratio} |",
            m.name,
            m.unit,
            cells.join(" | ")
        );
    }
    println!("\n(n×: this build is n times better; idle uses 20 sessions)");
}

fn gate(metrics: &[Metric]) -> bool {
    let mut ok = true;
    for (name, worst, min_ratio) in GATES {
        let Some(m) = metrics.iter().find(|m| m.name == *name) else {
            continue;
        };
        let Some(ours) = m.values[0] else { continue };
        let bad = if m.higher {
            ours < *worst
        } else {
            ours > *worst
        };
        if bad {
            eprintln!("gate: {name} = {ours:.2} {} (bound {worst})", m.unit);
            ok = false;
        }
        if let Some(r) = speedup(m).filter(|r| *r < *min_ratio) {
            eprintln!("gate: {name} is {r:.2}× the old runtime (bound {min_ratio}×)");
            ok = false;
        }
    }
    ok
}
