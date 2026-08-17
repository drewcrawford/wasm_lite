// SPDX-License-Identifier: MIT OR Apache-2.0
//! A test binary that **must fail**, holding both kinds of test at once.
//!
//! This target keeps cargo's default `harness = true`, so libtest owns `main`
//! and owns the plain `#[test]` below. The `#[wasm_lite_test]` beside it is
//! registered in `__wasm_lite_tests` and driven through its own export instead.
//!
//! The runner used to treat those as alternatives — a non-empty test section
//! meant `main` was never called — so the plain `#[test]` did not run and the
//! binary reported `ok`. The same thing happened, far more often, to every
//! doctest in an edition-2024 merged bundle that contained one registered test.
//!
//! Driven by `scripts/wasm32/negative`, which requires this to fail *and* to say
//! why. See `docs/testing.md`.

// The panic hook is what carries the message out to the runner's console; a bare
// trap would fail the run without saying which body did it, and the fixture
// checks for the message specifically so that it cannot pass on the wrong error.
#[test]
fn a_plain_libtest_test_must_run_and_fail() {
    wasm_lite::set_panic_hook();
    panic!("PLAIN LIBTEST TEST RAN");
}

// Passes, and is the reason the whole binary used to report `ok`: its mere
// presence in `__wasm_lite_tests` is what sent the runner down the path that
// skips `main`.
#[wasm_lite::wasm_lite_test]
fn a_registered_test_passes() {}
