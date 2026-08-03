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
//! Early. What works today is the **substitution mechanism** — the part
//! everything else depends on, and the part that was not obviously possible.
//! See [`__rt`]. The `#[wasm_bindgen]` attribute macro that translates
//! wasm-bindgen's grammar into these primitives is not written yet, so no
//! upstream crate compiles against this.

#![deny(missing_docs)]

pub use wasm_lite::JsValue;

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
    pub use crate::JsValue;
}
