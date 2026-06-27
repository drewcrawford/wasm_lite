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
