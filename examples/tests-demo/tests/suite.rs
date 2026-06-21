//! A wasm_lite test suite. Built with `harness = false`; `#[wasm_lite_test]`
//! generates each test's entry point and `test_main!` supplies `fn main`.

use wasm_lite::{console, wasm_lite_test};

#[wasm_lite_test]
fn arithmetic_works() {
    assert_eq!(2 + 2, 4);
}

#[wasm_lite_test]
fn can_log() {
    console::log("can_log ran inside the test harness");
}

#[wasm_lite_test]
fn deliberately_fails() {
    assert_eq!(2 + 2, 5, "math is definitely broken");
}

wasm_lite::test_main!();
