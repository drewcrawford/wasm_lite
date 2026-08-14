// SPDX-License-Identifier: MIT OR Apache-2.0
//! The blocking locking strategy behind `Mutex::lock_block` and its timeout form.
//!
//! Registers the calling thread on the mutex's waiter list and parks it (native
//! parking, or `Atomics.wait` on a wasm worker) until the unlock path unparks
//! it. Parking traps on the browser main thread, so callers who may run there
//! should go through `lock_sync` or `lock_async` instead.

use super::{Mutex, NotAvailable};
use crate::guard::Guard;

#[cfg(target_arch = "wasm32")]
use crate as thread;
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use crate::time::Instant;

pub(crate) fn lock_block<T>(mutex: &Mutex<T>) -> Guard<'_, T> {
    //insert our thread into the waiting list
    loop {
        let r = mutex.waiting_sync_threads.with_mut(|threads| {
            match mutex.try_lock() {
                Ok(guard) => {
                    // Return the guard
                    Ok(guard)
                }
                Err(_) => {
                    let handle = thread::current();
                    if !threads.iter().any(|thread| thread.id() == handle.id()) {
                        threads.push(handle);
                    }
                    Err(NotAvailable)
                }
            }
        });
        match r {
            Ok(guard) => return guard,
            Err(NotAvailable) => thread::park(),
        }
    }
}

pub(crate) fn lock_block_timeout<T>(mutex: &Mutex<T>, deadline: Instant) -> Option<Guard<'_, T>> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            // Try one last time
            if let Ok(guard) = mutex.try_lock() {
                return Some(guard);
            }
            return None;
        }

        let r = mutex
            .waiting_sync_threads
            .with_mut(|threads| match mutex.try_lock() {
                Ok(guard) => Ok(guard),
                Err(_) => {
                    let handle = thread::current();
                    if !threads.iter().any(|thread| thread.id() == handle.id()) {
                        threads.push(handle);
                    }
                    Err(NotAvailable)
                }
            });

        match r {
            Ok(guard) => return Some(guard),
            Err(NotAvailable) => {
                let remaining = deadline - Instant::now();
                thread::park_timeout(remaining);
                let current = thread::current();
                mutex
                    .waiting_sync_threads
                    .with_mut(|threads| threads.retain(|thread| thread.id() != current.id()));
            }
        }
    }
}
