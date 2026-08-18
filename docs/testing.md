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
| `WASM_LITE_MAX_BROWSERS` | cap concurrent browsers; default derived from free memory |
| `WASM_LITE_NO_OPEN` | serve without launching a browser |

Three of these exist because their defaults produce a *plausible wrong answer*
rather than an error, which is much harder to notice:

* **`WASM_LITE_GPU`.** Chrome is otherwise launched with `--disable-gpu`, so
  `navigator.gpu.requestAdapter()` resolves to `null` and every graphics test
  fails in a way indistinguishable from a bug in the code under test. Firefox
  has no headless WebGPU at all, so anything graphical also wants
  `WASM_LITE_BROWSER=chrome`. GPU mode enables unsafe WebGPU and explicitly
  selects Dawn's SwiftShader adapter, giving GPU-less CI hosts the same
  software-backed WebGPU device as developer machines.

  It is read for *presence*, not value: `WASM_LITE_GPU=0` enables GPU mode
  exactly as `=1` does. To get a browser without WebGPU, leave the variable
  unset — and note that Chrome's `--disable-features=WebGPU` does not remove
  `navigator.gpu`, so it cannot be used to make a WebGPU-capable Chrome pretend
  otherwise.
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

### How many browsers run at once

`cargo test --doc` invokes the runner **once per doctest** and runs those in
parallel across every core. Each invocation is its own browser, so a machine
with more cores than spare gigabytes tries to start more browsers than it can
hold — and the failures that follow do not look like memory pressure. They look
like `read: Resource temporarily unavailable (os error 11)` from a thread the
browser could not spawn, or `no such window: Browsing context has been
discarded` after the OOM killer took a content process. The giveaway is that the
casualties are arbitrary — tests with no threading in them at all — and that
they pass under `--test-threads=1`.

So the runner admits only so many browsers at once, waiting for a slot rather
than piling on. The default is free memory (`MemAvailable`), less a
reserve for everything that is not a browser, divided by a generous per-browser
estimate and clamped to the core count; `WASM_LITE_MAX_BROWSERS` overrides it
outright. Both numbers err on the cautious side, and the reserve exists because
the load competing for memory is usually a build that grows *after* the reading
is taken. On a memory-tight host this legitimately resolves to one browser —
that is the mechanism working, not failing. Slots are files in a temp directory, so the limit holds
across the separate runner *processes* rustdoc spawns — a lock inside one
process would not see the others. A slot whose holder was killed is reclaimed
by the next waiter.

`WASM_LITE_REUSE_BROWSER=1` is the complementary lever: instead of N browsers
capped at some number, one browser serves every invocation.

### Doctests that spawn threads

Doctests are linked by **rustdoc**, which does not read `rustflags` and ignores
`rustdocflags` under a `cfg(...)` predicate. A crate whose ordinary tests spawn
threads happily can therefore fail every doctest that spawns one — the symptom
is `wasm_lite: worker failed to start: TypeError: Cannot set properties of
undefined (setting 'value')` from `wl_worker.js`. `docs/threads-and-async.md`
has the configuration to copy and the rest of the diagnosis.

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

