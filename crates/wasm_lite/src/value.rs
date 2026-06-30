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
