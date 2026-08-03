// SPDX-License-Identifier: MIT OR Apache-2.0
//! `wasm-bindgen-test`'s API, lowered onto wasm_lite's harness.
//!
//! Companion to the `wasm-bindgen` shim: an application that substitutes one
//! usually needs the other, because the crates it is pulling in have test
//! suites written against `#[wasm_bindgen_test]`.
//!
//! # What a consuming crate has to change
//!
//! One line per test target, in its manifest:
//!
//! ```toml
//! [[test]]
//! name = "browser"
//! harness = false
//! ```
//!
//! This is not something a shim can paper over. `wasm-bindgen-test` runs under
//! libtest's harness with a custom runner; wasm_lite's runner discovers tests
//! from a custom wasm section and needs to own `main`. The harness setting
//! lives in the manifest, and no macro can reach it.
//!
//! `wasm_bindgen_test_configure!` emits that `main`, so a suite that already
//! calls it at the crate root (most do, for `run_in_browser`) needs nothing
//! else.
//!
//! If the call sits inside a `mod` — `logwise`'s suites do — the `main` lands
//! in that module and the crate still has none. The compiler says so plainly
//! ("`main` function not found"); add [`wasm_bindgen_test_main!`] at the crate
//! root to fix it. The macro does not try to guess where it was invoked from.

#![deny(missing_docs)]

pub use wasm_bindgen_test_macro_wl::wasm_bindgen_test;

#[doc(hidden)]
pub mod __rt {
    pub use wasm_lite::{set_panic_hook, test_main, wasm_lite_test};
}

/// The test entry point, for a suite whose `wasm_bindgen_test_configure!` is
/// not at the crate root.
///
/// Place it at the top level of the test file.
#[macro_export]
macro_rules! wasm_bindgen_test_main {
    () => {
        $crate::__rt::test_main!();
    };
}

/// Accepts `wasm-bindgen-test`'s configuration and emits the test entry point.
///
/// The arguments — `run_in_browser` and friends — select between environments
/// wasm_lite does not have a choice about, so they are ignored. What the macro
/// *does* do is emit wasm_lite's `test_main!()`, which the harness needs and
/// which has no equivalent on the wasm-bindgen side.
///
/// That only works when the call is at the crate root. Inside a `mod`, use
/// [`wasm_bindgen_test_main!`] instead — see the [crate docs](crate).
#[macro_export]
macro_rules! wasm_bindgen_test_configure {
    ($($tt:tt)*) => {
        $crate::__rt::test_main!();
    };
}
