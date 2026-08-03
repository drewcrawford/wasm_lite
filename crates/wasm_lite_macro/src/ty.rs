// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared type classification and descriptor tags.
//!
//! All three macros (`#[export]`, `js_class!`, `import!`) agree on how a Rust
//! type maps to a wasm ABI category and its descriptor tag. The *marshalling*
//! code differs by direction (import lowers Rust→ABI, export raises ABI→Rust),
//! but the classification here is common.

use proc_macro2::Span;
use quote::ToTokens;
use syn::{Ident, LitByteStr, Type};

/// An ident's name with any raw-identifier prefix stripped (`r#type` → `type`).
///
/// Descriptor strings and JS names must never carry the Rust-only `r#` prefix:
/// `Display` for a raw ident includes it, which would generate glue referring
/// to a name that doesn't exist on the JS side.
pub(crate) fn unraw(ident: &Ident) -> String {
    let s = ident.to_string();
    s.strip_prefix("r#").map(str::to_string).unwrap_or(s)
}

/// The ident of a bare path type with no generics (e.g. `JsValue`, `bool`).
pub(crate) fn simple_ident(ty: &Type) -> Option<&Ident> {
    if let Type::Path(tp) = ty
        && tp.qself.is_none()
        && tp.path.segments.len() == 1
    {
        let seg = &tp.path.segments[0];
        if seg.arguments.is_empty() {
            return Some(&seg.ident);
        }
    }
    None
}

/// Whether `ty` is the bare path `name`.
pub(crate) fn is_ident(ty: &Type, name: &str) -> bool {
    simple_ident(ty).is_some_and(|id| id == name)
}

pub(crate) fn is_jsvalue(ty: &Type) -> bool {
    is_ident(ty, "JsValue")
}

pub(crate) fn is_ref_jsvalue(ty: &Type) -> bool {
    matches!(ty, Type::Reference(r) if is_jsvalue(&r.elem))
}

pub(crate) fn is_str(ty: &Type) -> bool {
    matches!(ty, Type::Reference(r) if is_ident(&r.elem, "str"))
}

pub(crate) fn is_byte_slice(ty: &Type) -> bool {
    if let Type::Reference(r) = ty
        && let Type::Slice(s) = &*r.elem
    {
        return is_ident(&s.elem, "u8");
    }
    false
}

pub(crate) fn vec_u8(ty: &Type) -> bool {
    generic1(ty, "Vec").is_some_and(|inner| is_ident(inner, "u8"))
}

/// `&[T]` for a numeric `T` other than `u8` → the element tag.
///
/// `&[u8]` is deliberately excluded: it already has its own `bytes` tag and
/// `Uint8Array` lowering, and giving it a second spelling would mean two
/// descriptor tags for one type.
///
/// Arguments only. The slice is a *borrowed view* of Rust's own memory, so the
/// element alignment is whatever Rust already guaranteed. A `Vec<T>` return
/// would be the other direction — the host allocating through `__wl_malloc`,
/// which is align-1 — and a `Float32Array` cannot view an unaligned offset, so
/// that direction needs an aligned allocator first.
pub(crate) fn numeric_slice(ty: &Type) -> Option<String> {
    let Type::Reference(r) = ty else { return None };
    let Type::Slice(s) = &*r.elem else {
        return None;
    };
    let id = simple_ident(&s.elem)?.to_string();
    matches!(
        id.as_str(),
        "i8" | "i16" | "u16" | "i32" | "u32" | "f32" | "f64"
    )
    .then_some(id)
}

/// A Rust numeric type → its descriptor tag; otherwise `None`.
///
/// Everything here fits a wasm `i32`, `f32` or `f64` parameter. `i64`/`u64` are
/// deliberately absent: they cross as wasm `i64`, which surfaces in JS as a
/// `BigInt` rather than a Number, so they need their own marshalling decision
/// rather than being folded in with the rest.
pub(crate) fn numeric(ty: &Type) -> Option<String> {
    let id = simple_ident(ty)?.to_string();
    matches!(
        id.as_str(),
        "i8" | "i16" | "i32" | "isize" | "u8" | "u16" | "u32" | "usize" | "f32" | "f64"
    )
    .then_some(id)
}

/// Descriptor tag for a scalar payload / `Option`/`Result` inner type:
/// `i32`/`u32`/`f64`, `bool`, `JsValue`→`handle`, `String`→`str`, `Vec<u8>`→`bytes`.
pub(crate) fn payload_tag(ty: &Type) -> Option<String> {
    if let Some(scalar) = numeric(ty) {
        return Some(scalar);
    }
    if is_ident(ty, "bool") {
        return Some("bool".into());
    }
    if is_jsvalue(ty) {
        return Some("handle".into());
    }
    if is_ident(ty, "String") {
        return Some("str".into());
    }
    if vec_u8(ty) {
        return Some("bytes".into());
    }
    None
}

/// If `ty` is `Name<Inner>` with exactly one type argument, return `Inner`.
pub(crate) fn generic1<'a>(ty: &'a Type, name: &str) -> Option<&'a Type> {
    let seg = last_segment(ty, name)?;
    if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments
        && ab.args.len() == 1
        && let syn::GenericArgument::Type(t) = &ab.args[0]
    {
        return Some(t);
    }
    None
}

/// If `ty` is `Name<A, B>`, return `(A, B)`.
pub(crate) fn generic2<'a>(ty: &'a Type, name: &str) -> Option<(&'a Type, &'a Type)> {
    let seg = last_segment(ty, name)?;
    if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
        let types: Vec<&Type> = ab
            .args
            .iter()
            .filter_map(|a| {
                if let syn::GenericArgument::Type(t) = a {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        if types.len() == 2 {
            return Some((types[0], types[1]));
        }
    }
    None
}

fn last_segment<'a>(ty: &'a Type, name: &str) -> Option<&'a syn::PathSegment> {
    if let Type::Path(tp) = ty {
        let seg = tp.path.segments.last()?;
        if seg.ident == name {
            return Some(seg);
        }
    }
    None
}

pub(crate) fn type_string(ty: &Type) -> String {
    ty.to_token_stream().to_string()
}

/// A `*b"<text>\n"` byte-string literal for a descriptor/section entry.
pub(crate) fn section_literal(text: &str) -> LitByteStr {
    LitByteStr::new(format!("{text}\n").as_bytes(), Span::call_site())
}
