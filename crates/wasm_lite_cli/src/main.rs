//! `wasm-lite`: generate the JavaScript glue for a compiled wasm_lite module.
//!
//! Usage:
//!     wasm-lite <input.wasm>            # write glue to stdout
//!     wasm-lite <input.wasm> -o out.js  # write glue to a file

use std::path::PathBuf;
use std::process::exit;

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("{msg}");
            eprintln!("usage: wasm-lite <input.wasm> [-o <output.js>]");
            exit(2);
        }
    };

    if let Err(err) = run(args) {
        eprintln!("error: {err}");
        exit(1);
    }
}

struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut input = None;
    let mut output = None;
    let mut args = std::env::args_os().skip(1);

    while let Some(arg) = args.next() {
        if arg == "-o" || arg == "--output" {
            let path = args.next().ok_or("missing path after -o")?;
            output = Some(PathBuf::from(path));
        } else if input.is_none() {
            input = Some(PathBuf::from(arg));
        } else {
            return Err("too many arguments".to_string());
        }
    }

    Ok(Args {
        input: input.ok_or("missing input wasm path")?,
        output,
    })
}

fn run(args: Args) -> Result<(), String> {
    let wasm = std::fs::read(&args.input)
        .map_err(|e| format!("failed to read {}: {e}", args.input.display()))?;

    let descriptors = wasm_lite_codegen::descriptors_from_wasm(&wasm)?;
    let exports = wasm_lite_codegen::exports_from_wasm(&wasm)?;
    let memory = wasm_lite_codegen::imported_memory(&wasm)?;
    let glue = wasm_lite_codegen::generate_glue(&descriptors, &exports, memory.as_ref());

    match args.output {
        Some(path) => {
            std::fs::write(&path, glue)
                .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
            // A shared-memory build can spawn threads: emit the worker bootstrap
            // module as `wl_worker.js` beside the glue (the glue's __wl_spawn
            // loads "./wl_worker.js", which imports the glue back).
            if memory.is_some() {
                let glue_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or("output path has no file name")?;
                let worker = wasm_lite_codegen::generate_worker(&format!("./{glue_name}"));
                let worker_path = path.with_file_name("wl_worker.js");
                std::fs::write(&worker_path, worker)
                    .map_err(|e| format!("failed to write {}: {e}", worker_path.display()))?;
                eprintln!("wrote {} (thread worker bootstrap)", worker_path.display());
            }
        }
        None => print!("{glue}"),
    }
    Ok(())
}
