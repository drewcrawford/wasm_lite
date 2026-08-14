// SPDX-License-Identifier: MIT OR Apache-2.0
//! Expansion for `#[wasm_lite::export]`: the flattened wasm export ABI.
//!
//! Builds the `extern "C"` entry point for an exported Rust function — argument
//! flattening, the direct vs. sret return paths — plus the matching descriptor
//! line recorded in the `__wl_exports` section.

use crate::ty::*;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Error, FnArg, Ident, ItemFn, Pat, ReturnType, Type};

/// JS reserved words (module/strict-mode context, where the glue runs). Legal
/// as Rust fn names — some only via `r#` — but a top-level
/// `export function <word>` is a SyntaxError that kills the whole glue module.
fn is_js_reserved_word(name: &str) -> bool {
    matches!(
        name,
        "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "interface"
            | "let"
            | "new"
            | "null"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
}

/// Validate a borrowed string/byte export parameter.
///
/// The JS glue owns a temporary argument buffer and frees it immediately after
/// the call. A `'static` reference could therefore be retained by otherwise
/// safe Rust and become dangling. Mutable references are not supportable either:
/// the glue copies JS data into that temporary buffer but has no way to copy
/// mutations back into the caller's original JS value.
fn validate_export_ref(ty: &Type) -> syn::Result<()> {
    if let Type::Reference(r) = ty {
        if let Some(lt) = &r.lifetime
            && lt.ident == "static"
        {
            return Err(Error::new_spanned(
                ty,
                "#[wasm_lite::export]: `&'static` arguments are unsound — the \
                 argument buffer is freed right after the call returns, so a \
                 'static reference would dangle. Use `&str`/`&[u8]` instead.",
            ));
        }
        if r.mutability.is_some() {
            return Err(Error::new_spanned(
                ty,
                "#[wasm_lite::export]: mutable string/byte references are not \
                 supported — arguments cross through a temporary buffer and \
                 mutations cannot be copied back to the JavaScript caller",
            ));
        }
    }
    Ok(())
}

/// Numeric types that have a direct (non-sret) export ABI implemented by the
/// host-side export parser and glue generator.
fn direct_export_numeric(ty: &Type) -> Option<String> {
    let scalar = numeric(ty)?;
    matches!(scalar.as_str(), "i32" | "u32" | "f64").then_some(scalar)
}

pub(crate) fn build_export(krate: &syn::Path, func: &ItemFn) -> syn::Result<TokenStream2> {
    // An `async fn` export would have its future constructed and dropped
    // unpolled by the shim — a silent no-op from the JS caller's perspective.
    if let Some(asyncness) = &func.sig.asyncness {
        return Err(Error::new_spanned(
            asyncness,
            "#[wasm_lite::export] does not support `async fn`: the shim would \
             drop the future without polling it, so the export would do nothing. \
             Export a sync fn that spawns the async work instead.",
        ));
    }
    // A generated wasm entry point has one concrete ABI. Type/const generics
    // cannot express that, while a lifetime generic can smuggle in a
    // `'static` bound and defeat the dangling-reference check below.
    if !func.sig.generics.params.is_empty() || func.sig.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &func.sig.generics,
            "#[wasm_lite::export] does not support generic functions: an export \
             must have one concrete ABI (and generic lifetime bounds could make \
             a temporary argument borrow effectively `'static`)",
        ));
    }
    // The generated entry point is callable by arbitrary JavaScript and is
    // intentionally safe. It cannot uphold an unsafe function's preconditions.
    if let syn::Safety::Unsafe(unsafe_token) = &func.sig.safety {
        return Err(Error::new_spanned(
            unsafe_token,
            "#[wasm_lite::export] cannot expose an `unsafe fn` through a safe \
             JavaScript entry point",
        ));
    }

    let name = &func.sig.ident;
    // The descriptor (and thus the generated JS `export function <name>`) uses
    // the unraw'd name, matching `format_ident!`'s `r#`-stripped shim symbol.
    let name_str = unraw(name);

    // The wrapper is emitted as a top-level `export function <name>` in the
    // glue module, so a JS reserved word (or a name the glue itself defines)
    // would make the entire generated module a SyntaxError at load time.
    if is_js_reserved_word(&name_str) {
        return Err(Error::new_spanned(
            name,
            format!(
                "#[wasm_lite::export]: `{name_str}` is a reserved word in JavaScript, so the \
                 generated `export function {name_str}` would be invalid JS. Rename the function."
            ),
        ));
    }
    if matches!(
        name_str.as_str(),
        "instantiate" | "setInstance" | "makeImports" | "makeMemory"
    ) || name_str.starts_with("__wl_")
    {
        return Err(Error::new_spanned(
            name,
            format!(
                "#[wasm_lite::export]: `{name_str}` collides with a function the generated JS \
                 glue defines. Rename the function."
            ),
        ));
    }

    let export_ident = format_ident!("__wl_export_{}", name);

    let mut flat_params: Vec<TokenStream2> = Vec::new(); // shim parameter declarations
    let mut pre: Vec<TokenStream2> = Vec::new(); // statements reconstructing args
    let mut call_args: Vec<TokenStream2> = Vec::new(); // arguments to the user fn
    let mut arg_tags: Vec<String> = Vec::new(); // descriptor tags

    for input in &func.sig.inputs {
        let (pat, ty) = fn_arg(input)?;

        // `Option<T>` arg: a discriminant param plus T's normal flattening.
        if let Some(inner) = generic1(ty, "Option") {
            validate_export_ref(inner)?;
            let (flat, recon, tag) = option_arg(krate, &pat, inner)?;
            flat_params.extend(flat);
            pre.push(recon);
            call_args.push(quote! { #pat });
            arg_tags.push(format!("opt:{tag}"));
            continue;
        }

        if is_str(ty) {
            validate_export_ref(ty)?;
            let (p, l) = (format_ident!("{pat}_ptr"), format_ident!("{pat}_len"));
            flat_params.push(quote! { #p: *const u8 });
            flat_params.push(quote! { #l: usize });
            pre.push(quote! { let #pat = unsafe { ::core::str::from_utf8_unchecked(::core::slice::from_raw_parts(#p, #l)) }; });
            call_args.push(quote! { #pat });
            arg_tags.push("str".into());
        } else if is_byte_slice(ty) {
            validate_export_ref(ty)?;
            let (p, l) = (format_ident!("{pat}_ptr"), format_ident!("{pat}_len"));
            flat_params.push(quote! { #p: *const u8 });
            flat_params.push(quote! { #l: usize });
            pre.push(quote! { let #pat = unsafe { ::core::slice::from_raw_parts(#p, #l) }; });
            call_args.push(quote! { #pat });
            arg_tags.push("bytes".into());
        } else if is_jsvalue(ty) {
            // JS registers the object and passes its index; Rust takes ownership.
            flat_params.push(quote! { #pat: u32 });
            pre.push(quote! { let #pat = unsafe { #krate::JsValue::__wl_from_abi(#pat) }; });
            call_args.push(quote! { #pat });
            arg_tags.push("handle".into());
        } else if let Some(scalar) = direct_export_numeric(ty) {
            flat_params.push(quote! { #pat: #ty });
            call_args.push(quote! { #pat });
            arg_tags.push(scalar);
        } else if is_ident(ty, "bool") {
            flat_params.push(quote! { #pat: i32 });
            call_args.push(quote! { (#pat != 0) });
            arg_tags.push("bool".into());
        } else {
            return Err(Error::new_spanned(
                ty,
                format!(
                    "#[wasm_lite::export]: unsupported argument type `{}`",
                    type_string(ty)
                ),
            ));
        }
    }

    let call = quote! { #name( #(#call_args),* ) };
    let (ret_decl, ret_tag, ret_expr, is_sret) = build_return(krate, &call, &func.sig.output)?;

    // sret writes the payload into a JS-provided buffer; the export gains a
    // leading `__ret` pointer.
    if is_sret {
        flat_params.insert(0, quote! { __ret: *mut u8 });
    }

    // String/bytes marshalling needs the allocator exported even when the shim
    // doesn't call it directly; sret buffers are JS-allocated too. Force-keep it.
    let needs_alloc = arg_tags
        .iter()
        .any(|t| t.contains("str") || t.contains("bytes"))
        || ret_tag == "str"
        || ret_tag == "bytes"
        || is_sret;
    let keep_alloc = if needs_alloc {
        quote! {
            const _: () = {
                #[used] static __WL_KEEP_MALLOC: extern "C" fn(usize) -> *mut u8 = #krate::__wl_malloc;
                #[used] static __WL_KEEP_FREE: unsafe extern "C" fn(*mut u8, usize) = #krate::__wl_free;
            };
        }
    } else {
        quote! {}
    };

    let descriptor = format!("{name_str}|{}|{ret_tag}", arg_tags.join(","));
    let section = section_literal(&descriptor);
    let len = descriptor.len() + 1;

    Ok(quote! {
        #func
        #[doc(hidden)]
        #[allow(clippy::missing_safety_doc)]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn #export_ident( #(#flat_params),* ) #ret_decl {
            #(#pre)*
            #ret_expr
        }
        #keep_alloc
        const _: () = {
            #[used]
            #[cfg_attr(target_arch = "wasm32", unsafe(link_section = "__wl_exports"))]
            static __WL_EXPORT: [u8; #len] = *#section;
        };
    })
}

/// Build the return marshalling: `(signature_suffix, descriptor_tag, body_expr, is_sret)`.
///
/// `Option<T>`/`Result<T, E>` use a return pointer (sret): the export takes a
/// leading `__ret` buffer and writes a discriminant word plus the payload, since
/// a single scalar return can't carry both. Discriminant: Option 1=Some/0=None;
/// Result 0=Ok/1=Err.
fn build_return(
    krate: &syn::Path,
    call: &TokenStream2,
    output: &ReturnType,
) -> syn::Result<(TokenStream2, String, TokenStream2, bool)> {
    let ty = match output {
        ReturnType::Default => return Ok((quote! {}, String::new(), quote! { #call; }, false)),
        ReturnType::Type(_, ty) => ty.as_ref(),
    };

    // An explicitly written `-> ()` is the same ABI as an omitted return.
    if matches!(ty, Type::Tuple(t) if t.elems.is_empty()) {
        return Ok((quote! {}, String::new(), quote! { #call; }, false));
    }

    if let Some(inner) = generic1(ty, "Option") {
        let (tag, write) = payload(krate, inner, &format_ident!("__x"))?;
        let body = quote! {
            let __v: #krate::__Option<#inner> = #call;
            match __v {
                #krate::__Option::Some(__x) => {
                    unsafe { ::core::ptr::write_unaligned(__ret as *mut u32, 1u32); }
                    #write
                }
                #krate::__Option::None => unsafe { ::core::ptr::write_unaligned(__ret as *mut u32, 0u32); },
            }
        };
        return Ok((quote! {}, format!("opt:{tag}"), body, true));
    }

    if let Some((ok_ty, err_ty)) = generic2(ty, "Result") {
        let (ok_tag, ok_write) = payload(krate, ok_ty, &format_ident!("__x"))?;
        let (err_tag, err_write) = payload(krate, err_ty, &format_ident!("__e"))?;
        let body = quote! {
            let __v: #krate::__Result<#ok_ty, #err_ty> = #call;
            match __v {
                #krate::__Result::Ok(__x) => {
                    unsafe { ::core::ptr::write_unaligned(__ret as *mut u32, 0u32); }
                    #ok_write
                }
                #krate::__Result::Err(__e) => {
                    unsafe { ::core::ptr::write_unaligned(__ret as *mut u32, 1u32); }
                    #err_write
                }
            }
        };
        return Ok((quote! {}, format!("res:{ok_tag}:{err_tag}"), body, true));
    }

    if let Some(scalar) = direct_export_numeric(ty) {
        return Ok((quote! { -> #ty }, scalar, call.clone(), false));
    }
    if is_ident(ty, "bool") {
        return Ok((
            quote! { -> i32 },
            "bool".into(),
            quote! { ((#call) as i32) },
            false,
        ));
    }
    if is_ident(ty, "String") {
        return Ok((
            quote! { -> i64 },
            "str".into(),
            // `#[export]` takes no crate-path override, so this stays absolute.
            // Only `import!` is reached from a crate that may be `no_std`.
            pack_buffer(krate, call, quote! { #krate::__String }),
            false,
        ));
    }
    if vec_u8(ty) {
        return Ok((
            quote! { -> i64 },
            "bytes".into(),
            pack_buffer(krate, call, quote! { #krate::__Vec<u8> }),
            false,
        ));
    }
    if is_ident(ty, "JsValue") {
        // Hand the table slot to JS: take the index, then forget so Drop doesn't
        // free it — ownership transfers across the boundary.
        let expr = quote! {
            let __r: #krate::JsValue = #call;
            let __idx = #krate::JsValue::__wl_abi(&__r);
            ::core::mem::forget(__r);
            __idx
        };
        return Ok((quote! { -> u32 }, "handle".into(), expr, false));
    }

    Err(Error::new_spanned(
        ty,
        format!(
            "#[wasm_lite::export]: unsupported return type `{}`",
            type_string(ty)
        ),
    ))
}

/// Copy a `String`/`Vec<u8>` into a `__wl_malloc` buffer and return a packed
/// `(ptr << 32 | len)` i64 the JS side decodes and frees.
fn pack_buffer(krate: &syn::Path, call: &TokenStream2, ty: TokenStream2) -> TokenStream2 {
    quote! {
        let __r: #ty = #call;
        let __len = __r.len();
        let __ptr = #krate::__wl_malloc(__len);
        unsafe { ::core::ptr::copy_nonoverlapping(__r.as_ptr(), __ptr, __len); }
        (((__ptr as usize as u64) << 32) | (__len as u64)) as i64
    }
}

/// Code to write `binding` (of type `ty`) into an sret buffer at `__ret + 8`
/// (str/bytes also use `__ret + 12` for the length). Returns the descriptor tag
/// and the code. Writes are unaligned (the buffer is align-1).
fn payload(krate: &syn::Path, ty: &Type, binding: &Ident) -> syn::Result<(String, TokenStream2)> {
    let off8 = quote! { (__ret as *mut u8).add(8) };
    let off12 = quote! { (__ret as *mut u8).add(12) };

    if matches!(ty, Type::Tuple(t) if t.elems.is_empty()) {
        return Ok(("unit".into(), quote! { let _ = #binding; }));
    }
    if let Some(scalar) = numeric(ty) {
        // The glue reads each payload with the descriptor's exact DataView
        // width. Writing every non-i32/u32 value as f64 both failed to compile
        // for most scalar types and disagreed with that layout.
        let write =
            quote! { unsafe { ::core::ptr::write_unaligned(#off8 as *mut #ty, #binding); } };
        return Ok((scalar, write));
    }
    if is_ident(ty, "bool") {
        return Ok((
            "bool".into(),
            quote! { unsafe { ::core::ptr::write_unaligned(#off8 as *mut i32, (#binding) as i32); } },
        ));
    }
    if is_ident(ty, "JsValue") {
        return Ok((
            "handle".into(),
            quote! {
                {
                    let __h = #krate::JsValue::__wl_abi(&#binding);
                    ::core::mem::forget(#binding);
                    unsafe { ::core::ptr::write_unaligned(#off8 as *mut u32, __h); }
                }
            },
        ));
    }
    let buf = quote! {
        {
            let __len = #binding.len();
            let __ptr = #krate::__wl_malloc(__len);
            unsafe {
                ::core::ptr::copy_nonoverlapping(#binding.as_ptr(), __ptr, __len);
                ::core::ptr::write_unaligned(#off8 as *mut u32, __ptr as usize as u32);
                ::core::ptr::write_unaligned(#off12 as *mut u32, __len as u32);
            }
        }
    };
    if is_ident(ty, "String") {
        return Ok(("str".into(), buf));
    }
    if vec_u8(ty) {
        return Ok(("bytes".into(), buf));
    }
    Err(Error::new_spanned(
        ty,
        format!(
            "#[wasm_lite::export]: unsupported Option/Result payload type `{}`",
            type_string(ty)
        ),
    ))
}

/// Flatten an `Option<inner>` argument: a discriminant param `<name>_some: i32`
/// plus `inner`'s normal flattening, with conditional reconstruction. Returns
/// `(flat_params, reconstruction, inner_tag)`.
fn option_arg(
    krate: &syn::Path,
    pat: &Ident,
    inner: &Type,
) -> syn::Result<(Vec<TokenStream2>, TokenStream2, String)> {
    let some = format_ident!("{pat}_some");

    if let Some(scalar) = numeric(inner) {
        let val = format_ident!("{pat}_val");
        return Ok((
            vec![quote! { #some: i32 }, quote! { #val: #inner }],
            quote! { let #pat = if #some != 0 { #krate::__Option::Some(#val) } else { #krate::__Option::None }; },
            scalar,
        ));
    }
    if is_ident(inner, "bool") {
        let val = format_ident!("{pat}_val");
        return Ok((
            vec![quote! { #some: i32 }, quote! { #val: i32 }],
            quote! { let #pat = if #some != 0 { #krate::__Option::Some(#val != 0) } else { #krate::__Option::None }; },
            "bool".into(),
        ));
    }
    if is_jsvalue(inner) {
        let h = format_ident!("{pat}_h");
        return Ok((
            vec![quote! { #some: i32 }, quote! { #h: u32 }],
            quote! { let #pat = if #some != 0 { #krate::__Option::Some(unsafe { #krate::JsValue::__wl_from_abi(#h) }) } else { #krate::__Option::None }; },
            "handle".into(),
        ));
    }
    if is_str(inner) {
        let (p, l) = (format_ident!("{pat}_ptr"), format_ident!("{pat}_len"));
        return Ok((
            vec![
                quote! { #some: i32 },
                quote! { #p: *const u8 },
                quote! { #l: usize },
            ],
            quote! { let #pat = if #some != 0 { #krate::__Option::Some(unsafe { ::core::str::from_utf8_unchecked(::core::slice::from_raw_parts(#p, #l)) }) } else { #krate::__Option::None }; },
            "str".into(),
        ));
    }
    if is_byte_slice(inner) {
        let (p, l) = (format_ident!("{pat}_ptr"), format_ident!("{pat}_len"));
        return Ok((
            vec![
                quote! { #some: i32 },
                quote! { #p: *const u8 },
                quote! { #l: usize },
            ],
            quote! { let #pat = if #some != 0 { #krate::__Option::Some(unsafe { ::core::slice::from_raw_parts(#p, #l) }) } else { #krate::__Option::None }; },
            "bytes".into(),
        ));
    }
    Err(Error::new_spanned(
        inner,
        format!(
            "#[wasm_lite::export]: unsupported Option argument type `Option<{}>`",
            type_string(inner)
        ),
    ))
}

// ---------------------------------------------------------------------------
// js_class!
/// Extract `(name, type)` from a function argument (rejects `self`/patterns).
fn fn_arg(input: &FnArg) -> syn::Result<(Ident, &Type)> {
    match input {
        FnArg::Typed(pt) => match &*pt.pat {
            Pat::Ident(pi) => Ok((pi.ident.clone(), &pt.ty)),
            other => Err(Error::new_spanned(
                other,
                "#[wasm_lite::export]: argument must be a simple name",
            )),
        },
        FnArg::Receiver(r) => Err(Error::new_spanned(
            r,
            "#[wasm_lite::export] cannot be used on methods",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::format_ident;
    use syn::Type;

    fn default_crate() -> syn::Path {
        syn::parse_quote!(::wasm_lite)
    }

    #[test]
    fn rejects_static_borrow_hidden_inside_option() {
        let func: ItemFn = syn::parse_quote! {
            pub fn retain(value: Option<&'static str>) { let _ = value; }
        };
        let error = build_export(&default_crate(), &func).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`&'static` arguments are unsound")
        );
    }

    #[test]
    fn rejects_generic_lifetime_that_can_require_static() {
        let func: ItemFn = syn::parse_quote! {
            pub fn retain<'a: 'static>(value: &'a str) { let _ = value; }
        };
        let error = build_export(&default_crate(), &func).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not support generic functions")
        );
    }

    #[test]
    fn rejects_mutable_temporary_buffer_borrows() {
        let func: ItemFn = syn::parse_quote! {
            pub fn mutate(value: Option<&mut [u8]>) { let _ = value; }
        };
        let error = build_export(&default_crate(), &func).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("mutations cannot be copied back")
        );
    }

    #[test]
    fn numeric_sret_payloads_use_their_exact_rust_layout() {
        let binding = format_ident!("__x");
        for ty in [
            "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize", "f32", "f64",
        ] {
            let parsed: Type = syn::parse_str(ty).unwrap();
            let (tag, write) = payload(&default_crate(), &parsed, &binding).unwrap();
            assert_eq!(tag, ty);
            assert!(
                write.to_string().contains(&format!("as * mut {ty} , __x")),
                "wrong payload writer for {ty}: {write}"
            );
        }
    }

    #[test]
    fn unit_returns_and_result_payloads_are_supported() {
        let func: ItemFn = syn::parse_quote! {
            pub fn explicit_unit() -> () {}
        };
        assert!(build_export(&default_crate(), &func).is_ok());

        let unit: Type = syn::parse_quote!(());
        let (tag, _) = payload(&default_crate(), &unit, &format_ident!("__x")).unwrap();
        assert_eq!(tag, "unit");
    }

    #[test]
    fn rejects_direct_numeric_abis_the_glue_does_not_implement() {
        let func: ItemFn = syn::parse_quote! {
            pub fn round_trip(value: i64) -> i64 { value }
        };
        let error = build_export(&default_crate(), &func).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported argument type `i64`")
        );
    }

    #[test]
    fn generated_abi_entry_points_require_unsafe_calls_from_rust() {
        let func: ItemFn = syn::parse_quote! {
            pub fn echo(value: &str) -> String { value.to_string() }
        };
        let output = build_export(&default_crate(), &func).unwrap().to_string();
        assert!(
            output.contains("pub unsafe extern \"C\" fn __wl_export_echo"),
            "{output}"
        );
    }
}
