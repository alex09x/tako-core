/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

use tako_session::cfg::ProcessEnv;

fn main() {
    // A non-UTF-8 argument (a file name, say) must not abort the process.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let status = tako_session::cli::run(&args, &ProcessEnv, &mut out, &mut err);
    std::process::exit(status);
}
