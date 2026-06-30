// SPDX-License-Identifier: MIT OR Apache-2.0
// Fail-closed async test that PASSES: the body awaits a worker and asserts.
// `async_test!` defers the verdict past `main`; the body signals success only on
// completion. Build with nightly:  cargo +nightly run
fn main() {
    wasm_lite::set_panic_hook();
    wasm_lite_std::async_doctest!(async {
        let v = wasm_lite_std::spawn(|| -> u32 { 21 + 21 })
            .join_async()
            .await
            .unwrap();
        assert_eq!(v, 42);
        wasm_lite::console::log("async-pass: asserted v == 42");
    });
}
