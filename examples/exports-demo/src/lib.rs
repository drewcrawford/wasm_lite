//! Rust functions exported to JavaScript via `#[wasm_lite::export]`.

use wasm_lite::{export, JsValue};

// Imports this crate uses to build/manipulate JS objects it then hands back.
mod js {
    use wasm_lite::JsValue;
    wasm_lite::import! {
        "Array" {
            fn of3(a: f64, b: f64, c: f64) -> JsValue as "of"; // Array.of(a,b,c)
            fn push(this: &JsValue, value: f64) -> f64;         // arr.push(value)
        }
    }
}

#[export]
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[export]
pub fn is_even(n: i32) -> bool {
    n % 2 == 0
}

#[export]
pub fn scale(x: f64) -> f64 {
    x * 2.0
}

#[export]
pub fn greet(name: &str) -> String {
    format!("hello, {name}!")
}

#[export]
pub fn shout(s: &str) -> String {
    s.to_uppercase()
}

#[export]
pub fn sum_bytes(data: &[u8]) -> u32 {
    data.iter().map(|&b| b as u32).sum()
}

#[export]
pub fn make_bytes(n: i32) -> Vec<u8> {
    (0..n).map(|i| i as u8).collect()
}

// Returns a live JS object (a JS array) to the caller.
#[export]
pub fn make_array(a: f64, b: f64, c: f64) -> JsValue {
    js::of3(a, b, c)
}

// Takes a live JS object, mutates it via a method, and hands the same object
// back — exercising both a `JsValue` argument and return (and identity: the JS
// caller gets back the very object it passed).
#[export]
pub fn push_to(arr: JsValue, value: f64) -> JsValue {
    js::push(&arr, value);
    arr
}
