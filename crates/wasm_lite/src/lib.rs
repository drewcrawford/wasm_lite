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
