// SPDX-License-Identifier: MIT OR Apache-2.0
//! WebAssembly backend, retargeted onto `wasm_lite`.
//!
//! Thread spawning goes through `wasm_lite::thread::spawn` (a Web Worker that
//! shares this module's compiled `Module` and shared memory); parking and
//! blocking use the wasm `atomic.wait`/`atomic.notify` instructions directly
//! (see [`wasm_utils`]). No wasm-bindgen, js-sys, or web-sys.

mod executor;
mod thread_api;
mod wasm_utils;

pub use executor::spawn_local;

use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::sync::Arc;
#[cfg(nightly_rustc)]
// std::io::set_output_capture requires Arc<std::sync::Mutex<Vec<u8>>> specifically.
// crate::Mutex is not type-compatible with that nightly std API.
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

pub use thread_api::{
    AccessError, Builder, JoinHandle, LocalKey, Thread, ThreadId, available_parallelism, current,
    park, park_timeout, sleep, spawn, yield_now, yield_to_event_loop_async,
};
// Re-exported for `super::*` in `thread_api` and `crate::wasm_support`.
pub(crate) use wasm_utils::{
    WaitResult, atomics_wait_timeout_ms_try, get_available_parallelism, is_main_thread,
    mark_worker_thread, park_notify_at_addr, park_wait_at_addr, park_wait_timeout_at_addr,
    sleep_sync_ms,
};

static THREAD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Type alias for the panic sender closure stored in thread-local storage.
type PanicSender = Box<dyn FnOnce(String) + Send>;

std::thread_local! {
    static CURRENT_THREAD: std::cell::RefCell<Option<Thread>> = const { std::cell::RefCell::new(None) };

    /// Holds a closure that sends a panic error through the channel.
    /// This is set before running user code and called from the panic hook.
    static PANIC_SENDER: std::cell::RefCell<Option<PanicSender>> = const { std::cell::RefCell::new(None) };

    /// Tracks pending async tasks spawned on this thread.
    static PENDING_TASKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(nightly_rustc)]
std::thread_local! {
    static CONSOLE_CAPTURE: std::cell::RefCell<Option<Arc<StdMutex<Vec<u8>>>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(nightly_rustc)]
fn flush_captured_prints_to_console_current_thread_impl() {
    CONSOLE_CAPTURE.with(|slot| {
        let capture = {
            let borrowed = slot.borrow();
            borrowed.as_ref().cloned()
        };
        let Some(capture) = capture else {
            return;
        };

        let bytes = {
            let mut guard = capture.lock().expect("console capture lock poisoned");
            if guard.is_empty() {
                return;
            }
            std::mem::take(&mut *guard)
        };

        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            // internal_output_capture does not tag stdout vs stderr, so route by heuristic.
            if line.contains(" panicked at ") {
                wasm_lite::console::error(line);
            } else {
                wasm_lite::console::log(line);
            }
        }
    });
}

#[cfg(not(nightly_rustc))]
fn flush_captured_prints_to_console_current_thread_impl() {}

pub(crate) fn redirect_println_eprintln_to_console_current_thread_impl() {
    #[cfg(nightly_rustc)]
    CONSOLE_CAPTURE.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }

        // set_output_capture uses this exact buffer type internally on nightly.
        let capture = Arc::new(StdMutex::new(Vec::new()));
        let _ = std::io::set_output_capture(Some(Arc::clone(&capture)));
        *slot.borrow_mut() = Some(capture);
    });
}

/// Call before spawning an async task; the thread tracks it so a future event
/// loop can wait for completion. Each call pairs with [`task_finished`].
pub fn task_begin() {
    PENDING_TASKS.with(|c| c.set(c.get() + 1));
}

/// Call when an async task completes. Pairs with a prior [`task_begin`].
pub fn task_finished() {
    PENDING_TASKS.with(|c| {
        let current = c.get();
        debug_assert!(
            current > 0,
            "task_finished called without matching task_begin"
        );
        c.set(current - 1);
    });
}

/// Number of pending async tasks on this thread.
#[allow(dead_code)]
pub fn pending_tasks() -> u32 {
    PENDING_TASKS.with(|c| c.get())
}
