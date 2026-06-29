// Does a panic inside an AWAITED future propagate to the awaiter? It does: the
// worker's panic is delivered to `join_async().await` as `Err(message)` (the
// panic hook sends it through the channel before the worker aborts). A wrapper
// that returns `T` (not `Result`) would `.unwrap()` this and re-panic on the
// awaiter — failing the test, like `std`/`tokio`.
//
// Build with nightly:  cargo +nightly run

fn main() {
    wasm_lite_std::spawn_local(async {
        // Worker computes a u32 but panics instead.
        let handle = wasm_lite_std::spawn(|| -> u32 { panic!("kaboom inside the worker") });
        let r: Result<u32, Box<String>> = handle.join_async().await;
        match r {
            Ok(v) => wasm_lite::console::log(&format!("UNEXPECTED: got Ok({v})")),
            Err(e) => wasm_lite::console::log(&format!("join_async propagated the panic as Err: {e}")),
        }
    });
    wasm_lite::console::log("main: returning; the awaited result arrives via the event loop");
}
