// SPDX-License-Identifier: MIT OR Apache-2.0

//! wasm-bindgen-backed implementations of the `wasm_lite` binding macros.
//!
//! These reproduce the *grammar* of
//! [`wasm_lite`](https://github.com/drewcrawford/wasm_lite)'s `import!`, `#[export]`,
//! `js_class!` and `#[wasm_lite_test]`, but lower them to `#[wasm_bindgen]`
//! instead of wasm_lite's own ABI + descriptor sections. A crate authored
//! against wasm_lite therefore compiles unchanged inside an ordinary
//! wasm-bindgen build.
//!
//! This works because wasm_lite's binding surface is a strict subset of
//! wasm-bindgen's: every type wasm_lite marshals (`&str`/`String`,
//! `&[u8]`/`Vec<u8>`, `JsValue`, scalars, `Option<T>`, `Result<T, E>`) is a type
//! wasm-bindgen already marshals, so the translation is downward with no
//! missing features.
//!
//! Not a general-purpose crate: it is only meant to be reached through the
//! `wasm_lite` shim, which re-exports these under the names the real crate uses.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, FnArg, Ident, ItemFn, LitStr, Pat, PatType, Result as SynResult, ReturnType, Token,
    Type, Visibility, braced, parenthesized,
};

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Does this return type need wasm-bindgen's `catch`?
///
/// wasm_lite maps `Err(e)` to a *thrown* JS exception on both imports and
/// exports; wasm-bindgen spells that `catch` on the import side.
fn returns_result(ret: &ReturnType) -> bool {
    let ReturnType::Type(_, ty) = ret else {
        return false;
    };
    let Type::Path(p) = &**ty else { return false };
    p.path.segments.last().is_some_and(|s| s.ident == "Result")
}

/// The `js_namespace` attribute argument for a wasm_lite namespace string.
///
/// wasm_lite writes the JS namespace as a single string (`"Math"`, `"console"`,
/// and dotted paths like `"Foo.Bar"`). wasm-bindgen wants an ident or a list of
/// idents, so split on `.`.
fn js_namespace_arg(ns: &LitStr) -> TokenStream2 {
    let value = ns.value();
    let parts: Vec<&str> = value.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() == 1 {
        let id = format_ident!("{}", parts[0]);
        quote!(js_namespace = #id)
    } else {
        // The list form takes string literals, not idents.
        let lits = parts.iter().map(|p| LitStr::new(p, ns.span()));
        quote!(js_namespace = [#(#lits),*])
    }
}

/// True when the first parameter is the `this:` receiver wasm_lite uses to mean
/// "call this as a method on the given handle".
fn first_arg_is_this(args: &Punctuated<FnArg, Token![,]>) -> bool {
    let Some(FnArg::Typed(PatType { pat, .. })) = args.first() else {
        return false;
    };
    matches!(&**pat, Pat::Ident(p) if p.ident == "this")
}

// ---------------------------------------------------------------------------
// import!
// ---------------------------------------------------------------------------

/// One `fn name(args) -> Ret [as "jsName"];` line inside a namespace block.
struct ImportFn {
    attrs: Vec<Attribute>,
    name: Ident,
    args: Punctuated<FnArg, Token![,]>,
    ret: ReturnType,
    js_name: Option<LitStr>,
}

impl Parse for ImportFn {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        input.parse::<Token![fn]>()?;
        let name: Ident = input.parse()?;
        let content;
        parenthesized!(content in input);
        let args = content.parse_terminated(FnArg::parse, Token![,])?;
        let ret: ReturnType = input.parse()?;
        let js_name = if input.peek(Token![as]) {
            input.parse::<Token![as]>()?;
            Some(input.parse()?)
        } else {
            None
        };
        input.parse::<Token![;]>()?;
        Ok(ImportFn {
            attrs,
            name,
            args,
            ret,
            js_name,
        })
    }
}

/// A `"Namespace" { ... }` group.
struct ImportNamespace {
    namespace: LitStr,
    fns: Vec<ImportFn>,
}

impl Parse for ImportNamespace {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let namespace: LitStr = input.parse()?;
        let content;
        braced!(content in input);
        let mut fns = Vec::new();
        while !content.is_empty() {
            fns.push(content.parse()?);
        }
        Ok(ImportNamespace { namespace, fns })
    }
}

struct ImportInput {
    namespaces: Vec<ImportNamespace>,
}

impl Parse for ImportInput {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let mut namespaces = Vec::new();
        while !input.is_empty() {
            namespaces.push(input.parse()?);
        }
        Ok(ImportInput { namespaces })
    }
}

