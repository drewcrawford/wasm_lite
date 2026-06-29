//! Bindings to the JavaScript `Date` global.

crate::import! {
    "Date" {
        /// Returns the number of milliseconds elapsed since the Unix epoch (`Date.now()`).
        fn now() -> f64;
    }
}
