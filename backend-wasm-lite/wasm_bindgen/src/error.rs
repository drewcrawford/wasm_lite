// SPDX-License-Identifier: MIT OR Apache-2.0
//! wasm-bindgen's `JsError`.

use crate::{JsObject, JsValue};

mod ctor {
    // `import!` names `JsValue` by bare ident, so it has to be in scope here
    // even though nothing in this module mentions it directly.
    #[allow(unused_imports)]
    use wasm_lite::JsValue;

    wasm_lite::import! {
        "Error" {
            /// `new Error(message)`
            #[constructor]
            fn new_error(message: &str) -> JsValue as "Error";
        }
    }
    pub(super) use new_error as make;
}

/// A JavaScript `Error`, for a binding that reports failure by throwing.
///
/// Upstream code names this type in signatures like
/// `Result<bool, JsError>`, so it exists mainly so those parse and convert.
#[derive(Debug)]
#[repr(transparent)]
pub struct JsError {
    obj: JsValue,
}

impl JsError {
    /// A real JS `Error` with this message — not a bare string, so it carries
    /// a stack the way a thrown error is expected to.
    pub fn new(message: &str) -> JsError {
        JsError {
            obj: ctor::make(message),
        }
    }
}

impl JsObject for JsError {
    fn as_js(&self) -> &JsValue {
        &self.obj
    }
    fn from_js(obj: JsValue) -> JsError {
        JsError { obj }
    }
    fn into_js(self) -> JsValue {
        self.obj
    }
}

impl From<JsError> for JsValue {
    fn from(e: JsError) -> JsValue {
        e.obj
    }
}
