// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings written the way an upstream crate would have them — reaching only
//! for `wasm-bindgen`.
//!
//! The point of this crate is its dependency list: `wasm-bindgen` and nothing
//! else. `js-sys`, `web-sys` and `wgpu` are in exactly this position, and the
//! shim is only useful if code here compiles *unmodified*. Every path the
//! expansion below produces resolves through `::wasm_bindgen::__rt`.

use wasm_bindgen::JsValue;

wasm_bindgen::__rt::import! {
    crate = ::wasm_bindgen::__rt;

    "Math" {
        /// `Math.max(a, b)`
        fn max2(a: f64, b: f64) -> f64 as "max";
    }

    "JSON" {
        fn parse(text: &str) -> JsValue;
        fn stringify(v: &JsValue) -> String;
    }

    "URL" {
        /// `new URL(spec)`
        #[constructor]
        fn new_url(spec: &str) -> JsValue as "URL";

        /// `url.pathname`
        #[getter]
        fn pathname(this: &JsValue) -> String;

        /// `url.hash = value`
        #[setter]
        fn set_hash(this: &JsValue, value: &str) as "hash";

        /// `x instanceof URL`
        #[instanceof]
        fn is_url(this: &JsValue) -> bool as "URL";
    }

    "Array" {
        #[getter]
        fn length(this: &JsValue) -> f64;
        #[indexing_getter]
        fn at(this: &JsValue, i: u32) -> f64;
        /// A typed-array view over wasm memory.
        fn from_f32(v: &[f32]) -> JsValue as "from";
    }
}

/// A throwing import bound as `Result`, i.e. wasm-bindgen's `catch`.
pub mod fallible {
    use wasm_bindgen::JsValue;

    wasm_bindgen::__rt::import! {
        crate = ::wasm_bindgen::__rt;
        "JSON" {
            fn try_parse(text: &str) -> Result<JsValue, JsValue> as "parse";
        }
    }
}

/// Round-trips a value through `JSON` to prove handles cross correctly.
pub fn round_trip(text: &str) -> String {
    stringify(&parse(text))
}
