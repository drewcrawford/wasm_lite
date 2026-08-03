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

// ---------------------------------------------------------------------------
// The same surface, but written in web-sys's `#[wasm_bindgen]` grammar rather
// than hand-written `import!`. These are what show the *macro* works, not just
// the primitives underneath it.
// ---------------------------------------------------------------------------

mod websys_grammar {
    use consumer_demo::websys_style::*;
    use wasm_bindgen::JsValue;
    use wasm_lite::wasm_lite_test;

    #[wasm_lite_test]
    fn constructor_and_renamed_method() {
        let url = Url::new("https://example.com/a/b?q=1");
        assert_eq!(url.to_string_js(), "https://example.com/a/b?q=1");
    }

    #[wasm_lite_test]
    fn getter_and_setter_through_the_attribute_grammar() {
        let url = Url::new("https://example.com/a/b");
        assert_eq!(url.pathname(), "/a/b");
        assert_eq!(url.hash(), "");
        url.set_hash("section-2");
        assert_eq!(url.hash(), "#section-2");
        assert_eq!(url.to_string_js(), "https://example.com/a/b#section-2");
    }

    /// `extends` gives the inherited API through `Deref`, which is how every
    /// web-sys interface reaches its base.
    #[wasm_lite_test]
    fn extends_derefs_to_the_base_type() {
        let url = Url::new("https://example.com/");
        let base: &JsObjectBase = &url;
        // Reaching the base at all is the point; prove it is the same object.
        let via_base = json_stringify(wasm_bindgen::JsObject::as_js(base));
        let direct = json_stringify(wasm_bindgen::JsObject::as_js(&url));
        assert_eq!(via_base, direct);
    }

    #[wasm_lite_test]
    fn static_method_indexing_and_length() {
        let arr = JsArray::of2(10.0, 20.0);
        assert_eq!(arr.length(), 2.0);
        assert_eq!(arr.get(1), 20.0);
        arr.set(1, 99.0);
        assert_eq!(arr.get(1), 99.0);
        assert_eq!(arr.get(0), 10.0);
    }

    /// A newtype argument has to be unwrapped to a handle by the wrapper.
    #[wasm_lite_test]
    fn a_newtype_argument_crosses_as_a_handle() {
        let arr = JsArray::of2(1.0, 2.0);
        assert_eq!(stringify_array(&arr), "[1,2]");
    }

    #[wasm_lite_test]
    fn catch_maps_a_throw_to_err() {
        let ok = json_parse("[1,2]").expect("valid JSON parses");
        assert_eq!(json_stringify(&ok), "[1,2]");

        let err: Result<JsValue, JsValue> = json_parse("{definitely not json");
        assert!(err.is_err(), "malformed JSON must be Err, not a trap");
    }
}

wasm_lite::test_main!();
