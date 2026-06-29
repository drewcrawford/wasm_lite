//! Bindings to the JavaScript `Date` global.

// `now()` returns milliseconds elapsed since the Unix epoch (`Date.now()`).
crate::import! {
    "Date" {
        fn now() -> f64;
    }
}
