// SPDX-License-Identifier: MIT OR Apache-2.0
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
/// # Platform behavior
///
/// - **Native**: Uses a simple polling loop with thread yielding.
/// - **WASM (in worker context)**: Spawns a child worker that runs the future using
///   `wasm_bindgen_futures::spawn_local`, which properly integrates with the JS event loop.
///   The calling thread blocks on `Atomics.wait` until the result is ready.
///
/// # Requirements
///
/// On WASM, this function requires:
/// - Being called from a worker thread (not the main browser thread), since it uses `Atomics.wait`
/// - The future and its output must be `Send + 'static`
///
/// On wasm, ensure such code runs in a worker context (not the main browser
/// thread), since it relies on `Atomics.wait`.
///
/// # Panics
///
/// On WASM main thread, this will spin forever waiting for the result (same as before),
/// because `Atomics.wait` is not available there.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn<F, T>(future: F) -> T
where
    F: Future<Output = T>,
    T: Send + 'static,
{
    use std::pin::pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    static NOOP_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(std::ptr::null(), &NOOP_WAKER_VTABLE),
        |_| (),
        |_| (),
        |_| (),
    );

    let mut f = pin!(future);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &NOOP_WAKER_VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    loop {
        match f.as_mut().poll(&mut cx) {
            Poll::Pending => {
                std::thread::yield_now();
            }
            Poll::Ready(r) => return r,
        }
    }
}

/// WASM implementation that spawns a worker to run the future with proper event loop integration.
#[cfg(target_arch = "wasm32")]
pub fn spawn<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    use crate::mpsc::channel;
    use std::pin::pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    static NOOP_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(std::ptr::null(), &NOOP_WAKER_VTABLE),
        |_| (),
        |_| (),
        |_| (),
    );

    let (tx, rx) = channel();

    // Spawn a worker thread and drive the future to completion synchronously
    // there. A worker is a real thread, so it can block on `atomic.wait` while
    // polling — no JS event-loop integration is needed for the sync path.
    crate::spawn(move || {
        let mut f = pin!(future);
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &NOOP_WAKER_VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let result = loop {
            match f.as_mut().poll(&mut cx) {
                Poll::Pending => crate::yield_now(),
                Poll::Ready(r) => break r,
            }
        };
        let _ = tx.send_sync(result);
    });

    // Block waiting for the result (uses atomic.wait in worker context)
    rx.recv_sync()
        .expect("worker thread panicked or was terminated")
}
