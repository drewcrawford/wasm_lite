// SPDX-License-Identifier: MIT OR Apache-2.0
//! JS snippets linked into the binary.

use crate::JsValue;

mod imp {
    #[allow(unused_imports)]
    use wasm_lite::JsValue;

    wasm_lite::import! {
        "URL" {
            /// `URL.createObjectURL(blob)`
            fn create_object_url(blob: &JsValue) -> String as "createObjectURL";
        }
        "Blob" {
            /// `new Blob(parts, options)`
            #[constructor]
            fn new_blob(parts: &JsValue, options: &JsValue) -> JsValue as "Blob";
        }
        "Array" {
            /// `Array.of(v)`
            fn of(v: &JsValue) -> JsValue;
        }
        "JSON" {
            fn parse(text: &str) -> JsValue;
        }
    }
}

/// A URL serving `source` as JavaScript.
///
/// Backs [`link_to!`](crate::link_to). wasm-bindgen implements that by emitting
/// the snippet as a file next to the generated glue and returning its path,
/// which needs a build pipeline that knows about snippets. Embedding the text
/// and making a blob URL at runtime needs none: the source is already in the
/// wasm, and the URL is as good a `Worker` argument as a file path.
///
/// The blob is never revoked, matching wasm-bindgen's file, which is never
/// removed either — a snippet is linked for the life of the module.
#[doc(hidden)]
pub fn blob_url(source: &str) -> String {
    let parts = imp::of(&JsValue::from_str(source));
    let options = imp::parse(r#"{"type":"text/javascript"}"#);
    imp::create_object_url(&imp::new_blob(&parts, &options))
}

/// Link a JavaScript file into the binary and get a URL for it.
///
/// ```
/// # #[cfg(target_arch = "wasm32")]
/// # fn main() {
/// # wasm_bindgen::__rt::set_panic_hook();
/// let url = wasm_bindgen::link_to!(module = "/src/task/worker.js");
/// # assert!(url.starts_with("blob:"));
/// # }
/// # #[cfg(not(target_arch = "wasm32"))]
/// # fn main() {}
/// ```
///
/// The path is relative to the *calling* crate's root, as wasm-bindgen's is.
/// The file is embedded at compile time, so it travels with the wasm rather
/// than needing to be deployed beside it.
#[macro_export]
macro_rules! link_to {
    (module = $path:literal) => {
        $crate::snippet::blob_url(::core::include_str!(::core::concat!(
            ::core::env!("CARGO_MANIFEST_DIR"),
            $path
        )))
    };
}
