// SPDX-License-Identifier: MIT OR Apache-2.0
//! Headless test mode: run a wasm program in a browser and exit with a status
//! code, for use as a Cargo test runner
//! (`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER`).
//!
//! Two shapes are supported:
//!   * a `#[wasm_lite_test]` harness (a `__wasm_lite_tests` section): each test
//!     runs in a fresh page load (`?test=<path>`) so a panic only fails that test;
//!   * a plain `bin`: run `main` once (pass = ran to completion).

use crate::webdriver::Browser;
use crate::{BOOTSTRAP_JS, PROGRAM_JS, PROGRAM_WASM, Route, WL_WORKER_JS, bind, read, serve};
use std::path::Path;
use std::time::{Duration, Instant};

/// Run a wasm program headless in a browser and return a process exit code.
///
/// A `tests!`-harness wasm runs each test; a plain `bin` (including a rustdoc
/// doctest) runs `main` once (pass = ran to completion, trap = failure).
pub fn run(program: &Path) -> i32 {
    let module = match prepare(program) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("error: {err}");
            return 2;
        }
    };

    let listener = match bind() {
        Ok(l) => l,
        Err(err) => {
            eprintln!("error: could not bind a local port: {err}");
            return 2;
        }
    };
    let port = listener
        .local_addr()
        .expect("listener has an address")
        .port();
    let names = module.test_names.clone();
    std::thread::spawn(move || serve(listener, &module.routes));

    let browser = match Browser::open() {
        Ok(b) => b,
        Err(err) => {
            eprintln!("error: {err}");
            return 2;
        }
    };

    let result = if names.is_empty() {
        run_main(&browser, port)
    } else {
        run_suite(&browser, port, &names)
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            2
        }
    }
}

/// Routes to serve plus the discovered test names.
struct Prepared {
    routes: Vec<Route>,
    test_names: Vec<String>,
}

fn prepare(program: &Path) -> Result<Prepared, String> {
    if program.extension().and_then(|e| e.to_str()) != Some("wasm") {
        return Err("test mode supports .wasm programs".to_string());
    }
    let module = read(program)?;
    if wasm_lite_codegen::uses_wasm_bindgen(&module) {
        return Err("test mode does not yet support wasm-bindgen interop modules".to_string());
    }

    let descriptors = wasm_lite_codegen::descriptors_from_wasm(&module)?;
    let exports = wasm_lite_codegen::exports_from_wasm(&module)?;
    let memory = wasm_lite_codegen::imported_memory(&module)?;
    let glue = wasm_lite_codegen::generate_glue(&descriptors, &exports, memory.as_ref());
    let test_names = wasm_lite_codegen::test_names(&module);
    let body = if test_names.is_empty() {
        MAIN_BOOTSTRAP
    } else {
        HARNESS_BOOTSTRAP
    };
    // program.js is the glue ONLY (so a spawned worker can import it without
    // re-running the test); a separate bootstrap module drives the test.
    let bootstrap = format!("import {{ instantiate }} from \"./program.js\";\n{body}");

    let mut routes = vec![
        Route {
            path: "/",
            content_type: "text/html; charset=utf-8",
            body: TEST_HTML.as_bytes().to_vec(),
        },
        Route {
            path: PROGRAM_JS,
            content_type: "text/javascript; charset=utf-8",
            body: glue.into_bytes(),
        },
        Route {
            path: BOOTSTRAP_JS,
            content_type: "text/javascript; charset=utf-8",
            body: bootstrap.into_bytes(),
        },
        Route {
            path: PROGRAM_WASM,
            content_type: "application/wasm",
            body: module,
        },
    ];
    // Shared-memory builds spawn threads onto workers: serve the worker bootstrap.
    if memory.is_some() {
        routes.push(Route {
            path: WL_WORKER_JS,
            content_type: "text/javascript; charset=utf-8",
            body: wasm_lite_codegen::generate_worker("./program.js").into_bytes(),
        });
    }

    Ok(Prepared { routes, test_names })
}

/// Run a plain `bin`: success is `main` completing without a trap.
fn run_main(browser: &Browser, port: u16) -> Result<i32, String> {
    browser.goto(&format!("http://127.0.0.1:{port}/"))?;
    wait_done(browser)?;

    if browser.eval_bool("return globalThis.__wl_done.ok === true;")? {
        surface_worker_panics(browser)?;
        println!("test result: ok");
        return Ok(0);
    }

    // On failure, prefer the captured console (the panic message, if a panic
    // hook was installed); fall back to the raw trap for the no-hook case.
    let console = browser.eval_string(CONSOLE_JOIN)?;
    if !console.is_empty() {
        println!("{console}");
    } else {
        let error = browser.eval_string("return globalThis.__wl_done.error || \"\";")?;
        if !error.is_empty() {
            eprintln!("{error}");
        }
    }
    println!("test result: FAILED");
    Ok(1)
}

