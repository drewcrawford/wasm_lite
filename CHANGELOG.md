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

