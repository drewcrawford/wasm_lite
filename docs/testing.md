# Testing

*(Part of the [wasm_lite](../README.md) docs. See also: [binding model](./binding-model.md),
[threads & async](./threads-and-async.md), [interop](./interop.md),
[roadmap](./roadmap.md), [migration guide](../MIGRATION.md).)*

```toml
# .cargo/config.toml
[target.wasm32-unknown-unknown]
runner = "path/to/runner"
```

* `#[wasm_lite_test]` marks a test; it is recorded in `__wasm_lite_tests` and
  the runner discovers and drives each one in a browser (pass / fail / panic).
  By default the body runs on the **main thread**; `#[wasm_lite_test(worker)]`
  runs it on a dedicated Web Worker instead (a fail-closed `spawn` + `join_async`
  wrapper) so blocking APIs (`lock_block`, `recv_block`, `park`, synchronous
  `join`) — which trap on the main thread — can be tested.
* Tests can live in normal Rust test modules. For migration from native
  `#[test]` or `wasm-bindgen-test`, use `cfg_attr` so the same function is a
  native test on non-wasm targets and a browser-driven wasm_lite test on wasm:
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
  The runner uses Rust-style module paths such as
  `my_crate::tests::test_continue`, so tests with the same leaf name in
  different modules can coexist.
* Harness-less integration tests are still supported for standalone wasm suites:
  set `harness = false` on that `[[test]]` target and call
  `wasm_lite::test_main!()` once in the file.
* Plain `cargo run --example foo` serves the bin interactively in the browser;
  `cargo test` runs headless and exits — the runner distinguishes them by path.
* Doctests run too (rustdoc's doctest binaries are detected and driven headless).
  Call `wasm_lite::set_panic_hook()` at the top of a doctest so failures report
  the panic message.
* **Async / threaded code** is tested fail-closed with `wasm_lite_std::async_doctest!`
  (in a `#[wasm_lite_test]` body or a doctest). `wasm_lite_std`'s own browser suite
  (`crates/wasm_lite_std/tests/browser.rs`, a `harness = false` target) exercises
  `Mutex`/`RwLock`/`Condvar`/`mpsc`/`time` across
  the spin/block/sync/async variants (including timeouts) and `spawn`/`join_async`
  in a real browser — blocking variants via `(worker)` tests; run it with
  `crates/wasm_lite_std/run-browser-tests.sh`.
