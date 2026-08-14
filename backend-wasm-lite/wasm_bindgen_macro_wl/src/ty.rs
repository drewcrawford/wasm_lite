// SPDX-License-Identifier: MIT OR Apache-2.0
//! How a Rust type crosses the boundary, and the generics bookkeeping that
//! goes with it.

use crate::opts::is_ours;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use syn::{Ident, ReturnType, Type};

/// How a parameter or return value crosses the boundary.
pub(crate) enum Cross {
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
pub(crate) fn crosses_directly(ty: &Type) -> bool {
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

pub(crate) fn classify(ty: &Type) -> Cross {
    if crosses_directly(ty) {
        Cross::Direct
    } else {
        Cross::Handle
    }
}

/// Strip one layer of `&`.
pub(crate) fn deref_ty(ty: &Type) -> &Type {
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
pub(crate) fn clamped_inner(ty: &Type) -> Option<&Type> {
    generic_inner(deref_ty(ty), "Clamped")
}

/// Just the `#[cfg]`s, for items that must be gated but take no other
/// attributes — the per-binding shim module in particular. Without this the
/// wrapper is gated and its module is not, so paired declarations still
/// collide.
pub(crate) fn cfg_attrs(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
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
pub(crate) fn passthrough_attrs(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    attrs.iter().filter(|a| !is_ours(a.path())).collect()
}

// ---------------------------------------------------------------------------
// returns
// ---------------------------------------------------------------------------

/// The return type as the inner `import!` should see it.
pub(crate) fn shim_ret_ty(ty: &Type) -> TokenStream2 {
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
pub(crate) fn raise_ret(ty: &Type, value: TokenStream2) -> TokenStream2 {
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
pub(crate) fn callback_signature(ty: &Type) -> Option<Callback> {
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
        let inputs = p.inputs.iter().map(|input| input.ty.clone()).collect();
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
pub(crate) struct Callback {
    /// `Fn`, `FnMut` or `FnOnce` — kept so the `'static` retype below spells
    /// the same trait the caller declared.
    pub(crate) trait_name: Ident,
    pub(crate) mutable: bool,
    pub(crate) inputs: Vec<Type>,
    pub(crate) output: Option<Type>,
}

impl Callback {
    /// The declared type with an explicit `'static`, which is what the
    /// transmute has to name — inference cannot supply it.
    pub(crate) fn static_ty(&self) -> TokenStream2 {
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
pub(crate) fn unpack_arg(ty: &Type, i: usize) -> TokenStream2 {
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
pub(crate) fn pack_result(
    ret: Option<&Type>,
    call: TokenStream2,
) -> syn::Result<(TokenStream2, bool)> {
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
pub(crate) fn to_js_value(ty: &Type, value: TokenStream2) -> TokenStream2 {
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

/// Does `ty` mention the type parameter `name` anywhere?
pub(crate) fn mentions(ty: &Type, name: &Ident) -> bool {
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
pub(crate) fn split_generics(
    g: &syn::Generics,
    target: Option<&Type>,
) -> (syn::Generics, syn::Generics) {
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
pub(crate) fn wrapper_generics(g: &syn::Generics) -> (TokenStream2, TokenStream2) {
    if g.params.is_empty() {
        return (quote! {}, quote! {});
    }
    let params = g.params.iter().map(|p| match p {
        syn::GenericParam::Type(t) => {
            let mut t = t.clone();
            t.default = None;
            quote! { #t }
        }
        other => quote! { #other },
    });
    let where_clause = &g.where_clause;
    (quote! { <#(#params),*> }, quote! { #where_clause })
}

/// The type a constructor yields, looking through `Option`/`Result`.
pub(crate) fn constructed_ty(ty: &Type) -> &Type {
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
pub(crate) fn token_hash(tokens: &TokenStream2) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in tokens.to_string().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// A module-name-safe rendering of a type's last path segment.
pub(crate) fn type_ident(ty: &Type) -> String {
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

pub(crate) fn generic_inner<'a>(ty: &'a Type, name: &str) -> Option<&'a Type> {
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

pub(crate) fn generic_pair<'a>(ty: &'a Type, name: &str) -> Option<(&'a Type, &'a Type)> {
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
