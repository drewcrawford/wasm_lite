// SPDX-License-Identifier: MIT OR Apache-2.0
//! Spawning threads.
//!
//! The real `wasm_lite::thread` talks to its own worker bootstrap through the
//! `__wl_spawn` import. In a wasm-bindgen host that bootstrap does not exist, so
//! this is backed by [`wasm_safe_thread`] — the crate `wasm_lite_std` was ported
//! from, and the standard way to get Web Worker threads in a wasm-bindgen build.
//!
//! Like the real one, this still requires a shared-memory `+atomics` build.

/// Spawn `f` on a new thread (detached).
///
/// Mirrors `wasm_lite::thread::spawn`, which returns nothing: the handle is
/// dropped and the thread runs to completion on its own. Use `wasm_lite_std` if
/// you need a `JoinHandle`.
pub fn spawn<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    #[cfg(target_arch = "wasm32")]
    {
        wasm_safe_thread::spawn(f);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Matches the real crate's host-side story: the crate builds natively so
        // downstream `cargo check` works, and std threads are the sane stand-in.
        std::thread::spawn(f);
    }
}
