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
//! calls it (most do, for `run_in_browser`) needs nothing else.

#![deny(missing_docs)]

pub use wasm_bindgen_test_macro_wl::wasm_bindgen_test;

#[doc(hidden)]
pub mod __rt {
    pub use wasm_lite::{set_panic_hook, test_main, wasm_lite_test};
}

/// Accepts `wasm-bindgen-test`'s configuration and emits the test entry point.
///
/// The arguments — `run_in_browser` and friends — select between environments
/// wasm_lite does not have a choice about, so they are ignored. What the macro
/// *does* do is emit wasm_lite's `test_main!()`, which the harness needs and
/// which has no equivalent on the wasm-bindgen side.
#[macro_export]
macro_rules! wasm_bindgen_test_configure {
    ($($tt:tt)*) => {
        $crate::__rt::test_main!();
    };
}
