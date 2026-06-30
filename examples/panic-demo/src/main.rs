// SPDX-License-Identifier: MIT OR Apache-2.0
// A panic only traps its own worker — main and other workers keep running, and
// shared memory persists. The danger is a SILENT panic on a detached worker, so
// wasm_lite_std's panic hook always logs to the console with thread attribution
// (the default wasm32 panic prints nothing). Here a never-joined worker panics
// while three siblings run unaffected.
//
// Build with nightly:  cargo +nightly run

use std::sync::atomic::{AtomicU32, Ordering};

static SURVIVORS: AtomicU32 = AtomicU32::new(0);

#[wasm_lite::export]
pub fn survivors() -> u32 {
    SURVIVORS.load(Ordering::SeqCst)
}

fn main() {
    wasm_lite::console::log("panic demo: a detached worker panics; the program keeps running");

    // Detached: we never join it, so the message can only reach us via the console.
    let _ = wasm_lite_std::spawn(|| {
        panic!("intentional panic in a detached worker");
    });

    // Siblings are unaffected (separate instances; shared memory intact).
    for i in 0..3u32 {
        let _ = wasm_lite_std::spawn(move || {
            SURVIVORS.fetch_add(1, Ordering::SeqCst);
            wasm_lite::console::log(&format!("survivor worker {i} ran"));
        });
    }

    wasm_lite::console::log("main: returning; survivors run, the panic is logged (not silent)");
}
