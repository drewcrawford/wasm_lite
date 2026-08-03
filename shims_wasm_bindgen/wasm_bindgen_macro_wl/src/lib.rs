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
//! ```ignore
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
//! ```ignore
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
use quote::{ToTokens, format_ident, quote};
use syn::{
    Error, FnArg, ForeignItem, ForeignItemFn, ForeignItemType, Ident, Item, LitStr, Pat, PatType,
    Path, ReturnType, Type,
};

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

/// Reject the block-level options that change where bindings are looked up.
///
/// `inline_js`, `module` and `raw_module` all mean "these live in a JS module".
/// wasm_lite's codegen resolves every import against `globalThis`, so honouring
/// them needs a codegen feature, not a macro change — and pretending otherwise
/// produces glue that throws on first use.
fn check_block_attr(attr: &TokenStream2) -> syn::Result<()> {
    let text = attr.to_string();
    for opt in ["inline_js", "raw_module", "module"] {
        if text.contains(opt) {
            return Err(Error::new_spanned(
                attr,
                format!(
                    "#[wasm_bindgen({opt} = ..)] is not supported by the wasm_lite shim: it \
                     says these bindings live in a JS module, and wasm_lite's codegen \
                     resolves imports against `globalThis`. Supporting it needs a codegen \
                     feature; ignoring it would generate glue that throws on first call."
                ),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// attribute options
// ---------------------------------------------------------------------------

/// The `#[wasm_bindgen(...)]` arguments this shim understands.
#[derive(Default)]
struct Opts {
    method: bool,
    getter: bool,
    setter: bool,
    constructor: bool,
    catch: bool,
    indexing_getter: bool,
    indexing_setter: bool,
    indexing_deleter: bool,
    variadic: bool,
    thread_local: bool,
    js_name: Option<String>,
    js_class: Option<String>,
    js_namespace: Option<String>,
    static_method_of: Option<Ident>,
    extends: Vec<Path>,
    /// `is_type_of = <expr>` — a custom membership test, used where
    /// `instanceof` is wrong.
    is_type_of: Option<syn::Expr>,
}

impl Opts {
    /// Parse every `#[wasm_bindgen(..)]` attribute on an item, and report which
    /// of its attributes were consumed so the rest can be re-emitted.
    fn parse(attrs: &[syn::Attribute]) -> syn::Result<Opts> {
        let mut o = Opts::default();
        for a in attrs {
            if !is_ours(a.path()) {
                continue;
            }
            if matches!(a.meta, syn::Meta::Path(_)) {
                continue; // bare `#[wasm_bindgen]`
            }
            a.parse_nested_meta(|m| {
                let id = m
                    .path
                    .get_ident()
                    .ok_or_else(|| m.error("expected an identifier"))?
                    .clone();
                match id.to_string().as_str() {
                    "method" => o.method = true,
                    // `getter`, `getter = "name"` and `getter = name` are all
                    // in the wild; js-sys writes the bare-ident form.
                    "getter" => {
                        o.getter = true;
                        if m.input.peek(syn::Token![=]) {
                            o.js_name = Some(string_or_ident(&m)?);
                        }
                    }
                    "setter" => {
                        o.setter = true;
                        if m.input.peek(syn::Token![=]) {
                            o.js_name = Some(string_or_ident(&m)?);
                        }
                    }
                    "constructor" => o.constructor = true,
                    "catch" => o.catch = true,
                    "indexing_getter" => o.indexing_getter = true,
                    "indexing_setter" => o.indexing_setter = true,
                    "indexing_deleter" => o.indexing_deleter = true,
                    "js_name" => o.js_name = Some(string_or_ident(&m)?),
                    "js_class" => o.js_class = Some(string_or_ident(&m)?),
                    "static_method_of" => {
                        o.static_method_of = Some(m.value()?.parse::<Ident>()?);
                    }
                    "js_namespace" => o.js_namespace = Some(namespace(&m)?),
                    "extends" => o.extends.push(m.value()?.parse::<Path>()?),
                    // A *custom* membership test, replacing `instanceof`.
                    // Ignoring it is not cosmetic: js-sys uses it exactly where
                    // `instanceof` gives the wrong answer, because
                    // `"hi" instanceof String` is false for a primitive.
                    "is_type_of" => o.is_type_of = Some(m.value()?.parse()?),
                    // Accepted and ignored: these describe *how* wasm-bindgen
                    // looks a member up or what it emits for TypeScript, and
                    // wasm_lite's lowering already does the equivalent (a
                    // property lookup on the receiver) or has no TS output.
                    "structural" | "final" | "typescript_type" | "skip_typescript"
                    | "skip_jsdoc" | "getter_with_clone" | "no_deref" | "no_upcast"
                    | "no_promising" => {
                        if m.input.peek(syn::Token![=]) {
                            let _: syn::Expr = m.value()?.parse()?;
                        }
                    }
                    // Refused rather than ignored: silently dropping these
                    // generates glue that calls the wrong thing.
                    "variadic" => o.variadic = true,
                    "thread_local" | "thread_local_v2" => o.thread_local = true,
                    other @ ("module" | "raw_module" | "inline_js" | "start") => {
                        return Err(m.error(format!(
                            "#[wasm_bindgen({other})] is not supported by the wasm_lite shim yet"
                        )));
                    }
                    other => {
                        return Err(m.error(format!("unknown #[wasm_bindgen] argument `{other}`")));
                    }
                }
                Ok(())
            })?;
        }
        Ok(o)
    }
}

/// Is this attribute ours?
///
/// By the *last* segment, because the attribute is written fully qualified in
/// the wild — `#[wasm_bindgen::prelude::wasm_bindgen(method, ..)]`. Matching a
/// bare ident leaves those unconsumed, and they then re-expand on an item that
/// is no longer an extern block.
fn is_ours(path: &syn::Path) -> bool {
    path.segments
        .last()
        .is_some_and(|s| s.ident == "wasm_bindgen")
}

/// `js_name = "foo"` and `js_name = foo` are both in the wild.
fn string_or_ident(m: &syn::meta::ParseNestedMeta) -> syn::Result<String> {
    use syn::ext::IdentExt;
    let v = m.value()?;
    if v.peek(LitStr) {
        Ok(v.parse::<LitStr>()?.value())
    } else {
        // `parse_any`, not `parse`: JS member names are not constrained to Rust
        // idents, and js-sys really does write `js_name = match` and
        // `js_name = type`.
        Ok(Ident::parse_any(v)?.unraw().to_string())
    }
}

/// `js_namespace = Foo` or `js_namespace = ["Foo", "Bar"]`.
fn namespace(m: &syn::meta::ParseNestedMeta) -> syn::Result<String> {
    let v = m.value()?;
    if v.peek(syn::token::Bracket) {
        let content;
        syn::bracketed!(content in v);
        let parts: syn::punctuated::Punctuated<LitStr, syn::Token![,]> =
            content.parse_terminated(|p| p.parse::<LitStr>(), syn::Token![,])?;
        if parts.len() != 1 {
            // A dotted namespace would have to be `globalThis["a"]["b"]`, and
            // wasm_lite's codegen does a single lookup. Emitting it anyway
            // would produce glue that reads `globalThis["a.b"]`.
            return Err(Error::new_spanned(
                parts.first(),
                "the wasm_lite shim supports only a single-segment js_namespace",
            ));
        }
        Ok(parts[0].value())
    } else if v.peek(LitStr) {
        Ok(v.parse::<LitStr>()?.value())
    } else {
        Ok(v.parse::<Ident>()?.to_string())
    }
}

// ---------------------------------------------------------------------------
// extern blocks
// ---------------------------------------------------------------------------

/// Expand an `extern "C"` block, item by item.
///
/// A failing item becomes a `compile_error!` **in place** rather than aborting
/// the block. That matters more than it sounds: a block declares a type and
/// then its members, so failing the whole block over one unsupported method
/// deletes the type too, and every binding elsewhere that mentions it fails as
/// well. Most of the errors this shim reported against js-sys were that
/// cascade rather than distinct problems.
fn extern_block(fm: syn::ItemForeignMod) -> syn::Result<TokenStream2> {
    // The types this block declares. A namespaced free function whose
    // namespace *is* one of them is reached as an associated function —
    // js-sys writes `Uint8Array::copy_to_slice(..)` for a binding declared
    // with `js_namespace = Uint8Array`.
    let declared: Vec<Ident> = fm
        .items
        .iter()
        .filter_map(|i| match i {
            ForeignItem::Type(t) => Some(t.ident.clone()),
            _ => None,
        })
        .collect();

    let mut out = Vec::new();
    for item in fm.items {
        let expanded = match item {
            ForeignItem::Type(t) => extern_type(t),
            ForeignItem::Fn(f) => extern_fn(f, &declared),
            ForeignItem::Static(st) => extern_static(st),
            other => Err(Error::new_spanned(
                other,
                "#[wasm_bindgen]: only `type` and `fn` items are supported in an extern block",
            )),
        };
        out.push(expanded.unwrap_or_else(|e| e.to_compile_error()));
    }
    Ok(quote! { #(#out)* })
}

/// `pub type Foo;` → a transparent newtype over a handle.
fn extern_type(t: ForeignItemType) -> syn::Result<TokenStream2> {
    let opts = Opts::parse(&t.attrs)?;
    let name = &t.ident;
    let vis = &t.vis;
    let attrs = passthrough_attrs(&t.attrs);

    // js-sys declares generic extern types (`pub type Generator<T>;`), so the
    // parameters have to reach the struct. They are phantom — the handle is
    // untyped at the ABI — but `PhantomData` is a ZST, so `repr(transparent)`
    // still holds over the one real field.
    let generics = &t.generics;
    let (impl_g, ty_g, where_g) = t.generics.split_for_impl();
    let phantom = t.generics.type_params().map(|p| {
        let id = &p.ident;
        quote! { ::wasm_bindgen::__rt::core::marker::PhantomData<#id> }
    });
    let phantom_field = if t.generics.type_params().next().is_some() {
        quote! { _params: (#(#phantom,)*), }
    } else {
        quote! {}
    };
    let phantom_init = if t.generics.type_params().next().is_some() {
        quote! { _params: ::wasm_bindgen::__rt::core::default::Default::default(), }
    } else {
        quote! {}
    };

    // `extends = Base` gives the inherited API. One `Deref` target is possible
    // in Rust, so the first `extends` becomes `Deref` (which chains, so the
    // whole ancestry is reachable) and the rest become `AsRef`.
    // A type with no `extends` derefs to `JsValue`, so every chain bottoms out
    // there — which is what lets `JsValue::as_ref(&some_deep_type)` resolve.
    let root_deref = opts.extends.is_empty().then(|| {
        quote! {
            impl #impl_g ::wasm_bindgen::__rt::core::ops::Deref for #name #ty_g #where_g {
                type Target = ::wasm_bindgen::__rt::JsValue;
                fn deref(&self) -> &::wasm_bindgen::__rt::JsValue { &self.obj }
            }
        }
    });

    let deref = opts.extends.first().map(|base| {
        quote! {
            impl #impl_g ::wasm_bindgen::__rt::core::ops::Deref for #name #ty_g #where_g {
                type Target = #base;
                fn deref(&self) -> &#base {
                    // SAFETY: every type this macro generates is
                    // `#[repr(transparent)]` over the same `JsValue`, so the
                    // reference is valid at either type. The JS value really
                    // being a `#base` is the binding author's claim, exactly as
                    // it is under wasm-bindgen.
                    unsafe { &*(self as *const #name as *const #base) }
                }
            }
        }
    });
    let as_refs = opts.extends.iter().map(|base| {
        quote! {
            impl #impl_g ::wasm_bindgen::__rt::core::convert::AsRef<#base> for #name #ty_g #where_g {
                fn as_ref(&self) -> &#base {
                    // SAFETY: as above.
                    unsafe { &*(self as *const #name as *const #base) }
                }
            }
        }
    });

    // The JS class this type names, for `instanceof`. web-sys renames freely
    // (`pub type HtmlElement` ↔ `HTMLElement`), so `js_name` wins.
    let js_class = opts.js_name.clone().unwrap_or_else(|| name.to_string());
    let js_class_lit = LitStr::new(&js_class, name.span());
    let cast_mod = format_ident!("__wb_instanceof_{}", name);

    // `is_type_of` replaces the `instanceof` test entirely — that is the whole
    // point of it — so the binding is not emitted at all in that case.
    let (cast_mod_def, cast_test) = match &opts.is_type_of {
        Some(expr) => (
            quote! {},
            // Coerced through a `fn` pointer so a bare closure — js-sys writes
            // `is_type_of = |v| v.is_bigint()` — knows its parameter type.
            // Every such predicate is non-capturing, so the coercion holds.
            quote! {
                {
                    let __test: fn(&::wasm_bindgen::__rt::JsValue) -> bool = #expr;
                    __test(val)
                }
            },
        ),
        None => (
            quote! {
                #[allow(non_snake_case, unused_imports)]
                mod #cast_mod {
                    use ::wasm_bindgen::__rt::JsValue;
                    ::wasm_bindgen::__rt::import! {
                        crate = ::wasm_bindgen::__rt;
                        #js_class_lit {
                            #[instanceof]
                            fn shim(this: &JsValue) -> bool as #js_class_lit;
                        }
                    }
                }
            },
            quote! { #cast_mod::shim(val) },
        ),
    };

    Ok(quote! {
        #(#attrs)*
        #[repr(transparent)]
        #vis struct #name #generics {
            obj: ::wasm_bindgen::__rt::JsValue,
            #phantom_field
        }

        impl #impl_g ::wasm_bindgen::JsObject for #name #ty_g #where_g {
            fn as_js(&self) -> &::wasm_bindgen::__rt::JsValue { &self.obj }
            fn from_js(obj: ::wasm_bindgen::__rt::JsValue) -> Self {
                #name { obj, #phantom_init }
            }
            fn into_js(self) -> ::wasm_bindgen::__rt::JsValue { self.obj }
        }

        #cast_mod_def

        impl #impl_g ::wasm_bindgen::JsCast for #name #ty_g #where_g {
            fn instanceof(val: &::wasm_bindgen::__rt::JsValue) -> bool {
                #cast_test
            }
            fn unchecked_from_js(obj: ::wasm_bindgen::__rt::JsValue) -> Self {
                #name { obj, #phantom_init }
            }
        }

        // `JsValue::from(&x)` for this type. The blanket impl lives in
        // wasm_lite, since only the owning crate may implement `From` for
        // `JsValue`; this is the hook it keys on.
        impl #impl_g ::wasm_bindgen::JsArg for #name #ty_g #where_g {
            fn js_arg(&self) -> ::wasm_bindgen::JsArgRef<'_> {
                ::wasm_bindgen::JsArgRef::Borrowed(&self.obj)
            }
        }

        impl #impl_g ::wasm_bindgen::FromJs for #name #ty_g #where_g {
            fn from_js_value(obj: ::wasm_bindgen::__rt::JsValue) -> Self {
                #name { obj, #phantom_init }
            }
        }

        impl #impl_g ::wasm_bindgen::__rt::AsJsValue for #name #ty_g #where_g {
            fn as_js_value(&self) -> &::wasm_bindgen::__rt::JsValue { &self.obj }
        }

        // The conversions upstream code expects on every binding type.
        impl #impl_g ::wasm_bindgen::__rt::core::convert::AsRef<::wasm_bindgen::__rt::JsValue> for #name #ty_g #where_g {
            fn as_ref(&self) -> &::wasm_bindgen::__rt::JsValue { &self.obj }
        }

        impl #impl_g ::wasm_bindgen::__rt::core::convert::From<#name #ty_g> for ::wasm_bindgen::__rt::JsValue #where_g {
            fn from(v: #name #ty_g) -> ::wasm_bindgen::__rt::JsValue { v.obj }
        }

        /// Unchecked, matching wasm-bindgen. No explicit `TryFrom` alongside
        /// it: `From` induces the blanket one, and declaring both collides.
        /// Use `JsCast::dyn_into` for the checked conversion.
        impl #impl_g ::wasm_bindgen::__rt::core::convert::From<::wasm_bindgen::__rt::JsValue> for #name #ty_g #where_g {
            fn from(obj: ::wasm_bindgen::__rt::JsValue) -> Self { #name { obj, #phantom_init } }
        }

        #root_deref
        #deref
        #(#as_refs)*
    })
}