Write an `async fn`. `#[wasm_lite_test]` drives it —
`wasm_lite_std::block_on` off wasm32, the event loop in the browser — and
defers the verdict until the future completes, so a panic, dropped task, or
deadlock can't masquerade as a pass. See
[threads & async](./threads-and-async.md#async-lifecycle--failures--two-fixes-for-wasm-bindgen-footguns)
for why that matters here and not in a normal harness, where `main` returning
*is* the verdict.

```rust
#[wasm_lite::wasm_lite_test]
async fn awaited_worker() {
    let v = wasm_lite_std::spawn(|| 2 + 2).join_async().await.unwrap();
    assert_eq!(v, 4);
}
```

`wasm_lite_std::async_doctest!` is the same mechanism as an expression, for
places that cannot be an `async fn` — most of all a doctest, whose body is a
`main`:

```rust
#[wasm_lite::wasm_lite_test]
fn awaited_worker_the_long_way() {
    wasm_lite_std::async_doctest!(async {
        let v = wasm_lite_std::spawn(|| 2 + 2).join_async().await.unwrap();
        assert_eq!(v, 4);
    });
}
```

A body that awaits *and* blocks needs `#[wasm_lite_test(worker)] async fn`: the
future is driven by `block_on` on a worker — the one place it can block — while
the main thread awaits that worker's join.

`wasm_lite_std::block_on` is public for the cases that want it directly. It
blocks, so on wasm32 it must not run on the browser main thread; off wasm32
there is no such restriction.

## Share one test between native and wasm

Nothing to do: `#[wasm_lite_test]` *is* the shared spelling. Off wasm32 it
registers an ordinary libtest `#[test]`, so `cargo test` runs the same suite on
the host and `cargo test --target wasm32-unknown-unknown` runs it in a browser —
same names, same verdicts, same skips.

```rust
#[cfg(test)]
mod tests {
    #[wasm_lite::wasm_lite_test]
    fn test_continue() {
        assert_eq!(2 + 2, 4);
    }
}
```

`examples/dual-demo` is exactly that file. `scripts/native/tests` and
`scripts/wasm32/tests` each run it one way, and comparing the two outputs is the
demonstration.

Earlier versions could not do this — `#[wasm_lite_test]` emitted no libtest
registration — so the documented workaround was a `cfg_attr` pair:

```rust
#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_lite::wasm_lite_test)]
fn test_continue() {
    assert_eq!(2 + 2, 4);
}
```

That still works and needs no rewrite: off wasm32 the `cfg_attr` holding
`wasm_lite_test` is false, so the macro never runs and only libtest's `#[test]`
applies. New suites do not need the pair.

A *literal* `#[test]` alongside is a different thing, and worth avoiding. Below
the attribute it is detected and no second registration is made; above it, rustc
expands and consumes it before this macro runs, so the case is registered twice
and shows up twice in the output. Drop the `#[test]` — it is redundant now.

The one case that still wants a `cfg`: a test meaningful *only* in a browser.
It will now be run on the host too, so gate the item with
`#[cfg(target_arch = "wasm32")]` rather than gating the attribute. Inside a
`harness = false` target this does not arise — libtest is not linked there, so
the registration is inert and browser-only suites like
`crates/wasm_lite_std/tests/browser.rs` are unaffected.

## `#[should_panic]` and `#[ignore]`

Both work, and both are read from the ordinary libtest attributes rather than
from arguments to `#[wasm_lite_test]`:

```rust
#[wasm_lite_test]
#[should_panic(expected = "divide by zero")]
fn rejects_a_zero_divisor() { divide(1, 0); }

#[wasm_lite_test]
#[ignore = "needs a fixture we don't ship"]
fn slow_case() { /* … */ }
```

Reading them off the function rather than consuming them is what lets one
attribute mean the same thing on both targets: the same `#[should_panic]` is
honoured by libtest natively and by this runner on wasm32. It is also what keeps
the older `cfg_attr` pairing working.

On an `async fn` they are moved onto the generated libtest test rather than left
on the body, since the body is not itself the test — otherwise `#[ignore]` would
run the case anyway and `#[should_panic]` would report a correct test as failing.

`#[ignore]`d cases are skipped and reported as `ignored`, exactly as libtest
does; `--include-ignored` runs them alongside the rest and `--ignored` runs only
them (`cargo test -- --include-ignored`).

`#[should_panic]` inverts the verdict rather than catching anything. A wasm32
panic traps and poisons the instance, but every case gets a fresh page load, so
the instance is discarded regardless — the runner simply treats "trapped" as the
pass. The `expected = "…"` form matches against the panic message the hook
logged, so it needs `set_panic_hook()`, which `#[wasm_lite_test]` already calls
for you. A `#[should_panic]` test that *doesn't* panic fails, which is the point:
otherwise it could never fail.

`#[wasm_lite_bench]` accepts `#[ignore]` but rejects `#[should_panic]` at compile
time — a benchmark that panicked recorded no measurement, so there is no result
to invert.

## Run doctests

Doctests run too — rustdoc's doctest binaries are detected and driven headless.
Call `wasm_lite::set_panic_hook()` at the top of a doctest so failures report the
panic message rather than a bare trap.

They come from the **library target only**, which is cargo's rule rather than
anything this runner decides: a code block in a doc comment in `tests/*.rs` or
`src/bin/*.rs` is not collected as a doctest and never runs, on any target. If
you want the contents of one executed, it belongs in a `#[wasm_lite_test]`.

Either async form works in a doctest. `async_doctest!` is usually the one you
want, since a doctest's body is a `main`; a `#[wasm_lite_test] async fn` declared
inside a doctest also works, and is reported under its own name because it
registers in the harness section, where an `async_doctest!` is reported as part
of the merged bundle's `main`.

In edition 2024 rustdoc **merges** a crate's doctests into one binary that runs
them all against a single wasm instance. Two consequences are worth knowing:

- The runner's async-test verdict counts outstanding bodies rather than holding a
  single flag, so several `async_doctest!`s can be in flight at once and the
  first to finish does not pass the page on the others' behalf.
- `set_panic_hook()` in a doctest's `main` does **not** survive into a deferred
  body. libtest takes the current hook before each doctest and restores it
  afterwards, so a hook installed while `main` runs is gone by the time the event
  loop polls the body — and a panic there reported a bare wasm stack trace with
  no message. `async_doctest!` and `worker_doctest!` therefore install the hook
  *inside* the body themselves, so you get the message without doing anything. If
  you defer work by some other route, install the hook in the deferred part.

## `main` runs too

A binary can hold `#[wasm_lite_test]`s *and* a `main` that owns tests of its own,
and the runner runs both. After the registered suite it runs `main` as one more
case, reported as `test main ... ok`:

```
running 3 tests
test suite::adds ... ok
test suite::divides ... ok
test main ... ok
```

This matters because `main` is libtest's entry point whenever the target is not
`harness = false` — so it owns every plain `#[test]` in the binary, and every
doctest in a merged bundle. Those run nowhere else. Treating the two as
alternatives meant a single `#[wasm_lite_test]` anywhere in a binary silently
disabled all of them, and the suite still reported `ok`.

`main` takes part in filtering under the name `main`, so `cargo test -- --exact
main` runs only it and a filter that does not match it counts it as filtered out.
Its verdict is coarse — on `wasm32-unknown-unknown` libtest's own output goes
nowhere and `panic = abort` stops at the first failure, so a failure says *that*
something under `main` failed, with the panic message but no test name. If you
want per-test reporting, make each case a `#[wasm_lite_test]` — which now also
registers with libtest off wasm32, so nothing is lost by converting.

The one case where `main` is skipped is the one where it provably does nothing —
see below.

## Write a harness-less integration test

For a standalone wasm suite, set `harness = false` on the `[[test]]` target and
call `wasm_lite::test_main!()` once in the file:

```toml
# Cargo.toml
[[test]]
name = "browser"
harness = false
```

`test_main!()` (and `bench_main!()`) supply the `fn main() {}` the linker wants,
and also record in a custom wasm section that this `main` is inert, so the runner
skips the extra page load described above. That marker is the only way to tell
`fn main() {}` from libtest's entry point by looking at the module — both are
just a `main` export — so a hand-written `fn main() {}` is run rather than
skipped. Prefer the macro.

Such a target is wasm-only by construction: `harness = false` means libtest is
not linked, so off wasm32 the `#[test]` that `#[wasm_lite_test]` emits is inert
and *nothing in the file runs*. That is what makes the attribute safe to use in
browser-only suites, and it is also why "one attribute, both targets" applies to
ordinary `harness = true` targets rather than these. If you want a suite that
runs both ways, leave `harness` alone — see `examples/dual-demo`.

## Re-export the test macros from a wrapper crate

A crate that wraps wasm_lite and offers its own testing attributes has to point
the generated code at its own re-exports, because its users depend on neither
runtime by name and an absolute `::wasm_lite…` path will not resolve for them.
Both macros take the paths as arguments:

```rust
// The wrapper re-exports whatever it needs, under any names it likes.
pub use wasm_lite as __wl;
pub use wasm_lite_std as __wls;
```

```rust
// ...and forwards both halves. `crate =` covers the runtime; `std_crate =`
// covers the std veneer.
#[mycrate::__wl::wasm_lite_test(worker, crate = ::mycrate::__wl, std_crate = ::mycrate::__wls)]
fn blocking_body() { /* … */ }
```

`std_crate =` is needed only by the two expansions that mention the veneer:
`#[wasm_lite_test(worker)]` and `#[wasm_lite_bench]` on an `async fn`. It is a
separate argument rather than something derived from `crate =` because a wrapper
is free to re-export the two under unrelated names. `worker_doctest!` and
`async_doctest!` need nothing — they are `macro_rules!` and resolve through
`$crate` already.

`examples/reexport-demo` is the regression fixture: it renames both dependencies
so neither crate name is in its extern prelude, which turns any hard-coded path
that creeps back into a compile error.

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
