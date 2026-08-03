// SPDX-License-Identifier: MIT OR Apache-2.0
//! Non-blocking sleep.
//!
//! [`sleep`](crate::sleep) parks the calling thread in `Atomics.wait`, which the
//! browser main thread forbids. Anything scheduling work for later — an executor
//! honouring a `poll_after`, a retry backoff, an animation delay — needs to wait
//! *without* holding the thread, and that is what this is.

use crate::async_wait::waiter;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// Sleep for `dur` without blocking the calling thread.
///
/// Safe on the browser main thread, unlike [`sleep`](crate::sleep).
///
/// ```
/// # wasm_lite_std::async_doctest!(async {
/// use std::time::Duration;
/// wasm_lite_std::sleep_async(Duration::from_millis(10)).await;
/// # });
/// ```
///
/// Dropping the future cancels the timer, so an abandoned sleep does not keep a
/// callback alive to fire into nothing.
///
/// Resolution is the platform's: browsers clamp nested `setTimeout` to ~4 ms and
/// throttle background tabs much harder, so treat the duration as a floor rather
/// than a schedule.
pub fn sleep_async(dur: Duration) -> SleepAsync {
    SleepAsync::new(dur)
}

/// The future returned by [`sleep_async`].
pub struct SleepAsync {
    #[cfg(target_arch = "wasm32")]
    inner: wasm_impl::Timer,
    #[cfg(not(target_arch = "wasm32"))]
    inner: native_impl::Timer,
}

impl SleepAsync {
    fn new(dur: Duration) -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            SleepAsync {
                inner: wasm_impl::Timer::new(dur),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            SleepAsync {
                inner: native_impl::Timer::new(dur),
            }
        }
    }
}

impl Future for SleepAsync {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx)
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use super::*;
    use crate::async_wait::AsyncWait;
    use wasm_lite::Closure;

    /// A `setTimeout` whose callback wakes an async waiter.
    ///
    /// The `Closure` is owned by the timer rather than forgotten, so dropping
    /// the future drops the closure *and* clears the timeout — no callback
    /// firing into a future that is gone.
    pub(super) struct Timer {
        wait: AsyncWait,
        id: f64,
        // Kept alive for as long as the timer can fire.
        _cb: Closure,
    }

    impl Timer {
        pub(super) fn new(dur: Duration) -> Self {
            let (wake, wait) = waiter();
            // `Closure::new` takes FnMut, and a one-shot wake is FnOnce; an
            // Option makes it callable more than once without being wrong.
            let mut wake = Some(wake);
            let cb = Closure::new(move || {
                if let Some(w) = wake.take() {
                    w.wake();
                }
            });
            let id = wasm_lite::timer::set_timeout(cb.as_js_value(), dur.as_secs_f64() * 1000.0);
            Timer { wait, id, _cb: cb }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            wasm_lite::timer::clear_timeout(self.id);
        }
    }

    impl Future for Timer {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Pin::new(&mut self.wait).poll(cx).map(|_| ())
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native_impl {
    use super::*;
    use crate::async_wait::AsyncWait;

    /// A thread that sleeps and then wakes the waiter.
    ///
    /// Native has no event loop to hang a timer on, and this mirrors what the
    /// rest of `wasm_lite_std` does for native timeouts.
    pub(super) struct Timer {
        wait: AsyncWait,
    }

    impl Timer {
        pub(super) fn new(dur: Duration) -> Self {
            let (wake, wait) = waiter();
            std::thread::spawn(move || {
                std::thread::sleep(dur);
                wake.wake();
            });
            Timer { wait }
        }
    }

    impl Future for Timer {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Pin::new(&mut self.wait).poll(cx).map(|_| ())
        }
    }
}
