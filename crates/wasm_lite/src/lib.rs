//! wasm_lite: minimal JavaScript bindings for Rust compiled to wasm.
//!
//! Imports are declared with the [`import!`] macro, which records a descriptor
//! for each import into the `__wasm_lite_imports` custom wasm section. The
//! host-side `wasm_lite_codegen` crate reads that section and generates the
//! matching JavaScript shims, so no JS is hand-written per import.

// The proc-macros (`import!`, `#[export]`, `js_class!`) emit absolute
// `::wasm_lite::…` paths (a proc-macro can't use `$crate`). This self-alias lets
// those paths resolve when the macros are used *inside* this crate too.
extern crate self as wasm_lite;

mod macros;
mod value;

pub mod console;
pub mod performance;
pub mod thread;

/// Bridge to `wasm_bindgen::JsValue` (enable the `wasm-bindgen` feature).
#[cfg(feature = "wasm-bindgen")]
pub mod interop;

pub use value::JsValue;
pub use wasm_lite_macro::{export, import, js_class, wasm_lite_test};

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

/// Read one value out of an `Option`/`Result` sret buffer.
///
/// The host writes a discriminant at `base` and a payload at `base + 8`; this
/// reads that payload back into Rust. Implemented for every type usable as an
/// `Option`/`Result` inner type, so [`import!`](crate::import) needs only one
/// terminal rule per `Option`/`Result` (the per-type read dispatches here, in
/// Rust, instead of in the macro).
#[doc(hidden)]
pub trait FromSretPayload {
    /// # Safety
    /// The host must have written a payload of exactly this type at `base + 8`
    /// (and transferred ownership, for `String`/`Vec<u8>`/`JsValue`).
    unsafe fn __wl_read(base: *const u8) -> Self;
}

macro_rules! __impl_sret_scalar {
    ($($t:ty),*) => { $(
        impl FromSretPayload for $t {
            unsafe fn __wl_read(base: *const u8) -> Self {
                unsafe { core::ptr::read_unaligned(base.add(8) as *const $t) }
            }
        }
    )* };
}
__impl_sret_scalar!(i32, u32, f64);

impl FromSretPayload for bool {
    unsafe fn __wl_read(base: *const u8) -> Self {
        unsafe { core::ptr::read_unaligned(base.add(8) as *const i32) != 0 }
    }
}

impl FromSretPayload for JsValue {
    unsafe fn __wl_read(base: *const u8) -> Self {
        let idx = unsafe { core::ptr::read_unaligned(base.add(8) as *const u32) };
        JsValue::__wl_from_abi(idx)
    }
}

impl FromSretPayload for String {
    unsafe fn __wl_read(base: *const u8) -> Self {
        unsafe {
            let ptr = core::ptr::read_unaligned(base.add(8) as *const u32) as usize as *mut u8;
            let len = core::ptr::read_unaligned(base.add(12) as *const u32) as usize;
            String::from_raw_parts(ptr, len, len)
        }
    }
}

impl FromSretPayload for Vec<u8> {
    unsafe fn __wl_read(base: *const u8) -> Self {
        unsafe {
            let ptr = core::ptr::read_unaligned(base.add(8) as *const u32) as usize as *mut u8;
            let len = core::ptr::read_unaligned(base.add(12) as *const u32) as usize;
            Vec::from_raw_parts(ptr, len, len)
        }
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
