// SPDX-License-Identifier: MIT OR Apache-2.0
//! A minimal executor for this crate's own async tests.
//!
//! [`spawn`] blocks until a future completes: natively by driving it on the
//! calling thread with [`block_on`](crate::block_on), and on wasm32 by driving
//! it on a spawned worker (where `atomic.wait` is legal) and blocking on the
//! result.
//!
//! It exists because this crate cannot depend on `test_executors` — that crate
//! depends back on this one, and linking both copies collides on the
//! `#[no_mangle]` executor exports; see the note in the workspace `Cargo.toml`.
//! `#[doc(hidden)]`, and not part of the supported surface: [`block_on`] is the
//! public spelling of the native half.
//!
//! [`block_on`]: crate::block_on

use std::future::Future;

// Native-only: the wasm test path runs through `tests/browser.rs` via the
// wasm_lite runner (`#[wasm_lite::wasm_lite_test]`), so this emits nothing on
// wasm32 rather than depending on a wasm test harness.
#[macro_export]
macro_rules! async_test {
    (async fn $name:ident() $body:block) => {
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        fn $name() {
            $crate::test_executor::spawn(async $body)
        }
    };
}

/// Runs a future to completion, blocking the current thread until it's done.
///
/// Native-only helper for the `async_test!` macro. The wasm async test path goes
/// through the runner instead (`#[wasm_lite_test]` + `async_doctest!`), so this
/// is not compiled on wasm32.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn<F, T>(future: F) -> T
where
    F: Future<Output = T>,
    T: Send + 'static,
{
    crate::block_on(future)
}

/// WASM implementation that spawns a worker to run the future with proper event loop integration.
#[cfg(target_arch = "wasm32")]
pub fn spawn<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    use crate::mpsc::channel;

    let (tx, rx) = channel();

    // Spawn a worker thread and drive the future to completion synchronously
    // there. A worker is a real thread, so it can block on `atomic.wait` while
    // polling — no JS event-loop integration is needed for the sync path.
    crate::spawn(move || {
        let _ = tx.send_sync(crate::block_on(future));
    });

    // Block waiting for the result (uses atomic.wait in worker context)
    rx.recv_sync()
        .expect("worker thread panicked or was terminated")
}
