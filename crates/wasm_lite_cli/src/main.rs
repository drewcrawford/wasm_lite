// SPDX-License-Identifier: MIT OR Apache-2.0
//! `wasm-lite`: the host-side half of wasm_lite.
//!
//! Two jobs, one binary:
//!
//!     wasm-lite build <input.wasm> [-o out.js]   # generate the JavaScript glue
//!     wasm-lite run <program>                    # serve it and drive a browser
//!
//! `run` is meant to be reached through Cargo rather than typed. Point the
//! target's runner at it and `cargo run` / `cargo test` go through a real
//! browser:
//!
//! ```text
//! # .cargo/config.toml
//! [target.wasm32-unknown-unknown]
//! runner = ["wasm-lite", "run"]
//! ```
//!
//! or `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="wasm-lite run"`. Cargo
//! appends the artifact path, and any harness arguments, after the subcommand.

use std::ffi::OsString;

mod build;
mod run;

const USAGE: &str = "usage: wasm-lite <command> [args]\n\
                     \n\
                     commands:\n\
                     \x20 build <input.wasm> [-o <output.js>]  generate the JavaScript glue\n\
                     \x20 run <program.js|program.wasm>        serve it and drive a browser\n\
                     \n\
                     `wasm-lite <command> --help` describes each one.\n\
                     \n\
                     For `cargo run` and `cargo test` on wasm32, set:\n\
                     \x20 CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=\"wasm-lite run\"\n";

fn main() {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        println!("{USAGE}");
        return;
    }

    let command = args.remove(0);
    match command.to_string_lossy().as_ref() {
        "build" => build::main(args),
        "run" => run::main(args),
        "-h" | "--help" | "help" => println!("{USAGE}"),
        other => {
            eprintln!("error: unknown command `{other}`");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
