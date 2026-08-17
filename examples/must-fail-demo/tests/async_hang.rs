// SPDX-License-Identifier: MIT OR Apache-2.0
//! A `#[wasm_lite_test] async fn` whose body never completes. **Must fail.**
//!
//! The companion to `async_body.rs`, covering the other half of fail-closed. A
//! body that panics at least *reports* something; a body that never settles
//! reports nothing at all, so the only thing standing between it and a green run
//! is that the page was marked pending and never marked passed.
//!
//! This is the shape a dropped task takes too. If the generated entry point ever
//! goes back to letting `main` return decide the verdict, this fixture starts
//! passing — which is the alarm, not a fixture to update.
//!
//! `scripts/wasm32/negative` runs it under a short `WASM_LITE_TIMEOUT_SECS`, so
//! the failure arrives in seconds rather than at the 30s default.

#[wasm_lite::wasm_lite_test]
async fn an_async_body_that_never_completes_must_fail() {
    std::future::pending::<()>().await;
}
