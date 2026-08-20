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
//!
//! The first shape also runs `main` as a final case unless the module says it is
//! inert (`test_main!`), because the shapes are not exclusive: a binary can hold
//! registered tests *and* a libtest entry point that owns plain `#[test]`s or a
//! merged doctest bundle. Treating them as exclusive is how one registered test
//! came to silently disable every other test in its binary.

use super::webdriver::Browser;
use super::{
    Args, BOOTSTRAP_JS, PROGRAM_JS, PROGRAM_WASM, Route, WB_GLUE_JS, WL_GLUE_JS, WL_WORKER_JS,
    bind, read, serve,
};
use std::path::Path;
use std::time::{Duration, Instant};
use wasm_lite_codegen::{ShouldPanic, TestDecl};

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

    // Name filters and `#[ignore]` are counted differently, as libtest does: a
    // name that a filter excluded is "filtered out", while an ignored case that
    // matched is reported as ignored so it stays visible in the summary.
    let all = module.tests.clone();
    let tests: Vec<TestDecl> = all
        .iter()
        .filter(|t| args.selects(&t.path))
        .cloned()
        .collect();
    let filtered_out = all.len() - tests.len();

    let all_benches = module.benches.clone();
    let benches: Vec<String> = all_benches
        .iter()
        .filter(|b| args.selects(&b.path) && args.admits_ignored(b.ignored))
        .map(|b| b.path.clone())
        .collect();
    let benches_filtered_out = all_benches.len() - benches.len();

    // The entry point joins a harness run as one more case, under a name filters
    // can match, because it is one more thing that runs and can fail. It is only
    // a separate case when there is a harness at all: with neither tests nor
    // benchmarks, running `main` *is* the whole job and `run_main` reports it.
    let harness = !all.is_empty() || !all_benches.is_empty();
    let has_entry_point = module.entry_point && harness;
    let entry_point = has_entry_point && args.selects(ENTRY_POINT) && args.admits_ignored(false);
    // Only one of the two summaries is ever printed, so both carry the delta.
    let entry_filtered_out = usize::from(has_entry_point) - usize::from(entry_point);
    let filtered_out = filtered_out + entry_filtered_out;
    let benches_filtered_out = benches_filtered_out + entry_filtered_out;

    // `cargo test -- --list` / IDE test discovery: report names, don't run.
    if args.list {
        for test in &tests {
            println!("{}: test", test.path);
        }
        if entry_point {
            println!("{ENTRY_POINT}: test");
        }
        for name in &benches {
            println!("{name}: bench");
        }
        println!();
        println!(
            "{} tests, {} benchmarks",
            tests.len() + usize::from(entry_point),
            benches.len()
        );
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
        run_bench_suite(
            &browser,
            port,
            &benches,
            benches_filtered_out,
            entry_point,
            args.bench,
        )
    } else {
        run_suite(&browser, port, &tests, filtered_out, entry_point, args)
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            2
        }
    }
}

/// Routes to serve plus the discovered tests and benchmarks.
struct Prepared {
    routes: Vec<Route>,
    tests: Vec<TestDecl>,
    benches: Vec<TestDecl>,
    /// Whether `main` is worth running in its own right; see
    /// [`wasm_lite_codegen::runs_own_entry_point`].
    entry_point: bool,
}

