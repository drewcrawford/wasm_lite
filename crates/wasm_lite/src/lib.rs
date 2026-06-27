//! wasm_lite: minimal JavaScript bindings for Rust compiled to wasm.
//!
//! Imports are declared with the [`import!`] macro, which records a descriptor
//! for each import into the `__wasm_lite_imports` custom wasm section. The
//! host-side `wasm_lite_codegen` crate reads that section and generates the
//! matching JavaScript shims, so no JS is hand-written per import.

mod macros;
mod value;

pub mod console;
pub mod performance;

/// Bridge to `wasm_bindgen::JsValue` (enable the `wasm-bindgen` feature).
#[cfg(feature = "wasm-bindgen")]
pub mod interop;

pub use value::JsValue;
pub use wasm_lite_macro::{export, wasm_lite_test};

/// Install a panic hook that reports the panic message via `console.error`.
///
/// On `wasm32-unknown-unknown` a panic aborts (a trap) and the default hook has
/// nowhere to write — so without this, a failure surfaces only as
/// "unreachable", losing the message. [`wasm_lite_test`] installs it
/// automatically; call it yourself at the top of a **doctest** so its failures
/// report the panic message too:
///
/// ```
/// wasm_lite::set_panic_hook();
/// assert_eq!(2 + 2, 4);
/// ```
///
/// [`wasm_lite_test`]: crate::wasm_lite_test
pub fn set_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        crate::console::error(&format!("{info}"));
    }));
}

/// Allocate `len` bytes (align 1) for string/byte marshalling across the JS
/// boundary. Exported for the generated glue; freed with [`__wl_free`].
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_malloc(len: usize) -> *mut u8 {
    if len == 0 {
        return core::ptr::NonNull::<u8>::dangling().as_ptr();
    }
    match std::alloc::Layout::from_size_align(len, 1) {
        Ok(layout) => unsafe { std::alloc::alloc(layout) },
        Err(_) => core::ptr::null_mut(),
    }
}

/// Free a buffer from [`__wl_malloc`] (`len` must match the allocation).
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_free(ptr: *mut u8, len: usize) {
    if len == 0 {
        return;
    }
    if let Ok(layout) = std::alloc::Layout::from_size_align(len, 1) {
        unsafe { std::alloc::dealloc(ptr, layout) }
    }
}

/// Copy a `&str`'s bytes into a fixed-size array at compile time.
///
/// Used by [`import!`] to place its descriptor text into a `#[link_section]`
/// static (which must be an array by value, not a reference).
#[doc(hidden)]
pub const fn descriptor_bytes<const N: usize>(s: &str) -> [u8; N] {
    let src = s.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = src[i];
        i += 1;
    }
    out
}
