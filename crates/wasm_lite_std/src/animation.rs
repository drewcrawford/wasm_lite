// SPDX-License-Identifier: MIT OR Apache-2.0
//! `requestAnimationFrame`, with the closure lifetime handled.
//!
//! The raw binding takes a JS function, which means a [`Closure`] that must
//! outlive the call and be released once it fires. Doing that by hand is the
//! `Closure::once_into_js` / `forget` dance, which leaks. This owns the closure
//! in a thread-local slab and drops it when the frame callback runs.

#[cfg(target_arch = "wasm32")]
use std::cell::{Cell, RefCell};
#[cfg(target_arch = "wasm32")]
use std::collections::HashMap;
#[cfg(target_arch = "wasm32")]
use wasm_lite::Closure;

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// Closures for frames that have not fired yet, by id.
    static PENDING: RefCell<HashMap<u64, Closure>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
}

/// Run `callback` just before the next repaint.
///
/// The callback runs *inside* the frame callback, not in a microtask after it,
/// which is what makes it usable for rendering: work scheduled here lands in the
/// frame the browser is already preparing.
///
/// **Main thread only** — a worker has no `requestAnimationFrame`.
///
/// ```
/// # #[cfg(target_arch = "wasm32")]
/// # wasm_lite::set_panic_hook();
/// # let (tx, mut frame) = wasm_lite_std::mpsc::channel();
/// wasm_lite_std::request_animation_frame(move || {
///     // draw
/// #   tx.send_sync(()).unwrap();
/// });
/// # wasm_lite_std::async_doctest!(async move { frame.recv_async().await.unwrap(); });
/// ```
///
/// On native there is no compositor to wait for, so the callback runs
/// immediately — the same thing every wasm/native shim of this does, and the
/// reason a caller must not rely on it returning before the callback runs.
pub fn request_animation_frame<F: FnOnce() + 'static>(callback: F) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        callback();
    }
    #[cfg(target_arch = "wasm32")]
    {
        let key = NEXT_ID.with(|n| {
            let k = n.get();
            n.set(k + 1);
            k
        });
        // `Closure::new` takes `FnMut` and this is `FnOnce`; an Option makes it
        // callable more than once without running the body twice.
        let mut callback = Some(callback);
        let cb = Closure::new(move || {
            if let Some(f) = callback.take() {
                f();
            }
            // Fired: release the closure. Doing this *after* the body means a
            // callback that schedules the next frame does not free the slot the new
            // one just took.
            PENDING.with(|p| {
                p.borrow_mut().remove(&key);
            });
        });
        wasm_lite::timer::request_animation_frame(cb.as_js_value());
        PENDING.with(|p| {
            p.borrow_mut().insert(key, cb);
        });
    }
}