fn prepare(program: &Path) -> Result<Prepared, String> {
    if program.extension().and_then(|e| e.to_str()) != Some("wasm") {
        return Err("test mode supports .wasm programs".to_string());
    }
    let module = read(program)?;

    // A wasm-bindgen interop target takes the same shape, just assembled
    // differently: the CLI finalizes the module and the entry point is a merged
    // loader over two glues rather than one. The wasm-bindgen CLI preserves our
    // custom sections and `__wl_test_*` exports, so discovery and invocation are
    // unchanged from here on — only how `program.js` and `program.wasm` are
    // produced differs.
    // Both checks run before the interop branch, on the module as linked. A
    // test binary is the likeliest place to meet either, because it is where
    // threads first get spawned, and both are silent from here otherwise: the
    // worker throws before running the closure, so the run reports a timeout
    // rather than a bad link line. On the interop path they also pre-empt the
    // wasm-bindgen CLI, whose own `failed to find __tls_align` names a symbol
    // but not the flag, the file, or the reason.
    let missing = wasm_lite_codegen::missing_thread_exports(&module)?;
    if !missing.is_empty() {
        return Err(wasm_lite_codegen::missing_thread_exports_message(&missing));
    }
    if wasm_lite_codegen::spawns_without_shared_memory(&module)? {
        return Err(wasm_lite_codegen::spawns_without_shared_memory_message(
            &module,
        ));
    }

    let interop = wasm_lite_codegen::uses_wasm_bindgen(&module);
    let (module, entry_js, extra_routes) = if interop {
        let bundle = wasm_lite_codegen::build_interop(program)?;
        let loader =
            wasm_lite_codegen::interop_harness_loader(PROGRAM_WASM, WL_GLUE_JS, WB_GLUE_JS);
        let extra = vec![
            Route {
                path: WL_GLUE_JS,
                content_type: "text/javascript; charset=utf-8",
                body: bundle.wl_glue_js.into_bytes(),
            },
            Route {
                path: WB_GLUE_JS,
                content_type: "text/javascript; charset=utf-8",
                body: bundle.wb_glue_js.into_bytes(),
            },
        ];
        (bundle.wasm, loader, extra)
    } else {
        let descriptors = wasm_lite_codegen::descriptors_from_wasm(&module)?;
        let exports = wasm_lite_codegen::exports_from_wasm(&module)?;
        let memory = wasm_lite_codegen::imported_memory(&module)?;
        let glue = wasm_lite_codegen::generate_glue(&descriptors, &exports, memory.as_ref());
        (module, glue, Vec::new())
    };

    // Read the harness metadata off the module that will actually run: for
    // interop that is the finalized one, which is not byte-identical to the
    // input.
    let memory = wasm_lite_codegen::imported_memory(&module)?;
    let tests = wasm_lite_codegen::test_decls(&module)?;
    let benches = wasm_lite_codegen::bench_decls(&module)?;
    let entry_point = wasm_lite_codegen::runs_own_entry_point(&module)?;
    // One harness bootstrap covers both shapes, dispatching on the query
    // parameter rather than on which section exists — a target may declare
    // tests and benchmarks in the same file, and picking by section would make
    // one of them unreachable.
    let body = if tests.is_empty() && benches.is_empty() {
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
            body: entry_js.into_bytes(),
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
    routes.extend(extra_routes);
    // Shared-memory builds spawn threads onto workers: serve the worker
    // bootstrap. An interop bundle has no worker path of its own, so this only
    // ever fires for the plain glue.
    if memory.is_some() && !interop {
        routes.push(Route {
            path: WL_WORKER_JS,
            content_type: "text/javascript; charset=utf-8",
            body: wasm_lite_codegen::generate_worker("./program.js").into_bytes(),
        });
    }

    Ok(Prepared {
        routes,
        tests,
        benches,
        entry_point,
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
    let mut console = Console::default();
    if let Err(err) = wait_done(browser, &mut console) {
        eprintln!("error: {err}");
        console.report("--- console output before it stopped ---", &err);
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
    let structured = browser.eval_string(LOGWISE_JOIN)?;
    if !console.is_empty() {
        println!("{console}");
    } else {
        let error = browser.eval_string("return globalThis.__wl_done.error || \"\";")?;
        if !error.is_empty() {
            eprintln!("{error}");
        }
    }
    if !structured.is_empty() {
        println!("--- recent structured history ---\n{structured}");
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

/// The name the module's own `main` runs under in a suite's output.
///
/// Unqualified, so it cannot collide with a registered test: those are always
/// `module::path`-shaped.
const ENTRY_POINT: &str = "main";

/// Run a `tests!` harness: each test in a fresh page load, libtest-style output.
///
/// With `entry_point`, `main` runs as one last case. That is not redundant with
/// the registered tests: `#[wasm_lite_test]` emits no libtest `#[test]`, and the
/// documented `cfg_attr` pairing applies only one of the two per target, so
/// nothing runs twice — but a plain `#[test]`, or another doctest sharing an
/// edition-2024 merged bundle, runs *only* here.
fn run_suite(
    browser: &Browser,
    port: u16,
    tests: &[TestDecl],
    filtered_out: usize,
    entry_point: bool,
    args: &Args,
) -> Result<i32, String> {
    let total = tests.len() + usize::from(entry_point);
    println!("\nrunning {total} test{}", plural(total));
    let mut failed = 0;
    let mut ignored = 0;

    for test in tests {
        let name = &test.path;
        if !args.admits_ignored(test.ignored) {
            ignored += 1;
            println!("test {name} ... ignored");
            continue;
        }

        let encoded_name = encode_query_component(name);
        browser.goto(&format!("http://127.0.0.1:{port}/?test={encoded_name}"))?;
        // A hung test is that test's failure, not the suite's: report it and
        // move on so the remaining tests still run and get a summary. (Errors
        // from goto/eval still abort — those mean the browser session is gone.)
        let mut watch = Console::default();
        if let Err(err) = wait_done(browser, &mut watch) {
            failed += 1;
            println!("test {name} ... FAILED");
            for line in err.lines() {
                println!("    {line}");
            }
            // Same reasoning as `run_main`: what it logged before it stopped is
            // usually the only clue to what happened, and if the page was
            // discarded this snapshot is the only copy left. The failure that
            // ended the run is normally in both, so print it once.
            for line in watch
                .text
                .lines()
                .chain(watch.structured.lines())
                .filter(|l| !err.contains(*l))
            {
                println!("    {line}");
            }
            continue;
        }

        let completed = browser.eval_bool("return globalThis.__wl_done.ok === true;")?;
        // The panic hook logged the message through console.error, which the
        // page captured. It is both what we print on failure and what
        // `should_panic(expected = "…")` matches against.
        let output = browser.eval_string(CONSOLE_JOIN)?;
        let structured = browser.eval_string(LOGWISE_JOIN)?;

        // A `#[should_panic]` test inverts the verdict: the module traps, and
        // only the runner can tell "trapped as intended" from "trapped". Each
        // test gets a fresh page, so the poisoned instance is discarded anyway.
        if let Some(should_panic) = &test.should_panic {
            if completed {
                failed += 1;
                println!("test {name} ... FAILED");
                println!("    note: test did not panic as expected");
            } else if let ShouldPanic::Expected(expected) = should_panic
                && !output.contains(expected.as_str())
            {
                failed += 1;
                println!("test {name} ... FAILED");
                println!("    note: panic did not contain the expected message");
                println!("    expected: {expected}");
                for line in output.lines() {
                    println!("    panicked: {line}");
                }
                for line in structured.lines() {
                    println!("    {line}");
                }
            } else {
                println!("test {name} ... ok");
            }
            continue;
        }

        if completed {
            surface_worker_panics(browser)?;
            println!("test {name} ... ok");
        } else {
            failed += 1;
            println!("test {name} ... FAILED");
            for line in output.lines() {
                println!("    {line}");
            }
            if output.is_empty() {
                let error = browser.eval_string("return globalThis.__wl_done.error || \"\";")?;
                if !error.is_empty() {
                    println!("    {error}");
                }
            }
            for line in structured.lines() {
                println!("    {line}");
            }
        }
    }

    if entry_point && !run_entry_point(browser, port)? {
        failed += 1;
    }

    let passed = total - failed - ignored;
    println!();
    if failed == 0 {
        println!(
            "test result: ok. {passed} passed; 0 failed; {ignored} ignored; {filtered_out} filtered out"
        );
        Ok(0)
    } else {
        println!(
            "test result: FAILED. {passed} passed; {failed} failed; {ignored} ignored; {filtered_out} filtered out"
        );
        Ok(1)
    }
}

/// Run the module's `main` as the suite's last case; true if it passed.
///
/// Reported like any other case rather than folded into the summary silently,
/// because on `wasm32-unknown-unknown` it is a coarse verdict: libtest's own
/// output goes nowhere (`println!` has no console here), and `panic = abort`
/// ends the process at the first failure, so what comes back is "something under
/// `main` failed" with the panic message and no test name. Coarse and visible
/// beats the alternative it replaced, which was not running them at all.
fn run_entry_point(browser: &Browser, port: u16) -> Result<bool, String> {
    browser.goto(&format!("http://127.0.0.1:{port}/"))?;

    let mut watch = Console::default();
    if let Err(err) = wait_done(browser, &mut watch) {
        println!("test {ENTRY_POINT} ... FAILED");
        for line in err.lines() {
            println!("    {line}");
        }
        for line in watch
            .text
            .lines()
            .chain(watch.structured.lines())
            .filter(|l| !err.contains(*l))
        {
            println!("    {line}");
        }
        return Ok(false);
    }

    if browser.eval_bool("return globalThis.__wl_done.ok === true;")? {
        surface_worker_panics(browser)?;
        println!("test {ENTRY_POINT} ... ok");
        return Ok(true);
    }

    println!("test {ENTRY_POINT} ... FAILED");
    let output = browser.eval_string(CONSOLE_JOIN)?;
    let structured = browser.eval_string(LOGWISE_JOIN)?;
    for line in output.lines() {
        println!("    {line}");
    }
    if output.is_empty() {
        let error = browser.eval_string("return globalThis.__wl_done.error || \"\";")?;
        if !error.is_empty() {
            println!("    {error}");
        }
    }
    for line in structured.lines() {
        println!("    {line}");
    }
    Ok(false)
}

/// Run a `#[wasm_lite_bench]` harness: each benchmark in a fresh page load.
///
/// With `measure`, prints libtest's `cargo bench` line; without it, each
/// benchmark still runs (so a broken one fails CI) but only its pass/fail is
/// reported — a timing taken while the rest of the suite is compiling or a CI
/// box is oversubscribed is worse than no timing, because it looks like data.
///
/// `entry_point` runs `main` afterwards for the same reason [`run_suite`] does:
/// a `#[wasm_lite_bench]` module that did not use `bench_main!` may be a libtest
/// binary whose `#[test]`s run nowhere else.
fn run_bench_suite(
    browser: &Browser,
    port: u16,
    names: &[String],
    filtered_out: usize,
    entry_point: bool,
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
        let mut watch = Console::default();
        if let Err(err) = wait_done(browser, &mut watch) {
            failed += 1;
            println!("test {name} ... FAILED");
            for line in err.lines() {
                println!("    {line}");
            }
            for line in watch.text.lines().chain(watch.structured.lines()) {
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

    let mut total = names.len();
    if entry_point {
        total += 1;
        if !run_entry_point(browser, port)? {
            failed += 1;
        }
    }

    let passed = total - failed;
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
/// The last console text the page was seen to hold.
///
/// Kept as we go because a page that dies takes its console with it: once a
/// browsing context is discarded every later `eval` fails with a WebDriver
/// error, so reading the log *after* noticing the failure gets nothing. Firefox
/// discards the context where Chrome reports the exception, which is why the
/// same crash used to be diagnosable in one browser and silent in the other.
#[derive(Default)]
struct Console {
    text: String,
    structured: String,
}

impl Console {
    /// Refresh from the page, keeping the previous text if it can no longer be
    /// read (which is itself usually the interesting case).
    fn refresh(&mut self, browser: &Browser) {
        if let Ok(text) = browser.eval_string(CONSOLE_JOIN) {
            self.text = text;
        }
        if let Ok(structured) = browser.eval_string(LOGWISE_JOIN) {
            self.structured = structured;
        }
    }

    /// Print what the page logged, however the run ended.
    fn report(&self, heading: &str, already_said: &str) {
        // The failure that ended the run is usually both the error and the last
        // console line; saying it twice reads like two problems.
        let rest: Vec<&str> = self
            .text
            .lines()
            .filter(|l| !already_said.contains(*l))
            .collect();
        if rest.is_empty() && self.structured.is_empty() {
            return;
        }
        eprintln!("{heading}");
        if !rest.is_empty() {
            eprintln!("{}", rest.join("\n"));
        }
        if !self.structured.is_empty() {
            eprintln!("--- recent structured history ---");
            eprintln!("{}", self.structured);
        }
    }
}

/// Is this the browser telling us the page is gone rather than a real fault?
///
/// Firefox reports a crashed or self-discarded page this way and says nothing
/// else, so the raw WebDriver text is a poor thing to hand a user.
fn is_discarded_context(err: &str) -> bool {
    err.contains("no such window") || err.contains("Browsing context has been discarded")
}

fn wait_done(browser: &Browser, console: &mut Console) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs());
    let mut next_console_read = Instant::now();
    loop {
        match browser.eval_bool("return !!globalThis.__wl_done;") {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(err) if is_discarded_context(&err) => {
                return Err(
                    "the page was discarded before the run finished — it crashed, or ran out \
                     of memory. What it logged first is below."
                        .to_string(),
                );
            }
            Err(err) => return Err(err),
        }

        // A worker that failed to start will never resolve whatever is waiting
        // on it, so this is an ending, not a delay. Reporting it as a timeout
        // told users to raise the timeout, which could never help.
        let start_error = browser.eval_string("return globalThis.__wl_worker_start_error || \"\";");
        if let Ok(text) = start_error
            && !text.is_empty()
        {
            console.refresh(browser);
            return Err(text);
        }

        // Cheap enough at this cadence, and the only way to have the log at all
        // if the page dies between polls.
        if Instant::now() >= next_console_read {
            console.refresh(browser);
            next_console_read = Instant::now() + Duration::from_millis(500);
        }

        if Instant::now() > deadline {
            console.refresh(browser);
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
const LOGWISE_JOIN: &str = r#"
const retained = (globalThis.__wl_logwise || []).slice(-32).map(record => {
    try {
        const bytes = Uint8Array.from(record);
        const view = new DataView(bytes.buffer);
        const sequence = view.getBigUint64(12, true);
        const dropped = view.getBigUint64(20, true);
        const truncated = view.getBigUint64(28, true);
        const worker = view.getBigUint64(36, true);
        const context = view.getBigUint64(44, true);
        let offset = 60;
        const links = view.getUint16(offset, true);
        offset += 2 + links * 16 + 3;
        const text = () => {
            const length = view.getUint16(offset, true);
            offset += 2;
            const value = new TextDecoder().decode(bytes.slice(offset, offset + length));
            offset += length;
            return value;
        };
        const event = text();
        text(); // package
        text(); // target
        text(); // module
        if (bytes[offset++]) text(); // optional domain
        const test = bytes[offset++] ? ` test=${text()}` : "";
        return `logwise[${sequence}] ${event}${test} worker=${worker} context=${context} dropped=${dropped} truncated=${truncated}`;
    } catch (_) {
        return "logwise: malformed retained envelope";
    }
});
const overwritten = globalThis.__wl_logwise_dropped || 0;
if (overwritten) retained.unshift(`logwise host ring overwrote ${overwritten} record(s)`);
return retained.join("\n");
"#;

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
        for (const level of [\"log\", \"error\", \"warn\", \"info\", \"debug\"]) {{\n\
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
/// `?test=<name>` or `?bench=<name>`, or the module's own `main` when neither is
/// given.
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
    } else if (test !== null) {
        instance.exports["__wl_test_" + test]();
    } else {
        // No case named: this page is the suite's entry-point step, where `main`
        // is libtest's and owns the plain `#[test]`s and doctests that have no
        // `__wl_test_` export of their own. The runner only asks for this when
        // the module has a `main` that is not a `test_main!` no-op.
        instance.exports.main();
    }
    if (!globalThis.__wl_async_pending) globalThis.__wl_done = { ok: true, error: "" };
} catch (e) {
    globalThis.__wl_done = { ok: false, error: String((e && e.stack) || e) };
}
"#;
