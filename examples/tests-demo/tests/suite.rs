//! Covers the three outcomes: a pass, a failed assertion, and an explicit panic.

use wasm_lite::wasm_lite_test;

#[wasm_lite_test]
fn passes() {
    assert_eq!(2 + 2, 4);
}

#[wasm_lite_test]
fn fails_assertion() {
    assert_eq!(2 + 2, 5, "math is definitely broken");
}

#[wasm_lite_test]
fn explicit_panic() {
    panic!("something went terribly wrong");
}

wasm_lite::test_main!();
