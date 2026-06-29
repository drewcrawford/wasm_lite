// SPDX-License-Identifier: MIT OR Apache-2.0
//! A minimal single-threaded async executor driven by the JS event loop.
//!
//! The main thread can't block (`atomic.wait` traps there), so async work is
//! driven by re-polling on the event loop: [`spawn_local`] queues a future and
//! schedules a tick (`setTimeout(0)` via the `__wl_schedule` runtime import,
//! provided by the generated glue); each tick polls every queued future and, if
//! any are still pending, schedules another tick.
//!
//! This is **level-triggered polling**, not edge-triggered. That is deliberate:
//! a future awaiting a lock/channel released by *another* worker is woken from
//! that worker's realm, which can't enqueue a microtask on this realm's event
//! loop. Re-polling each tick picks the completion up via shared memory. The
//! cost is that the loop ticks (~every 4 ms) while any task is pending.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, RawWaker, RawWakerVTable, Waker};

type Task = Pin<Box<dyn Future<Output = ()> + 'static>>;

std::thread_local! {
    static TASKS: RefCell<Vec<Task>> = const { RefCell::new(Vec::new()) };
    static SCHEDULED: Cell<bool> = const { Cell::new(false) };
}

// Runtime import: schedule one [`__wl_async_tick`] on the event loop. Provided by
// the generated glue (`setTimeout(() => exports.__wl_async_tick(), 0)`).
#[link(wasm_import_module = "__wasm_lite")]
unsafe extern "C" {
    #[link_name = "__wl_schedule"]
    fn schedule_tick();
}

/// Run `future` to completion on this thread's event loop (fire-and-forget).
///
/// The future need not be `Send` (it stays on this realm). Requires the glue's
/// `__wl_schedule` runtime support (any wasm_lite glue provides it).
pub fn spawn_local<F: Future<Output = ()> + 'static>(future: F) {
    keep_tick_export();
    TASKS.with(|t| t.borrow_mut().push(Box::pin(future)));
    ensure_tick();
}

fn ensure_tick() {
    SCHEDULED.with(|s| {
        if !s.get() {
            s.set(true);
            unsafe { schedule_tick() };
        }
    });
}

/// Poll every queued task once; reschedule if any remain pending.
///
/// Called by the glue from a scheduled event-loop callback. Not for direct use.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_async_tick() {
    SCHEDULED.with(|s| s.set(false));

    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);

    // Take the queue so tasks spawned *during* polling land in a fresh TASKS.
    let mut tasks: Vec<Task> = TASKS.with(|t| std::mem::take(&mut *t.borrow_mut()));
    tasks.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());

    let remaining = TASKS.with(|t| {
        let mut q = t.borrow_mut();
        // Keep still-pending tasks ahead of any newly spawned ones.
        tasks.append(&mut q);
        *q = tasks;
        q.len()
    });

    if remaining > 0 {
        ensure_tick();
    }
}

/// A no-op waker. Wakes are ignored because the executor re-polls every tick
/// while tasks remain (level-triggered); see the module docs.
fn noop_waker() -> Waker {
    const VT: RawWakerVTable =
        RawWakerVTable::new(|_| RawWaker::new(std::ptr::null(), &VT), |_| {}, |_| {}, |_| {});
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VT)) }
}

/// Force the linker to keep `__wl_async_tick` (only ever called from JS).
fn keep_tick_export() {
    #[used]
    static KEEP: extern "C" fn() = __wl_async_tick;
}
