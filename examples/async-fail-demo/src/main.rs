// Fail-closed async test that FAILS: an AWAITED worker panics, and the panic
// propagates (Err -> unwrap -> re-panic on the awaiter), failing the test — even
// though main returned cleanly. Build with nightly:  cargo +nightly run
fn main() {
    wasm_lite::set_panic_hook();
    wasm_lite_std::async_doctest!(async {
        // Worker panics; join_async yields Err; unwrap re-panics on this (awaiter) thread.
        let v = wasm_lite_std::spawn(|| -> u32 { panic!("kaboom inside the worker") })
            .join_async()
            .await
            .unwrap();
        wasm_lite::console::log(&format!("UNREACHABLE: got {v}"));
    });
}
