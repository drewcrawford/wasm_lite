// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{Mutex, NotAvailable};
use crate::async_wait::waiter;
use crate::guard::Guard;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

#[cfg(target_arch = "wasm32")]
use crate as thread;
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use crate::time::Instant;

pub(crate) async fn lock_async<T>(mutex: &Mutex<T>) -> Guard<'_, T> {
    loop {
        let a = mutex.waiting_async_threads.with_mut(|senders| {
            match mutex.try_lock() {
                Ok(guard) => Ok(guard),
                Err(NotAvailable) => {
                    // Create a new channel to signal when the lock is available
                    let (sender, receiver) = waiter();
                    senders.push(sender);
                    Err(receiver)
                }
            }
        });
        match a {
            Ok(guard) => return guard,
            Err(receiver) => {
                // Wait for the signal that the lock is available
                receiver.await;
            }
        }
    }
}

pub(crate) async fn lock_async_timeout<T>(
    mutex: &Mutex<T>,
    deadline: Instant,
) -> Option<Guard<'_, T>> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            // Try one last time
            if let Ok(guard) = mutex.try_lock() {
                return Some(guard);
            }
            return None;
        }

        let a = mutex.waiting_async_threads.with_mut(|senders| {
            match mutex.try_lock() {
                Ok(guard) => Ok(guard),
                Err(NotAvailable) => {
                    // Create a new channel to signal when the lock is available
                    let (sender, receiver) = waiter();
                    senders.push(sender);
                    Err(receiver)
                }
            }
        });

        match a {
            Ok(guard) => return Some(guard),
            Err(receiver) => {
                // Create a channel for timeout
                let (timeout_sender, timeout_receiver) = waiter();

                // Compute the remaining time on the calling thread: `Instant`s
                // are not comparable across threads on wasm (each worker's
                // clock starts at its own time origin), so the spawned thread
                // must receive a `Duration`, never the deadline itself.
                let timeout = deadline - Instant::now();

                // Spawn a thread to handle the timeout
                thread::Builder::new()
                    .name("lock_async_timeout".to_string())
                    .spawn(move || {
                        thread::sleep(timeout);
                        // Send timeout signal
                        timeout_sender.wake();
                    })
                    .expect("Failed to spawn timeout thread");

                // Race between notification and timeout
                struct Race<F1, F2> {
                    notify: Option<F1>,
                    timeout: Option<F2>,
                    deadline: Instant,
                }

                impl<F1: Future + Unpin, F2: Future + Unpin> Future for Race<F1, F2> {
                    type Output = bool; // true if timed out

                    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                        // The deadline is authoritative: once it has elapsed, report a
                        // timeout even if a notification also became ready. The timeout
                        // future (a worker sleeping to the deadline) only guarantees a
                        // wake — the clock decides the verdict. Without this, a
                        // notification arriving at/after the deadline is preferred (it is
                        // polled first), so a contended lock released past its deadline
                        // would be granted instead of timing out.
                        if Instant::now() >= self.deadline {
                            return Poll::Ready(true);
                        }

                        // Poll notification future
                        if let Some(ref mut notify) = self.notify
                            && Pin::new(notify).poll(cx).is_ready()
                        {
                            self.notify = None;
                            return Poll::Ready(false); // Got notification
                        }

                        // Poll timeout future
                        if let Some(ref mut timeout) = self.timeout
                            && Pin::new(timeout).poll(cx).is_ready()
                        {
                            self.timeout = None;
                            return Poll::Ready(true); // Timed out
                        }

                        Poll::Pending
                    }
                }

                let timed_out = Race {
                    notify: Some(receiver),
                    timeout: Some(timeout_receiver),
                    deadline,
                }
                .await;

                if timed_out {
                    // Our wake may have been consumed concurrently with the
                    // deadline elapsing (the unlocker chose us as the waiter to
                    // wake). Try once more so a free lock is taken rather than
                    // stranded; if another thread won it instead, its unlock
                    // passes the wake along to the next waiter.
                    return mutex.try_lock().ok();
                }
                // If not timed out, we loop and try to lock again
            }
        }
    }
}
