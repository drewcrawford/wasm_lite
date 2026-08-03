// SPDX-License-Identifier: MIT OR Apache-2.0
//! `#[wasm_bindgen_test]`, lowered onto wasm_lite's test harness.

use proc_macro::TokenStream;
use quote::quote;
use syn::ItemFn;

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

    // An `async fn` cannot be handed to wasm_lite's harness directly — it would
    // drop the future unpolled and the test would always pass — so the body is
    // moved into a sync wrapper that drives it through an executor.
    if func.sig.asyncness.is_some() {
        let mut inner = func.clone();
        let name = func.sig.ident.clone();
        inner.sig.ident = syn::Ident::new("__wbt_body", name.span());
        inner.vis = syn::Visibility::Inherited;
        let attrs = &func.attrs;
        return quote! {
            #(#attrs)*
            #[::wasm_bindgen_test::__rt::wasm_lite_test(crate = ::wasm_bindgen_test::__rt)]
            fn #name() {
                #inner
                ::wasm_bindgen_test::__rt::run_async(__wbt_body());
            }
        }
        .into();
    }

    quote! {
        #[::wasm_bindgen_test::__rt::wasm_lite_test(crate = ::wasm_bindgen_test::__rt)]
        #func
    }
    .into()
}
