// SPDX-License-Identifier: MIT OR Apache-2.0
//! An all-passing suite, to verify a clean run exits 0.

use wasm_lite::wasm_lite_test;

#[wasm_lite_test]
fn one_plus_one() {
    assert_eq!(1 + 1, 2);
}

#[wasm_lite_test]
fn strings_match() {
    assert_eq!("hi", "hi");
}

wasm_lite::test_main!();
