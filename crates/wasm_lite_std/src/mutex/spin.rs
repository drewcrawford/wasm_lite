// SPDX-License-Identifier: MIT OR Apache-2.0
use super::Mutex;
use crate::guard::Guard;
use std::sync::atomic::Ordering;

use crate::time::Instant;

pub(crate) fn lock_spin<T>(mutex: &Mutex<T>) -> Guard<'_, T> {
    // Spin until we can acquire the lock
    while mutex.data_lock.swap(true, Ordering::Acquire) {
        std::hint::spin_loop();
    }
    // SAFETY: We have exclusive access to the data now
    let data = unsafe { &mut *mutex.inner.get() };
    Guard { mutex, data }
}

pub(crate) fn lock_spin_timeout<T>(mutex: &Mutex<T>, deadline: Instant) -> Option<Guard<'_, T>> {
    // Spin until we can acquire the lock
    while mutex.data_lock.swap(true, Ordering::Acquire) {
        if Instant::now() >= deadline {
            return None;
        }
        std::hint::spin_loop();
    }
    // SAFETY: We have exclusive access to the data now
    let data = unsafe { &mut *mutex.inner.get() };
    Some(Guard { mutex, data })
}