/// `pub static PI: f64;` — a constant on a JS namespace, e.g. `Math.PI`.
///
/// Emitted as a function rather than a Rust `static`, because reading it is a
/// call into JS: there is nothing to initialise at load time.
fn extern_static(st: syn::ForeignItemStatic) -> syn::Result<TokenStream2> {
    let opts = Opts::parse(&st.attrs)?;
    let attrs = passthrough_attrs(&st.attrs);
    let name = &st.ident;
    let vis = &st.vis;
    let ty = &st.ty;

    let js_name = opts.js_name.clone().unwrap_or_else(|| name.to_string());
    let namespace = opts
        .js_namespace
        .clone()
        .or_else(|| opts.js_class.clone())
        .unwrap_or_else(|| "globalThis".to_string());
    let ns_lit = LitStr::new(&namespace, name.span());
    let js_lit = LitStr::new(&js_name, name.span());
    let module = format_ident!("__wb_static_{}", name);

    let shim_ret = shim_ret_ty(ty);
    let body = raise_ret(ty, quote! { #module::shim() });
    let cfgs = cfg_attrs(&st.attrs);

    let shim_mod = quote! {
        #(#cfgs)*
        #[allow(non_snake_case, unused_imports)]
        mod #module {
            use ::wasm_bindgen::__rt::JsValue;
            ::wasm_bindgen::__rt::import! {
                crate = ::wasm_bindgen::__rt;
                #ns_lit {
                    #[static_getter]
                    fn shim() -> #shim_ret as #js_lit;
                }
            }
        }
    };

    if !opts.thread_local {
        return Ok(quote! {
            #shim_mod
            #(#attrs)*
            #[allow(non_snake_case)]
            #vis fn #name() -> #ty { #body }
        });
    }

    // `thread_local_v2` asks for a value accessed through `.with(|v| ..)`
    // rather than a call. The property is re-read on each access, which is the
    // honest reading: it lives in JS and could change.
    let holder = format_ident!("__WbThreadLocal_{}", name);
    Ok(quote! {
        #shim_mod

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #vis struct #holder;

        impl #holder {
            /// Read the JS property and hand it to `f`.
            pub fn with<R>(&self, f: impl ::wasm_bindgen::__rt::core::ops::FnOnce(&#ty) -> R) -> R {
                f(&{ #body })
            }
        }

        #(#attrs)*
        #[allow(non_upper_case_globals, non_snake_case)]
        #vis static #name: #holder = #holder;
    })
}

/// How a parameter or return value crosses the boundary.
enum Cross {
    /// Passes through unchanged (scalars, `&str`, slices, `JsValue`).
    Direct,
    /// A generated newtype: crosses as a handle.
    Handle,
}

/// Types the ABI carries without a newtype wrapper.
///
/// `JsValue` is deliberately *not* here. It is a handle like any other, and
/// `import!` only ever lends handles (`&JsValue`), so a by-value `JsValue`
/// argument — which js-sys writes — has to take the same borrow the generated
/// newtypes do.
fn crosses_directly(ty: &Type) -> bool {
    match ty {
        Type::Reference(r) => crosses_directly(&r.elem),
        // A slice of scalars is a typed-array view; a slice of anything else is
        // a run of handles, which needs the conversion below.
        Type::Slice(s) => crosses_directly(&s.elem),
        Type::Path(p) => {
            let Some(seg) = p.path.segments.last() else {
                return true;
            };
            // `Option<T>` is not itself a handle: `import!` flattens it to a
            // discriminant plus `T`'s parameters, so it crosses exactly when
            // `T` does.
            if seg.ident == "Option"
                && let syn::PathArguments::AngleBracketed(ab) = &seg.arguments
                && let Some(syn::GenericArgument::Type(inner)) = ab.args.first()
            {
                return crosses_directly(inner);
            }
            matches!(
                seg.ident.to_string().as_str(),
                "f32"
                    | "f64"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "usize"
                    | "isize"
                    | "bool"
                    | "str"
                    | "String"
                    | "Vec"
            )
        }
        _ => true,
    }
}

fn classify(ty: &Type) -> Cross {
    if crosses_directly(ty) {
        Cross::Direct
    } else {
        Cross::Handle
    }
}

/// Strip one layer of `&`.
fn deref_ty(ty: &Type) -> &Type {
    match ty {
        Type::Reference(r) => deref_ty(&r.elem),
        other => other,
    }
}

/// The type inside a `Clamped<T>`, if any.
///
/// `Clamped` marks a byte buffer as JS's clamped kind; the bytes themselves
/// cross the same way, so the wrapper is unwrapped for marshalling and put
/// back on the Rust side.
fn clamped_inner(ty: &Type) -> Option<&Type> {
    generic_inner(deref_ty(ty), "Clamped")
}

/// Just the `#[cfg]`s, for items that must be gated but take no other
/// attributes — the per-binding shim module in particular. Without this the
/// wrapper is gated and its module is not, so paired declarations still
/// collide.
fn cfg_attrs(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
        .collect()
}

/// Every attribute except our own.
///
/// `#[cfg]` in particular has to survive: js-sys declares the same function
/// twice under complementary `cfg`s, so dropping them makes both expand and
/// collide. User `#[derive]`s pass through for the same reason — they are the
/// author's, not ours to discard.
fn passthrough_attrs(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    attrs.iter().filter(|a| !is_ours(a.path())).collect()
}

// ---------------------------------------------------------------------------
// returns
// ---------------------------------------------------------------------------

/// The return type as the inner `import!` should see it.
fn shim_ret_ty(ty: &Type) -> TokenStream2 {
    if let Some(inner) = generic_inner(ty, "Clamped") {
        return shim_ret_ty(inner);
    }
    if let Some(inner) = generic_inner(ty, "Option") {
        let m = shim_ret_ty(inner);
        return quote! { ::wasm_bindgen::__rt::core::option::Option<#m> };
    }
    if let Some((ok, err)) = generic_pair(ty, "Result") {
        let o = shim_ret_ty(ok);
        let e = shim_ret_ty(err);
        return quote! { ::wasm_bindgen::__rt::core::result::Result<#o, #e> };
    }
    match classify(ty) {
        Cross::Direct => quote! { #ty },
        // Bare, not `::wasm_bindgen::__rt::JsValue`: `import!` recognises the
        // handle type by ident, and the generated module imports it.
        Cross::Handle => quote! { JsValue },
    }
}

/// Rebuild the declared return type from `value`, the shim's result.
fn raise_ret(ty: &Type, value: TokenStream2) -> TokenStream2 {
    if let Some(inner) = generic_inner(ty, "Clamped") {
        let inner_body = raise_ret(inner, value);
        return quote! { ::wasm_bindgen::Clamped(#inner_body) };
    }
    if let Some(inner) = generic_inner(ty, "Option") {
        let m = raise_ret(inner, quote!(__v));
        return quote! { ::wasm_bindgen::__rt::core::option::Option::map(#value, |__v| #m) };
    }
    if let Some((ok, err)) = generic_pair(ty, "Result") {
        let o = raise_ret(ok, quote!(__v));
        let e = raise_ret(err, quote!(__e));
        return quote! {
            match #value {
                ::wasm_bindgen::__rt::core::result::Result::Ok(__v) => ::wasm_bindgen::__rt::core::result::Result::Ok(#o),
                ::wasm_bindgen::__rt::core::result::Result::Err(__e) => ::wasm_bindgen::__rt::core::result::Result::Err(#e),
            }
        };
    }
    match classify(ty) {
        Cross::Direct => value,
        Cross::Handle => quote! { <#ty as ::wasm_bindgen::FromJs>::from_js_value(#value) },
    }
}

// ---------------------------------------------------------------------------
// callback arguments
// ---------------------------------------------------------------------------

/// `&mut dyn FnMut(A, B) -> R` (or `&dyn Fn(..)`) → its inputs and output.
///
/// js-sys passes callbacks this way — borrowed, for the duration of the call —
/// rather than as an owned `Closure`.
fn callback_signature(ty: &Type) -> Option<Callback> {
    let Type::Reference(r) = ty else { return None };
    let Type::TraitObject(obj) = &*r.elem else {
        return None;
    };
    for b in &obj.bounds {
        let syn::TypeParamBound::Trait(tb) = b else {
            continue;
        };
        let seg = tb.path.segments.last()?;
        if !matches!(seg.ident.to_string().as_str(), "FnMut" | "Fn" | "FnOnce") {
            continue;
        }
        let syn::PathArguments::Parenthesized(p) = &seg.arguments else {
            continue;
        };
        let inputs = p.inputs.iter().cloned().collect();
        let output = match &p.output {
            ReturnType::Default => None,
            ReturnType::Type(_, t) => Some((**t).clone()),
        };
        return Some(Callback {
            trait_name: seg.ident.clone(),
            mutable: r.mutability.is_some(),
            inputs,
            output,
        });
    }
    None
}

/// A callback parameter, decomposed.
struct Callback {
    /// `Fn`, `FnMut` or `FnOnce` — kept so the `'static` retype below spells
    /// the same trait the caller declared.
    trait_name: Ident,
    mutable: bool,
    inputs: Vec<Type>,
    output: Option<Type>,
}

impl Callback {
    /// The declared type with an explicit `'static`, which is what the
    /// transmute has to name — inference cannot supply it.
    fn static_ty(&self) -> TokenStream2 {
        let tr = &self.trait_name;
        let inputs = &self.inputs;
        let arrow = self
            .output
            .as_ref()
            .map(|o| quote! { -> #o })
            .unwrap_or_default();
        let obj = quote! { (dyn #tr( #(#inputs),* ) #arrow + 'static) };
        if self.mutable {
            quote! { &'static mut #obj }
        } else {
            quote! { &'static #obj }
        }
    }
}

/// Unpack `args[i]` into the callback's declared parameter type.
fn unpack_arg(ty: &Type, i: usize) -> TokenStream2 {
    let idx = syn::Index::from(i);
    let slot = quote! { __args[#idx] };
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
    {
        match seg.ident.to_string().as_str() {
            "bool" => return quote! { #slot.as_bool().unwrap_or_default() },
            "String" => return quote! { #slot.as_string().unwrap_or_default() },
            // Exact, not via `as_f64`: a JS number is a double and these
            // ranges exceed it.
            "i64" | "u64" | "i128" | "u128" => {
                return quote! {
                    <#ty as ::wasm_bindgen::__rt::core::convert::TryFrom<::wasm_bindgen::__rt::JsValue>>::try_from(
                        #slot.clone(),
                    )
                    .unwrap_or_default()
                };
            }
            n if matches!(
                n,
                "f32" | "f64" | "i8" | "i16" | "i32" | "u8" | "u16" | "u32" | "usize" | "isize"
            ) =>
            {
                // JS numbers are all doubles; the cast back is the same one
                // wasm-bindgen performs.
                return quote! { (#slot.as_f64().unwrap_or_default() as #ty) };
            }
            _ => {}
        }
    }
    // A handle. Cloned rather than moved: the slice owns the arguments and
    // frees them when the call returns.
    quote! { <#ty as ::wasm_bindgen::FromJs>::from_js_value(#slot.clone()) }
}

/// Convert the callback's result into what the trampoline returns.
///
/// `(body, fallible)`. A fallible body produces
/// `Result<Option<JsValue>, JsValue>`, which the trampoline turns into a thrown
/// JS exception; an infallible one produces `Option<JsValue>`.
fn pack_result(ret: Option<&Type>, call: TokenStream2) -> syn::Result<(TokenStream2, bool)> {
    let Some(ty) = ret else {
        return Ok((
            quote! { { #call; ::wasm_bindgen::__rt::core::option::Option::None } },
            false,
        ));
    };
    if matches!(ty, Type::Tuple(t) if t.elems.is_empty()) {
        return Ok((
            quote! { { #call; ::wasm_bindgen::__rt::core::option::Option::None } },
            false,
        ));
    }
    if let Some((ok, err)) = generic_pair(ty, "Result") {
        // The `Err` becomes a thrown JS exception, which is how the JS API
        // being bound reports failure in the first place.
        let (ok_body, _) = pack_result(Some(ok), quote!(__ok))?;
        let err_body = to_js_value(err, quote!(__err));
        return Ok((
            quote! {
                match #call {
                    ::wasm_bindgen::__rt::core::result::Result::Ok(__ok) => ::wasm_bindgen::__rt::core::result::Result::Ok(#ok_body),
                    ::wasm_bindgen::__rt::core::result::Result::Err(__err) => ::wasm_bindgen::__rt::core::result::Result::Err(#err_body),
                }
            },
            true,
        ));
    }
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
    {
        let n = seg.ident.to_string();
        if matches!(
            n.as_str(),
            "bool"
                | "f32"
                | "f64"
                | "i8"
                | "i16"
                | "i32"
                | "u8"
                | "u16"
                | "u32"
                | "usize"
                | "isize"
        ) {
            return Ok((
                quote! { ::wasm_bindgen::__rt::core::option::Option::Some(::wasm_bindgen::JsValue::from(#call)) },
                false,
            ));
        }
        if n == "String" {
            return Ok((
                quote! { ::wasm_bindgen::__rt::core::option::Option::Some(::wasm_bindgen::JsValue::from_str(&#call)) },
                false,
            ));
        }
        if n == "Vec" {
            // A run of handles becomes a JS array, which is what the JS caller
            // of the callback expects to receive.
            return Ok((
                quote! {
                    ::wasm_bindgen::__rt::core::option::Option::Some(
                        ::wasm_bindgen::JsValue::from_handles(&#call),
                    )
                },
                false,
            ));
        }
    }
    Ok((
        quote! { ::wasm_bindgen::__rt::core::option::Option::Some(::wasm_bindgen::JsObject::into_js(#call)) },
        false,
    ))
}

/// Lower a value of type `ty` to a `JsValue`, for the thrown-error path.
fn to_js_value(ty: &Type, value: TokenStream2) -> TokenStream2 {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
    {
        let n = seg.ident.to_string();
        if n == "JsValue" {
            return value;
        }
        if n == "String" {
            return quote! { ::wasm_bindgen::JsValue::from_str(&#value) };
        }
        if matches!(
            n.as_str(),
            "bool"
                | "f32"
                | "f64"
                | "i8"
                | "i16"
                | "i32"
                | "u8"
                | "u16"
                | "u32"
                | "usize"
                | "isize"
        ) {
            return quote! { ::wasm_bindgen::JsValue::from(#value) };
        }
    }
    quote! { ::wasm_bindgen::JsObject::into_js(#value) }
}

// ---------------------------------------------------------------------------
// imported functions
// ---------------------------------------------------------------------------

/// One imported function.
///
/// Emits a private module holding the `import!`, plus a wrapper that converts
/// newtypes to and from handles. The wrapper is an **inherent method** when the
/// attributes say so — `method`/`getter`/`setter` become `impl Recv { fn .. }`,
/// `constructor`/`static_method_of` become associated functions — because that
/// is what wasm-bindgen produces, and it is why callers write
/// `element.tag_name()` rather than `tag_name(&element)`.
fn extern_fn(f: ForeignItemFn, declared: &[Ident]) -> syn::Result<TokenStream2> {
    let opts = Opts::parse(&f.attrs)?;
    let name = &f.sig.ident;
    let vis = &f.vis;
    let attrs = passthrough_attrs(&f.attrs);

    // A constructor's JS name is the *class*, not the Rust function — every
    // web-sys constructor is spelled `fn new()`, and `new globalThis["new"]`
    // is a TypeError. Fall back to the constructed type's name when `js_class`
    // is absent, which is what wasm-bindgen does.
    let js_name = opts.js_name.clone().unwrap_or_else(|| {
        if opts.constructor {
            opts.js_class
                .clone()
                .unwrap_or_else(|| match &f.sig.output {
                    ReturnType::Type(_, ty) => type_ident(constructed_ty(ty)),
                    ReturnType::Default => name.to_string(),
                })
        } else {
            name.to_string().trim_start_matches("r#").to_string()
        }
    });

    // A `getter` hung off a class rather than an instance (`Symbol.iterator`)
    // reads a *namespaced* property, with nothing to read it from.
    let static_property = opts.getter && opts.static_method_of.is_some();

    let kind_attr = if opts.constructor {
        Some(quote!(#[constructor]))
    } else if static_property {
        Some(quote!(#[static_getter]))
    } else if opts.getter {
        Some(quote!(#[getter]))
    } else if opts.setter {
        Some(quote!(#[setter]))
    } else if opts.indexing_getter {
        Some(quote!(#[indexing_getter]))
    } else if opts.indexing_setter {
        Some(quote!(#[indexing_setter]))
    } else if opts.indexing_deleter {
        Some(quote!(#[indexing_deleter]))
    } else {
        None
    };

    // Anything that reads or writes a member of a receiver takes one.
    let takes_receiver = !static_property
        && (opts.method
            || opts.getter
            || opts.setter
            || opts.indexing_getter
            || opts.indexing_setter
            || opts.indexing_deleter);

    let mut wrapper_params = Vec::new();
    let mut shim_params = Vec::new();
    let mut call_args = Vec::new();
    // Statements that must run before the call (callback adapters).
    let mut prelude: Vec<TokenStream2> = Vec::new();
    let mut receiver_ty: Option<Type> = None;

    for (i, arg) in f.sig.inputs.iter().enumerate() {
        let FnArg::Typed(PatType { pat, ty, .. }) = arg else {
            return Err(Error::new_spanned(
                arg,
                "#[wasm_bindgen]: unexpected `self`",
            ));
        };
        let Pat::Ident(pi) = &**pat else {
            return Err(Error::new_spanned(
                pat,
                "#[wasm_bindgen]: parameters must be plain identifiers",
            ));
        };
        let orig_ident = &pi.ident;

        if takes_receiver && i == 0 {
            // The declared `this: &Foo` becomes `&self`, and wasm_lite spells
            // the receiver `this` on its side.
            receiver_ty = Some(deref_ty(ty).clone());
            wrapper_params.push(quote! { &self });
            shim_params.push(quote! { this: &JsValue });
            call_args.push(quote! { ::wasm_bindgen::JsObject::as_js(self) });
            continue;
        }

        wrapper_params.push(quote! { #orig_ident: #ty });

        // A borrowed callback becomes a variadic Closure for the duration of
        // the call.
        if let Some(cb) = callback_signature(ty) {
            let unpacked = cb.inputs.iter().enumerate().map(|(i, t)| unpack_arg(t, i));
            let (body, fallible) =
                pack_result(cb.output.as_ref(), quote! { __f( #(#unpacked),* ) })?;
            let ctor = if fallible {
                quote!(new_variadic_fallible)
            } else {
                quote!(new_variadic)
            };
            let closure_ident = format_ident!("__cb_{}", orig_ident);
            let static_ty = cb.static_ty();
            prelude.push(quote! {
                let #closure_ident = {
                    // SAFETY: the Closure is dropped at the end of this
                    // function, before the caller's borrow ends, so the
                    // 'static bound it requires is never actually observed.
                    let __f: #static_ty = unsafe { ::wasm_bindgen::__rt::core::mem::transmute(#orig_ident) };
                    ::wasm_bindgen::__rt::Closure::#ctor(
                        move |__args: &[::wasm_bindgen::__rt::JsValue]| #body,
                    )
                };
            });
            shim_params.push(quote! { #orig_ident: &JsValue });
            call_args.push(quote! { #closure_ident.as_js_value() });
            continue;
        }

        if let Some(inner) = clamped_inner(ty) {
            let by_ref = matches!(&**ty, Type::Reference(_));
            let shim_ty = if by_ref {
                quote! { &#inner }
            } else {
                quote! { #inner }
            };
            shim_params.push(quote! { #orig_ident: #shim_ty });
            call_args.push(if by_ref {
                quote! { &#orig_ident.0 }
            } else {
                quote! { #orig_ident.0 }
            });
            continue;
        }

        // `Option<&Element>` is a nullable *handle*: `import!` takes
        // `Option<&JsValue>`, so only the element type needs converting.
        if let Some(inner) = generic_inner(ty, "Option")
            && matches!(classify(deref_ty(inner)), Cross::Handle)
        {
            shim_params
                .push(quote! { #orig_ident: ::wasm_bindgen::__rt::core::option::Option<&JsValue> });
            let borrowed = if matches!(inner, Type::Reference(_)) {
                quote! { #orig_ident }
            } else {
                quote! { #orig_ident.as_ref() }
            };
            call_args.push(quote! {
                #borrowed.map(::wasm_bindgen::JsObject::as_js)
            });
            continue;
        }

        match classify(deref_ty(ty)) {
            Cross::Direct => {
                shim_params.push(quote! { #orig_ident: #ty });
                call_args.push(quote! { #orig_ident });
            }
            // A slice of newtypes: every element is `#[repr(transparent)]`
            // over a `JsValue`, so the run is already the layout `import!`
            // wants and only the element type has to be reinterpreted.
            Cross::Handle if matches!(deref_ty(ty), Type::Slice(_)) => {
                shim_params.push(quote! { #orig_ident: &[JsValue] });
                call_args.push(quote! {
                    // SAFETY: `T` is one of the `#[repr(transparent)]`
                    // newtypes this macro generates, so `[T]` and `[JsValue]`
                    // have the same layout.
                    unsafe {
                        ::wasm_bindgen::__rt::core::slice::from_raw_parts(
                            #orig_ident.as_ptr() as *const ::wasm_bindgen::__rt::JsValue,
                            #orig_ident.len(),
                        )
                    }
                });
            }
            Cross::Handle => {
                // Handles are always *lent* to an import, so the shim takes a
                // reference whether the declared parameter was one or not —
                // which means only a by-value parameter needs the extra `&`.
                shim_params.push(quote! { #orig_ident: &JsValue });
                let borrow = if matches!(&**ty, Type::Reference(_)) {
                    quote! { #orig_ident }
                } else {
                    quote! { &#orig_ident }
                };
                call_args.push(quote! { &*::wasm_bindgen::JsArg::js_arg(#borrow) });
            }
        }
    }

    // The namespace keys the import-object slot; for a method it is the class,
    // for a static the class the method hangs off, for a free function the JS
    // namespace (or the global object).
    let namespace = opts
        .js_class
        .clone()
        .or_else(|| opts.static_method_of.as_ref().map(|i| i.to_string()))
        .or_else(|| opts.js_namespace.clone())
        .unwrap_or_else(|| "globalThis".to_string());
    let ns_lit = LitStr::new(&namespace, name.span());
    let js_lit = LitStr::new(&js_name, name.span());

    // Where the wrapper hangs: the receiver for a method, the named class for a
    // static, the constructed type for a constructor.
    let impl_target: Option<Type> = if takes_receiver {
        receiver_ty
    } else if let Some(cls) = &opts.static_method_of {
        Some(syn::parse_quote!(#cls))
    } else if opts.constructor {
        match &f.sig.output {
            ReturnType::Type(_, ty) => Some(constructed_ty(ty).clone()),
            ReturnType::Default => None,
        }
    } else if let Some(ns) = &opts.js_namespace
        && let Some(id) = declared.iter().find(|d| *d == ns)
    {
        Some(syn::parse_quote!(#id))
    } else {
        None
    };

    // Module names must not collide when two classes share a member name.
    let bare = name.to_string();
    let bare = bare.trim_start_matches("r#");
    // Hashed, not just named: js-sys generates the same member on many types
    // from one macro, and `__wb_<Type>_<name>` still collides when the
    // generated `impl` targets share a spelling. The signature is what
    // actually distinguishes them.
    let disambiguator = token_hash(&quote! { #impl_target #ns_lit #js_lit #(#shim_params)* });
    let module = match &impl_target {
        Some(t) => format_ident!("__wb_{}_{}_{:x}", type_ident(t), bare, disambiguator),
        None => format_ident!("__wb_{}_{:x}", bare, disambiguator),
    };
    let call = quote! { #module::shim( #(#call_args),* ) };

    let (wrapper_ret, shim_ret, body) = match &f.sig.output {
        ReturnType::Default => (quote! {}, quote! {}, call),
        ReturnType::Type(_, ty) => (
            quote! { -> #ty },
            {
                let s = shim_ret_ty(ty);
                quote! { -> #s }
            },
            raise_ret(ty, call),
        ),
    };

    let variadic_attr = opts.variadic.then(|| quote!(#[variadic]));
    let cfgs = cfg_attrs(&f.attrs);
    let shim_mod = quote! {
        #(#cfgs)*
        #[allow(non_snake_case, unused_imports)]
        mod #module {
            use ::wasm_bindgen::__rt::JsValue;
            ::wasm_bindgen::__rt::import! {
                crate = ::wasm_bindgen::__rt;
                #ns_lit {
                    #kind_attr
                    #variadic_attr
                    fn shim( #(#shim_params),* ) #shim_ret as #js_lit;
                }
            }
        }
    };

    let (impl_generics, fn_generics) = split_generics(&f.sig.generics, impl_target.as_ref());
    let (generics, where_clause) = wrapper_generics(&fn_generics);
    let wrapper = quote! {
        #(#attrs)*
        #[allow(non_snake_case, clippy::too_many_arguments)]
        #vis fn #name #generics ( #(#wrapper_params),* ) #wrapper_ret #where_clause {
            #(#prelude)*
            #body
        }
    };

    Ok(match impl_target {
        Some(t) => {
            let (ig, _, _) = impl_generics.split_for_impl();
            quote! {
                #shim_mod
                impl #ig #t { #wrapper }
            }
        }
        None => quote! {
            #shim_mod
            #wrapper
        },
    })
}

/// Does `ty` mention the type parameter `name` anywhere?
fn mentions(ty: &Type, name: &Ident) -> bool {
    // Token comparison rather than a full visitor: these are extern-block
    // signatures, so the types are shallow and a parameter name cannot be
    // shadowed by anything that would make this wrong.
    ty.to_token_stream()
        .into_iter()
        .any(|t| matches!(&t, proc_macro2::TokenTree::Ident(i) if i == name))
        || matches!(ty, Type::Path(p) if p.path.segments.iter().any(|seg| {
            matches!(&seg.arguments, syn::PathArguments::AngleBracketed(ab)
                if ab.args.iter().any(|a| matches!(a, syn::GenericArgument::Type(t) if mentions(t, name))))
        }))
}

/// Split a binding's generics between the `impl` and the function.
///
/// A parameter the impl target mentions has to be declared on the *impl* —
/// `impl<T> Array<T>`, not `impl Array<T>` — while the rest stay on the
/// function. Getting this wrong is not subtle: `T` simply does not resolve,
/// and every use of the type cascades.
fn split_generics(g: &syn::Generics, target: Option<&Type>) -> (syn::Generics, syn::Generics) {
    let mut on_impl = syn::Generics::default();
    let mut on_fn = syn::Generics::default();
    for p in &g.params {
        let goes_on_impl = match (&target, p) {
            (Some(t), syn::GenericParam::Type(tp)) => mentions(t, &tp.ident),
            _ => false,
        };
        let mut p = p.clone();
        if let syn::GenericParam::Type(tp) = &mut p {
            // A default is legal in an extern block but not on a real function.
            tp.eq_token = None;
            tp.default = None;
        }
        if goes_on_impl {
            on_impl.params.push(p);
        } else {
            on_fn.params.push(p);
        }
    }
    (on_impl, on_fn)
}

/// A function's generics with any type-parameter *defaults* stripped.
///
/// `fn add<T: TypedArray = Int32Array>(..)` is accepted inside an `extern`
/// block but not on a real function, so the default has to go. Every generic
/// parameter here ends up bound by `JsObject` in practice, since the only
/// reason a binding is generic is to accept a family of handle types.
fn wrapper_generics(g: &syn::Generics) -> (TokenStream2, TokenStream2) {
    if g.params.is_empty() {
        return (quote! {}, quote! {});
    }
    let params = g.params.iter().map(|p| match p {
        syn::GenericParam::Type(t) => {
            let mut t = t.clone();
            t.eq_token = None;
            t.default = None;
            quote! { #t }
        }
        other => quote! { #other },
    });
    let where_clause = &g.where_clause;
    (quote! { <#(#params),*> }, quote! { #where_clause })
}

/// The type a constructor yields, looking through `Option`/`Result`.
fn constructed_ty(ty: &Type) -> &Type {
    if let Some(inner) = generic_inner(ty, "Option") {
        return constructed_ty(inner);
    }
    if let Some((ok, _)) = generic_pair(ty, "Result") {
        return constructed_ty(ok);
    }
    ty
}

/// A stable hash of a token stream, for disambiguating generated module names.
///
/// FNV-1a over the rendered tokens: a proc macro has no ambient counter, and
/// spans are not usable as identifiers, so the signature itself has to be what
/// makes the name unique.
fn token_hash(tokens: &TokenStream2) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in tokens.to_string().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// A module-name-safe rendering of a type's last path segment.
fn type_ident(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_else(|| "ty".into()),
        _ => "ty".into(),
    }
}

fn generic_inner<'a>(ty: &'a Type, name: &str) -> Option<&'a Type> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    if seg.ident != name {
        return None;
    }
    let syn::PathArguments::AngleBracketed(ab) = &seg.arguments else {
        return None;
    };
    if ab.args.len() != 1 {
        return None;
    }
    match &ab.args[0] {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    }
}

fn generic_pair<'a>(ty: &'a Type, name: &str) -> Option<(&'a Type, &'a Type)> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    if seg.ident != name {
        return None;
    }
    let syn::PathArguments::AngleBracketed(ab) = &seg.arguments else {
        return None;
    };
    let types: Vec<&Type> = ab
        .args
        .iter()
        .filter_map(|a| match a {
            syn::GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .collect();
    if types.len() == 2 {
        Some((types[0], types[1]))
    } else {
        None
    }
}
