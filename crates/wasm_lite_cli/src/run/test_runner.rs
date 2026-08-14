// SPDX-License-Identifier: MIT OR Apache-2.0
//! Headless test mode: run a wasm program in a browser and exit with a status
//! code, for use as a Cargo test runner
//! (`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER`).
//!
//! Three shapes are supported:
//!   * a `#[wasm_lite_test]` harness (a `__wasm_lite_tests` section): each test
//!     runs in a fresh page load (`?test=<path>`) so a panic only fails that test;
//!   * a `#[wasm_lite_bench]` harness (a `__wasm_lite_benches` section): the same,
//!     one page load per benchmark, reporting ns/iter;
//!   * a plain `bin`: run `main` once (pass = ran to completion).

use super::webdriver::Browser;
use super::{Args, BOOTSTRAP_JS, PROGRAM_JS, PROGRAM_WASM, Route, WL_WORKER_JS, bind, read, serve};
use std::path::Path;
use std::time::{Duration, Instant};

/// Run a wasm program headless in a browser and return a process exit code.
///
/// A `tests!`-harness wasm runs each test matching the libtest-style filters;
/// a plain `bin` (including a rustdoc doctest) runs `main` once (pass = ran to
/// completion, trap = failure).
pub fn run(args: &Args) -> i32 {
    let module = match prepare(&args.program) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("error: {err}");
            return 2;
        }
    };

    let all = module.test_names.clone();
    let names: Vec<String> = all.iter().filter(|n| args.selects(n)).cloned().collect();
    let filtered_out = all.len() - names.len();

    let all_benches = module.bench_names.clone();
    let benches: Vec<String> = all_benches
        .iter()
        .filter(|n| args.selects(n))
        .cloned()
        .collect();
    let benches_filtered_out = all_benches.len() - benches.len();

    // `cargo test -- --list` / IDE test discovery: report names, don't run.
    if args.list {
        for name in &names {
            println!("{name}: test");
        }
        for name in &benches {
            println!("{name}: bench");
        }
        println!();
        println!("{} tests, {} benchmarks", names.len(), benches.len());
        return 0;
    }

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
    std::thread::spawn(move || serve(listener, &module.routes));

    let browser = match Browser::open() {
        Ok(b) => b,
        Err(err) => {
            eprintln!("error: {err}");
            return 2;
        }
    };

    // A bench target under `cargo test` (no `--bench`) still runs each
    // benchmark, but only to prove it doesn't panic — libtest does the same,
    // and it is the only thing that keeps benchmarks compiling and working in
    // CI where nobody reads the timings. `--bench` asks for the measurement.
    let result = if all.is_empty() && all_benches.is_empty() {
        run_main(&browser, port)
    } else if all.is_empty() {
        run_bench_suite(&browser, port, &benches, benches_filtered_out, args.bench)
    } else {
        run_suite(&browser, port, &names, filtered_out)
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            2
        }
    }
}

