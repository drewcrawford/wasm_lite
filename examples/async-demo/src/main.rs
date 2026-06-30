// SPDX-License-Identifier: MIT OR Apache-2.0
// Async path: the main thread can't block (`atomic.wait` traps there), so it
// joins worker threads *asynchronously* via the wasm_lite_std event-loop
// executor. `spawn` runs work on Web Workers; `spawn_local` drives an async task
// on the main thread; `join_async().await` yields until each worker's result
// arrives — no blocking.
//
// Build with nightly:  cargo +nightly run

use std::sync::atomic::{AtomicU32, Ordering};

static DONE: AtomicU32 = AtomicU32::new(0);

#[wasm_lite::export]
pub fn done() -> u32 {
    DONE.load(Ordering::SeqCst)
}

fn main() {
    wasm_lite::console::log("async demo: main thread joins workers without blocking");

    // Spawn three workers, each returning a value.
    let handles: Vec<_> = (1..=3u32)
        .map(|i| wasm_lite_std::spawn(move || i * 10))
        .collect();

    // Drive an async task on the main thread's event loop.
    wasm_lite_std::spawn_local(async move {
        let mut sum = 0u32;
        for h in handles {
            let r = h.join_async().await.expect("worker panicked");
            wasm_lite::console::log(&format!("async joined a worker -> {r}"));
            sum += r;
        }
        wasm_lite::console::log(&format!("all joined; sum = {sum}"));
        DONE.store(sum, Ordering::SeqCst);
    });

    wasm_lite::console::log("main returning; results arrive via the event loop");
}
