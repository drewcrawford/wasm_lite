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
use quote::{format_ident, quote};
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
        Item::ForeignMod(fm) => extern_block(fm),
        Item::Fn(f) => {
            // `#[wasm_bindgen]` on a Rust fn exports it to JS, which is exactly
            // what wasm_lite's `#[export]` does.
            let _ = attr;
            Ok(quote! { #[::wasm_bindgen::__rt::export] #f })
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
            pub fn from_js_str(s: &str) -> ::core::option::Option<#name> {
                match s {
                    #(#strings => ::core::option::Option::Some(#name::#idents),)*
                    _ => ::core::option::Option::None,
                }
            }
        }

        impl ::core::fmt::Display for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.to_js_str())
            }
        }
    })
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
    js_name: Option<String>,
    js_class: Option<String>,
    js_namespace: Option<String>,
    static_method_of: Option<Ident>,
    extends: Vec<Path>,
}

impl Opts {
    /// Parse every `#[wasm_bindgen(..)]` attribute on an item, and report which
    /// of its attributes were consumed so the rest can be re-emitted.
    fn parse(attrs: &[syn::Attribute]) -> syn::Result<Opts> {
        let mut o = Opts::default();
        for a in attrs {
            if !a.path().is_ident("wasm_bindgen") {
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
                    "js_name" => o.js_name = Some(string_or_ident(&m)?),
                    "js_class" => o.js_class = Some(string_or_ident(&m)?),
                    "static_method_of" => {
                        o.static_method_of = Some(m.value()?.parse::<Ident>()?);
                    }
                    "js_namespace" => o.js_namespace = Some(namespace(&m)?),
                    "extends" => o.extends.push(m.value()?.parse::<Path>()?),
                    // Accepted and ignored: these describe *how* wasm-bindgen
                    // looks a member up or what it emits for TypeScript, and
                    // wasm_lite's lowering already does the equivalent (a
                    // property lookup on the receiver) or has no TS output.
                    "structural" | "final" | "typescript_type" | "skip_typescript"
                    | "skip_jsdoc" | "getter_with_clone" | "no_deref" | "is_type_of" => {
                        // `is_type_of` carries a closure; consume whatever the
                        // value is rather than trying to parse it.
                        if m.input.peek(syn::Token![=]) {
                            let _: syn::Expr = m.value()?.parse()?;
                        }
                    }
                    // Refused rather than ignored: silently dropping these
                    // generates glue that calls the wrong thing.
                    other @ ("variadic" | "module" | "raw_module" | "inline_js" | "start") => {
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
    let mut out = Vec::new();
    for item in fm.items {
        let expanded = match item {
            ForeignItem::Type(t) => extern_type(t),
            ForeignItem::Fn(f) => extern_fn(f),
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
        quote! { ::core::marker::PhantomData<#id> }
    });
    let phantom_field = if t.generics.type_params().next().is_some() {
        quote! { _params: (#(#phantom,)*), }
    } else {
        quote! {}
    };
    let phantom_init = if t.generics.type_params().next().is_some() {
        quote! { _params: ::core::default::Default::default(), }
    } else {
        quote! {}
    };

    // `extends = Base` gives the inherited API. One `Deref` target is possible
    // in Rust, so the first `extends` becomes `Deref` (which chains, so the
    // whole ancestry is reachable) and the rest become `AsRef`.
    let deref = opts.extends.first().map(|base| {
        quote! {
            impl #impl_g ::core::ops::Deref for #name #ty_g #where_g {
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
            impl #impl_g ::core::convert::AsRef<#base> for #name #ty_g #where_g {
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

        impl #impl_g ::wasm_bindgen::JsCast for #name #ty_g #where_g {
            fn instanceof(val: &::wasm_bindgen::__rt::JsValue) -> bool {
                #cast_mod::shim(val)
            }
            fn unchecked_from_js(obj: ::wasm_bindgen::__rt::JsValue) -> Self {
                #name { obj, #phantom_init }
            }
        }

        #deref
        #(#as_refs)*
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
        Type::Slice(_) => true,
        Type::Path(p) => {
            let Some(seg) = p.path.segments.last() else {
                return true;
            };
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
    attrs
        .iter()
        .filter(|a| !a.path().is_ident("wasm_bindgen"))
        .collect()
}

// ---------------------------------------------------------------------------
// returns
// ---------------------------------------------------------------------------

/// The return type as the inner `import!` should see it.
fn shim_ret_ty(ty: &Type) -> TokenStream2 {
    if let Some(inner) = generic_inner(ty, "Option") {
        let m = shim_ret_ty(inner);
        return quote! { ::core::option::Option<#m> };
    }
    if let Some((ok, err)) = generic_pair(ty, "Result") {
        let o = shim_ret_ty(ok);
        let e = shim_ret_ty(err);
        return quote! { ::core::result::Result<#o, #e> };
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
    if let Some(inner) = generic_inner(ty, "Option") {
        let m = raise_ret(inner, quote!(__v));
        return quote! { ::core::option::Option::map(#value, |__v| #m) };
    }
    if let Some((ok, err)) = generic_pair(ty, "Result") {
        let o = raise_ret(ok, quote!(__v));
        let e = raise_ret(err, quote!(__e));
        return quote! {
            match #value {
                ::core::result::Result::Ok(__v) => ::core::result::Result::Ok(#o),
                ::core::result::Result::Err(__e) => ::core::result::Result::Err(#e),
            }
        };
    }
    match classify(ty) {
        Cross::Direct => value,
        Cross::Handle => quote! { <#ty as ::wasm_bindgen::JsObject>::from_js(#value) },
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
    quote! { <#ty as ::wasm_bindgen::JsObject>::from_js(#slot.clone()) }
}

/// Convert the callback's result into what the trampoline returns.
///
/// `(body, fallible)`. A fallible body produces
/// `Result<Option<JsValue>, JsValue>`, which the trampoline turns into a thrown
/// JS exception; an infallible one produces `Option<JsValue>`.
fn pack_result(ret: Option<&Type>, call: TokenStream2) -> syn::Result<(TokenStream2, bool)> {
    let Some(ty) = ret else {
        return Ok((quote! { { #call; ::core::option::Option::None } }, false));
    };
    if matches!(ty, Type::Tuple(t) if t.elems.is_empty()) {
        return Ok((quote! { { #call; ::core::option::Option::None } }, false));
    }
    if let Some((ok, err)) = generic_pair(ty, "Result") {
        // The `Err` becomes a thrown JS exception, which is how the JS API
        // being bound reports failure in the first place.
        let (ok_body, _) = pack_result(Some(ok), quote!(__ok))?;
        let err_body = to_js_value(err, quote!(__err));
        return Ok((
            quote! {
                match #call {
                    ::core::result::Result::Ok(__ok) => ::core::result::Result::Ok(#ok_body),
                    ::core::result::Result::Err(__err) => ::core::result::Result::Err(#err_body),
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
                quote! { ::core::option::Option::Some(::wasm_bindgen::JsValue::from(#call)) },
                false,
            ));
        }
        if n == "String" {
            return Ok((
                quote! { ::core::option::Option::Some(::wasm_bindgen::JsValue::from_str(&#call)) },
                false,
            ));
        }
    }
    Ok((
        quote! { ::core::option::Option::Some(::wasm_bindgen::JsObject::into_js(#call)) },
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
fn extern_fn(f: ForeignItemFn) -> syn::Result<TokenStream2> {
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

    let kind_attr = if opts.constructor {
        Some(quote!(#[constructor]))
    } else if opts.getter {
        Some(quote!(#[getter]))
    } else if opts.setter {
        Some(quote!(#[setter]))
    } else if opts.indexing_getter {
        Some(quote!(#[indexing_getter]))
    } else if opts.indexing_setter {
        Some(quote!(#[indexing_setter]))
    } else {
        None
    };

    // Anything that reads or writes a member of a receiver takes one.
    let takes_receiver =
        opts.method || opts.getter || opts.setter || opts.indexing_getter || opts.indexing_setter;

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
                    let __f: #static_ty = unsafe { ::core::mem::transmute(#orig_ident) };
                    ::wasm_bindgen::__rt::Closure::#ctor(
                        move |__args: &[::wasm_bindgen::__rt::JsValue]| #body,
                    )
                };
            });
            shim_params.push(quote! { #orig_ident: &JsValue });
            call_args.push(quote! { #closure_ident.as_js_value() });
            continue;
        }

        match classify(deref_ty(ty)) {
            Cross::Direct => {
                shim_params.push(quote! { #orig_ident: #ty });
                call_args.push(quote! { #orig_ident });
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
                call_args.push(quote! { ::wasm_bindgen::JsObject::as_js(#borrow) });
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
    } else {
        None
    };

    // Module names must not collide when two classes share a member name.
    let bare = name.to_string();
    let bare = bare.trim_start_matches("r#");
    let module = match &impl_target {
        Some(t) => format_ident!("__wb_{}_{}", type_ident(t), bare),
        None => format_ident!("__wb_{}", bare),
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
                    fn shim( #(#shim_params),* ) #shim_ret as #js_lit;
                }
            }
        }
    };

    let (generics, where_clause) = wrapper_generics(&f.sig.generics);
    let wrapper = quote! {
        #(#attrs)*
        #[allow(non_snake_case, clippy::too_many_arguments)]
        #vis fn #name #generics ( #(#wrapper_params),* ) #wrapper_ret #where_clause {
            #(#prelude)*
            #body
        }
    };

    Ok(match impl_target {
        Some(t) => quote! {
            #shim_mod
            impl #t { #wrapper }
        },
        None => quote! {
            #shim_mod
            #wrapper
        },
    })
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
