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
