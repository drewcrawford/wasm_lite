// SPDX-License-Identifier: MIT OR Apache-2.0
//! `wasm-bindgen`'s API, lowered onto [`wasm_lite`].
//!
//! This is the **fake-wasm-bindgen** shim from `docs/design-notes.md`: rather
//! than reconciling two binding systems or rewriting `wgpu` by hand, replace
//! wasm-bindgen itself. An application substitutes this crate graph-wide with
//!
//! ```toml
//! [patch.crates-io]
//! wasm-bindgen = { path = ".../shims_wasm_bindgen/wasm_bindgen" }
//! ```
//!
//! and because `js-sys`, `web-sys`, `wasm-bindgen-futures` and `wgpu` are all
//! written *against* wasm-bindgen, the **unmodified upstream crates** then
//! compile on wasm_lite and emit wasm_lite descriptors. One `wasm-lite` codegen
//! pass covers the whole module, with no second binding system to subordinate.
//!
//! It is the mirror of `shims/`, which lowers wasm_lite's API onto
//! wasm-bindgen for an app that stays on wasm-bindgen.
//!
//! # Status
//!
//! Early, but past the interesting part. The substitution mechanism works (see
//! [`__rt`]), and [`macro@wasm_bindgen`] translates the attribute grammar
//! web-sys is written in: `extends`, `method`, `getter`, `setter`,
//! `constructor`, `static_method_of`, `indexing_getter`/`indexing_setter`,
//! `js_name`, `js_class`, `js_namespace`, `catch`, and the lookup hints
//! (`structural`, `final`) that wasm_lite's lowering already satisfies.
//!
//! `shims_wasm_bindgen/consumer-demo` exercises all of that in a browser from a
//! crate whose only dependency is this one.
//!
//! [`JsCast`] is generated per type, so `dyn_into`/`dyn_ref`/`is_instance_of`
//! work.
//!
//! Not yet handled — each an explicit error rather than silently wrong glue:
//! `variadic`, `module`/`raw_module`/`inline_js`, `start`, and multi-segment
//! `js_namespace`. `js-sys` and `web-sys` have not been tried.

#![deny(missing_docs)]

pub use wasm_bindgen_macro_wl::wasm_bindgen;
pub use wasm_lite::JsValue;

pub mod closure;
pub use closure::Closure;

/// The handle behind a generated extern-type newtype.
///
/// `#[wasm_bindgen] extern "C" { pub type Element; }` produces a
/// `#[repr(transparent)]` struct wrapping a [`JsValue`], and this is how the
/// generated wrappers get at it — a trait rather than field access, so the
/// conversion works across the module boundary each binding is generated into.
pub trait JsObject {
    /// The underlying handle, to lend to an import.
    fn as_js(&self) -> &JsValue;
    /// Wrap a handle returned by an import.
    fn from_js(obj: JsValue) -> Self
    where
        Self: Sized;
    /// Take the handle out, consuming the wrapper.
    fn into_js(self) -> JsValue
    where
        Self: Sized;
}

impl JsObject for JsValue {
    fn as_js(&self) -> &JsValue {
        self
    }
    fn from_js(obj: JsValue) -> JsValue {
        obj
    }
    fn into_js(self) -> JsValue {
        self
    }
}

/// Checked and unchecked downcasting between JS types.
///
/// The same shape as wasm-bindgen's trait of this name, because the point is
/// that upstream code calling `dyn_into`/`dyn_ref`/`is_instance_of` compiles
/// unchanged. `instanceof` is generated per type from the `#[instanceof]`
/// binding kind.
///
/// # Invariant
///
/// The reference conversions ([`JsCast::dyn_ref`], [`JsCast::unchecked_ref`])
/// reinterpret `&Self` as `&T`. That is sound only because every implementor is
/// `#[repr(transparent)]` over the same [`JsValue`] — which holds for the types
/// `#[wasm_bindgen]` generates, and is the same bargain wasm-bindgen makes.
/// Hand-writing an implementation for a type of a different shape breaks it.
pub trait JsCast: JsObject + Sized {
    /// Is this handle an instance of `Self`'s JS class?
    fn instanceof(val: &JsValue) -> bool;

    /// Wrap without checking.
    fn unchecked_from_js(val: JsValue) -> Self;

    /// Checked downcast, giving the original back on failure.
    fn dyn_into<T: JsCast>(self) -> Result<T, Self> {
        if T::instanceof(self.as_js()) {
            Ok(T::unchecked_from_js(self.into_js()))
        } else {
            Err(self)
        }
    }

    /// Checked downcast by reference.
    fn dyn_ref<T: JsCast>(&self) -> Option<&T> {
        if T::instanceof(self.as_js()) {
            // SAFETY: see the trait's invariant.
            Some(unsafe { &*(self as *const Self as *const T) })
        } else {
            None
        }
    }

    /// Is this value an instance of `T`?
    fn is_instance_of<T: JsCast>(&self) -> bool {
        T::instanceof(self.as_js())
    }

    /// Unchecked downcast by value.
    fn unchecked_into<T: JsCast>(self) -> T {
        T::unchecked_from_js(self.into_js())
    }

    /// Unchecked downcast by reference.
    fn unchecked_ref<T: JsCast>(&self) -> &T {
        // SAFETY: see the trait's invariant.
        unsafe { &*(self as *const Self as *const T) }
    }
}

impl JsCast for JsValue {
    /// Every JS value is a `JsValue`, so this is the one type the check is
    /// unconditionally true for.
    fn instanceof(_val: &JsValue) -> bool {
        true
    }
    fn unchecked_from_js(val: JsValue) -> JsValue {
        val
    }
}

/// The wasm_lite runtime, re-exported for generated code.
///
/// This module is why the shim can work at all.
///
/// `wasm_lite`'s macros emit absolute paths — `::wasm_lite::JsValue`,
/// `::wasm_lite::descriptor_bytes`, and so on — which is right when the calling
/// crate depends on wasm_lite, and fatal here: `js-sys`, `web-sys` and `wgpu`
/// depend on `wasm-bindgen` and have never heard of wasm_lite. Requiring them
/// to add a dependency would defeat the entire premise, which is that
/// *unmodified* upstream crates compile.
///
/// So `import!` takes an optional `crate = <path>;` prefix, and generated code
/// reaches the runtime through here instead:
///
/// ```ignore
/// wasm_bindgen::__rt::import! {
///     crate = ::wasm_bindgen::__rt;
///     "Math" { fn random() -> f64; }
/// }
/// ```
///
/// Every path in that expansion resolves through `wasm-bindgen`, so a crate
/// that depends only on this shim can host wasm_lite's lowering.
///
/// Not a stable API.
#[doc(hidden)]
pub mod __rt {
    pub use wasm_lite::{
        __wl_free, __wl_malloc, Closure, FromSretPayload, JsFuture, JsValue, descriptor_bytes,
        export, import, js_class, set_panic_hook,
    };
}

/// The names wasm-bindgen users expect from `wasm_bindgen::prelude::*`.
pub mod prelude {
    pub use crate::{JsCast, JsObject, JsValue, wasm_bindgen};
}
