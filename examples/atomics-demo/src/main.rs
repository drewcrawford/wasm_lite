// SPDX-License-Identifier: MIT OR Apache-2.0
// Demonstrates a shared-memory `+atomics` build: linear memory is a
// SharedArrayBuffer (created by JS and imported), and Rust uses real atomic
// instructions and thread-local storage. This is the single-threaded
// foundation — the same module/memory can later be handed to Web Workers to
// run these atomics across threads.
//
// Build with nightly (see `.cargo/config.toml`):  cargo +nightly run

use core::sync::atomic::{AtomicU32, Ordering};

// A global counter in shared linear memory, mutated with atomic instructions.
static COUNTER: AtomicU32 = AtomicU32::new(0);

// Thread-local storage: each thread (worker) gets its own copy. On the main
// thread the runtime's start function sets up TLS automatically at instantiation.
thread_local! {
    static LOCAL_HITS: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

/// Atomically add `n` to the shared counter and return the new value. Callable
/// from JS — successive calls accumulate, proving the memory is live and shared.
#[wasm_lite::export]
pub fn bump(n: u32) -> u32 {
    LOCAL_HITS.with(|c| c.set(c.get() + 1));
    COUNTER.fetch_add(n, Ordering::SeqCst) + n
}

/// How many times *this thread* called `bump` (read from thread-local storage).
#[wasm_lite::export]
pub fn local_hits() -> u32 {
    LOCAL_HITS.with(|c| c.get())
}

fn main() {
    wasm_lite::console::log("atomics demo — shared memory + atomic ops");

    // Atomic read-modify-write.
    COUNTER.store(10, Ordering::SeqCst);
    let prev = COUNTER.fetch_add(5, Ordering::SeqCst);
    wasm_lite::console::log(&format!(
        "fetch_add(5) returned old value {prev}, now {}",
        COUNTER.load(Ordering::SeqCst)
    ));

    // Atomic compare-and-swap.
    match COUNTER.compare_exchange(15, 100, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(old) => wasm_lite::console::log(&format!("compare_exchange(15 -> 100) ok, was {old}")),
        Err(cur) => wasm_lite::console::log(&format!("compare_exchange failed, current is {cur}")),
    }

    // Drive the atomic counter through the same exported entry point JS uses,
    // which also bumps the thread-local hit count.
    let after = bump(7);
    wasm_lite::console::log(&format!("bump(7) -> {after}"));

    // Thread-local storage on the main thread: counts this thread's bump calls.
    wasm_lite::console::log(&format!("thread-local local_hits() = {}", local_hits()));

    wasm_lite::console::log("atomics demo done");
}
