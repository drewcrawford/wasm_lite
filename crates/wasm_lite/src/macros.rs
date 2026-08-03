// SPDX-License-Identifier: MIT OR Apache-2.0
//! Small declarative macros.
//!
//! `import!` used to live here as a `macro_rules!` tt-muncher; it is now a
//! proc-macro in `wasm_lite_macro` (re-exported from the crate root). What
//! remains is the tiny `test_main!` helper.

/// Supply the entry point for a `harness = false` test target.
///
/// `harness = false` test binaries need a `fn main`, but the runner drives each
/// test via its `#[wasm_lite_test]`-generated export, so `main` is a no-op. Call
/// once per test file alongside your [`wasm_lite_test`](crate::wasm_lite_test)
/// functions.
///
/// [`wasm_lite_test`]: crate::wasm_lite_test
#[macro_export]
macro_rules! test_main {
    () => {
        fn main() {}
    };
}

/// Supply the entry point for a `harness = false` bench target.
///
/// The benchmark counterpart of [`test_main!`](crate::test_main): the runner
/// drives each benchmark through its `#[wasm_lite_bench]`-generated export, so
/// `main` has nothing to do.
///
/// A `[[bench]]` target needs `harness = false` in `Cargo.toml`, exactly as a
/// test target does — libtest's own bench harness would otherwise claim `main`.
#[macro_export]
macro_rules! bench_main {
    () => {
        fn main() {}
    };
}
