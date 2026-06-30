// Spawns threads through the std-like `wasm_lite_std` API (ported from
// wasm_safe_thread), running on wasm_lite's worker primitive. Workers coordinate
// via atomics; the main thread can't join (Atomics.wait is forbidden there).
//
// Build with nightly:  cargo +nightly run

use std::sync::atomic::{AtomicU32, Ordering};

static SUM: AtomicU32 = AtomicU32::new(0);
static DONE: AtomicU32 = AtomicU32::new(0);

#[wasm_lite::export]
pub fn done_count() -> u32 {
    DONE.load(Ordering::SeqCst)
}
#[wasm_lite::export]
pub fn sum() -> u32 {
    SUM.load(Ordering::SeqCst)
}

fn main() {
    wasm_lite::console::log("std-threads demo: spawning via wasm_lite_std::spawn");
    for i in 0..4u32 {
        // `spawn` returns a JoinHandle; we coordinate via atomics instead of join.
        let _ = wasm_lite_std::spawn(move || {
            SUM.fetch_add(i + 1, Ordering::SeqCst);
            let d = DONE.fetch_add(1, Ordering::SeqCst) + 1;
            wasm_lite::console::log(&format!("std worker {i} ran ({d}/4 done)"));
            if d == 4 {
                wasm_lite::console::log(&format!(
                    "all std workers done; sum = {}",
                    SUM.load(Ordering::SeqCst)
                ));
            }
        });
    }
    wasm_lite::console::log("main: returning (workers run after we yield to the event loop)");
}
