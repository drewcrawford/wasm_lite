// SPDX-License-Identifier: MIT OR Apache-2.0
//! `#[wasm_bindgen_test]`, lowered onto wasm_lite's test harness.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Error, ItemFn};

/// Mark a function as a browser test.
///
/// Translates to `#[wasm_lite_test]`. The attribute's arguments
/// (`async`, `unsupported = ...`) are accepted and ignored: wasm_lite's runner
/// always drives a real browser, which is the only configuration
/// wasm-bindgen-test's arguments select between.
#[proc_macro_attribute]
pub fn wasm_bindgen_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = match syn::parse::<ItemFn>(item) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error().into(),
    };

    // Refused rather than silently mistranslated: wasm_lite's harness would
    // drop the future unpolled, so the test would always pass. Saying so here
    // is much clearer than the same error arriving from a macro the author did
    // not write.
    if let Some(asyncness) = &func.sig.asyncness {
        return Error::new_spanned(
            asyncness,
            "#[wasm_bindgen_test]: the wasm_lite shim does not support `async fn` tests — \
             wasm_lite's harness would drop the future unpolled and the test would always \
             pass. Drive the future to completion in a sync body \
             (`wasm_lite_std::async_doctest!`) instead.",
        )
        .to_compile_error()
        .into();
    }

    quote! {
        #[::wasm_bindgen_test::__rt::wasm_lite_test(crate = ::wasm_bindgen_test::__rt)]
        #func
    }
    .into()
}
