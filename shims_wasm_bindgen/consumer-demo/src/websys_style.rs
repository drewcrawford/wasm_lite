// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings written the way `web-sys` generates them.
//!
//! Everything here is `#[wasm_bindgen]` in the attribute grammar web-sys
//! actually emits — `extends`, `method`, `getter`, `setter`, `js_class`,
//! `js_name`, `constructor`, `static_method_of`, `catch`, `structural`/`final`
//! — with no hand-written `import!` anywhere. If this compiles and runs, the
//! shim is translating the grammar rather than just hosting the primitives.
//!
//! `URL` and `Array` stand in for real Web IDL types because they exist in
//! every engine, so the test can assert on real behaviour.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// A JS object, the root of the little hierarchy below.
    ///
    /// `js_name` is the JS class, which the Rust name need not match — web-sys
    /// renames constantly (`HtmlElement` ↔ `HTMLElement`). It is also what
    /// `JsCast` tests against, so omitting it makes every `instanceof` look up
    /// a global that does not exist and answer `false`.
    #[wasm_bindgen(js_name = Object)]
    #[derive(Debug)]
    pub type JsObjectBase;

    /// `URL`, deriving from [`JsObjectBase`] — the `extends` chain web-sys
    /// builds for every Web IDL interface.
    #[wasm_bindgen(extends = JsObjectBase, js_name = URL)]
    #[derive(Debug)]
    pub type Url;

    /// `new URL(spec)`
    #[wasm_bindgen(constructor, js_class = "URL")]
    pub fn new(spec: &str) -> Url;

    /// `url.pathname`
    #[wasm_bindgen(method, getter, structural, js_class = "URL")]
    pub fn pathname(this: &Url) -> String;

    /// `url.hash`
    #[wasm_bindgen(method, getter, structural, js_class = "URL")]
    pub fn hash(this: &Url) -> String;

    /// `url.hash = value`
    #[wasm_bindgen(method, setter = "hash", structural, js_class = "URL")]
    pub fn set_hash(this: &Url, value: &str);

    /// `url.toString()` — a plain method with a renamed JS target.
    #[wasm_bindgen(method, js_class = "URL", js_name = "toString")]
    pub fn to_string_js(this: &Url) -> String;
}

#[wasm_bindgen]
extern "C" {
    /// `Array`.
    #[wasm_bindgen(extends = JsObjectBase, js_name = Array)]
    #[derive(Debug)]
    pub type JsArray;

    /// `Array.of(a, b)` — a static method, i.e. hung off the class rather than
    /// an instance.
    #[wasm_bindgen(static_method_of = JsArray, js_name = "of", js_class = "Array")]
    pub fn of2(a: f64, b: f64) -> JsArray;

    /// `arr.length`
    #[wasm_bindgen(method, getter, js_class = "Array")]
    pub fn length(this: &JsArray) -> f64;

    /// `arr[i]`
    #[wasm_bindgen(method, indexing_getter, js_class = "Array")]
    pub fn get(this: &JsArray, index: u32) -> f64;

    /// `arr[i] = v`
    #[wasm_bindgen(method, indexing_setter, js_class = "Array")]
    pub fn set(this: &JsArray, index: u32, value: f64);
}

#[wasm_bindgen]
extern "C" {
    /// `JSON.parse`, bound with `catch` so a malformed document is an `Err`
    /// rather than a trap.
    #[wasm_bindgen(js_namespace = JSON, js_name = "parse", catch)]
    pub fn json_parse(text: &str) -> Result<JsValue, JsValue>;

    /// `JSON.stringify`, taking a handle.
    #[wasm_bindgen(js_namespace = JSON, js_name = "stringify")]
    pub fn json_stringify(value: &JsValue) -> String;

    /// Takes one of our newtypes rather than a bare handle, so the wrapper has
    /// to unwrap it.
    #[wasm_bindgen(js_namespace = JSON, js_name = "stringify")]
    pub fn stringify_array(value: &JsArray) -> String;
}
