// SPDX-License-Identifier: MIT OR Apache-2.0
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

// The two ways a `#[should_panic]` test fails. Both matter: without them a
// `#[should_panic]` that never panics would pass, which is the same
// can't-fail bug as an unpolled `async fn` body.
#[wasm_lite_test]
#[should_panic]
fn should_panic_but_does_not() {
    // Reported as "test did not panic as expected".
}

#[wasm_lite_test]
#[should_panic(expected = "a specific message")]
fn should_panic_with_the_wrong_message() {
    panic!("some other message");
}

wasm_lite::test_main!();
