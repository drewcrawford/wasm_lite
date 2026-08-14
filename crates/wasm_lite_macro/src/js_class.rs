// SPDX-License-Identifier: MIT OR Apache-2.0
//! Expansion for `js_class!`: typed `JsValue` newtype wrappers.
//!
//! Parses `type Class; impl Class { <methods> }` and emits the newtype plus one
//! method per declaration, delegating each method's ABI to a generated
//! [`import!`](macro@crate::import) block and adding only the typed veneer.

use crate::ty::*;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Error, Ident, LitStr, Token, Type, braced, parenthesized};

/// A parsed `js_class!`: `type Class; impl Class { <methods> }`.
pub(crate) struct JsClass {
    class: Ident,
    methods: Vec<JsMethod>,
}

/// A parsed method: `fn name(&self, params) -> ret as "js";`.
struct JsMethod {
    name: Ident,
    params: Vec<(Ident, Type)>,
    ret: Option<Type>,
    js: Option<String>,
}

impl Parse for JsClass {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<Token![type]>()?;
        let class: Ident = input.parse()?;
        input.parse::<Token![;]>()?;

        input.parse::<Token![impl]>()?;
        let class2: Ident = input.parse()?;
        if class2 != class {
            return Err(Error::new_spanned(
                &class2,
                format!("`impl {class2}` does not match `type {class}`"),
            ));
        }
        let body;
        braced!(body in input);
        let mut methods = Vec::new();
        while !body.is_empty() {
            methods.push(body.parse()?);
        }
        Ok(JsClass { class, methods })
    }
}

impl Parse for JsMethod {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<Token![fn]>()?;
        let name: Ident = input.parse()?;

        let args;
        parenthesized!(args in input);
        args.parse::<Token![&]>()?;
        args.parse::<Token![self]>()?;
        let mut params = Vec::new();
        while !args.is_empty() {
            args.parse::<Token![,]>()?;
            if args.is_empty() {
                break; // trailing comma
            }
            let pname: Ident = args.parse()?;
            args.parse::<Token![:]>()?;
            let ty: Type = args.parse()?;
            params.push((pname, ty));
        }

        let ret = if input.peek(Token![->]) {
            input.parse::<Token![->]>()?;
            Some(input.parse::<Type>()?)
        } else {
            None
        };
        let js = if input.peek(Token![as]) {
            input.parse::<Token![as]>()?;
            Some(input.parse::<LitStr>()?.value())
        } else {
            None
        };
        input.parse::<Token![;]>()?;

        Ok(JsMethod {
            name,
            params,
            ret,
            js,
        })
    }
}