/// Routes to serve plus the discovered test and benchmark names.
struct Prepared {
    routes: Vec<Route>,
    test_names: Vec<String>,
    bench_names: Vec<String>,
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
    let test_names = wasm_lite_codegen::test_names(&module)?;
    let bench_names = wasm_lite_codegen::bench_names(&module)?;
    // One harness bootstrap covers both shapes, dispatching on the query
    // parameter rather than on which section exists — a target may declare
    // tests and benchmarks in the same file, and picking by section would make
    // one of them unreachable.
    let body = if test_names.is_empty() && bench_names.is_empty() {
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
            body: test_html().into_bytes(),
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

    Ok(Prepared {
        routes,
        test_names,
        bench_names,
    })
}

/// Run a plain `bin`: success is `main` completing without a trap.
fn run_main(browser: &Browser, port: u16) -> Result<i32, String> {
    browser.goto(&format!("http://127.0.0.1:{port}/"))?;

    // An application whose work outlives `main` — a render loop, an executor,
    // anything driven from the event loop — otherwise "passes" the instant
    // `main` returns, and its console is discarded on success. That is right
    // for a doctest and useless for watching a program run, so
    // `WASM_LITE_RUN_SECONDS` keeps the page alive and always prints what it
    // logged.
    if let Some(secs) = run_seconds() {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            // Stop early if the program itself reported a failure; there is no
            // point watching a dead instance for another minute.
            if browser
                .eval_bool("return !!globalThis.__wl_done && globalThis.__wl_done.ok === false;")?
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let console = browser.eval_string(CONSOLE_JOIN)?;
        if !console.is_empty() {
            println!("{console}");
        }
        let failed = browser
            .eval_bool("return !!globalThis.__wl_done && globalThis.__wl_done.ok === false;")?;
        if failed {
            // Say *what* failed. Without this the whole verdict is the word
            // "FAILED" under a log that may be tens of thousands of lines of a
            // program working perfectly well — the failure is usually `main`
            // itself throwing, which appears nowhere in the console.
            let error = browser.eval_string("return globalThis.__wl_done.error || \"\";")?;
            if error.is_empty() {
                eprintln!("error: the program reported failure but recorded no message");
            } else {
                eprintln!("error: {error}");
            }
        }
        println!("test result: {}", if failed { "FAILED" } else { "ok" });
        return Ok(if failed { 1 } else { 0 });
    }

    // A hang is a failure of this program, not of the runner, so report what it
    // logged before it stopped. Without this a timeout says only "timed out",
    // which is the least informative thing we know about it.
    if let Err(err) = wait_done(browser) {
        eprintln!("error: {err}");
        let console = browser.eval_string(CONSOLE_JOIN)?;
        if console.is_empty() {
            eprintln!("(the program logged nothing before it stopped)");
        } else {
            eprintln!("--- console output before the timeout ---");
            eprintln!("{console}");
        }
        println!("test result: FAILED");
        return Ok(1);
    }

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
fn run_suite(
    browser: &Browser,
    port: u16,
    names: &[String],
    filtered_out: usize,
) -> Result<i32, String> {
    println!("\nrunning {} test{}", names.len(), plural(names.len()));
    let mut failed = 0;

    for name in names {
        let encoded_name = encode_query_component(name);
        browser.goto(&format!("http://127.0.0.1:{port}/?test={encoded_name}"))?;
        // A hung test is that test's failure, not the suite's: report it and
        // move on so the remaining tests still run and get a summary. (Errors
        // from goto/eval still abort — those mean the browser session is gone.)
        if let Err(err) = wait_done(browser) {
            failed += 1;
            println!("test {name} ... FAILED");
            println!("    {err}");
            // Same reasoning as `run_main`: what it logged before hanging is
            // usually the only clue to where it hung.
            for line in browser.eval_string(CONSOLE_JOIN)?.lines() {
                println!("    {line}");
            }
            continue;
        }

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
            if output.is_empty() {
                let error = browser.eval_string("return globalThis.__wl_done.error || \"\";")?;
                if !error.is_empty() {
                    println!("    {error}");
                }
            }
        }
    }

    let passed = names.len() - failed;
    println!();
    if failed == 0 {
        println!("test result: ok. {passed} passed; 0 failed; {filtered_out} filtered out");
        Ok(0)
    } else {
        println!(
            "test result: FAILED. {passed} passed; {failed} failed; {filtered_out} filtered out"
        );
        Ok(1)
    }
}

/// Run a `#[wasm_lite_bench]` harness: each benchmark in a fresh page load.
///
/// With `measure`, prints libtest's `cargo bench` line; without it, each
/// benchmark still runs (so a broken one fails CI) but only its pass/fail is
/// reported — a timing taken while the rest of the suite is compiling or a CI
/// box is oversubscribed is worse than no timing, because it looks like data.
fn run_bench_suite(
    browser: &Browser,
    port: u16,
    names: &[String],
    filtered_out: usize,
    measure: bool,
) -> Result<i32, String> {
    println!(
        "\nrunning {} benchmark{}{}",
        names.len(),
        plural(names.len()),
        if measure { "" } else { " (not measured)" }
    );
    let mut failed = 0;

    for name in names {
        let encoded_name = encode_query_component(name);
        browser.goto(&format!("http://127.0.0.1:{port}/?bench={encoded_name}"))?;
        if let Err(err) = wait_done(browser) {
            failed += 1;
            println!("test {name} ... FAILED");
            println!("    {err}");
            for line in browser.eval_string(CONSOLE_JOIN)?.lines() {
                println!("    {line}");
            }
            continue;
        }

        if !browser.eval_bool("return globalThis.__wl_done.ok === true;")? {
            failed += 1;
            println!("test {name} ... FAILED");
            for line in browser.eval_string(CONSOLE_JOIN)?.lines() {
                println!("    {line}");
            }
            continue;
        }

        if !measure {
            println!("test {name} ... ok");
            continue;
        }

        let raw = browser.eval_string("return globalThis.__wl_bench || \"\";")?;
        match Measurement::parse(&raw) {
            Some(m) if m.iters > 0.0 => {
                println!(
                    "test {name} ... bench: {:>11} ns/iter (+/- {})",
                    thousands(m.median_ns),
                    thousands(m.max_ns - m.min_ns)
                );
            }
            // Ran cleanly but measured nothing: the body never called
            // `Bencher::iter`. Reporting `0 ns/iter` would read as an
            // astonishing result rather than as the mistake it is.
            _ => {
                failed += 1;
                println!("test {name} ... FAILED");
                println!("    the benchmark never called `Bencher::iter`, so nothing was measured");
            }
        }
    }

    let passed = names.len() - failed;
    println!();
    if failed == 0 {
        println!("test result: ok. {passed} passed; 0 failed; {filtered_out} filtered out");
        Ok(0)
    } else {
        println!(
            "test result: FAILED. {passed} passed; {failed} failed; {filtered_out} filtered out"
        );
        Ok(1)
    }
}

/// One benchmark's timings, as read back from the module's exports.
struct Measurement {
    median_ns: f64,
    min_ns: f64,
    max_ns: f64,
    iters: f64,
}

impl Measurement {
    /// Parse the `median,min,max,iters` string the bootstrap builds.
    ///
    /// `None` for anything malformed, which the caller treats as "not
    /// measured" — a partially-parsed timing is not worth printing.
    fn parse(raw: &str) -> Option<Measurement> {
        let mut fields = raw.split(',').map(|f| f.trim().parse::<f64>().ok());
        let m = Measurement {
            median_ns: fields.next()??,
            min_ns: fields.next()??,
            max_ns: fields.next()??,
            iters: fields.next()??,
        };
        // A trailing field means the shape changed and this parse is a guess.
        if fields.next().is_some() {
            return None;
        }
        Some(m)
    }
}

/// Round to whole nanoseconds and group with underscores, as `cargo bench` does
/// with commas. Underscores because they are what a Rust literal uses, and the
/// output is often pasted straight into a comment or a threshold constant.
fn thousands(value: f64) -> String {
    let digits = format!("{:.0}", value.max(0.0));
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('_');
        }
        out.push(ch);
    }
    out
}

