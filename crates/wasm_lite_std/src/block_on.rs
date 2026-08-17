// SPDX-License-Identifier: MIT OR Apache-2.0
//! Drive a future to completion on the calling thread.
//!
//! The blocking counterpart of `spawn_local` (wasm32 only, so not linked here),
//! and the native half of every test entry point this workspace generates: a
//! `#[wasm_lite_test] async fn` off wasm32, and the native arm of
//! [`async_doctest!`](crate::async_doctest).
//!
//! It is deliberately small. This is not a general-purpose runtime — there is no
//! task queue, no timer wheel, no reactor. It polls one future and sleeps
//! between polls, which is all a test entry point needs and all that can be
//! provided without the dependencies this crate does not take.

use std::future::Future;

/// Runs a future to completion, blocking the calling thread until it resolves.
///
/// # Blocking
///
/// The name is the warning. On wasm32 this blocks with `atomic.wait`, which
/// **traps on the browser main thread** — the same rule that governs
/// [`park`](crate::park), [`Mutex::lock_block`](crate::Mutex::lock_block) and
/// [`Receiver::recv_block`](crate::mpsc::Receiver::recv_block). Call it from a
/// worker (`#[wasm_lite_test(worker)]`, or inside a [`spawn`](crate::spawn)),
/// or use `spawn_local` to hand the future to the event loop instead. Off
/// wasm32 there is no such restriction.
///
/// # Examples
///
/// ```
/// # // A doctest runs on the browser main thread, where this would trap, so the
/// # // demonstration is off-wasm32. See "Blocking" above.
/// # #[cfg(not(target_arch = "wasm32"))] {
/// let answer = wasm_lite_std::block_on(async { 6 * 7 });
/// assert_eq!(answer, 42);
/// # }
/// ```
///
/// A future that never wakes blocks forever, exactly as a `recv` on a channel
/// nobody sends to would. In a test that surfaces as the harness timeout.
pub fn block_on<F: Future>(future: F) -> F::Output {
    imp::block_on(future)
}

/// Off wasm32: park the thread on a condvar between polls.
///
/// Polling in a spin loop would work too, and is what this crate did while
/// `block_on` was an internal helper. It burns a core per blocked future, which
/// `cargo test` multiplies by the number of test threads — enough to slow the
/// rest of the suite it is sharing the machine with.
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::{Arc, Condvar, Mutex};
    use std::task::{Context, Poll, Wake, Waker};

    /// A wake flag plus the condvar the polling thread waits on.
    ///
    /// The flag is *sticky*: `wake` sets it whether or not anyone is waiting
    /// yet, so a wake delivered from inside `poll` — before the thread reaches
    /// `wait` — is still there when it gets there. Without that, a future woken
    /// by its own poll would hang.
    struct Signal {
        woken: Mutex<bool>,
        condvar: Condvar,
    }

    impl Wake for Signal {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            // A panic in another thread must not turn a wake into a hang, and
            // this mutex guards one `bool` — there is no invariant a poisoned
            // lock could have left broken.
            let mut woken = self
                .woken
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *woken = true;
            self.condvar.notify_one();
        }
    }

    pub(super) fn block_on<F: Future>(future: F) -> F::Output {
        let signal = Arc::new(Signal {
            woken: Mutex::new(false),
            condvar: Condvar::new(),
        });
        let waker = Waker::from(Arc::clone(&signal));
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);

        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
            let mut woken = signal
                .woken
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while !*woken {
                woken = signal
                    .condvar
                    .wait(woken)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            *woken = false;
        }
    }
}

/// On wasm32: poll, then yield the thread, and repeat.
///
/// A condvar would be the better shape here too, but it would only work for a
/// future that reliably wakes. On this target a future is as likely to be
/// waiting on the *event loop* — a timer, a settled promise — which cannot run
/// while a worker sits in `atomic.wait` on a condvar nobody will notify. Yield
/// with a timeout instead, so the loop always makes progress; the waker is
/// therefore only a formality.
#[cfg(target_arch = "wasm32")]
mod imp {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    pub(super) fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = pin!(future);

        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
            // Traps on the browser main thread; see the note on `block_on`.
            crate::yield_now();
        }
    }
}
