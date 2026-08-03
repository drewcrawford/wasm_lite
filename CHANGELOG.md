# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- Benchmarking: `#[wasm_lite_bench]`, `Bencher`, and `bench_main!`, driven by the
  runner one page load per benchmark and reported in `cargo bench`'s format.
  `cargo bench` measures; `cargo test --benches` runs each once without
  measuring. Timing is batch-calibrated because `performance.now()` is coarsened
  to 5 µs even under cross-origin isolation — see
  [docs/testing.md](docs/testing.md#benchmark-in-a-browser).
- `wasm_lite::performance::time_origin()` — `performance.timeOrigin`.
- Runner environment variables, now documented in
  [docs/testing.md](docs/testing.md#configure-the-runner): `WASM_LITE_GPU` (a
  real WebGPU adapter in headless Chrome), `WASM_LITE_BROWSER_ARGS`,
  `WASM_LITE_SERVE_DIR` (serve a directory alongside the program; confined to
  that directory — `..`, absolute components and symlinks pointing outside are
  all refused, and the generated routes always win),
  `WASM_LITE_TIMEOUT_SECS`, and `WASM_LITE_RUN_SECONDS` (watch a long-running
  `bin`).
- The `wasm_lite_std` browser suite now runs in **Chrome as well as Firefox**
  (`scripts/wasm32/tests`), with `a_worker_can_spawn_a_worker` and
  `nesting_composes_to_a_third_level` covering nested worker spawn.

### Fixed

- **`Instant` was not comparable across threads.** It was raw
  `performance.now()`, which is per-realm — a Web Worker's zero is the moment
  that worker started, so an `Instant` taken on one thread and read on another
  was out by however long the page had been up. `Instant` is `Ord` and deadlines
  cross threads constantly (`join_async`, every `*_timeout`, an executor's
  `poll_after`), so this affected anything threaded. Now
  `performance.timeOrigin + performance.now()`.
- **A Web Worker could not spawn a Web Worker in Chrome.** The spawn returned a
  handle, nothing reported an error, and the thread never ran; joining it blocked
  forever. Chrome fetches a nested worker's module script through its parent, and
  a parent in `Atomics.wait` — which is every blocking primitive — never services
  it. Worker *creation* is now delegated to the main thread. Firefox was
  unaffected, which is why a Firefox-only CI stayed green.
- A spawned worker that fails to start now reports through `onerror` /
  `onmessageerror` instead of failing silently, and a worker bootstrap that
  throws before the closure runs reports through `error` / `unhandledrejection`
  rather than becoming an invisible unhandled rejection.
- A timeout now prints the console the program captured before it hung, on all
  three run shapes (`bin`, test suite, bench suite). Previously it printed only
  "timed out".
- Three held-lock timeout tests raced: a holder that kept the lock for a fixed
  duration while the other side asserted a short attempt failed. They now hold
  until told to release.

## 0.1.0 - 2026-06-30

Initial release.

### Added

- `wasm_lite`, a dependency-light Rust/JavaScript binding system for `wasm32-unknown-unknown`.
- Descriptor-based import, export, class, and test metadata emitted into custom wasm sections.
- Host-side `wasm_lite_codegen` for dependency-free descriptor parsing and generated ES module glue.
- `wasm-lite` CLI and browser runner support for `cargo run`, `cargo test`, and rustdoc doctests through WebDriver.
- Core ABI support for strings, byte slices, vectors, `JsValue` handles, `Option`, `Result`, and sret payloads.
- Proc-macro support for `import!`, `#[export]`, `#[wasm_lite_test]`, and `js_class!`.
- Browser-oriented exports, imports, doctests, test suites, panic reporting, and interop examples.
- Threading, atomics, async execution, worker bootstrap, and `wasm_lite_std` synchronization/time APIs.
- CI, formatting, clippy, docs, and wasm test scripts for release validation.

### Changed

- Moved macro parsing onto a unified `syn`/`quote` build-time implementation while keeping runtime crates dependency-free.
- Improved documentation for the binding model, testing flow, threading/async behavior, interop, and migration story.

