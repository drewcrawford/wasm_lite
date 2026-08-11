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

impl std::fmt::Debug for SleepAsync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SleepAsync")
            .field("remaining", &self.remaining)
            .finish_non_exhaustive()
    }
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
    use crate::async_wait::{AsyncWait, AsyncWake};
    use crate::spinlock::Spinlock;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT_TOKEN: AtomicU32 = AtomicU32::new(1);
    static TIMERS: Spinlock<Vec<(u32, AsyncWake)>> = Spinlock::new(Vec::new());

    #[link(wasm_import_module = "__wasm_lite")]
    unsafe extern "C" {
        #[link_name = "__wl_timer_arm"]
        fn timer_arm(token: u32, delay_ms: f64);
        #[link_name = "__wl_timer_cancel"]
        fn timer_cancel(token: u32);
    }

    /// A host timer identified by an opaque integer in a Rust-owned registry.
    ///
    /// In a shared build the root realm owns the actual timeout; workers only
    /// proxy arm/cancel operations, so the timeout survives its creator worker
    /// exiting. Keeping pointers out of the JavaScript-visible token prevents a
    /// forged callback from becoming an arbitrary `Box::from_raw` in Rust.
    pub(super) struct Timer {
        wait: AsyncWait,
        token: u32,
    }

    impl Timer {
        pub(super) fn new(dur: Duration) -> Self {
            let (wake, wait) = waiter();
            let token = TIMERS.with_mut(|timers| {
                loop {
                    let candidate = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
                    if candidate != 0 && timers.iter().all(|(token, _)| *token != candidate) {
                        timers.push((candidate, wake));
                        break candidate;
                    }
                }
            });
            unsafe { timer_arm(token, dur.as_secs_f64() * 1000.0) };
            Timer { wait, token }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            unsafe { timer_cancel(self.token) };
        }
    }

    impl Future for Timer {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Pin::new(&mut self.wait).poll(cx).map(|_| ())
        }
    }

    /// Complete a host timer and wake its Rust future.
    #[doc(hidden)]
    #[unsafe(no_mangle)]
    pub extern "C" fn __wl_timer_fire(token: u32) {
        if let Some(wake) = take_timer(token) {
            wake.wake();
        }
    }

    /// Acknowledge cancellation and release the host-owned wake allocation.
    #[doc(hidden)]
    #[unsafe(no_mangle)]
    pub extern "C" fn __wl_timer_cancelled(token: u32) {
        drop(take_timer(token));
    }

    fn take_timer(token: u32) -> Option<AsyncWake> {
        TIMERS.with_mut(|timers| {
            timers
                .iter()
                .position(|(candidate, _)| *candidate == token)
                .map(|position| timers.swap_remove(position).1)
        })
    }

    // The exports are invoked only by generated JavaScript, so keep them even
    // when Rust's call graph has no references to them.
    #[used]
    static KEEP_TIMER_FIRE: extern "C" fn(u32) = __wl_timer_fire;
    #[used]
    static KEEP_TIMER_CANCELLED: extern "C" fn(u32) = __wl_timer_cancelled;
}

#[cfg(not(target_arch = "wasm32"))]
mod native_impl {
    use super::*;
    use crate::async_wait::{AsyncWait, AsyncWake};
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, HashMap};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::time::Instant;

    pub(super) struct Timer {
        wait: AsyncWait,
        id: u64,
    }

    impl Timer {
        pub(super) fn new(dur: Duration) -> Self {
            let (wake, wait) = waiter();
            let id = driver().arm(dur, wake);
            Timer { wait, id }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            driver().cancel(self.id);
        }
    }

    impl Future for Timer {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Pin::new(&mut self.wait).poll(cx).map(|_| ())
        }
    }

    struct Driver {
        next_id: AtomicU64,
        shared: Arc<(Mutex<Queue>, Condvar)>,
    }

    #[derive(Default)]
    struct Queue {
        deadlines: BinaryHeap<Reverse<(Instant, u64)>>,
        pending: HashMap<u64, AsyncWake>,
    }

    fn driver() -> &'static Driver {
        static DRIVER: OnceLock<Driver> = OnceLock::new();
        DRIVER.get_or_init(Driver::new)
    }

    impl Driver {
        fn new() -> Self {
            let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
            let thread_shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("wasm_lite_std timer driver".to_owned())
                .spawn(move || run(thread_shared))
                .expect("failed to spawn timer driver");
            Driver {
                next_id: AtomicU64::new(1),
                shared,
            }
        }

        fn arm(&self, dur: Duration, wake: AsyncWake) -> u64 {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let deadline = Instant::now() + dur;
            let (queue, changed) = &*self.shared;
            let mut queue = queue.lock().expect("timer driver lock poisoned");
            queue.pending.insert(id, wake);
            queue.deadlines.push(Reverse((deadline, id)));
            changed.notify_one();
            id
        }

        fn cancel(&self, id: u64) {
            let (queue, changed) = &*self.shared;
            let removed = queue
                .lock()
                .expect("timer driver lock poisoned")
                .pending
                .remove(&id)
                .is_some();
            if removed {
                changed.notify_one();
            }
        }
    }

    fn run(shared: Arc<(Mutex<Queue>, Condvar)>) {
        let (mutex, changed) = &*shared;
        let mut queue = mutex.lock().expect("timer driver lock poisoned");
        loop {
            while let Some(Reverse((_, id))) = queue.deadlines.peek() {
                if queue.pending.contains_key(id) {
                    break;
                }
                queue.deadlines.pop();
            }

            let Some(Reverse((deadline, id))) = queue.deadlines.peek().copied() else {
                queue = changed.wait(queue).expect("timer driver lock poisoned");
                continue;
            };

            let now = Instant::now();
            if deadline > now {
                let (next, _) = changed
                    .wait_timeout(queue, deadline - now)
                    .expect("timer driver lock poisoned");
                queue = next;
                continue;
            }

            queue.deadlines.pop();
            if let Some(wake) = queue.pending.remove(&id) {
                drop(queue);
                wake.wake();
                queue = mutex.lock().expect("timer driver lock poisoned");
            }
        }
    }
}
