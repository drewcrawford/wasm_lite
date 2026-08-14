// SPDX-License-Identifier: MIT OR Apache-2.0

//! wasm-bindgen-backed shim for `wasm_lite_std`.
//!
//! See the [`wasm_lite`](https://github.com/drewcrawford/wasm_lite) shim for the
//! rationale. This crate provides the
//! `wasm_lite_std` surface — threads, sync primitives, time — on top of
//! [`wasm_safe_thread`], which is the crate `wasm_lite_std` was itself ported
//! from and whose API is near-identical. That makes the bulk of this file a
//! re-export; only the pieces `wasm_lite_std` added after the port are
//! implemented here.
//!
//! What is re-exported wholesale: `condvar`, `guard`, `mpsc`, `mutex`,
//! `rwlock`, `spinlock`, `test_executor`, `Builder`, `JoinHandle`, `LocalKey`,
//! `Thread`, `ThreadId`, `spawn`, `spawn_named`, `current`, `sleep`,
//! `yield_now`, `park`, `park_timeout`, `available_parallelism`, and the spawn
//! hooks.
//!
//! What is implemented here because `wasm_safe_thread` has no equivalent:
//! [`spawn_local`], [`time`], [`is_main_thread`], and the `__rt` test hooks.

// The overwhelming majority of the surface is identical, so take it verbatim.
pub use wasm_safe_thread::{
    AccessError, Builder, Guard, JoinHandle, LocalKey, Mutex, NotAvailable, Thread, ThreadId,
};
pub use wasm_safe_thread::{
    available_parallelism, current, install_println_eprintln_console_hook, park, park_timeout,
    redirect_println_eprintln_to_console_current_thread, sleep, spawn, spawn_named, task_begin,
    task_finished, yield_now, yield_to_event_loop_async,
};
pub use wasm_safe_thread::{clear_spawn_hooks, register_spawn_hook, remove_spawn_hook};
pub use wasm_safe_thread::{condvar, guard, mpsc, mutex, rwlock, spinlock, test_executor};

/// Clock types.
///
/// `wasm_lite_std::time` is a drop-in `Instant`/`SystemTime` that works on
/// wasm without wasm-bindgen. In a wasm-bindgen host that constraint is moot,
/// so this is `web-time` — the standard shim, and one that yields the same
/// monotonic `performance.now()` clock underneath.
pub mod time {
    // `Duration` is platform-independent and comes from core either way.
    pub use core::time::Duration;

    #[cfg(not(target_arch = "wasm32"))]
    pub use std::time::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};
    #[cfg(target_arch = "wasm32")]
    pub use web_time::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};
}

/// Runs an async block as a doctest/test body.
///
/// Mirrors `wasm_lite_std::async_doctest!`. The real macro signals the wasm_lite
/// runner that a test is pending and later that it passed; under the shim the
/// wasm-bindgen test harness owns that bookkeeping, so the wasm arm just spawns
/// onto the event loop and the pending/pass hooks are no-ops.
#[macro_export]
macro_rules! async_doctest {
    ($fut:expr) => {{
        #[cfg(target_arch = "wasm32")]
        {
            $crate::__rt::test_pending();
            $crate::spawn_local(async move {
                let _ = $fut.await;
                $crate::__rt::test_pass();
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = $crate::__rt::block_on($fut);
        }
    }};
}

/// Spawns a `!Send` future on the current thread's event loop.
///
/// `wasm_lite_std` runs this on its own event-loop executor; the wasm-bindgen
/// equivalent is `wasm_bindgen_futures::spawn_local`, which schedules onto the
/// browser microtask queue.
#[cfg(target_arch = "wasm32")]
pub fn spawn_local<F>(future: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}

/// Native counterpart to [`spawn_local`].
///
/// There is no ambient event loop on the host, so the future is driven to
/// completion on a dedicated thread. This keeps host-side `cargo check`/`cargo
/// test` of a shimmed leaf working; it is not a scheduler.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_local<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    std::thread::spawn(move || {
        __rt::block_on(future);
    });
}

/// Whether the caller is on the browser's main thread.
///
/// Worker threads have no `window`, so its presence is the test. On the host
/// this reports whether the caller is the process's initial thread.
pub fn is_main_thread() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        // A DedicatedWorkerGlobalScope has no `document`; a Window does. This
        // avoids a web-sys dependency just to name the Window type.
        js_sys::Reflect::get(&js_sys::global(), &"document".into())
            .map(|v| !v.is_undefined() && !v.is_null())
            .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // `main` is the conventional name std gives the initial thread.
        std::thread::current().name() == Some("main")
    }
}

/// Test-harness plumbing mirroring `wasm_lite_std::__rt`.
///
/// Under the shim, tests run through `wasm_bindgen_test` rather than the
/// wasm_lite runner, so the pass/pending signals are no-ops; `block_on` remains
/// useful and is kept.
#[doc(hidden)]
pub mod __rt {
    /// No-op: the wasm-bindgen test harness tracks pending tests itself.
    pub fn test_pending() {}

    /// No-op: the wasm-bindgen test harness tracks passing tests itself.
    pub fn test_pass() {}

    /// Drives `future` to completion on the current thread by polling.
    ///
    /// Mirrors the real crate's native fallback: a no-op waker plus a spin, which
    /// is enough for the test paths this backs and avoids pulling in an executor.
    pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::pin::pin;
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        static VT: RawWakerVTable = RawWakerVTable::new(
            |_| RawWaker::new(std::ptr::null(), &VT),
            |_| {},
            |_| {},
            |_| {},
        );
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VT)) };
        let mut cx = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => std::hint::spin_loop(),
            }
        }
    }
}
