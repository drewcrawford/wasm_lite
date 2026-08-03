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
///
/// Sleeps longer than [`MAX_TIMEOUT`] are chained across several timeouts,
/// because a browser truncates the delay to a signed 32-bit integer — a naive
/// 30-day `setTimeout` fires *immediately*, which is the opposite of what was
/// asked for.
pub fn sleep_async(dur: Duration) -> SleepAsync {
    SleepAsync::new(dur)
}

/// The longest delay a browser will honour: `setTimeout` truncates to `i32`.
///
/// Beyond this the delay wraps or clamps and the timer fires at once, so
/// [`sleep_async`] chains instead.
pub const MAX_TIMEOUT: Duration = Duration::from_millis(i32::MAX as u64);

/// The future returned by [`sleep_async`].
pub struct SleepAsync {
    #[cfg(target_arch = "wasm32")]
    inner: wasm_impl::Timer,
    #[cfg(not(target_arch = "wasm32"))]
    inner: native_impl::Timer,
    /// Still to sleep after `inner` fires; non-zero only past [`MAX_TIMEOUT`].
    remaining: Duration,
}

#[cfg(target_arch = "wasm32")]
fn new_timer(dur: Duration) -> wasm_impl::Timer {
    wasm_impl::Timer::new(dur)
}

#[cfg(not(target_arch = "wasm32"))]
fn new_timer(dur: Duration) -> native_impl::Timer {
    native_impl::Timer::new(dur)
}

impl SleepAsync {
    fn new(dur: Duration) -> Self {
        let first = dur.min(MAX_TIMEOUT);
        SleepAsync {
            inner: new_timer(first),
            remaining: dur - first,
        }
    }
}

impl Future for SleepAsync {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match Pin::new(&mut self.inner).poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(()) => {
                    if self.remaining.is_zero() {
                        return Poll::Ready(());
                    }
                    // Another leg of a sleep too long for one timeout.
                    let next = self.remaining.min(MAX_TIMEOUT);
                    self.remaining -= next;
                    self.inner = new_timer(next);
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use super::*;
    use crate::async_wait::AsyncWait;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use wasm_lite::Closure;

    thread_local! {
        /// Closures for timers that have not fired, by id.
        ///
        /// The closure cannot live in the future itself: `Closure` wraps a
        /// realm-bound handle and is `!Send`, and a sleep that is not `Send` is
        /// most of the use gone — an executor cannot move the task. Parking it
        /// here keeps the future `Send` while still owning the closure, so a
        /// cancelled sleep drops it instead of leaking.
        static PENDING: RefCell<HashMap<u64, Closure>> = RefCell::new(HashMap::new());
        static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// A `setTimeout` whose callback wakes an async waiter.
    pub(super) struct Timer {
        wait: AsyncWait,
        js_id: f64,
        key: u64,
        /// The thread that armed it. Clearing a timeout or dropping a `Closure`
        /// is only meaningful in the realm that created it.
        owner: crate::ThreadId,
    }

    impl Timer {
        pub(super) fn new(dur: Duration) -> Self {
            let (wake, wait) = waiter();
            let key = NEXT_ID.with(|n| {
                let k = n.get();
                n.set(k + 1);
                k
            });
            // `Closure::new` takes FnMut; a one-shot wake is FnOnce, so an
            // Option makes it callable more than once without being wrong.
            let mut wake = Some(wake);
            let cb = Closure::new(move || {
                if let Some(w) = wake.take() {
                    w.wake();
                }
                // Fired: the closure is no longer needed.
                PENDING.with(|p| {
                    p.borrow_mut().remove(&key);
                });
            });
            let js_id = wasm_lite::timer::set_timeout(cb.as_js_value(), dur.as_secs_f64() * 1000.0);
            PENDING.with(|p| {
                p.borrow_mut().insert(key, cb);
            });
            Timer {
                wait,
                js_id,
                key,
                owner: crate::current().id(),
            }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            // Off-thread drop: the timer and its closure belong to another
            // realm, where neither `clearTimeout` nor dropping the handle means
            // anything from here. Leave them; the callback will fire, find its
            // waiter gone, and clean itself up.
            if crate::current().id() != self.owner {
                return;
            }
            wasm_lite::timer::clear_timeout(self.js_id);
            PENDING.with(|p| {
                p.borrow_mut().remove(&self.key);
            });
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
