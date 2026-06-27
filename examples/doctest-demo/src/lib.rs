//! A crate whose doctests run in the browser via the wasm_lite runner.

/// Doubles a number.
///
/// ```
/// assert_eq!(doctest_demo::double(2), 4);
/// ```
pub fn double(x: i32) -> i32 {
    x * 2
}

/// Logs a greeting via the browser console.
///
/// ```
/// doctest_demo::greet("world");
/// ```
pub fn greet(name: &str) {
    wasm_lite::console::log(&format!("hello, {name}"));
}
