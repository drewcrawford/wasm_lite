// SPDX-License-Identifier: MIT OR Apache-2.0
//! A "third-party" library that happens to use wasm-bindgen. It knows nothing
//! about wasm_lite; it's linked into a wasm_lite binary to demonstrate interop.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

/// Logs through wasm-bindgen's own glue — proves the two systems link and run.
pub fn greet() {
    log("greet() — called through wasm-bindgen");
}

/// Mints a `JsValue` with wasm-bindgen, to be bridged into wasm_lite.
pub fn make_message() -> JsValue {
    JsValue::from_str("a JsValue minted by wasm-bindgen, logged by wasm_lite")
}
