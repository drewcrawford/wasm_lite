// Spawns real worker threads over wasm_lite's shared memory and coordinates
// them with atomics. Each worker adds its index to a shared sum; the last one
// to finish logs the total. The browser main thread cannot block
// (`Atomics.wait` is forbidden there), so it does not join — it spawns and
// returns, and JS (or the workers themselves) observe completion via atomics.
//
// Build with nightly (see `.cargo/config.toml`):  cargo +nightly run

use core::sync::atomic::{AtomicU32, Ordering};
use wasm_lite::thread;

/// Number of worker threads to spawn.
const N: u32 = 4;

/// Shared accumulator: each worker adds (index + 1), so the total is 1+2+3+4 = 10.
static SUM: AtomicU32 = AtomicU32::new(0);
/// Count of workers that have finished.
static DONE: AtomicU32 = AtomicU32::new(0);

/// Read by JS to poll for completion (main thread can't block on the workers).
#[wasm_lite::export]
pub fn done_count() -> u32 {
    DONE.load(Ordering::SeqCst)
}

/// Read by JS once `done_count() == N` to check the accumulated result.
#[wasm_lite::export]
pub fn sum() -> u32 {
    SUM.load(Ordering::SeqCst)
}

fn main() {
    wasm_lite::console::log(&format!("threads demo: spawning {N} workers"));

    for i in 0..N {
        thread::spawn(move || {
            // Runs on its own worker, on the shared memory.
            SUM.fetch_add(i + 1, Ordering::SeqCst);
            let finished = DONE.fetch_add(1, Ordering::SeqCst) + 1;
            wasm_lite::console::log(&format!("worker {i} ran; {finished}/{N} done"));
            if finished == N {
                wasm_lite::console::log(&format!("all workers done; sum = {}", SUM.load(Ordering::SeqCst)));
            }
        });
    }

    wasm_lite::console::log("main: spawned all workers, returning (no join on main thread)");
}
