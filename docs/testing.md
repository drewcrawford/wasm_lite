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
runner = "path/to/runner"
```

Then `cargo test` runs headless and exits, while `cargo run` serves a bin
interactively in the browser — the runner distinguishes them by path.

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
fail-closed `spawn` + `join_async` wrapper:

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

`crates/wasm_lite_std/tests/browser.rs` (a `harness = false` target) exercises
`Mutex`/`RwLock`/`Condvar`/`mpsc`/`time` across the spin/block/sync/async variants
(including timeouts) and `spawn`/`join_async` in a real browser — blocking
variants via `(worker)` tests. Run it with:

```bash
crates/wasm_lite_std/run-browser-tests.sh
```

It requires **nightly** (atomics ⇒ recompiled `std`) and a WebDriver browser; pass
`--no-run` to just build.
