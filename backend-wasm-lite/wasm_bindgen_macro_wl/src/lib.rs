// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `#[wasm_bindgen]` attribute, lowered onto wasm_lite.
//!
//! Reproduces the *grammar* `js-sys`/`web-sys`/`wgpu` are written in, and
//! translates it to `wasm_lite::import!`. Not a general-purpose crate: it is
//! only meant to be reached through the `wasm-bindgen` shim, which re-exports
//! it under the name upstream code expects.
//!
//! # Shape of the translation
//!
//! An extern type becomes a `#[repr(transparent)]` newtype over `JsValue`, and
//! each binding becomes a private module containing one `import!` plus a public
//! wrapper that converts newtypes to and from handles:
//!
//! ```text
//! #[wasm_bindgen]
//! extern "C" {
//!     pub type Element;
//!     #[wasm_bindgen(method, getter, js_class = "Element")]
//!     pub fn tag_name(this: &Element) -> String;
//! }
//! ```
//!
//! becomes (roughly)
//!
//! ```text
//! #[repr(transparent)] pub struct Element { obj: JsValue }
//! mod __wb_tag_name {
//!     import! { crate = ::wasm_bindgen::__rt;
//!         "Element" { #[getter] fn shim(this: &JsValue) -> String as "tagName"; } }
//! }
//! pub fn tag_name(this: &Element) -> String { __wb_tag_name::shim(this.as_js()) }
//! ```
//!
//! One module per binding keeps the wasm import symbol
//! (`module_path!() + "::shim"`) unique without having to group by namespace.
//!
//! # Unknown types are handles
//!
//! A parameter whose type is not a scalar, string, slice, `JsValue`, `Option`
//! or `Result` is assumed to be an extern-type newtype and crosses as a handle.
//! That is the right default for this ecosystem — 51.6% of web-sys's type
//! surface is extern types — and a type that is none of those would not have
//! had a wasm-bindgen lowering either.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Error, Item};

mod externs;
mod func;
mod opts;
mod ty;

use crate::externs::extern_block;
use crate::ty::passthrough_attrs;

