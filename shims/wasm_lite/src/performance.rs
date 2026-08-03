// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings to the JavaScript `performance` global.

crate::import! {
    "performance" {
        /// Returns a high-resolution timestamp in milliseconds since the page load (`performance.now()`).
        fn now() -> f64;
    }
}
