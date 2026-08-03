// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings to the JavaScript `console` global.

use crate::JsValue;

crate::import! {
    "console" {
        /// Logs a string message to the browser console (`console.log`).
        fn log(msg: &str);
        /// Logs a string message to the browser console as an error (`console.error`).
        fn error(msg: &str);
        /// `console.warn` — a warning.
        fn warn(msg: &str);
        /// `console.info` — informational.
        ///
        /// Distinct from [`log`] in devtools' level filter even though most
        /// browsers render the two identically.
        fn info(msg: &str);
        /// `console.debug` — hidden unless devtools' verbose level is enabled.
        fn debug(msg: &str);
        /// `console.trace` — logs the message **and a stack trace**.
        ///
        /// Note this is not "trace level": it is `log` plus a stack trace, which
        /// is much more expensive. A logging framework mapping a TRACE level
        /// here usually wants [`debug`] instead.
        fn trace(msg: &str);
        /// Logs a `JsValue` to the browser console (`console.log`).
        fn log_value(value: &JsValue) as "log";
    }
}
