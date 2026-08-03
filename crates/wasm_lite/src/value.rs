// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opaque handles to JavaScript values.
//!
//! Rust can't hold a JS object directly, so the host keeps a *value table* (an
//! array of live JS values) and hands Rust an integer index. A [`JsValue`] wraps
//! that index. When a JS function returns an object, the generated shim stores
//! it in the table and returns the index; when Rust passes a `&JsValue` back,
//! the shim looks the object up. Dropping a `JsValue` frees its table slot via
//! the `__wasm_lite.__wl_drop` runtime import.

use core::fmt;
use core::marker::PhantomData;

/// A handle to a JavaScript value living in the host's value table.
///
/// The table belongs to one JS realm, so a handle is only meaningful on the
/// thread (worker) that created it — sending it elsewhere would index a
/// *different* table. The `PhantomData<*const ()>` makes `JsValue` `!Send` and
/// `!Sync` so the type system forbids that.
pub struct JsValue {
    idx: u32,
    _not_thread_safe: PhantomData<*const ()>,
}

impl JsValue {
    /// Lower a borrowed handle to its ABI form (the table index).
    #[doc(hidden)]
    pub fn __wl_abi(&self) -> u32 {
        self.idx
    }

    /// Wrap a table index received across the ABI into an owned handle.
    #[doc(hidden)]
    pub fn __wl_from_abi(idx: u32) -> JsValue {
        JsValue {
            idx,
            _not_thread_safe: PhantomData,
        }
    }
}

// Runtime support imports; the generated glue always provides them.
#[link(wasm_import_module = "__wasm_lite")]
unsafe extern "C" {
    #[link_name = "__wl_num"]
    fn make_num(v: f64) -> u32;
    #[link_name = "__wl_prim"]
    fn make_prim(kind: u32) -> u32;
    #[link_name = "__wl_str_val"]
    fn make_str(ptr: *const u8, len: usize) -> u32;
    #[link_name = "__wl_as_f64"]
    fn as_f64_out(idx: u32, out: *mut f64) -> i32;
    #[link_name = "__wl_as_bool"]
    fn as_bool_out(idx: u32, out: *mut i32) -> i32;
    #[link_name = "__wl_as_str"]
    fn as_str_out(idx: u32, out: *mut u32) -> i32;
}

impl JsValue {
    /// A handle to the JS number `v`.
    ///
    /// Every JS number is a double, so this is the one numeric constructor;
    /// the `From` impls below funnel into it.
    pub fn from_f64(v: f64) -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_num(v) })
    }

    /// A handle to a JS boolean.
    pub fn from_bool(v: bool) -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_prim(if v { 2 } else { 3 }) })
    }

    /// A handle to a JS string, copied out of wasm memory.
    // Named to match wasm-bindgen's `JsValue::from_str`, which the shim
    // re-exports, so upstream call sites compile unchanged. It is infallible,
    // so it is not `FromStr` and never could be.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_str(s.as_ptr(), s.len()) })
    }

    /// A handle to JS `null`.
    pub fn null() -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_prim(0) })
    }

    /// A handle to JS `undefined`.
    pub fn undefined() -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_prim(1) })
    }

    /// The number this handle holds, or `None` if it is not a JS number.
    ///
    /// The presence flag is separate from the value, so a genuine `NaN` is
    /// `Some(NaN)` rather than indistinguishable from "not a number".
    pub fn as_f64(&self) -> Option<f64> {
        let mut out = 0.0f64;
        let ok = unsafe { as_f64_out(self.idx, &mut out) };
        (ok != 0).then_some(out)
    }

    /// The boolean this handle holds, or `None` if it is not a JS boolean.
    pub fn as_bool(&self) -> Option<bool> {
        let mut out = 0i32;
        let ok = unsafe { as_bool_out(self.idx, &mut out) };
        (ok != 0).then_some(out != 0)
    }

    /// The string this handle holds, or `None` if it is not a JS string.
    ///
    /// The empty string is `Some("")`; only a non-string is `None`.
    pub fn as_string(&self) -> Option<String> {
        let mut out = [0u32; 2];
        let ok = unsafe { as_str_out(self.idx, out.as_mut_ptr()) };
        if ok == 0 {
            return None;
        }
        // The host allocated with `__wl_malloc` and transferred ownership.
        let (ptr, len) = (out[0] as usize as *mut u8, out[1] as usize);
        Some(unsafe { String::from_utf8_unchecked(Vec::from_raw_parts(ptr, len, len)) })
    }
}

macro_rules! __from_number {
    ($($t:ty),*) => { $(
        impl From<$t> for JsValue {
            fn from(v: $t) -> JsValue { JsValue::from_f64(v as f64) }
        }
    )* };
}
__from_number!(i8, i16, i32, u8, u16, u32, f32, f64);

impl From<bool> for JsValue {
    fn from(v: bool) -> JsValue {
        JsValue::from_bool(v)
    }
}

impl From<&str> for JsValue {
    fn from(v: &str) -> JsValue {
        JsValue::from_str(v)
    }
}

impl Clone for JsValue {
    /// A second handle to the same JavaScript value.
    ///
    /// The value table holds references, so this allocates a table slot rather
    /// than copying the object: both handles denote the *same* JS value, and
    /// each frees its own slot on drop. That matches how JS references behave,
    /// and it is what lets generated bindings derive `Clone` on their newtypes.
    fn clone(&self) -> JsValue {
        // Runtime support import; the generated glue always provides it.
        #[link(wasm_import_module = "__wasm_lite")]
        unsafe extern "C" {
            #[link_name = "__wl_clone"]
            fn clone_handle(idx: u32) -> u32;
        }
        JsValue::__wl_from_abi(unsafe { clone_handle(self.idx) })
    }
}

impl fmt::Debug for JsValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsValue").field("idx", &self.idx).finish()
    }
}

impl Drop for JsValue {
    fn drop(&mut self) {
        // Runtime support import; the generated glue always provides it.
        #[link(wasm_import_module = "__wasm_lite")]
        unsafe extern "C" {
            #[link_name = "__wl_drop"]
            fn drop_handle(idx: u32);
        }
        unsafe { drop_handle(self.idx) }
    }
}
