// SPDX-License-Identifier: MIT OR Apache-2.0
//! The capability probe the adaptive `*_sync` paths consult on wasm32.
//!
//! Two `cfg`-selected definitions of `atomics_wait_supported`: a shared-memory
//! `+atomics` build answers "yes off the main thread", a plain non-atomics build
//! always answers "no". The module is empty on native, where blocking is always
//! legal and callers take the `#[cfg(not(target_arch = "wasm32"))]` arm instead.

/// Whether blocking `atomic.wait` works in the current context.
///
/// The wasm `atomic.wait` instruction traps on the main thread, so callers use
/// this to pick a blocking vs. spinning strategy. A shared-memory `+atomics`
/// build always has the capability on worker threads.
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
pub(crate) fn atomics_wait_supported() -> bool {
    !crate::wasm::is_main_thread()
}

/// Without the `atomics` target feature there is no `atomic.wait` at all, so no
/// context can block; every caller falls back to spinning or async.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
pub(crate) fn atomics_wait_supported() -> bool {
    false
}
