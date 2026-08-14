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

// A `#[should_panic]` test traps the module. Each test gets a fresh page, so
// the poisoned instance is discarded either way; the runner is what turns the
// trap into a pass.
#[wasm_lite_test]
#[should_panic]
fn panicking_passes_when_expected() {
    panic!("this is supposed to happen");
}

// The expected message is matched against what the panic hook logged.
#[wasm_lite_test]
#[should_panic(expected = "supposed to happen")]
fn panic_message_is_matched() {
    panic!("this is supposed to happen");
}

// Skipped unless the runner is asked for ignored cases; it would fail if run,
// which is what makes the skip observable rather than merely claimed.
#[wasm_lite_test]
#[ignore = "asserts something false, to prove it is not run"]
fn ignored_is_not_run() {
    panic!("an #[ignore]d test was executed");
}

wasm_lite::test_main!();
