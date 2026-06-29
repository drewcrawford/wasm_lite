//! Async doctests run fail-closed in the browser via the wasm_lite runner.

/// Doubles a number — with an **async** doctest that joins a worker.
///
/// ```
/// wasm_lite::set_panic_hook();
/// wasm_lite_std::async_doctest!(async {
///     let v = wasm_lite_std::spawn(|| 2 + 2).join_async().await.unwrap();
///     assert_eq!(v, 4);
///     assert_eq!(async_doctest_demo::double(v), 8);
/// });
/// ```
pub fn double(x: i32) -> i32 {
    x * 2
}
