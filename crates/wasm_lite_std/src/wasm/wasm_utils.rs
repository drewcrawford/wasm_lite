// SPDX-License-Identifier: MIT OR Apache-2.0
//! WASM utility functions for parking, sleeping, and environment detection.
//!
//! Retargeted off wasm-bindgen: parking and waiting use the wasm `atomic.wait`/
//! `atomic.notify` instructions directly (`core::arch::wasm32`), so no JS is
//! involved. `atomic.wait` traps on the main thread, so [`is_main_thread`] gates
//! every blocking path (the main thread falls back to spinning / "unsupported").

use core::arch::wasm32::{memory_atomic_notify, memory_atomic_wait32};
use std::sync::atomic::{AtomicI32, Ordering};

std::thread_local! {
    /// True on a thread we spawned (a Web Worker). The main thread never runs
    /// the spawn trampoline, so its value stays `false`.
    static IS_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the current thread as a spawned worker (called once at thread start).
pub fn mark_worker_thread() {
    IS_WORKER.with(|c| c.set(true));
}

/// Whether the current thread is the main thread (where `atomic.wait` traps).
pub fn is_main_thread() -> bool {
    IS_WORKER.with(|c| !c.get())
}

/// The result of a park/wait operation, mirroring `Atomics.wait`'s return values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResult {
    /// Woken by a notify (a token was consumed).
    Ok,
    /// Timed out before being woken.
    TimedOut,
    /// Waiting is not available here (the main thread).
    Unsupported,
}

/// Scratch atomic for context-less timed waits (`yield_now`, `sleep`).
static SCRATCH: AtomicI32 = AtomicI32::new(0);

/// Park at `ptr` (a 4-byte-aligned address of an `AtomicI32` token) until
/// notified. Consumes a pending token without waiting if one is present.
pub fn park_wait_at_addr(ptr: u32) -> WaitResult {
    if is_main_thread() {
        return WaitResult::Unsupported;
    }
    let p = ptr as *mut i32;
    let token = unsafe { &*(ptr as *const AtomicI32) };
    loop {
        // Consume an existing unpark token (1 -> 0).
        if token
            .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return WaitResult::Ok;
        }
        // Wait while the token is still 0. Returns: 0 = woken, 1 = not-equal,
        // 2 = timed-out (cannot happen with an infinite timeout).
        let r = unsafe { memory_atomic_wait32(p, 0, -1) };
        if r == 1 {
            // The value changed (a token was stored) — loop to consume it.
            continue;
        }
        token.store(0, Ordering::Release);
        return WaitResult::Ok;
    }
}

/// Park at `ptr` until notified or `timeout_ms` elapses.
pub fn park_wait_timeout_at_addr(ptr: u32, timeout_ms: f64) -> WaitResult {
    if is_main_thread() {
        return WaitResult::Unsupported;
    }
    let p = ptr as *mut i32;
    let token = unsafe { &*(ptr as *const AtomicI32) };
    if token
        .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        return WaitResult::Ok;
    }
    let timeout_ns = (timeout_ms * 1.0e6) as i64;
    match unsafe { memory_atomic_wait32(p, 0, timeout_ns) } {
        2 => WaitResult::TimedOut,
        _ => {
            // Woken or value-changed: consume any token.
            token.store(0, Ordering::Release);
            WaitResult::Ok
        }
    }
}

/// Unpark a thread parked at `ptr`: set its token and wake one waiter.
pub fn park_notify_at_addr(ptr: u32) {
    let token = unsafe { &*(ptr as *const AtomicI32) };
    token.store(1, Ordering::Release);
    unsafe {
        memory_atomic_notify(ptr as *mut i32, 1);
    }
}

/// Best-effort timed wait on a scratch address (used by `yield_now`). A no-op on
/// the main thread.
pub fn atomics_wait_timeout_ms_try(timeout_ms: f64) -> WaitResult {
    if is_main_thread() {
        return WaitResult::Unsupported;
    }
    let p = SCRATCH.as_ptr();
    let timeout_ns = (timeout_ms * 1.0e6) as i64;
    match unsafe { memory_atomic_wait32(p, 0, timeout_ns) } {
        2 => WaitResult::TimedOut,
        _ => WaitResult::Ok,
    }
}

/// Sleep synchronously for `ms` milliseconds. Workers block on `atomic.wait`;
/// the main thread busy-waits (it cannot block).
pub fn sleep_sync_ms(ms: f64) {
    if ms <= 0.0 {
        return;
    }
    if !is_main_thread() {
        let timeout_ns = (ms * 1.0e6) as i64;
        // Wait on a never-notified scratch address: returns on timeout.
        unsafe {
            memory_atomic_wait32(SCRATCH.as_ptr(), 0, timeout_ns);
        }
        return;
    }
    // Main thread: busy-wait on the high-resolution clock.
    let end = wasm_lite::performance::now() + ms;
    while wasm_lite::performance::now() < end {}
}

/// Logical processors available. Property reads (`navigator.hardwareConcurrency`)
/// are not yet bindable in wasm_lite, so this returns a conservative default.
//
// TODO: bind `navigator.hardwareConcurrency` once js_class! supports property
// gets, or expose it through a wasm_lite import.
pub fn get_available_parallelism() -> u32 {
    4
}

/// Log a string via `console.log`.
#[allow(unused)]
pub fn log_str(s: &str) {
    wasm_lite::console::log(s);
}
