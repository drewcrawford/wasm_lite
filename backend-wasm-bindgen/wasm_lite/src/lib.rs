// SPDX-License-Identifier: MIT OR Apache-2.0

//! wasm-bindgen-backed shim for [`wasm_lite`](https://github.com/drewcrawford/wasm_lite).
//!
//! # Why this exists
//!
//! `wgpu`'s web backend is irreducibly wasm-bindgen/web-sys (WebGPU, WebGL,
//! canvas) and cannot be migrated off it in the near term. That leaves any app
//! that renders with wgpu — `images_and_words`, and therefore Metropolis —
//! unable to *host* wasm_lite, because running two binding systems in one
//! binary is an unsolved problem.
//!
//! This crate resolves that from the other direction. A leaf crate is authored
//! once against `wasm_lite`; an application that stays on wasm-bindgen patches
//! `wasm_lite` to this shim, so the leaf's `import!` / `#[export]` /
//! `js_class!` lower onto wasm-bindgen and the whole binary is an ordinary
//! wasm-bindgen build. No `wasm_lite` codegen step, no glue merge, no change to
//! the app's existing pipeline.
//!
//! The payoff for the leaf author is **one source tree, dual deployment**:
//! native wasm_lite, and — via this shim — the wasm-bindgen world.
//!
//! # How to use it
//!
//! In the *application* (not the leaf):
//!
//! ```toml
//! [patch.crates-io]
//! wasm_lite     = { path = "../wasm_lite/backend-wasm-bindgen/wasm_lite" }
//! wasm_lite_std = { path = "../wasm_lite/backend-wasm-bindgen/wasm_lite_std" }
//! ```
//!
//! Leaf crates depend on `wasm_lite` normally and need no conditional code.
//!
//! # What is deliberately not covered
//!
//! Impersonating wasm_lite's *own* worker spawning down to the
//! `__wl_spawn`/`wl_worker.js` protocol. [`thread::spawn`] here is backed by
//! `wasm_safe_thread` (the crate `wasm_lite_std` was itself ported from), which
//! is the correct primitive in a wasm-bindgen host but is not bit-compatible
//! with wasm_lite's own worker bootstrap. Code that reaches past the public API
//! into that protocol is out of scope.
//!
//! Test-only surface (`#[wasm_lite_test(worker)]`) maps onto `wasm_bindgen_test`,
//! which has no per-test worker concept; the argument is accepted and ignored.
//!
//! # The one thing a shimmed leaf must add
//!
//! A leaf that uses `#[wasm_lite_test]` needs `wasm-bindgen-test` as a wasm32
//! dev-dependency:
//!
//! ```toml
//! [target.'cfg(target_arch="wasm32")'.dev-dependencies]
//! wasm-bindgen-test = "0.3"
//! ```
//!
//! This cannot be hidden behind the shim. `#[wasm_lite_test]` expands to
//! `#[wasm_bindgen_test]`, whose *own* expansion emits absolute `::wasm_bindgen_test`
//! paths that must resolve in the leaf's crate root — re-exporting the macro from
//! here does not satisfy them. Since it is a dev-dependency it costs the leaf
//! nothing in a real build, and it is inert when the leaf builds against the
//! real wasm_lite.

pub mod console;
pub mod date;
pub mod performance;
pub mod thread;

pub use wasm_lite_macro_wb::{export, import, js_class, wasm_lite_test};

/// A handle to a JavaScript value.
///
/// The real `wasm_lite::JsValue` is an index into a host-side value table and is
/// `!Send`/`!Sync` because that table belongs to one realm. `wasm_bindgen`'s
/// `JsValue` has exactly the same ownership story and the same auto-trait
/// profile, so the shim re-exports it rather than wrapping it — which also means
/// a shimmed leaf interoperates directly with `js-sys`/`web-sys` values in the
/// hosting application.
pub use wasm_bindgen::JsValue;

/// Installs a panic hook that reports panics to the browser console.
///
/// wasm_lite's own hook routes panics to its runner; here there is no runner, so
/// the message goes to `console.error`, which is where a wasm-bindgen app's
/// panics are read from anyway.
pub fn set_panic_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prior = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            console::error(&format!("{info}"));
            prior(info);
        }));
    });
}
