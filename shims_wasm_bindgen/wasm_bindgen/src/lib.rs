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
//! Not yet handled — each an explicit error rather than silently wrong glue:
//! `variadic`, `module`/`raw_module`/`inline_js`, `start`, and multi-segment
//! `js_namespace`. Nor is there a `JsCast` implementation, so downcasting is
//! unavailable even though the `instanceof` primitive underneath it exists.
//! `js-sys` and `web-sys` have not been tried.

#![deny(missing_docs)]

pub use wasm_bindgen_macro_wl::wasm_bindgen;
pub use wasm_lite::JsValue;

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
}

impl JsObject for JsValue {
    fn as_js(&self) -> &JsValue {
        self
    }
    fn from_js(obj: JsValue) -> JsValue {
        obj
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
        Closure, FromSretPayload, JsFuture, JsValue, __wl_free, __wl_malloc, descriptor_bytes,
        export, import, js_class, set_panic_hook,
    };
}

/// The names wasm-bindgen users expect from `wasm_bindgen::prelude::*`.
pub mod prelude {
    pub use crate::{JsObject, JsValue, wasm_bindgen};
}
