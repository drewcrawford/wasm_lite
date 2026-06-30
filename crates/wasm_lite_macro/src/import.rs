//! The `import!` proc-macro: declare imported JS functions grouped by namespace.
//!
//! For each `fn`, emits a safe Rust wrapper plus a function-local wasm import
//! with a *flattened* ABI (`&str` → `*const u8, usize`), and contributes a line
//! to the `__wasm_lite_imports` section so the host codegen can generate a
//! matching JS shim. This is the inverse direction of `#[export]`: arguments are
//! lowered Rust→ABI and returns raised ABI→Rust, but the type classification is
//! shared (see [`crate::ty`]).
//!
//! The import symbol is `concat!(module_path!(), "::", name)` — unique per
//! (crate, module, fn) so independent crates never collide. A proc-macro can't
//! evaluate `module_path!()`, but it can *emit* that `concat!` so the
//! `#[link_name]` and the descriptor agree at compile time.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Error, Ident, LitStr, Token, Type, braced, parenthesized};

use crate::ty::*;

/// `import! { "ns" { fns } ... }`.
struct Import {
    namespaces: Vec<Namespace>,
}

struct Namespace {
    ns: LitStr,
    fns: Vec<ImportFn>,
}

/// `fn name(params) -> ret as "js";`.
struct ImportFn {
    doc_attrs: Vec<Attribute>,
    name: Ident,
    params: Vec<(Ident, Type)>,
    ret: Option<Type>,
    js: Option<String>,
}

impl Parse for Import {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut namespaces = Vec::new();
        while !input.is_empty() {
            namespaces.push(input.parse()?);
        }
        Ok(Import { namespaces })
    }
}

impl Parse for Namespace {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ns: LitStr = input.parse()?;
        let body;
        braced!(body in input);
        let mut fns = Vec::new();
        while !body.is_empty() {
            fns.push(body.parse()?);
        }
        Ok(Namespace { ns, fns })
    }
}

impl Parse for ImportFn {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let doc_attrs = attrs
            .into_iter()
            .filter(|a| a.path().is_ident("doc"))
            .collect();
        input.parse::<Token![fn]>()?;
        let name: Ident = input.parse()?;

        let args;
        parenthesized!(args in input);
        let mut params = Vec::new();
        while !args.is_empty() {
            let pname: Ident = args.parse()?;
            args.parse::<Token![:]>()?;
            let ty: Type = args.parse()?;
            params.push((pname, ty));
            if args.is_empty() {
                break;
            }
            args.parse::<Token![,]>()?;
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

        Ok(ImportFn {
            doc_attrs,
            name,
            params,
            ret,
            js,
        })
    }
}