/// Binds JavaScript functions, grouped by namespace.
///
/// Accepts the same grammar as `wasm_lite::import!` and emits `#[wasm_bindgen]`
/// extern blocks. A leading `this: &JsValue` parameter becomes a `structural`
/// method call; `-> Result<..>` adds `catch`; `as "name"` becomes `js_name`.
#[proc_macro]
pub fn import(input: TokenStream) -> TokenStream {
    let parsed = syn::parse_macro_input!(input as ImportInput);
    let mut out = TokenStream2::new();

    for ns in &parsed.namespaces {
        let ns_arg = js_namespace_arg(&ns.namespace);
        for f in &ns.fns {
            let ImportFn {
                attrs,
                name,
                args,
                ret,
                js_name,
            } = f;

            // The JS-side name: explicit `as "..."` wins, else the Rust name.
            let js_name_lit = js_name
                .clone()
                .unwrap_or_else(|| LitStr::new(&name.to_string(), name.span()));

            let is_method = first_arg_is_this(args);
            let catch = returns_result(ret).then(|| quote!(catch,));

            // Bind under a hidden name, then re-expose a `pub` wrapper: the real
            // `import!` makes its bindings callable from outside the module, and
            // a wrapper is also where a non-`pub` extern signature gets widened.
            let extern_name = format_ident!("__wl_shim_import_{}", name);
            let params: Vec<&FnArg> = args.iter().collect();
            let arg_idents: Vec<TokenStream2> = args
                .iter()
                .map(|a| match a {
                    FnArg::Typed(PatType { pat, .. }) => quote!(#pat),
                    FnArg::Receiver(_) => quote!(self),
                })
                .collect();

            if is_method {
                // wasm-bindgen lowers a `method` import to an inherent `impl` on
                // the receiver type. Naming `JsValue` there would be an orphan
                // impl (E0116), since `JsValue` belongs to wasm-bindgen. So
                // declare a private extern type to own the impl and cast the
                // caller's `&JsValue` into it. `structural` keeps the call a
                // plain `receiver[name](args)` lookup, which is exactly what
                // wasm_lite's own lowering does.
                let recv_ty = format_ident!("__WlShimRecv{}", name);
                let rest: Vec<&FnArg> = params.iter().skip(1).copied().collect();
                let rest_idents: Vec<TokenStream2> = arg_idents.iter().skip(1).cloned().collect();

                out.extend(quote! {
                    #[allow(non_camel_case_types, non_snake_case)]
                    #[::wasm_bindgen::prelude::wasm_bindgen]
                    unsafe extern "C" {
                        #[::wasm_bindgen::prelude::wasm_bindgen(js_name = Object)]
                        type #recv_ty;

                        #[::wasm_bindgen::prelude::wasm_bindgen(method, structural, #catch js_name = #js_name_lit)]
                        fn #extern_name(this: &#recv_ty #(, #rest)*) #ret;
                    }

                    #(#attrs)*
                    #[inline]
                    pub fn #name(#(#params),*) #ret {
                        let __wl_recv: &#recv_ty =
                            ::wasm_bindgen::JsCast::unchecked_ref(this);
                        // wasm-bindgen exposes a `method` import as an inherent
                        // method on the receiver, not a free function.
                        __wl_recv.#extern_name(#(#rest_idents),*)
                    }
                });
            } else {
                out.extend(quote! {
                    #[allow(non_snake_case)]
                    #[::wasm_bindgen::prelude::wasm_bindgen]
                    unsafe extern "C" {
                        #[::wasm_bindgen::prelude::wasm_bindgen(#ns_arg, #catch js_name = #js_name_lit)]
                        fn #extern_name(#(#params),*) #ret;
                    }

                    #(#attrs)*
                    #[inline]
                    pub fn #name(#(#params),*) #ret {
                        #extern_name(#(#arg_idents),*)
                    }
                });
            }
        }
    }

    out.into()
}

// ---------------------------------------------------------------------------
// #[export]
// ---------------------------------------------------------------------------

/// Exports a Rust function to JavaScript.
///
/// The wasm_lite version emits a descriptor the `wasm-lite` CLI turns into glue;
/// here it is simply `#[wasm_bindgen]`, which produces the same callable export
/// in a wasm-bindgen build.
#[proc_macro_attribute]
pub fn export(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func = syn::parse_macro_input!(item as ItemFn);
    // wasm-bindgen requires exported functions to be public.
    if matches!(func.vis, Visibility::Inherited) {
        func.vis = syn::parse_quote!(pub);
    }
    quote! {
        #[::wasm_bindgen::prelude::wasm_bindgen]
        #func
    }
    .into()
}

// ---------------------------------------------------------------------------
// js_class!
// ---------------------------------------------------------------------------

/// One method line inside a `js_class!` impl block.
struct ClassMethod {
    attrs: Vec<Attribute>,
    name: Ident,
    /// Args after the `&self` receiver.
    args: Punctuated<FnArg, Token![,]>,
    ret: ReturnType,
    js_name: Option<LitStr>,
}

