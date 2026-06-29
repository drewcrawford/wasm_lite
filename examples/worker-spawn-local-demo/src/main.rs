// A WORKER that itself calls spawn_local. Before drain-before-teardown the worker
// closed the instant its closure returned, dropping the async task and freeing
// the TLS its queue lived in. Now the worker stays alive until its executor
// drains. The test passes only if the worker's async task actually ran to
// completion (otherwise DONE stays 0 and the fail-closed test times out).
//
// Build with nightly:  cargo +nightly run
use std::sync::atomic::{AtomicU32, Ordering};

static DONE: AtomicU32 = AtomicU32::new(0);

fn main() {
    wasm_lite::set_panic_hook();

    // Worker spawns async work, then its closure returns immediately.
    wasm_lite_std::spawn(|| {
        wasm_lite_std::spawn_local(async {
            for _ in 0..3 {
                wasm_lite_std::yield_to_event_loop_async().await; // needs the executor to keep running
            }
            DONE.store(99, Ordering::SeqCst);
            wasm_lite::console::log("worker's spawn_local task completed");
        });
    });

    // Fail-closed: only passes if the worker drained its task (DONE becomes 99).
    wasm_lite_std::async_doctest!(async {
        while DONE.load(Ordering::SeqCst) == 0 {
            wasm_lite_std::yield_to_event_loop_async().await;
        }
        assert_eq!(DONE.load(Ordering::SeqCst), 99);
        wasm_lite::console::log("main observed the worker's task result");
    });
}
