//! Rust functions exported to JavaScript via `#[wasm_lite::export]`.

use wasm_lite::export;

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
