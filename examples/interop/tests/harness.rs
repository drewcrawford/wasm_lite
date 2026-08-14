// SPDX-License-Identifier: MIT OR Apache-2.0
//! `#[wasm_lite_test]` inside a wasm-bindgen interop module.
//!
//! Enabling wasm_lite's `wasm-bindgen` feature puts interop descriptors in the
//! module, which used to cost you `cargo test` entirely: the runner refused
//! interop modules in test mode. It no longer does, and this is what keeps that
//! true — the wasm-bindgen CLI has to preserve our `__wasm_lite_tests` section
//! and `__wl_test_*` exports for any of it to work.

use wasm_lite::interop::ToWasmLite;

#[wasm_lite::wasm_lite_test]
fn a_wasm_bindgen_call_works_inside_a_wasm_lite_test() {
    wb_widget::greet();
}

#[wasm_lite::wasm_lite_test]
fn the_jsvalue_bridge_works_inside_a_wasm_lite_test() {
    // The bridge is the reason the two worlds share a module at all, so it is
    // the thing most worth exercising from a test rather than only from `main`.
    let wl_value = wb_widget::make_message().to_wasm_lite();
    wasm_lite::console::log_value(&wl_value);
}

wasm_lite::test_main!();
