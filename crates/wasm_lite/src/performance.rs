// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings to the JavaScript `performance` global.

crate::import! {
    "performance" {
        /// Returns a high-resolution timestamp in milliseconds since the page load (`performance.now()`).
        fn now() -> f64;
        /// `performance.timeOrigin` — the Unix-epoch milliseconds at which this
        /// realm's `now()` was zero.
        ///
        /// **Every realm has its own.** A Web Worker's origin is the moment that
        /// worker started, so `now()` readings from two threads are on different
        /// timelines and cannot be compared. Adding `timeOrigin` puts them on the
        /// shared one, which is what any cross-thread deadline needs.
        #[static_getter]
        fn time_origin() -> f64 as "timeOrigin";
    }
}
