// SPDX-License-Identifier: MIT OR Apache-2.0
//! One attribute, both targets — the suite that proves it.
//!
//! Unlike this crate's other test targets, this one keeps cargo's default
//! `harness = true`. That is the whole point: off wasm32 `#[wasm_lite_test]`
//! registers with libtest, so `cargo test` runs exactly these cases on the host,
//! and `cargo test --target wasm32-unknown-unknown` runs them in a browser. The
//! `#[cfg_attr(not(target_arch = "wasm32"), test)]` pairing this crate's docs
//! used to teach is no longer needed.
//!
//! Run it both ways; the reported names, verdicts and skips should match.

use wasm_lite::wasm_lite_test;

#[wasm_lite_test]
fn sync_body_runs_on_both_targets() {
    assert_eq!(2 + 2, 4);
}

// An `async fn` is driven rather than rejected: `block_on` off wasm32, and on
// the event loop with a deferred verdict in the browser. A body that panics,
// hangs, or is dropped fails either way — `examples/must-fail-demo` holds the
// fixtures that check the failing direction, since a passing suite cannot.
#[wasm_lite_test]
async fn async_body_is_driven_to_completion() {
    let value = async { 41 }.await;
    assert_eq!(value + 1, 42);
}

#[wasm_lite_test]
async fn async_body_awaits_more_than_once() {
    let mut total = 0;
    for i in 1..=4 {
        total += async move { i }.await;
    }
    assert_eq!(total, 10);
}

// `#[should_panic]` and `#[ignore]` are read off the function by both harnesses,
// so one spelling covers both targets. On an `async fn` they describe the
// generated test rather than the body, which is where libtest looks for them.
#[wasm_lite_test]
#[should_panic(expected = "expected panic")]
async fn async_should_panic_is_honoured() {
    async {}.await;
    panic!("an expected panic");
}

#[wasm_lite_test]
#[ignore = "proves #[ignore] reaches the generated test"]
async fn async_ignored_does_not_run() {
    panic!("an ignored test must not run");
}

#[wasm_lite_test]
#[should_panic]
fn sync_should_panic_is_honoured() {
    panic!("also expected");
}

#[wasm_lite_test]
#[ignore]
fn sync_ignored_does_not_run() {
    panic!("an ignored test must not run");
}
