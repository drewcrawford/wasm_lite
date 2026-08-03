// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runs `consumer-demo`'s bindings in a real browser.
//!
//! Compiling proves the paths resolve; only running proves the descriptors the
//! shim's expansion emitted are the ones the codegen reads, and that the glue
//! calls the right JS.

use consumer_demo::{at, from_f32, is_url, length, max2, new_url, pathname, round_trip, set_hash};
use wasm_lite::wasm_lite_test;

#[wasm_lite_test]
fn a_namespaced_function_reaches_javascript() {
    assert_eq!(max2(3.0, 7.0), 7.0);
}

#[wasm_lite_test]
fn handles_round_trip_through_json() {
    assert_eq!(round_trip("[1,2,3]"), "[1,2,3]");
}

#[wasm_lite_test]
fn constructor_getter_and_setter() {
    let url = new_url("https://example.com/a/b");
    assert_eq!(pathname(&url), "/a/b");
    set_hash(&url, "frag");
    assert!(is_url(&url));
}

#[wasm_lite_test]
fn instanceof_discriminates() {
    let url = new_url("https://example.com/");
    let arr = from_f32(&[1.0, 2.0]);
    assert!(is_url(&url));
    assert!(!is_url(&arr));
}

#[wasm_lite_test]
fn typed_slices_and_indexing() {
    let arr = from_f32(&[1.5, 2.5, 3.5]);
    assert_eq!(length(&arr), 3.0);
    assert_eq!(at(&arr, 1), 2.5);
}

#[wasm_lite_test]
fn catch_maps_a_javascript_throw_to_err() {
    // `catch` in wasm-bindgen's grammar: a throwing call becomes `Err` instead
    // of taking the instance down.
    assert!(consumer_demo::fallible::try_parse("[1,2]").is_ok());
    assert!(consumer_demo::fallible::try_parse("{definitely not json").is_err());
}

wasm_lite::test_main!();
