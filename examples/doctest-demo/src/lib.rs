// SPDX-License-Identifier: MIT OR Apache-2.0
//! A crate whose doctests run in the browser via the wasm_lite runner.
//!
//! Calling [`wasm_lite::set_panic_hook`] at the top of a doctest makes failures
//! report the panic message (rustdoc owns the doctest's `main`, so the hook is
//! not installed automatically as it is for `#[wasm_lite_test]`).

/// Doubles a number.
///
/// ```
/// wasm_lite::set_panic_hook();
/// assert_eq!(doctest_demo::double(2), 4);
/// ```
pub fn double(x: i32) -> i32 {
    x * 2
}

/// Logs a greeting via the browser console.
///
/// ```
/// wasm_lite::set_panic_hook();
/// doctest_demo::greet("world");
/// ```
pub fn greet(name: &str) {
    wasm_lite::console::log(&format!("hello, {name}"));
}