/// Surface worker-thread panics on an otherwise-passing test.
///
/// A panic only traps its own worker, so a detached worker's panic doesn't fail
/// the test (matching `std`, where an unjoined thread's panic prints but doesn't
/// fail) — but it must not be *silent* on the CLI. Worker console output is
/// bridged to the main realm (see the generated glue), so here we scan the
/// captured console for panic lines and print them as warnings. Best-effort: a
/// short grace lets a just-detached worker flush before we look.
fn surface_worker_panics(browser: &Browser) -> Result<(), String> {
    if !browser.eval_bool("return (globalThis.__wl_spawn_count || 0) > 0;")? {
        return Ok(()); // no workers spawned — nothing to wait for
    }
    // Wait until the directly-spawned workers have each reported "done" (they do
    // so even after a panic, via the bootstrap's `finally`), so their bridged
    // console output has landed. Bounded, so a genuinely stuck worker can't hang
    // a passing test.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline
        && !browser.eval_bool(
            "return (globalThis.__wl_worker_done || 0) >= (globalThis.__wl_spawn_count || 0);",
        )?
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    let panics = browser.eval_string(
        "return (globalThis.__wl_console || []).filter(e => e.includes('panicked')).join('\\n');",
    )?;
    if !panics.trim().is_empty() {
        eprintln!("warning: a worker thread panicked (test still passed — likely a bug):");
        for line in panics.lines() {
            eprintln!("    {line}");
        }
    }
    Ok(())
}

/// Run a `tests!` harness: each test in a fresh page load, libtest-style output.
fn run_suite(browser: &Browser, port: u16, names: &[String]) -> Result<i32, String> {
    println!("\nrunning {} test{}", names.len(), plural(names.len()));
    let mut failed = 0;

    for name in names {
        let encoded_name = encode_query_component(name);
        browser.goto(&format!("http://127.0.0.1:{port}/?test={encoded_name}"))?;
        wait_done(browser)?;

        if browser.eval_bool("return globalThis.__wl_done.ok === true;")? {
            surface_worker_panics(browser)?;
            println!("test {name} ... ok");
        } else {
            failed += 1;
            println!("test {name} ... FAILED");
            // The panic hook logged the message via console.error.
            let output = browser.eval_string(CONSOLE_JOIN)?;
            for line in output.lines() {
                println!("    {line}");
            }
        }
    }

    let passed = names.len() - failed;
    println!();
    if failed == 0 {
        println!("test result: ok. {passed} passed; 0 failed");
        Ok(0)
    } else {
        println!("test result: FAILED. {passed} passed; {failed} failed");
        Ok(1)
    }
}

/// Poll until the page records a result (or time out).
fn wait_done(browser: &Browser) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if browser.eval_bool("return !!globalThis.__wl_done;")? {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("timed out waiting for the program to finish".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn encode_query_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    encoded
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Script that returns all captured console output joined by newlines.
const CONSOLE_JOIN: &str = "return (globalThis.__wl_console || []).join(\"\\n\");";

/// HTML shell for test mode: captures console output into a global the runner
/// polls, then loads the program module.
const TEST_HTML: &str = "<!DOCTYPE html>\n\
    <html lang=\"en\"><head><meta charset=\"utf-8\"><title>wasm_lite test</title></head>\n\
    <body>\n\
    <script>\n\
    globalThis.__wl_console = [];\n\
    for (const level of [\"log\", \"error\", \"warn\", \"info\"]) {\n\
        const original = console[level].bind(console);\n\
        console[level] = (...args) => { original(...args); globalThis.__wl_console.push(args.join(\" \")); };\n\
    }\n\
    </script>\n\
    <script type=\"module\" src=\"/bootstrap.js\"></script>\n\
    </body></html>\n";

/// Bootstrap for a plain `bin`: run `main`, recording success or the error.
///
/// An async test marks `__wl_async_pending`, so we do NOT declare success at
/// main-return — the async body sets the verdict when it completes (fail-closed).
const MAIN_BOOTSTRAP: &str = r#"
try {
    const instance = await instantiate("/program.wasm");
    instance.exports.main();
    if (!globalThis.__wl_async_pending) globalThis.__wl_done = { ok: true, error: "" };
} catch (e) {
    globalThis.__wl_done = { ok: false, error: String((e && e.stack) || e) };
}
"#;

/// Bootstrap for a test harness: run the single test named by `?test=<name>`.
const HARNESS_BOOTSTRAP: &str = r#"
const name = new URLSearchParams(location.search).get("test");
try {
    const instance = await instantiate("/program.wasm");
    instance.exports["__wl_test_" + name]();
    if (!globalThis.__wl_async_pending) globalThis.__wl_done = { ok: true, error: "" };
} catch (e) {
    globalThis.__wl_done = { ok: false, error: String((e && e.stack) || e) };
}
"#;
