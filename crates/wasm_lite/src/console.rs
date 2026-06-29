//! Bindings to the JavaScript `console` global.

use crate::JsValue;

crate::import! {
    "console" {
        /// Logs a string message to the browser console (`console.log`).
        fn log(msg: &str);
        /// Logs a string message to the browser console as an error (`console.error`).
        fn error(msg: &str);
        /// Logs a `JsValue` to the browser console (`console.log`).
        fn log_value(value: &JsValue) as "log";
    }
}
