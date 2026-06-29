// Async Mutex across threads: a worker holds the lock and mutates the value;
// the main thread acquires it with `lock_async().await` — which must wait for the
// worker's release and is woken *cross-thread* (the worker's drop notifies the
// main realm's event-loop executor). The main thread never blocks.
//
// Build with nightly:  cargo +nightly run

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use wasm_lite_std::Mutex;

/// Shared across threads (const `Mutex::new`, so it lives at a static address).
static M: Mutex<u32> = Mutex::new(0);
/// Set once the worker has acquired the lock, so main waits to contend.
static HOLDING: AtomicU32 = AtomicU32::new(0);
/// Final value the main thread read under the async lock.
static FINAL: AtomicU32 = AtomicU32::new(0);

#[wasm_lite::export]
pub fn final_value() -> u32 {
    FINAL.load(Ordering::SeqCst)
}

fn main() {
    wasm_lite::console::log("async-mutex demo: main acquires a worker-held lock without blocking");

    // Worker: take the lock, set 100, hold ~50ms, add 23, then release on drop.
    wasm_lite_std::spawn(|| {
        let mut g = M.lock_sync();
        *g = 100;
        HOLDING.store(1, Ordering::SeqCst);
        wasm_lite::console::log("worker holds the lock; sleeping");
        wasm_lite_std::sleep(Duration::from_millis(50));
        *g += 23;
        wasm_lite::console::log("worker releasing the lock");
        // guard drops here -> releases -> wakes the main thread's async waiter
    });

    // Main: wait (async) until the worker holds it, then contend with lock_async.
    wasm_lite_std::spawn_local(async {
        while HOLDING.load(Ordering::SeqCst) == 0 {
            wasm_lite_std::yield_to_event_loop_async().await;
        }
        let g = M.lock_async().await; // blocks until the worker releases
        FINAL.store(*g, Ordering::SeqCst);
        wasm_lite::console::log(&format!("main acquired via lock_async; value = {}", *g));
    });

    wasm_lite::console::log("main returning; the lock arrives via the event loop");
}
