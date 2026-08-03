// SPDX-License-Identifier: MIT OR Apache-2.0
//! The non-call binding kinds, exercised against real browser objects.
//!
//! `import!` can express a JS *call* two ways (namespaced function, method on a
//! handle). These are the operations that are not calls — property read and
//! write, `new`, and computed indexing — which a binding surface like web-sys
//! needs for the majority of its API. They cannot be inferred from a Rust
//! signature, so each is requested with an attribute.
//!
//! Everything here runs against `URL` and `Array`, so a passing run means the
//! generated glue is right in a real engine rather than just matching a string
//! in a codegen unit test.

use wasm_lite::{JsValue, wasm_lite_test};

wasm_lite::import! {
    "URL" {
        /// `new URL(spec)`.
        #[constructor]
        fn new_url(spec: &str) -> JsValue as "URL";

        /// `url.pathname`
        #[getter]
        fn pathname(this: &JsValue) -> String;

        /// `url.hash`
        #[getter]
        fn hash(this: &JsValue) -> String;

        /// `url.hash = value`
        #[setter]
        fn set_hash(this: &JsValue, value: &str) as "hash";

        /// `url.toString()` — a plain method, for contrast with the getters.
        fn href(this: &JsValue) -> String as "toString";
    }

    "JSON" {
        fn parse(text: &str) -> JsValue;
    }

    "Array" {
        /// `arr[i]`
        #[indexing_getter]
        fn at(this: &JsValue, i: u32) -> f64;

        /// `arr[i] = value`
        #[indexing_setter]
        fn put(this: &JsValue, i: u32, value: f64);

        /// `arr.length` — the case that motivates getters: as a method call
        /// this would throw, because `length` is a number, not a function.
        #[getter]
        fn length(this: &JsValue) -> f64;
    }
}

#[wasm_lite_test]
fn constructor_builds_an_object() {
    let url = new_url("https://example.com/a/b?q=1");
    assert_eq!(href(&url), "https://example.com/a/b?q=1");
}

#[wasm_lite_test]
fn getter_reads_a_property() {
    let url = new_url("https://example.com/a/b");
    assert_eq!(pathname(&url), "/a/b");
}

#[wasm_lite_test]
fn setter_writes_a_property() {
    let url = new_url("https://example.com/a/b");
    assert_eq!(hash(&url), "", "no fragment to start with");
    set_hash(&url, "section-2");
    assert_eq!(hash(&url), "#section-2");
    // The write must be visible through an unrelated binding too, i.e. it
    // landed on the object rather than on a copy.
    assert_eq!(href(&url), "https://example.com/a/b#section-2");
}

#[wasm_lite_test]
fn getter_on_a_non_function_property() {
    let arr = parse("[10, 20, 30]");
    assert_eq!(length(&arr), 3.0);
}

#[wasm_lite_test]
fn indexing_reads_and_writes_elements() {
    let arr = parse("[10, 20, 30]");
    assert_eq!(at(&arr, 1), 20.0);
    put(&arr, 1, 99.0);
    assert_eq!(at(&arr, 1), 99.0);
    // Untouched neighbours stay put, so the index really was computed.
    assert_eq!(at(&arr, 0), 10.0);
    assert_eq!(at(&arr, 2), 30.0);
}

wasm_lite::test_main!();
