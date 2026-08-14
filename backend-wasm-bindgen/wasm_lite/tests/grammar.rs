// SPDX-License-Identifier: MIT OR Apache-2.0

//! Grammar coverage for the shim's binding macros.
//!
//! Every form here is taken from wasm_lite's own `docs/binding-model.md`. The
//! point is that a crate authored against the real `wasm_lite` compiles verbatim
//! against the shim — so this file must stay source-compatible with what the
//! real macros accept, and is a compile-time test above all.

use wasm_lite::JsValue;

// --- import!: namespaces, renames, methods, fallible calls ------------------

wasm_lite::import! {
    "Math" {
        /// Doc comments must survive.
        fn random() -> f64;
        fn max2(a: f64, b: f64) -> f64 as "max";
    }
    "JSON" {
        fn parse(text: &str) -> JsValue;
        fn stringify(value: &JsValue) -> String;
        /// `Result` return => wasm-bindgen `catch`.
        fn try_parse(text: &str) -> Result<f64, JsValue> as "parse";
    }
    "Array" {
        /// Leading `this:` means "call as a method on the handle".
        fn push(this: &JsValue, value: f64) -> f64;
    }
}

// A dotted namespace has to lower to wasm-bindgen's list form.
wasm_lite::import! {
    "Intl.NumberFormat" {
        fn supported_locales_of(locales: &JsValue) -> JsValue as "supportedLocalesOf";
    }
}

// Byte slices and Option arguments/returns.
wasm_lite::import! {
    "globalThis" {
        fn take_bytes(data: &[u8]);
        fn give_bytes() -> Vec<u8>;
        fn maybe(flag: Option<f64>) -> Option<String>;
    }
}

// --- #[export] --------------------------------------------------------------

#[wasm_lite::export]
pub fn greet(name: &str) -> String {
    format!("hello, {name}!")
}

#[wasm_lite::export]
pub fn divide(a: f64, b: f64) -> Result<f64, String> {
    if b == 0.0 {
        Err("division by zero".into())
    } else {
        Ok(a / b)
    }
}

#[wasm_lite::export]
pub fn sum(values: &[u8]) -> u32 {
    values.iter().map(|v| *v as u32).sum()
}

// --- js_class! --------------------------------------------------------------

wasm_lite::js_class! {
    type JsArray;
    impl JsArray {
        /// Scalar in, scalar out.
        fn push(&self, value: f64) -> f64;
        /// String argument and String return.
        fn join(&self, sep: &str) -> String;
        /// Typed argument and typed return.
        fn concat(&self, other: &JsArray) -> JsArray;
        /// Renamed method.
        fn index_of(&self, value: f64) -> f64 as "indexOf";
    }
}

// --- host-side assertions ---------------------------------------------------

/// The exported functions must be ordinary callable Rust on the host, so a leaf
/// crate's own native unit tests keep working under the shim.
#[test]
fn exports_are_callable_natively() {
    assert_eq!(greet("world"), "hello, world!");
    assert_eq!(divide(6.0, 2.0), Ok(3.0));
    assert_eq!(divide(1.0, 0.0), Err("division by zero".to_string()));
    assert_eq!(sum(&[1, 2, 3]), 6);
}
