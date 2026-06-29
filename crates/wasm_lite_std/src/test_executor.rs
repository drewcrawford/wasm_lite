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
/// Native-only helper for the `async_test!` macro: it drives the future with a
/// simple polling loop and thread yielding. The wasm async test path goes
/// through the runner instead (`#[wasm_lite_test]` + `async_doctest!`), so this
/// is not compiled on wasm32.
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
