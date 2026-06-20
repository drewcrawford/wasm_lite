//! Bindings to the JavaScript `console` global.

use crate::JsValue;

crate::import! {
    "console" {
        fn log(msg: &str);
        fn error(msg: &str);
        fn log_value(value: &JsValue) as "log";
    }
}