/// How long a single page load may take before it is called a hang.
///
/// 30 s suits a test. It does not suit a large release-less module that
/// instantiates itself on several workers before it logs anything, so
/// `WASM_LITE_TIMEOUT_SECS` raises it. An unparseable value is ignored rather
/// than fatal — a typo in an env var should not stop the suite.
fn run_seconds() -> Option<u64> {
    std::env::var("WASM_LITE_RUN_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
}

fn timeout_secs() -> u64 {
    std::env::var("WASM_LITE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30)
}

/// Poll until the page records a result (or time out).
fn wait_done(browser: &Browser) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs());
    loop {
        if browser.eval_bool("return !!globalThis.__wl_done;")? {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "timed out after {}s waiting for the program to finish \
                 (raise WASM_LITE_TIMEOUT_SECS if it just needs longer)",
                timeout_secs()
            ));
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

/// How long the page tolerates hearing nothing from the runner before it
/// assumes the runner is gone and tears itself down.
///
/// Only an absent runner can trip this — a live one stamps `__wl_hb` on every
/// WebDriver script, which is several times a second — so the threshold is set
/// far above any real gap between polls rather than tuned close to one.
const WATCHDOG_MS: u64 = 30_000;

/// HTML shell for test mode: captures console output into a global the runner
/// polls, then loads the program module.
///
/// It also runs the dead-runner watchdog. A `SIGKILL`ed runner cannot close
/// the browser on its way out — no handler runs — so the page has to notice
/// on its own; navigating away discards the module and terminates the workers
/// it spawned, which is what stops a spin-waiting test from pinning a core.
///
/// The check is a main-thread timer, so it fires for the blocking work this
/// matters most for: bodies that block run under `#[wasm_lite_test(worker)]`,
/// leaving the main thread free to service it. A test that instead spins on
/// the main thread starves the timer and is beyond the page's reach.
fn test_html() -> String {
    format!(
        "<!DOCTYPE html>\n\
        <html lang=\"en\"><head><meta charset=\"utf-8\"><title>wasm_lite test</title></head>\n\
        <body>\n\
        <script>\n\
        globalThis.__wl_console = [];\n\
        for (const level of [\"log\", \"error\", \"warn\", \"info\"]) {{\n\
            const original = console[level].bind(console);\n\
            console[level] = (...args) => {{ original(...args); globalThis.__wl_console.push(args.join(\" \")); }};\n\
        }}\n\
        globalThis.__wl_hb = Date.now();\n\
        setInterval(() => {{\n\
            if (Date.now() - globalThis.__wl_hb <= {WATCHDOG_MS}) return;\n\
            try {{ window.close(); }} catch (e) {{}}\n\
            location.replace(\"about:blank\");\n\
        }}, 1000);\n\
        </script>\n\
        <script type=\"module\" src=\"/bootstrap.js\"></script>\n\
        </body></html>\n"
    )
}

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

/// Bootstrap for a test or benchmark harness: run the single case named by
/// `?test=<name>` or `?bench=<name>`.
///
/// A benchmark additionally publishes its measurement in `__wl_bench`, read
/// back through the module's own exports rather than through a JS global the
/// benchmark would have to know the name of.
const HARNESS_BOOTSTRAP: &str = r#"
const params = new URLSearchParams(location.search);
const test = params.get("test");
const bench = params.get("bench");
try {
    const instance = await instantiate("/program.wasm");
    if (bench !== null) {
        const read = () => {
            globalThis.__wl_bench = [
                instance.exports.__wl_bench_median_ns(),
                instance.exports.__wl_bench_min_ns(),
                instance.exports.__wl_bench_max_ns(),
                instance.exports.__wl_bench_iters(),
            ].join(",");
        };
        instance.exports["__wl_bench_" + bench]();
        // An async benchmark has not measured anything yet when its entry point
        // returns — it marked the run pending and spawned the body. Reading the
        // exports now would capture zeros. Wait for the verdict, then read.
        if (globalThis.__wl_async_pending) {
            while (!globalThis.__wl_done) {
                await new Promise(r => setTimeout(r, 10));
            }
            read();
        } else {
            read();
        }
    } else {
        instance.exports["__wl_test_" + test]();
    }
    if (!globalThis.__wl_async_pending) globalThis.__wl_done = { ok: true, error: "" };
} catch (e) {
    globalThis.__wl_done = { ok: false, error: String((e && e.stack) || e) };
}
"#;
