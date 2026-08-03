// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings to the JavaScript timer globals.
//!
//! These take a callback as a `JsValue`, which in practice means a
//! [`Closure`](crate::Closure). Keeping the binding untyped avoids a second
//! closure-shaped API here; the typed convenience belongs in `wasm_lite_std`,
//! which can also own the waker plumbing.

use crate::JsValue;

crate::import! {
    "globalThis" {
        /// `setTimeout(handler, ms)` — returns the id needed to cancel it.
        ///
        /// The id is a Number in browsers and an object in Node; wasm_lite
        /// targets browsers, so `f64` is the faithful type.
        fn set_timeout(handler: &JsValue, ms: f64) -> f64 as "setTimeout";
        /// `clearTimeout(id)` — cancelling an already-fired or unknown id is
        /// defined to do nothing, so this needs no result.
        fn clear_timeout(id: f64) as "clearTimeout";
    }
}