pub(crate) fn build(input: TokenStream2) -> syn::Result<TokenStream2> {
    let parsed: Import = syn::parse2(input)?;

    let mut items: Vec<TokenStream2> = Vec::new();
    let mut descr_frags: Vec<TokenStream2> = Vec::new();

    for ns in &parsed.namespaces {
        for f in &ns.fns {
            let (item, frag) = build_fn(&ns.ns, f)?;
            items.push(item);
            descr_frags.push(frag);
        }
    }

    // One descriptor section entry per invocation; `module_path!()` is emitted
    // (not evaluated here) so the import symbol is resolved in context.
    let descriptors = quote! {
        const _: () = {
            const DESCR_STR: &str = concat!( #(#descr_frags),* );
            #[used]
            #[cfg_attr(target_arch = "wasm32", unsafe(link_section = "__wasm_lite_imports"))]
            static DESCR: [u8; DESCR_STR.len()] =
                ::wasm_lite::descriptor_bytes::<{ DESCR_STR.len() }>(DESCR_STR);
        };
    };

    Ok(quote! {
        #(#items)*
        #descriptors
    })
}

fn build_fn(ns: &LitStr, f: &ImportFn) -> syn::Result<(TokenStream2, TokenStream2)> {
    let name = &f.name;
    let fname_str = name.to_string();

    let mut orig_params: Vec<TokenStream2> = Vec::new();
    let mut extern_params: Vec<TokenStream2> = Vec::new();
    let mut call_args: Vec<TokenStream2> = Vec::new();
    let mut arg_tags: Vec<String> = Vec::new();

    for (pname, ty) in &f.params {
        orig_params.push(quote! { #pname: #ty });

        if let Some(inner) = generic1(ty, "Option") {
            let (ep, ca, tag) = option_arg(pname, inner)?;
            extern_params.extend(ep);
            call_args.extend(ca);
            arg_tags.push(format!("opt:{tag}"));
        } else if is_str(ty) {
            extern_params.push(quote! { _: *const u8 });
            extern_params.push(quote! { _: usize });
            call_args.push(quote! { #pname.as_ptr() });
            call_args.push(quote! { #pname.len() });
            arg_tags.push("str".into());
        } else if is_byte_slice(ty) {
            extern_params.push(quote! { _: *const u8 });
            extern_params.push(quote! { _: usize });
            call_args.push(quote! { #pname.as_ptr() });
            call_args.push(quote! { #pname.len() });
            arg_tags.push("bytes".into());
        } else if is_ref_jsvalue(ty) {
            extern_params.push(quote! { _: u32 });
            call_args.push(quote! { #pname.__wl_abi() });
            arg_tags.push("handle".into());
        } else if is_ident(ty, "bool") {
            extern_params.push(quote! { _: i32 });
            call_args.push(quote! { #pname as i32 });
            arg_tags.push("bool".into());
        } else if let Some(scalar) = numeric(ty) {
            extern_params.push(quote! { _: #ty });
            call_args.push(quote! { #pname });
            arg_tags.push(scalar);
        } else {
            return Err(Error::new_spanned(
                ty,
                format!("import!: unsupported argument type `{}`", type_string(ty)),
            ));
        }
    }

    // `m` if the first parameter is `this: &JsValue`, else `f`.
    let kind = match f.params.first() {
        Some((n, t)) if n == "this" && is_ref_jsvalue(t) => "m",
        _ => "f",
    };

    let js_name = f.js.clone().unwrap_or_else(|| fname_str.clone());
    let ret = build_return(name, ns, &extern_params, &call_args, f.ret.as_ref())?;

    let Return {
        wrapper_ret,
        extern_decl,
        body,
        ret_tag,
        needs_malloc,
    } = ret;

    let keep_malloc = if needs_malloc {
        quote! {
            const _: () = {
                #[used] static __WL_KEEP_MALLOC: extern "C" fn(usize) -> *mut u8 = ::wasm_lite::__wl_malloc;
            };
        }
    } else {
        quote! {}
    };

    let doc_attrs = &f.doc_attrs;
    let item = quote! {
        #(#doc_attrs)*
        pub fn #name( #(#orig_params),* ) #wrapper_ret {
            #extern_decl
            #body
        }
        #keep_malloc
    };

    let arg_tags = arg_tags.join(",");
    let frag = quote! {
        concat!(
            #kind, "|", #ns, "|", concat!(module_path!(), "::", #fname_str), "|",
            #js_name, "|", #arg_tags, "|", #ret_tag, "\n"
        )
    };

    Ok((item, frag))
}

/// The pieces of an import wrapper's return handling.
struct Return {
    /// `-> Type` for the wrapper, or empty.
    wrapper_ret: TokenStream2,
    /// The function-local `extern` block declaring the wasm import.
    extern_decl: TokenStream2,
    /// The wrapper body (calls the import and marshals the result).
    body: TokenStream2,
    /// Descriptor return tag.
    ret_tag: String,
    /// Whether the host allocates (so `__wl_malloc` must be kept exported).
    needs_malloc: bool,
}

fn build_return(
    name: &Ident,
    ns: &LitStr,
    extern_params: &[TokenStream2],
    call_args: &[TokenStream2],
    ret: Option<&Type>,
) -> syn::Result<Return> {
    let fname_str = name.to_string();
    let link = quote! { #[link_name = concat!(module_path!(), "::", #fname_str)] };

    // A non-sret import: `extern { fn name(flat) -> abi; }` and `name(call)`.
    let scalar_extern = |abi: TokenStream2| {
        quote! {
            #[link(wasm_import_module = #ns)]
            unsafe extern "C" {
                #link
                fn #name( #(#extern_params),* ) #abi;
            }
        }
    };
    let call = quote! { #name( #(#call_args),* ) };

    let Some(ty) = ret else {
        return Ok(Return {
            wrapper_ret: quote! {},
            extern_decl: scalar_extern(quote! {}),
            body: quote! { unsafe { #call } },
            ret_tag: String::new(),
            needs_malloc: false,
        });
    };

    if is_ident(ty, "bool") {
        return Ok(Return {
            wrapper_ret: quote! { -> bool },
            extern_decl: scalar_extern(quote! { -> i32 }),
            body: quote! { unsafe { #call != 0 } },
            ret_tag: "bool".into(),
            needs_malloc: false,
        });
    }
    if is_jsvalue(ty) {
        return Ok(Return {
            wrapper_ret: quote! { -> ::wasm_lite::JsValue },
            extern_decl: scalar_extern(quote! { -> u32 }),
            body: quote! { ::wasm_lite::JsValue::__wl_from_abi(unsafe { #call }) },
            ret_tag: "handle".into(),
            needs_malloc: false,
        });
    }
    if is_ident(ty, "String") {
        return Ok(Return {
            wrapper_ret: quote! { -> ::std::string::String },
            extern_decl: scalar_extern(quote! { -> i64 }),
            body: unpack_buffer(&call, quote! { ::std::string::String::from_raw_parts }),
            ret_tag: "str".into(),
            needs_malloc: true,
        });
    }
    if vec_u8(ty) {
        return Ok(Return {
            wrapper_ret: quote! { -> ::std::vec::Vec<u8> },
            extern_decl: scalar_extern(quote! { -> i64 }),
            body: unpack_buffer(&call, quote! { ::std::vec::Vec::from_raw_parts }),
            ret_tag: "bytes".into(),
            needs_malloc: true,
        });
    }
    if let Some(scalar) = numeric(ty) {
        return Ok(Return {
            wrapper_ret: quote! { -> #ty },
            extern_decl: scalar_extern(quote! { -> #ty }),
            body: quote! { unsafe { #call } },
            ret_tag: scalar,
            needs_malloc: false,
        });
    }

    // sret returns: a leading `__ret` buffer, no scalar return.
    let sret_extern = quote! {
        #[link(wasm_import_module = #ns)]
        unsafe extern "C" {
            #link
            fn #name(__ret: *mut u8, #(#extern_params),*);
        }
    };
    let sret_call = quote! { #name(__buf.as_mut_ptr(), #(#call_args),*) };

    if let Some(inner) = generic1(ty, "Option") {
        let tag = payload_tag(inner).ok_or_else(|| {
            Error::new_spanned(
                inner,
                format!(
                    "import!: unsupported Option payload type `{}`",
                    type_string(inner)
                ),
            )
        })?;
        let body = quote! {
            let mut __buf = [0u8; 16];
            unsafe { #sret_call };
            if u32::from_le_bytes([__buf[0], __buf[1], __buf[2], __buf[3]]) == 1 {
                ::core::option::Option::Some(unsafe { <#inner as ::wasm_lite::FromSretPayload>::__wl_read(__buf.as_ptr()) })
            } else {
                ::core::option::Option::None
            }
        };
        return Ok(Return {
            wrapper_ret: quote! { -> ::core::option::Option<#inner> },
            extern_decl: sret_extern,
            body,
            ret_tag: format!("opt:{tag}"),
            needs_malloc: true,
        });
    }
    if let Some((ok_ty, err_ty)) = generic2(ty, "Result") {
        let ok_tag = payload_tag(ok_ty).ok_or_else(|| {
            Error::new_spanned(
                ok_ty,
                format!(
                    "import!: unsupported Result Ok type `{}`",
                    type_string(ok_ty)
                ),
            )
        })?;
        let err_tag = payload_tag(err_ty).ok_or_else(|| {
            Error::new_spanned(
                err_ty,
                format!(
                    "import!: unsupported Result Err type `{}`",
                    type_string(err_ty)
                ),
            )
        })?;
        let body = quote! {
            let mut __buf = [0u8; 16];
            unsafe { #sret_call };
            if u32::from_le_bytes([__buf[0], __buf[1], __buf[2], __buf[3]]) == 0 {
                ::core::result::Result::Ok(unsafe { <#ok_ty as ::wasm_lite::FromSretPayload>::__wl_read(__buf.as_ptr()) })
            } else {
                ::core::result::Result::Err(unsafe { <#err_ty as ::wasm_lite::FromSretPayload>::__wl_read(__buf.as_ptr()) })
            }
        };
        return Ok(Return {
            wrapper_ret: quote! { -> ::core::result::Result<#ok_ty, #err_ty> },
            extern_decl: sret_extern,
            body,
            ret_tag: format!("res:{ok_tag}:{err_tag}"),
            needs_malloc: true,
        });
    }

    Err(Error::new_spanned(
        ty,
        format!("import!: unsupported return type `{}`", type_string(ty)),
    ))
}

/// Body for a `String`/`Vec<u8>` return: unpack the packed `(ptr << 32 | len)`
/// the host returned and take ownership via `from_raw_parts`.
fn unpack_buffer(call: &TokenStream2, from_raw_parts: TokenStream2) -> TokenStream2 {
    quote! {
        let __packed = unsafe { #call } as u64;
        let __ptr = (__packed >> 32) as usize as *mut u8;
        let __len = (__packed & 0xffff_ffff) as usize;
        // SAFETY: the host allocated `__len` bytes (align 1) with `__wl_malloc`,
        // matching the collection's allocator, and transfers ownership.
        unsafe { #from_raw_parts(__ptr, __len, __len) }
    }
}

/// Flatten an `Option<inner>` argument (import direction): a discriminant param
/// plus `inner`'s lowering, read conditionally from the `Copy` option.
fn option_arg(
    pname: &Ident,
    inner: &Type,
) -> syn::Result<(Vec<TokenStream2>, Vec<TokenStream2>, String)> {
    if is_str(inner) {
        return Ok((
            vec![
                quote! { _: i32 },
                quote! { _: *const u8 },
                quote! { _: usize },
            ],
            vec![
                quote! { #pname.is_some() as i32 },
                quote! { #pname.map_or(::core::ptr::null(), |__s| __s.as_ptr()) },
                quote! { #pname.map_or(0, |__s| __s.len()) },
            ],
            "str".into(),
        ));
    }
    if is_byte_slice(inner) {
        return Ok((
            vec![
                quote! { _: i32 },
                quote! { _: *const u8 },
                quote! { _: usize },
            ],
            vec![
                quote! { #pname.is_some() as i32 },
                quote! { #pname.map_or(::core::ptr::null(), |__s| __s.as_ptr()) },
                quote! { #pname.map_or(0, |__s| __s.len()) },
            ],
            "bytes".into(),
        ));
    }
    if is_ref_jsvalue(inner) {
        return Ok((
            vec![quote! { _: i32 }, quote! { _: u32 }],
            vec![
                quote! { #pname.is_some() as i32 },
                quote! { #pname.map_or(0u32, |__v| __v.__wl_abi()) },
            ],
            "handle".into(),
        ));
    }
    if is_ident(inner, "bool") {
        return Ok((
            vec![quote! { _: i32 }, quote! { _: i32 }],
            vec![
                quote! { #pname.is_some() as i32 },
                quote! { #pname.unwrap_or_default() as i32 },
            ],
            "bool".into(),
        ));
    }
    if let Some(scalar) = numeric(inner) {
        return Ok((
            vec![quote! { _: i32 }, quote! { _: #inner }],
            vec![
                quote! { #pname.is_some() as i32 },
                quote! { #pname.unwrap_or_default() },
            ],
            scalar,
        ));
    }
    Err(Error::new_spanned(
        inner,
        format!(
            "import!: unsupported Option argument type `Option<{}>`",
            type_string(inner)
        ),
    ))
}