impl Parse for ClassMethod {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        input.parse::<Token![fn]>()?;
        let name: Ident = input.parse()?;
        let content;
        parenthesized!(content in input);
        let all = content.parse_terminated(FnArg::parse, Token![,])?;
        // Drop the `&self` receiver; the extern form takes an explicit `this`.
        let args: Punctuated<FnArg, Token![,]> = all
            .into_iter()
            .filter(|a| !matches!(a, FnArg::Receiver(_)))
            .collect();
        let ret: ReturnType = input.parse()?;
        let js_name = if input.peek(Token![as]) {
            input.parse::<Token![as]>()?;
            Some(input.parse()?)
        } else {
            None
        };
        input.parse::<Token![;]>()?;
        Ok(ClassMethod {
            attrs,
            name,
            args,
            ret,
            js_name,
        })
    }
}

struct JsClass {
    ty: Ident,
    js_class_name: Option<LitStr>,
    methods: Vec<ClassMethod>,
}

impl Parse for JsClass {
    fn parse(input: ParseStream) -> SynResult<Self> {
        input.parse::<Token![type]>()?;
        let ty: Ident = input.parse()?;
        let js_class_name = if input.peek(Token![as]) {
            input.parse::<Token![as]>()?;
            Some(input.parse()?)
        } else {
            None
        };
        input.parse::<Token![;]>()?;

        let mut methods = Vec::new();
        if input.peek(Token![impl]) {
            input.parse::<Token![impl]>()?;
            let _impl_ty: Ident = input.parse()?;
            let content;
            braced!(content in input);
            while !content.is_empty() {
                methods.push(content.parse()?);
            }
        }
        Ok(JsClass {
            ty,
            js_class_name,
            methods,
        })
    }
}

/// Declares a typed wrapper over a JavaScript object.
///
/// wasm_lite's version is a newtype over `JsValue` whose methods lower to
/// `receiver[name](args)`. wasm-bindgen's equivalent is an imported extern type
/// plus `structural` methods, so that is what this emits — with an inherent
/// `impl` on top so call sites keep writing `obj.method(..)`.
#[proc_macro]
pub fn js_class(input: TokenStream) -> TokenStream {
    let JsClass {
        ty,
        js_class_name,
        methods,
    } = syn::parse_macro_input!(input as JsClass);

    let class_lit = js_class_name.unwrap_or_else(|| LitStr::new(&ty.to_string(), ty.span()));

    let mut externs = TokenStream2::new();
    let mut inherents = TokenStream2::new();

    for m in &methods {
        let ClassMethod {
            attrs,
            name,
            args,
            ret,
            js_name,
        } = m;
        let js_name_lit = js_name
            .clone()
            .unwrap_or_else(|| LitStr::new(&name.to_string(), name.span()));
        let catch = returns_result(ret).then(|| quote!(catch,));
        let extern_name = format_ident!("__wl_shim_{}_{}", ty, name);

        let params: Vec<&FnArg> = args.iter().collect();
        let arg_idents: Vec<TokenStream2> = args
            .iter()
            .map(|a| match a {
                FnArg::Typed(PatType { pat, .. }) => quote!(#pat),
                FnArg::Receiver(_) => quote!(self),
            })
            .collect();

        externs.extend(quote! {
            #[::wasm_bindgen::prelude::wasm_bindgen(method, structural, #catch js_class = #class_lit, js_name = #js_name_lit)]
            fn #extern_name(this: &#ty, #(#params),*) #ret;
        });

        inherents.extend(quote! {
            #(#attrs)*
            #[inline]
            pub fn #name(&self, #(#params),*) #ret {
                // `method` imports arrive as inherent methods on the extern type.
                Self::#extern_name(self, #(#arg_idents),*)
            }
        });
    }

    quote! {
        #[::wasm_bindgen::prelude::wasm_bindgen]
        unsafe extern "C" {
            #[::wasm_bindgen::prelude::wasm_bindgen(js_name = #class_lit)]
            pub type #ty;

            #externs
        }

        impl #ty {
            #inherents
        }
    }
    .into()
}

// ---------------------------------------------------------------------------
// #[wasm_lite_test]
// ---------------------------------------------------------------------------

/// Marks an in-browser test.
///
/// wasm_lite runs these through its own runner; under the shim the crate is an
/// ordinary wasm-bindgen build, so they become `wasm_bindgen_test`s. The
/// `worker` argument wasm_lite accepts has no wasm-bindgen equivalent and is
/// accepted-and-ignored so the same source compiles both ways.
#[proc_macro_attribute]
pub fn wasm_lite_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);
    quote! {
        #[::wasm_bindgen_test::wasm_bindgen_test]
        #func
    }
    .into()
}
