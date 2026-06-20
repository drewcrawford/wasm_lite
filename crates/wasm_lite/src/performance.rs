//! Bindings to the JavaScript `performance` global.

crate::import! {
    "performance" {
        fn now() -> f64;
    }
}
