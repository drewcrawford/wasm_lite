//! Headless test mode: run a wasm program in a browser and exit with a status
//! code, for use as a Cargo test runner
//! (`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER`).
//!
//! Two shapes are supported:
//!   * a `wasm_lite::tests!` harness (a `__wasm_lite_tests` section): each test
//!     runs in a fresh page load (`?test=i`) so a panic only fails that test;
//!   * a plain `bin`: run `main` once (pass = ran to completion).

use crate::webdriver::Browser;
use crate::{PROGRAM_JS, PROGRAM_WASM, Route, bind, read, serve};
use std::path::Path;
use std::time::{Duration, Instant};

/// Should this program be run in test mode? True for a `tests!` harness wasm.
pub fn is_test(program: &Path) -> bool {
    program.extension().and_then(|e| e.to_str()) == Some("wasm")
        && read(program)
            .map(|m| !wasm_lite_codegen::test_names(&m).is_empty())
            .unwrap_or(false)
}

/// Run a wasm program headless in a browser and return a process exit code.
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
    let port = listener.local_addr().expect("listener has an address").port();
    let names = module.test_names.clone();
    std::thread::spawn(move || serve(listener, &module.routes));

    let browser = match Browser::launch() {
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
    let glue = wasm_lite_codegen::generate_glue(&descriptors);
    let test_names = wasm_lite_codegen::test_names(&module);
    let bootstrap = if test_names.is_empty() {
        MAIN_BOOTSTRAP
    } else {
        HARNESS_BOOTSTRAP
    };
    let program_js = format!("{glue}{bootstrap}");

    Ok(Prepared {
        routes: vec![
            Route {
                path: "/",
                content_type: "text/html; charset=utf-8",
                body: TEST_HTML.as_bytes().to_vec(),
            },
            Route {
                path: PROGRAM_JS,
                content_type: "text/javascript; charset=utf-8",
                body: program_js.into_bytes(),
            },
            Route {
                path: PROGRAM_WASM,
                content_type: "application/wasm",
                body: module,
            },
        ],
        test_names,
    })
}

/// Run a plain `bin`: success is `main` completing without a trap.
fn run_main(browser: &Browser, port: u16) -> Result<i32, String> {
    browser.goto(&format!("http://127.0.0.1:{port}/"))?;
    wait_done(browser)?;

    let output = browser.eval_string(CONSOLE_JOIN)?;
    if !output.is_empty() {
        println!("{output}");
    }
    if browser.eval_bool("return globalThis.__wl_done.ok === true;")? {
        println!("test result: ok");
        Ok(0)
    } else {
        let error = browser.eval_string("return globalThis.__wl_done.error || \"\";")?;
        if !error.is_empty() {
            eprintln!("error: {error}");
        }
        println!("test result: FAILED");
        Ok(1)
    }
}

/// Run a `tests!` harness: each test in a fresh page load, libtest-style output.
fn run_suite(browser: &Browser, port: u16, names: &[String]) -> Result<i32, String> {
    println!("\nrunning {} test{}", names.len(), plural(names.len()));
    let mut failed = 0;

    for name in names {
        browser.goto(&format!("http://127.0.0.1:{port}/?test={name}"))?;
        wait_done(browser)?;

        if browser.eval_bool("return globalThis.__wl_done.ok === true;")? {
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
    <script type=\"module\" src=\"/program.js\"></script>\n\
    </body></html>\n";

/// Bootstrap for a plain `bin`: run `main`, recording success or the error.
const MAIN_BOOTSTRAP: &str = r#"
try {
    const instance = await instantiate("/program.wasm");
    instance.exports.main();
    globalThis.__wl_done = { ok: true, error: "" };
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
    globalThis.__wl_done = { ok: true, error: "" };
} catch (e) {
    globalThis.__wl_done = { ok: false, error: String((e && e.stack) || e) };
}
"#;
