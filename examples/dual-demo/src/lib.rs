// SPDX-License-Identifier: MIT OR Apache-2.0
//! One `#[wasm_lite_test]` suite, two targets.
//!
//! The library is empty on purpose; everything this example demonstrates is in
//! `tests/dual.rs`, which `scripts/native/tests` and `scripts/wasm32/tests` both
//! run. Comparing those two outputs is the demonstration: same file, same test
//! names, same verdicts, same skips — libtest on the host, the wasm_lite runner
//! in a browser.
