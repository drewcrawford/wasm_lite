//! Bindings to the JavaScript `console` global.

crate::import! {
    "console" {
        fn log(msg: &str);
        fn error(msg: &str);
    }
}
