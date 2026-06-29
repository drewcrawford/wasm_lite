// SPDX-License-Identifier: MIT OR Apache-2.0

/// Whether blocking `atomic.wait` works in the current context.
///
/// The wasm `atomic.wait` instruction traps on the main thread, so callers use
/// this to pick a blocking vs. spinning strategy. A shared-memory `+atomics`
/// build always has the capability on worker threads.
#[cfg(target_arch = "wasm32")]
pub(crate) fn atomics_wait_supported() -> bool {
    !crate::wasm::is_main_thread()
}
