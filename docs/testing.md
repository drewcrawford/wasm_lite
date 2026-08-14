# Testing

*(Part of the [wasm_lite](../README.md) docs. See also: [binding model](./binding-model.md),
[threads & async](./threads-and-async.md), [interop](./interop.md),
[roadmap](./roadmap.md), [migration guide](../MIGRATION.md).)*

wasm_lite runs your tests in a *real* browser. The runner discovers each test,
drives it headless, and reports pass / fail / panic back to `cargo`. This page is
organized by task.

## Testing goals

Testing is a first-class part of the design, not a separate JS-side harness that
users assemble later.

* **Browser by default.** The
  [wasm-bindgen-test runner](https://github.com/rustwasm/wasm-bindgen/blob/main/crates/test/README.md)
  documents a Node execution path unless a suite opts into browser mode.
  wasm_lite's runner always uses a real browser, because modern browser behavior
  — module workers, `SharedArrayBuffer`, COOP/COEP, `Atomics.waitAsync`, and
  WebDriver-observed failures — is the target.
* **Cargo-shaped workflow.** The same binary is both server and test runner. It
  serves `cargo run` interactively, runs `cargo test` headless, and detects
  rustdoc doctest artifacts (`rustdoctest...`) so doctests go through the same
  browser path.
* **Isolation by default.** `#[wasm_lite_test]` names are discovered from the
  `__wasm_lite_tests` section, and each test runs in a fresh page load. That
  avoids one test's panic or mutated JS state poisoning the next test. The
  wasm-bindgen-test runner docs list "running each test in its own Wasm
  instance" as future work in that harness.
* **Fail-closed async.** The generated glue has explicit pending/pass test
  hooks. An async test must mark itself pending and later mark itself passed; if
  a future panics, is dropped, or hangs, the runner reports failure or timeout
  instead of accepting `main` returning as success.
* **CLI-visible logs and panics.** The HTML shell captures `console.log`,
  `console.error`, `console.warn`, and `console.info`; generated worker glue
  forwards worker console output to the main realm; and the test runner prints
  captured panic messages rather than leaving users with a bare wasm trap.

This is informed by the [`wasm_ffi`](https://github.com/drewcrawford/wasm_ffi)
work on [wasm-bindgen](https://wasm-bindgen.github.io/wasm-bindgen/): that fork
exists largely because real applications exposed gaps in doctests, worker log
capture, realtime headless output, Node/thread behavior, and logging
performance. wasm_lite bakes those lessons into the runner instead of treating
them as after-the-fact patches.

## Point `cargo test` at the runner

Set the runner as the wasm target's test/run command (see the
[README quickstart](../README.md#quickstart) for building it):

```toml
# .cargo/config.toml
[target.wasm32-unknown-unknown]
runner = ["wasm_lite", "run"]
```

Then `cargo test` runs headless and exits, while `cargo run` serves a bin
interactively in the browser — the runner distinguishes them by path.

## Configure the runner

Everything below is an environment variable, because it is set per-invocation
rather than per-project.

| variable | what it changes |
|---|---|
| `WASM_LITE_BROWSER` | `chrome` / `chromium` / `safari`; default **firefox** |
| `WASM_LITE_GPU` | give headless Chrome a real (software) WebGPU adapter |
| `WASM_LITE_BROWSER_ARGS` | extra browser flags, space-separated |
| `WASM_LITE_SERVE_DIR` | serve a directory alongside the program |
| `WASM_LITE_TIMEOUT_SECS` | per-page deadline; default 30 |
| `WASM_LITE_RUN_SECONDS` | watch a long-running `bin` for N seconds (selects headless mode) |
| `WASM_LITE_REUSE_BROWSER` | keep one browser across invocations (`--stop-browser` ends it) |
| `WASM_LITE_NO_OPEN` | serve without launching a browser |

Three of these exist because their defaults produce a *plausible wrong answer*
rather than an error, which is much harder to notice:

* **`WASM_LITE_GPU`.** Chrome is otherwise launched with `--disable-gpu`, so
  `navigator.gpu.requestAdapter()` resolves to `null` and every graphics test
  fails in a way indistinguishable from a bug in the code under test. Firefox
  has no headless WebGPU at all, so anything graphical also wants
  `WASM_LITE_BROWSER=chrome`.
* **`WASM_LITE_SERVE_DIR`.** Without it the runner serves only the program's own
  files, so a fetch for a texture or a shader 404s and the program concludes its
  assets are missing — which they are, but not for the reason it will report.
  Serving is confined to that directory: `..`, absolute components and symlinks
  leading outside are refused, and the generated routes always take precedence,
  so nothing on disk can shadow `program.wasm` or the glue.
* **`WASM_LITE_RUN_SECONDS`.** `cargo test` mode declares success the instant
  `main` returns and then discards the console. For a doctest that is right; for
  an application whose work lives on the event loop — a render loop, an executor
  — it means the program "passes" immediately and prints nothing. This keeps the
  page alive and always prints the log.

  Setting it also *selects* headless mode, so it works with `cargo run` — which
  would otherwise take the interactive path, serve forever and print nothing at
  all. If the program reports a failure, the reason it recorded is printed too:
  a bare `FAILED` under 60 000 lines of a program working normally is not a
  diagnosis.

A **timeout** dumps the captured console, so a hang tells you where it hung.

### A killed runner does not leave a browser behind

The runner closes its WebDriver session and kills its driver on the way out, but
that teardown runs from `Drop` — which a signal does not run. Left alone, a
killed runner therefore strands a driver and a headless browser still executing
the test page, and a test that was spin-waiting keeps a core pinned until the
machine is rebooted. Two mechanisms prevent that:

* **`SIGINT`/`SIGTERM`/`SIGHUP`** are handled. The handler only records the
  signal; a watchdog thread does the actual close and then exits `128 + signal`.
  This covers Ctrl-C, a CI job cancellation, and a killed shell.
* **`SIGKILL` cannot be handled**, so the page defends itself instead. Every
  WebDriver script the runner sends stamps a heartbeat, and the test shell
  checks it once a second: 30 s without one means the runner is gone, and the
  page discards itself, terminating the module and any workers it spawned.

The page-side check is a main-thread timer, so it covers the case it is meant
to — blocking bodies run under `#[wasm_lite_test(worker)]`, leaving the main
thread free to service it. A body that instead spins on the main thread starves
the timer and can only be cleaned up from outside.

If a live-but-stalled runner ever trips the 30 s threshold, it reports
`no such window: Browsing context has been discarded` rather than hanging.

## Write a test

Mark a function with `#[wasm_lite_test]`; it is recorded in `__wasm_lite_tests`
and the runner drives it in a browser. Tests can live in normal Rust test
modules — the runner uses Rust-style module paths (e.g.
`my_crate::tests::test_continue`), so tests with the same leaf name in different
modules coexist.

```rust
#[cfg(test)]
mod tests {
    #[wasm_lite::wasm_lite_test]
    fn two_plus_two() {
        assert_eq!(2 + 2, 4);
    }
}
```

## Test blocking or threaded code

By default a test body runs on the **main thread**, where blocking APIs
(`lock_block`, `recv_block`, `park`, synchronous `join`) trap. To test them, run
the body on a dedicated Web Worker with `#[wasm_lite_test(worker)]` — a
fail-closed `spawn` + `join_async` wrapper. This form requires a shared-memory
`+atomics` build; stable non-atomic wasm has no worker-spawn backend:

```rust
#[wasm_lite::wasm_lite_test(worker)]
fn blocking_lock() {
    let m = wasm_lite_std::Mutex::new(0);
    *m.lock_block() += 1;          // would trap on the main thread
    assert_eq!(*m.lock_block(), 1);
}
```

## Test async or fail-closed code

Wrap the body in `wasm_lite_std::async_doctest!` (usable in a `#[wasm_lite_test]`
body or a doctest). Unlike a normal harness — where `main` returning *is* the
verdict — this defers the verdict until the future completes, so a panic, dropped
task, or deadlock can't masquerade as a pass. See
[threads & async](./threads-and-async.md#async-lifecycle--failures--two-fixes-for-wasm-bindgen-footguns)
for why.

```rust
#[wasm_lite::wasm_lite_test]
fn awaited_worker() {
    wasm_lite::set_panic_hook();
    wasm_lite_std::async_doctest!(async {
        let v = wasm_lite_std::spawn(|| 2 + 2).join_async().await.unwrap();
        assert_eq!(v, 4);
    });
}
```

## Share one test between native and wasm

Use `cfg_attr` so the same function is a native `#[test]` off-wasm and a
browser-driven wasm_lite test on wasm — handy when migrating from `#[test]` or
`wasm-bindgen-test`:

```rust
#[cfg(test)]
mod tests {
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_lite::wasm_lite_test)]
    fn test_continue() {
        assert_eq!(2 + 2, 4);
    }
}
```

## Run doctests

Doctests run too — rustdoc's doctest binaries are detected and driven headless.
Call `wasm_lite::set_panic_hook()` at the top of a doctest so failures report the
panic message rather than a bare trap.

## Write a harness-less integration test

For a standalone wasm suite, set `harness = false` on the `[[test]]` target and
call `wasm_lite::test_main!()` once in the file:

```toml
# Cargo.toml
[[test]]
name = "browser"
harness = false
```

## Run the `wasm_lite_std` browser suite

`crates/wasm_lite_std/tests/browser.rs` (a `harness = false` target), together
with the crate's wasm-enabled lib tests, exercises
`Mutex`/`RwLock`/`Condvar`/`mpsc`/`time`, `sleep_async`, `park`/`unpark`, and
`spawn`/`join_async` in a real browser. Blocking variants use `(worker)` tests.
Run the full shared-memory suite with:

```bash
crates/wasm_lite_std/run-browser-tests.sh
```

It requires **nightly** (atomics ⇒ recompiled `std`) and a WebDriver browser; pass
`--no-run` to just build. The repository-wide `scripts/wasm32/tests` also runs a
stable, non-atomic browser smoke test for the local executor and `sleep_async`,
so accidental dependencies on atomic waits cannot hide behind the threaded run.

## Benchmark in a browser

`#[wasm_lite_bench]` is the benchmark counterpart of `#[wasm_lite_test]`. The
function takes a `&mut Bencher` and hands it the work to measure; the harness
picks the iteration count and reports ns/iter in the shape `cargo bench` prints.

```toml
# Cargo.toml — `harness = false` for the same reason a test target needs it
[[bench]]
name = "arith"
harness = false
```

```rust
use std::hint::black_box;
use wasm_lite::{Bencher, wasm_lite_bench};

#[wasm_lite_bench]
fn multiply(b: &mut Bencher) {
    b.iter(|| black_box(black_box(6364136223846793005u64).wrapping_mul(1442695040888963407)));
}

wasm_lite::bench_main!();
```

```text
$ cargo bench
running 5 benchmarks
test arith::empty ... bench:           0 ns/iter (+/- 0)
test arith::multiply ... bench:           2 ns/iter (+/- 1)
test arith::sum_to_1000 ... bench:          11 ns/iter (+/- 3)
test arith::allocate_a_vec ... bench:          65 ns/iter (+/- 10)
... awaited_yield reports in the same shape, with browser-dependent timing ...
```

Each benchmark gets its own page load, exactly as each test does, so one that
traps cannot take the rest of the suite with it.

### Why not criterion

[criterion](https://docs.rs/criterion) needs a wall clock it can trust at
sub-microsecond resolution, spawns processes for its baseline comparisons, and
writes its history to disk. A browser tab has none of those. What it has is
`performance.now()` — coarsened to **5 µs** even in a cross-origin-isolated page
(and to 1 ms without it) as a Spectre mitigation.

That single fact sets the design. A timer that cannot resolve one iteration has
to time a *batch*, so `iter` calibrates a batch size before measuring anything:
it grows the count until one batch takes at least 10 ms, at which point the
clock's granularity contributes under 0.1% of the reading. It then takes 50 such
batches and reports the **median** ns/iter, with `+/-` as the full spread
(max − min), the same numbers libtest's `cargo bench` prints.

Median rather than mean because a browser will occasionally interrupt a sample
with GC or another tab, and one such sample drags a mean much further than it
drags the truth. The spread is the full range rather than a standard deviation
so those interruptions stay *visible* — a suspiciously wide `+/-` says the
machine was busy, not that the code is variable.

That is not hypothetical. The allocating benchmark in `examples/bench-demo`
regularly reports something like:

```text
test arith::allocate_a_vec ... bench:          71 ns/iter (+/- 575)
```

A spread eight times the median, on a body whose real cost is stable — one
sample met the garbage collector. The median is still the number you want; the
`+/-` is what tells you not to trust a *single* run of anything else in that
suite. A standard deviation would have quietly absorbed it.

The honest limitation: every figure is a batch average, so this measures
throughput and cannot see per-iteration variance below the batch. It is the
right tool for "did this change make it faster" and the wrong one for tail
latency.

### Async benchmarks

Some work cannot be driven synchronously at all — a `requestAnimationFrame`
loop, an awaited GPU submission — and some work has setup that should not be in
the measurement. `iter_custom_async` covers both: the routine is handed the
iteration count, runs them itself, and returns **the duration it measured**.

```rust
#[wasm_lite_bench]
async fn awaited_yield(b: &mut Bencher) {
    b.iter_custom_async(|iters| async move {
        let start = wasm_lite_std::time::Instant::now();
        for _ in 0..iters {
            wasm_lite_std::yield_to_event_loop_async().await;
        }
        start.elapsed()
    }).await;
}
```

Calibration and sampling follow the same policy as `iter`; only the measurement
is delegated. The reported duration is trusted as given — a routine that returns
something unrelated to the work it did will be believed, which is the price of
letting it exclude setup.

An `async` benchmark defers its verdict rather than reporting when the entry
point returns, so a body that never completes fails via the runner's timeout
instead of publishing whatever the result exports happened to hold. The
fail-closed rule is unchanged: an async benchmark that never calls
`iter_custom_async` still **fails**.

This needs `wasm_lite_std`, but no longer inherently needs atomics: the stable
single-realm executor can drive an async benchmark that does not spawn workers.
Use the nightly shared-memory configuration only when the benchmark itself
spawns threads or measures cross-thread work.

### Benchmarks in CI

`cargo bench` measures; `cargo test --benches` runs each benchmark exactly once
without measuring. Use the latter in CI: it keeps benchmarks compiling and
working, without publishing timings taken while the rest of the suite competes
for the same cores — a number produced under those conditions is worse than no
number, because it looks like data.

A benchmark that never calls `Bencher::iter` **fails** rather than reporting
`0 ns/iter`, which would read as an astonishing result rather than the mistake
it is.

There is no `(worker)` form. A benchmark timed on a worker measures a thread the
browser is free to deprioritize, and `performance.now()` on a worker is
coarsened independently of the main thread's — two ways for the number to be
wrong that don't show up in the output. Benchmark on the main thread; if the
threading *is* what you want to measure, measure it end-to-end from there.

See `examples/bench-demo` for a runnable suite.
