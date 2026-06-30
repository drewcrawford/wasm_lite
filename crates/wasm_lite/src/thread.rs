//! Low-level thread spawning over Web Workers.
//!
//! This is the primitive a `std::thread`-like layer builds on. It requires a
//! **shared-memory `+atomics` build** (see the crate README, "Shared memory &
//! atomics"): the spawned thread runs on a Web Worker that shares this module's
//! compiled `WebAssembly.Module` and its shared linear memory.
//!
//! The dance is split between Rust and the generated glue:
//!   * [`spawn`] boxes the closure and hands the glue a thin pointer (`work`).
//!   * The glue's `__wl_spawn` allocates a fresh stack + TLS block (via
//!     [`__wl_thread_alloc`]) and starts a Worker, postMessaging the module,
//!     the shared memory, and `work`.
//!   * The Worker instantiates the same module on the same memory, points
//!     `__stack_pointer` at the new stack, calls `__wasm_init_tls`, then calls
//!     [`__wl_thread_entry`], which reconstitutes the closure and runs it.
//!
//! Spawning is **detached**: there is no join here. A higher layer
//! (`wasm_lite_std`) adds `JoinHandle`/`park`/sync primitives on top using
//! atomics. Coordinate between threads with `core::sync::atomic` for now.

// Runtime import provided by the generated glue (only wired for shared-memory
// builds). Starts a Worker for the boxed closure identified by `work`.
#[link(wasm_import_module = "__wasm_lite")]
unsafe extern "C" {
    #[link_name = "__wl_spawn"]
    fn spawn_worker(work: u32);
}

/// The boxed work a spawned thread runs. The outer `Box` makes the pointer we
/// pass across the ABI *thin* (one `u32`), even though `dyn FnOnce` is fat.
type Work = Box<dyn FnOnce() + Send + 'static>;

/// Spawn `f` on a new worker thread (detached).
///
/// Requires a shared-memory `+atomics` build; in a non-threaded build the
/// `__wl_spawn` import is unresolved (the glue only wires it for shared memory).
pub fn spawn<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    keep_thread_exports();
    let work: Box<Work> = Box::new(Box::new(f));
    let ptr = Box::into_raw(work) as u32;
    unsafe { spawn_worker(ptr) };
}

/// Worker-side trampoline: reconstitute the boxed closure and run it.
///
/// Called by the worker bootstrap after it sets up this thread's stack and TLS.
/// Not for direct use.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_thread_entry(work: u32) {
    let work = unsafe { Box::from_raw(work as *mut Work) };
    (work)();
}

/// Allocate `size` bytes aligned for a thread stack / TLS block (align 16).
///
/// Exported for the glue, which carves a per-thread stack and TLS block. Paired
/// with [`__wl_thread_free`].
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_thread_alloc(size: usize) -> *mut u8 {
    if size == 0 {
        return core::ptr::NonNull::<u8>::dangling().as_ptr();
    }
    match std::alloc::Layout::from_size_align(size, 16) {
        Ok(layout) => unsafe { std::alloc::alloc(layout) },
        Err(_) => core::ptr::null_mut(),
    }
}

/// Free a block from [`__wl_thread_alloc`] (`size` must match).
#[doc(hidden)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_thread_free(ptr: *mut u8, size: usize) {
    if size == 0 {
        return;
    }
    if let Ok(layout) = std::alloc::Layout::from_size_align(size, 16) {
        unsafe { std::alloc::dealloc(ptr, layout) }
    }
}

/// Force the linker to keep the JS-called thread exports.
///
/// `__wl_thread_entry`/`alloc`/`free` are only ever called from JS, so nothing
/// in Rust references them and dead-code elimination would drop them. Touching
/// their addresses from [`spawn`] (which user code does reference) keeps them.
fn keep_thread_exports() {
    #[used]
    static K_ENTRY: extern "C" fn(u32) = __wl_thread_entry;
    #[used]
    static K_ALLOC: extern "C" fn(usize) -> *mut u8 = __wl_thread_alloc;
    #[used]
    static K_FREE: extern "C" fn(*mut u8, usize) = __wl_thread_free;
}
