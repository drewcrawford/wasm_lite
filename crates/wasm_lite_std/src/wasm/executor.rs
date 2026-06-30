// SPDX-License-Identifier: MIT OR Apache-2.0
//! A minimal single-threaded async executor driven by the JS event loop.
//!
//! The main thread can't block (`atomic.wait` traps there), so async work runs
//! on the event loop: [`spawn_local`] queues a future; each tick polls every
//! queued future, and if any are still pending the executor *sleeps* on
//! `Atomics.waitAsync` (via the `__wl_wait_async` runtime import) until woken —
//! it does **not** busy-poll.
//!
//! Waking is edge-triggered and works across threads. Each executor owns a wake
//! atom (in shared memory); a task's [`Waker`] bumps that atom and issues a
//! `memory.atomic.notify`. That wakes the owning realm's `Atomics.waitAsync`
//! Promise even when the notify comes from *another* worker — which is exactly
//! the case for a future awaiting a lock/channel another worker released. (A
//! worker can't enqueue a microtask on the main realm, but its atomic notify
//! still resolves the main realm's `waitAsync`.)

use core::arch::wasm32::memory_atomic_notify;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI32, Ordering};
use std::task::{Context, RawWaker, RawWakerVTable, Waker};

type Task = Pin<Box<dyn Future<Output = ()> + 'static>>;

std::thread_local! {
    static TASKS: RefCell<Vec<Task>> = const { RefCell::new(Vec::new()) };
    /// True while a tick is scheduled or the executor is asleep on `waitAsync`.
    static RUNNING: Cell<bool> = const { Cell::new(false) };
    /// This realm's wake atom (a leaked `AtomicI32` in shared memory). A `Waker`
    /// bumps + notifies it; a tick sleeps on it via `waitAsync`.
    static WAKE: Cell<*const AtomicI32> = const { Cell::new(std::ptr::null()) };
}

// Runtime imports provided by the generated glue:
//   __wl_schedule:   setTimeout(() => exports.__wl_async_tick(), 0)  — initial kick
//   __wl_wait_async: Atomics.waitAsync(atom, expected).then(tick)    — sleep until woken
#[link(wasm_import_module = "__wasm_lite")]
unsafe extern "C" {
    #[link_name = "__wl_schedule"]
    fn schedule_tick();
    #[link_name = "__wl_wait_async"]
    fn wait_async(ptr: *const i32, expected: i32);
}

/// This realm's wake atom, created on first use and never freed (so its shared
/// address stays valid for any `Waker` that captured it, on any thread).
fn wake_atom() -> *const AtomicI32 {
    WAKE.with(|w| {
        let mut p = w.get();
        if p.is_null() {
            p = Box::leak(Box::new(AtomicI32::new(0))) as *const AtomicI32;
            w.set(p);
        }
        p
    })
}

/// Bump the wake atom and notify any waiter (the owning realm's `waitAsync`).
/// Safe to call from any thread.
fn wake(atom: *const AtomicI32) {
    unsafe {
        (*atom).fetch_add(1, Ordering::SeqCst);
        memory_atomic_notify(atom as *mut i32, u32::MAX);
    }
}

/// Run `future` to completion on this thread's event loop (fire-and-forget).
///
/// The future need not be `Send` (it stays on this realm). Requires the glue's
/// `__wl_schedule`/`__wl_wait_async` runtime support (any wasm_lite glue provides it).
pub fn spawn_local<F: Future<Output = ()> + 'static>(future: F) {
    keep_tick_export();
    let atom = wake_atom();
    TASKS.with(|t| t.borrow_mut().push(Box::pin(future)));
    if RUNNING.with(|r| r.replace(true)) {
        // Already running (likely asleep on waitAsync): wake it to re-poll.
        wake(atom);
    } else {
        unsafe { schedule_tick() };
    }
}

/// Poll every queued task once; sleep on the wake atom if any remain pending.
///
/// Called by the glue from a scheduled event-loop callback. Not for direct use.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_async_tick() {
    let atom = wake_atom();
    // Snapshot the wake counter *before* polling so a wake racing the poll is
    // detected by waitAsync (it returns "not-equal" and we re-tick).
    let expected = unsafe { (*atom).load(Ordering::SeqCst) };

    let waker = make_waker(atom);
    let mut cx = Context::from_waker(&waker);

    // Take the queue so tasks spawned *during* polling land in a fresh TASKS.
    let mut tasks: Vec<Task> = TASKS.with(|t| std::mem::take(&mut *t.borrow_mut()));
    tasks.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());

    let remaining = TASKS.with(|t| {
        let mut q = t.borrow_mut();
        tasks.append(&mut q); // still-pending ahead of newly spawned
        *q = tasks;
        q.len()
    });

    if remaining == 0 {
        RUNNING.with(|r| r.set(false));
    } else {
        // Sleep until a Waker bumps the atom (resolves the waitAsync Promise).
        unsafe { wait_async(atom as *const i32, expected) };
    }
}

/// Build a `Waker` that bumps + notifies `atom` (cross-thread safe).
fn make_waker(atom: *const AtomicI32) -> Waker {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |p| RawWaker::new(p, &VTABLE),   // clone
        |p| wake(p as *const AtomicI32), // wake
        |p| wake(p as *const AtomicI32), // wake_by_ref
        |_| {},                          // drop
    );
    unsafe { Waker::from_raw(RawWaker::new(atom as *const (), &VTABLE)) }
}

/// Whether this thread's executor has drained (no pending tasks).
///
/// Called by the worker bootstrap to decide when it may free this thread's
/// stack/TLS and `close()` — it must not tear down while `spawn_local` tasks are
/// still pending (their queue lives in this thread's TLS). `RUNNING` stays true
/// from the first `spawn_local` until the last task completes.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_executor_idle() -> i32 {
    if RUNNING.with(|r| r.get()) { 0 } else { 1 }
}

/// Force the linker to keep the JS-called executor exports.
fn keep_tick_export() {
    #[used]
    static KEEP_TICK: extern "C" fn() = __wl_async_tick;
    #[used]
    static KEEP_IDLE: extern "C" fn() -> i32 = __wl_executor_idle;
}