pub(crate) fn build_js_class(class_def: &JsClass) -> syn::Result<TokenStream2> {
    // `js_class!` takes no crate-path override — it is wasm_lite's own
    // spelling, not something a shim re-exports — so this is always the
    // default.
    let krate: syn::Path = syn::parse_quote!(::wasm_lite);
    let class = &class_def.class;
    let module = format_ident!("__wl_class_{}", snake_case_ident(class));
    let class_lit = LitStr::new(&class.to_string(), Span::call_site());

    let mut wrappers: Vec<TokenStream2> = Vec::new();
    let mut import_decls: Vec<TokenStream2> = Vec::new();

    for m in &class_def.methods {
        let mname = &m.name;
        let mut imp_args = vec![quote! { this: &JsValue }];
        let mut wrap_params = Vec::new();
        let mut call_args = vec![quote! { self.as_js() }];

        for (n, ty) in &m.params {
            wrap_params.push(quote! { #n: #ty });
            match arg_kind(ty) {
                ArgKind::Passthrough => {
                    imp_args.push(quote! { #n: #ty });
                    call_args.push(quote! { #n });
                }
                ArgKind::ObjectRef => {
                    imp_args.push(quote! { #n: &JsValue });
                    call_args.push(quote! { #n.as_js() });
                }
                ArgKind::Unsupported => {
                    return Err(Error::new_spanned(
                        ty,
                        format!(
                            "js_class method `{mname}`: unsupported argument type `{}` (object args must be `&T`)",
                            type_string(ty)
                        ),
                    ));
                }
            }
        }

        let call = quote! { #module::#mname( #(#call_args),* ) };
        let (wrap_ret, imp_ret, body) = match &m.ret {
            None => (quote! {}, quote! {}, quote! { #call; }),
            Some(ty) if is_builtin_ret(ty) => {
                (quote! { -> #ty }, quote! { -> #ty }, quote! { #call })
            }
            // A typed object return: the import yields a handle; wrap it.
            Some(ty) => (
                quote! { -> #ty },
                quote! { -> JsValue },
                quote! { #ty::from_js(#call) },
            ),
        };

        let recv = if wrap_params.is_empty() {
            quote! { &self }
        } else {
            quote! { &self, #(#wrap_params),* }
        };
        wrappers.push(quote! { pub fn #mname(#recv) #wrap_ret { #body } });

        // A raw Rust identifier is only an escape hatch for the source parser;
        // the JavaScript property is named `type`, not `r#type`.
        let js = m.js.clone().unwrap_or_else(|| unraw(mname));
        let js_lit = LitStr::new(&js, Span::call_site());
        import_decls.push(quote! { fn #mname( #(#imp_args),* ) #imp_ret as #js_lit; });
    }

    Ok(quote! {
        pub struct #class(#krate::JsValue);
        impl #class {
            /// Wrap a `JsValue` as this type (unchecked — no runtime type test).
            pub fn from_js(v: #krate::JsValue) -> Self { #class(v) }
            /// Borrow the underlying handle.
            pub fn as_js(&self) -> &#krate::JsValue { &self.0 }
            /// Unwrap into the underlying handle.
            pub fn into_js(self) -> #krate::JsValue { self.0 }
            #(#wrappers)*
        }
        impl ::core::convert::From<#class> for #krate::JsValue {
            fn from(v: #class) -> Self { v.0 }
        }
        mod #module {
            use #krate::JsValue;
            ::wasm_lite::import! {
                #class_lit {
                    #(#import_decls)*
                }
            }
        }
    })
}

fn snake_case_ident(ident: &Ident) -> String {
    let raw = ident.to_string();
    let raw = raw.strip_prefix("r#").unwrap_or(&raw);
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    let mut prev: Option<char> = None;

    while let Some(ch) = chars.next() {
        if ch.is_uppercase() {
            let next = chars.peek().copied();
            let needs_sep = prev.is_some_and(|p| {
                p != '_'
                    && (p.is_lowercase()
                        || p.is_ascii_digit()
                        || next.is_some_and(char::is_lowercase))
            });
            if needs_sep {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
        prev = Some(ch);
    }

    out
}

/// How a `js_class!` method argument crosses into the underlying `import!` call.
enum ArgKind {
    /// A builtin (`&str`, `&[u8]`, `&JsValue`, numeric, `bool`): passed unchanged.
    Passthrough,
    /// A typed object handle (`&Foo`): lowered to `&JsValue` via `as_js()`.
    ObjectRef,
    Unsupported,
}

fn arg_kind(ty: &Type) -> ArgKind {
    if is_str(ty) || is_byte_slice(ty) || is_ref_jsvalue(ty) {
        return ArgKind::Passthrough;
    }
    if numeric(ty).is_some() || is_ident(ty, "bool") {
        return ArgKind::Passthrough;
    }
    if matches!(ty, Type::Reference(_)) {
        return ArgKind::ObjectRef;
    }
    ArgKind::Unsupported
}

/// Whether a return type is a builtin (marshalled by `import!`) vs a typed class.
fn is_builtin_ret(ty: &Type) -> bool {
    numeric(ty).is_some()
        || is_ident(ty, "bool")
        || is_ident(ty, "String")
        || is_ident(ty, "JsValue")
        || vec_u8(ty)
}

// ---------------------------------------------------------------------------
// Export/js_class helpers (type classification is shared via `crate::ty`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_class_unraws_default_javascript_method_names() {
        let class: JsClass = syn::parse_quote! {
            type Widget;
            impl Widget {
                fn r#type(&self) -> String;
            }
        };
        let output = build_js_class(&class).unwrap().to_string();
        assert!(output.contains("as \"type\""), "{output}");
        assert!(!output.contains("\"r#type\""), "{output}");
    }
}
