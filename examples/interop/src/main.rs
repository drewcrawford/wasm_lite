// SPDX-License-Identifier: MIT OR Apache-2.0
// One wasm module containing BOTH worlds: this bin uses wasm_lite, and links a
// library (`wb_widget`) that uses wasm-bindgen. A JsValue minted by wasm-bindgen
// is bridged into wasm_lite and logged through it.

use wasm_lite::interop::ToWasmLite;

fn main() {
    wasm_lite::console::log("interop: wasm_lite + a wasm-bindgen crate in one module");

    // Pure wasm-bindgen call — proves the two crates link and run together.
    wb_widget::greet();

    // Mint a JsValue in wasm-bindgen, bridge it to wasm_lite, log via wasm_lite.
    let wb_value = wb_widget::make_message(); // wasm_bindgen::JsValue
    let wl_value = wb_value.to_wasm_lite(); // -> wasm_lite::JsValue (the bridge)
    wasm_lite::console::log_value(&wl_value); // logged through wasm_lite
}
