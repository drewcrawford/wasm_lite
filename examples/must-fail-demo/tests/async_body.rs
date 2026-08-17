// SPDX-License-Identifier: MIT OR Apache-2.0
//! A `#[wasm_lite_test] async fn` whose body panics. **Must fail.**
//!
//! `#[wasm_lite_test]` used to reject an `async fn` outright, on the grounds
//! that the generated entry point would build the future and drop it unpolled —
//! a test that could never fail. Now it drives the future instead, which means
//! the guarantee has to be re-established by a fixture rather than by the
//! absence of the feature.
//!
//! The panic happens *after* an await, so it lands on a later turn of the event
//! loop than the entry point that started it. That is the case that would
//! regress: the entry point has long since returned by then, and if `main`
//! returning were the verdict the page would already have reported `ok`.
//!
//! Driven by `scripts/wasm32/negative`. See `docs/testing.md`.

// No `set_panic_hook()` call here, deliberately — `#[wasm_lite_test]` installs
// it, and the fixture greps for the message, so its absence would show up as a
// failure without a message rather than as a pass.
#[wasm_lite::wasm_lite_test]
async fn an_async_body_that_panics_must_fail() {
    async {}.await;
    panic!("ASYNC TEST BODY PANICKED");
}