/// `#[wasm_bindgen]` — see the [module docs](self).
#[proc_macro_attribute]
pub fn wasm_bindgen(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    match expand(attr, item.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let parsed: Item = syn::parse2(item)?;
    match parsed {
        Item::ForeignMod(fm) => {
            // The *block's* own attribute matters: `inline_js`/`module` say the
            // bindings live in a JS module rather than on `globalThis`.
            // Ignoring it does not fail loudly — it generates glue that looks
            // the functions up in the wrong place and throws at the first call,
            // which is how `wasm_safe_thread` failed before this check.
            check_block_attr(&attr)?;
            extern_block(fm)
        }
        Item::Fn(f) => {
            // `#[wasm_bindgen]` on a Rust fn exports it to JS, which is exactly
            // what wasm_lite's `#[export]` does.
            let _ = attr;
            Ok(quote! { #[::wasm_bindgen::__rt::export(crate = ::wasm_bindgen::__rt)] #f })
        }
        Item::Enum(e) => string_enum(e),
        other => Err(Error::new_spanned(
            other,
            "#[wasm_bindgen] supports `extern \"C\"` blocks, free functions and string \
             enums here",
        )),
    }
}

/// wasm-bindgen's *string enum*: `pub enum Key { Calendar = "calendar", .. }`.
///
/// The discriminants are string literals, which is not a Rust enum, so the
/// declaration has to be rewritten rather than passed through. The variants
/// become ordinary unit variants and the strings become a lookup — which keeps
/// the derives js-sys puts on these (`Clone, Copy, Debug, PartialEq, Eq`)
/// working, since the type carries no handle.
fn string_enum(e: syn::ItemEnum) -> syn::Result<TokenStream2> {
    let name = &e.ident;
    let vis = &e.vis;
    let attrs = passthrough_attrs(&e.attrs);

    let mut idents = Vec::new();
    let mut strings = Vec::new();
    let mut variants = Vec::new();
    for v in &e.variants {
        let Some((_, expr)) = &v.discriminant else {
            return Err(Error::new_spanned(
                v,
                "#[wasm_bindgen]: a string enum's variants each need a string discriminant",
            ));
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(sl),
            ..
        }) = expr
        else {
            return Err(Error::new_spanned(
                expr,
                "#[wasm_bindgen]: a string enum's discriminant must be a string literal",
            ));
        };
        let v_attrs = passthrough_attrs(&v.attrs);
        let id = &v.ident;
        variants.push(quote! { #(#v_attrs)* #id });
        idents.push(id.clone());
        strings.push(sl.clone());
    }

    Ok(quote! {
        #(#attrs)*
        #vis enum #name {
            #(#variants,)*
        }

        impl #name {
            /// The JavaScript string this variant denotes.
            pub fn to_js_str(&self) -> &'static str {
                match self {
                    #(#name::#idents => #strings,)*
                }
            }

            /// The variant a JavaScript string denotes, if any.
            pub fn from_js_str(s: &str) -> ::wasm_bindgen::__rt::core::option::Option<#name> {
                match s {
                    #(#strings => ::wasm_bindgen::__rt::core::option::Option::Some(#name::#idents),)*
                    _ => ::wasm_bindgen::__rt::core::option::Option::None,
                }
            }
        }

        // Allowed despite `JsValue` being foreign: the enum is local to the
        // crate this expands in.
        impl ::wasm_bindgen::__rt::core::convert::From<#name> for ::wasm_bindgen::__rt::JsValue {
            fn from(v: #name) -> ::wasm_bindgen::__rt::JsValue {
                ::wasm_bindgen::JsValue::from_str(v.to_js_str())
            }
        }

        impl ::wasm_bindgen::JsArg for #name {
            fn js_arg(&self) -> ::wasm_bindgen::JsArgRef<'_> {
                ::wasm_bindgen::JsArgRef::Owned(
                    ::wasm_bindgen::JsValue::from_str(self.to_js_str()),
                )
            }
        }

        impl ::wasm_bindgen::FromJs for #name {
            fn from_js_value(v: ::wasm_bindgen::__rt::JsValue) -> Self {
                let s = ::wasm_bindgen::JsValue::as_string(&v).unwrap_or_default();
                match #name::from_js_str(&s) {
                    ::wasm_bindgen::__rt::core::option::Option::Some(v) => v,
                    // The binding says the JS side only ever produces these
                    // strings; anything else is a contract violation worth
                    // naming rather than papering over with a default.
                    ::wasm_bindgen::__rt::core::option::Option::None => ::wasm_bindgen::__rt::core::panic!(
                        concat!("unexpected value for ", stringify!(#name), ": {:?}"),
                        s
                    ),
                }
            }
        }

        impl ::wasm_bindgen::__rt::core::fmt::Display for #name {
            fn fmt(&self, f: &mut ::wasm_bindgen::__rt::core::fmt::Formatter<'_>) -> ::wasm_bindgen::__rt::core::fmt::Result {
                f.write_str(self.to_js_str())
            }
        }
    })
}

/// Reject *any* argument on the block's own `#[wasm_bindgen(..)]`.
///
/// Block-level options change how every item inside is resolved —
/// `inline_js`/`module`/`raw_module` put the bindings in a JS module,
/// `js_namespace` moves them under a namespace. None are supported, and the
/// failure mode for ignoring one is not a missing feature but *wrong glue*
/// that throws on first call. `wasm_safe_thread` demonstrated exactly that.
///
/// So the rule is a whitelist of one: no arguments. `#[wasm_bindgen]` bare on
/// an extern block is the normal spelling, and anything else stops the build
/// with a message instead of shipping something broken.
fn check_block_attr(attr: &TokenStream2) -> syn::Result<()> {
    if attr.is_empty() {
        return Ok(());
    }
    Err(Error::new_spanned(
        attr,
        format!(
            "#[wasm_bindgen({attr})] on an `extern` block is not supported by the wasm_lite \
             shim. Block-level options change where every binding inside is resolved — \
             `inline_js`/`module`/`raw_module` place them in a JS module, `js_namespace` \
             moves them under a namespace — and wasm_lite's codegen resolves imports \
             against `globalThis`. Honouring them needs a codegen feature; ignoring them \
             would generate glue that throws on first call."
        ),
    ))
}
