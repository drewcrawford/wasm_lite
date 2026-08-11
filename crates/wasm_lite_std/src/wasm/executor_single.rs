// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stable, non-atomic wasm event-loop executor.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, RawWaker, RawWakerVTable, Waker};

type Task = Pin<Box<dyn Future<Output = ()> + 'static>>;

std::thread_local! {
    static TASKS: RefCell<Vec<Task>> = const { RefCell::new(Vec::new()) };
    /// True when an executor turn has been queued but has not started yet.
    static SCHEDULED: Cell<bool> = const { Cell::new(false) };
}

#[link(wasm_import_module = "__wasm_lite")]
unsafe extern "C" {
    #[link_name = "__wl_schedule"]
    fn schedule_tick();
}

fn schedule() {
    if !SCHEDULED.with(|scheduled| scheduled.replace(true)) {
        unsafe { schedule_tick() };
    }
}

/// Run a future on this realm's browser event loop.
pub fn spawn_local<F: Future<Output = ()> + 'static>(future: F) {
    keep_exports();
    TASKS.with(|tasks| tasks.borrow_mut().push(Box::pin(future)));
    schedule();
}

/// Poll every runnable task once.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_async_tick() {
    SCHEDULED.with(|scheduled| scheduled.set(false));
    let waker = make_waker();
    let mut cx = Context::from_waker(&waker);

    let mut tasks = TASKS.with(|tasks| std::mem::take(&mut *tasks.borrow_mut()));
    tasks.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());
    TASKS.with(|queued| {
        let mut queued = queued.borrow_mut();
        tasks.append(&mut queued);
        *queued = tasks;
    });
}

fn make_waker() -> Waker {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(std::ptr::null(), &VTABLE),
        |_| schedule(),
        |_| schedule(),
        |_| {},
    );
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_executor_idle() -> i32 {
    if SCHEDULED.with(|scheduled| scheduled.get())
        || TASKS.with(|tasks| !tasks.borrow().is_empty())
        || super::pending_tasks() > 0
    {
        0
    } else {
        1
    }
}

fn keep_exports() {
    #[used]
    static KEEP_TICK: extern "C" fn() = __wl_async_tick;
    #[used]
    static KEEP_IDLE: extern "C" fn() -> i32 = __wl_executor_idle;
}
