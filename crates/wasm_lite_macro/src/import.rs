// SPDX-License-Identifier: MIT OR Apache-2.0
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
use syn::{Attribute, Error, Ident, LitStr, Path, Token, Type, braced, parenthesized};

use crate::ty::*;

/// `import! { [crate = <path>;] "ns" { fns } ... }`.
struct Import {
    /// Where the generated code finds the wasm_lite runtime.
    ///
    /// Defaults to `::wasm_lite`, which is right whenever the calling crate
    /// depends on wasm_lite directly. A shim that re-exports the runtime under
    /// its own name overrides it, so that code generated inside a crate which
    /// has never heard of wasm_lite still resolves — see the module docs.
    krate: Path,
    namespaces: Vec<Namespace>,
}

struct Namespace {
    ns: LitStr,
    fns: Vec<ImportFn>,
}

/// `fn name(params) -> ret as "js";`.
struct ImportFn {
    doc_attrs: Vec<Attribute>,
    /// The explicit `#[getter]`/`#[setter]`/… kind, if one was given. `None`
    /// means "infer a call", which is the `m`/`f` split on the receiver.
    kind: Option<KindAttr>,
    /// Spread the final argument into the call.
    variadic: bool,
    name: Ident,
    params: Vec<(Ident, Type)>,
    ret: Option<Type>,
    js: Option<String>,
}

/// An explicit binding kind, written as an attribute on the imported `fn`.
///
/// These name JS operations that are not calls, so they cannot be inferred
/// from a Rust signature: `fn tag_name(this: &JsValue) -> String` is
/// indistinguishable from a zero-argument method until you say which you meant.
#[derive(Clone, Copy, PartialEq, Eq)]
struct KindAttr {
    /// The descriptor tag: `g`, `s`, `n`, `ig`, `is`.
    tag: &'static str,
    /// The attribute as written, for diagnostics.
    spelled: &'static str,
    /// Exact parameter count, including the receiver.
    arity: usize,
    /// Whether the binding must have a return type; `None` when either is
    /// allowed.
    returns: Option<bool>,
    /// Whether the first parameter must be `this: &JsValue`.
    receiver: bool,
}

impl KindAttr {
    fn from_ident(id: &Ident) -> Option<Self> {
        let (tag, spelled, arity, returns, receiver) = match () {
            _ if id == "getter" => ("g", "getter", 1, Some(true), true),
            // A setter may return `Result<(), E>` when the assignment can throw.
            _ if id == "setter" => ("s", "setter", 2, None, true),
            // A constructor's argument count is whatever the JS class takes.
            _ if id == "constructor" => ("n", "constructor", usize::MAX, Some(true), false),
            _ if id == "indexing_getter" => ("ig", "indexing_getter", 2, Some(true), true),
            _ if id == "indexing_setter" => ("is", "indexing_setter", 3, Some(false), true),
            _ if id == "instanceof" => ("io", "instanceof", 1, Some(true), true),
            _ if id == "static_getter" => ("sg", "static_getter", 0, Some(true), false),
            // `delete` yields a bool, but discarding it is normal.
            _ if id == "indexing_deleter" => ("id", "indexing_deleter", 2, None, true),
            _ => return None,
        };
        Some(KindAttr {
            tag,
            spelled,
            arity,
            returns,
            receiver,
        })
    }
}

impl Parse for Import {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let krate = if input.peek(Token![crate]) {
            input.parse::<Token![crate]>()?;
            input.parse::<Token![=]>()?;
            let path: Path = input.parse()?;
            input.parse::<Token![;]>()?;
            path
        } else {
            syn::parse_quote!(::wasm_lite)
        };
        let mut namespaces = Vec::new();
        while !input.is_empty() {
            namespaces.push(input.parse()?);
        }
        Ok(Import { krate, namespaces })
    }
}

/// Reject strings that would corrupt the descriptor section's line/pipe format.
/// The descriptor is `kind|ns|name|js_name|args|ret\n`, emitted without any
/// escaping, so `|` or a control character in a user string silently shifts
/// every following field and generates wrong glue.
fn check_descriptor_str(lit: &LitStr, what: &str) -> syn::Result<()> {
    let v = lit.value();
    if v.is_empty() {
        return Err(Error::new_spanned(lit, format!("{what} must not be empty")));
    }
    if let Some(bad) = v.chars().find(|c| *c == '|' || c.is_control()) {
        return Err(Error::new_spanned(
            lit,
            format!(
                "{what} must not contain {bad:?}: it is embedded verbatim in the binding descriptor, which uses `|`-separated fields and newline-separated entries"
            ),
        ));
    }
    Ok(())
}

impl Parse for Namespace {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ns: LitStr = input.parse()?;
        check_descriptor_str(&ns, "import! namespace")?;
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
        // Doc comments and the binding-kind attributes are honored. Anything
        // else — `#[cfg(...)]` in particular — would be silently discarded
        // while the binding, the wasm import, and the descriptor line were
        // still emitted unconditionally; reject it rather than pretend the
        // attribute took effect.
        let mut doc_attrs = Vec::new();
        let mut kind: Option<KindAttr> = None;
        let mut variadic = false;
        for a in attrs {
            if a.path().is_ident("doc") {
                doc_attrs.push(a);
                continue;
            }
            // Orthogonal to the kind: any calling kind can be variadic.
            if a.path().is_ident("variadic") {
                variadic = true;
                continue;
            }
            let found = a.path().get_ident().and_then(KindAttr::from_ident);
            let Some(k) = found else {
                return Err(Error::new_spanned(
                    a,
                    "import!: only doc comments and the binding-kind attributes \
                     (#[getter], #[setter], #[constructor], #[indexing_getter], \
                     #[indexing_setter], #[indexing_deleter], #[instanceof], \
                     #[static_getter], #[variadic]) are supported on imported \
                     functions; other \
                     attributes (including #[cfg]) are not honored here — apply them to \
                     the surrounding import! invocation or module instead",
                ));
            };
            if let Some(prev) = kind {
                return Err(Error::new_spanned(
                    a,
                    format!(
                        "import!: a binding may have only one kind attribute; \
                         already saw #[{}]",
                        prev.spelled
                    ),
                ));
            }
            kind = Some(k);
        }
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
            let lit: LitStr = input.parse()?;
            check_descriptor_str(&lit, "`as` JS name")?;
            Some(lit.value())
        } else {
            None
        };
        input.parse::<Token![;]>()?;

        Ok(ImportFn {
            doc_attrs,
            kind,
            variadic,
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
            let (item, frag) = build_fn(&parsed.krate, &ns.ns, f)?;
            items.push(item);
            descr_frags.push(frag);
        }
    }

    // One descriptor section entry per invocation; `module_path!()` is emitted
    // (not evaluated here) so the import symbol is resolved in context.
    let krate = &parsed.krate;
    let descriptors = quote! {
        const _: () = {
            const DESCR_STR: &str = concat!( #(#descr_frags),* );
            #[used]
            #[cfg_attr(target_arch = "wasm32", unsafe(link_section = "__wasm_lite_imports"))]
            static DESCR: [u8; DESCR_STR.len()] =
                #krate::descriptor_bytes::<{ DESCR_STR.len() }>(DESCR_STR);
        };
    };

    Ok(quote! {
        #(#items)*
        #descriptors
    })
}

fn build_fn(krate: &Path, ns: &LitStr, f: &ImportFn) -> syn::Result<(TokenStream2, TokenStream2)> {
    let name = &f.name;
    let fname_str = name.to_string();

    let mut orig_params: Vec<TokenStream2> = Vec::new();
    let mut extern_params: Vec<TokenStream2> = Vec::new();
    let mut call_args: Vec<TokenStream2> = Vec::new();
    let mut arg_tags: Vec<String> = Vec::new();

    for (pname, ty) in &f.params {
        // `Option<&mut [u8]>` needs `&mut self` to obtain a pointer without
        // weakening the borrow to `&[u8]`. A plain `&mut [T]` binding can
        // reborrow mutably without the binding itself being `mut`.
        let option_mut_slice =
            generic1(ty, "Option").is_some_and(|inner| is_byte_slice(inner) && is_mut_ref(inner));
        if option_mut_slice {
            orig_params.push(quote! { mut #pname: #ty });
        } else {
            orig_params.push(quote! { #pname: #ty });
        }

        if let Some(inner) = generic1(ty, "Option") {
            let (ep, ca, tag) = option_arg(krate, pname, inner)?;
            extern_params.extend(ep);
            call_args.extend(ca);
            arg_tags.push(format!("opt:{tag}"));
        } else if is_ident(ty, "String") {
            // An owned `String` argument crosses exactly like `&str` — the
            // wrapper owns it for the call and drops it after. Generated
            // binding surfaces write this (`console_error_panic_hook` does),
            // and refusing it only forces the caller to borrow for no gain.
            extern_params.push(quote! { _: *const u8 });
            extern_params.push(quote! { _: usize });
            call_args.push(quote! { #pname.as_ptr() });
            call_args.push(quote! { #pname.len() });
            arg_tags.push("str".into());
        } else if is_str(ty) {
            extern_params.push(quote! { _: *const u8 });
            extern_params.push(quote! { _: usize });
            call_args.push(quote! { #pname.as_ptr() });
            call_args.push(quote! { #pname.len() });
            arg_tags.push("str".into());
        } else if is_byte_slice(ty) {
            let (ptr_ty, ptr) = if is_mut_ref(ty) {
                (quote! { *mut u8 }, quote! { #pname.as_mut_ptr() })
            } else {
                (quote! { *const u8 }, quote! { #pname.as_ptr() })
            };
            extern_params.push(quote! { _: #ptr_ty });
            extern_params.push(quote! { _: usize });
            call_args.push(ptr);
            call_args.push(quote! { #pname.len() });
            arg_tags.push("bytes".into());
        } else if is_handle_slice(ty) {
            extern_params.push(quote! { _: *const u8 });
            extern_params.push(quote! { _: usize });
            call_args.push(quote! { #pname.as_ptr() as *const u8 });
            call_args.push(quote! { #pname.len() });
            arg_tags.push("handles".into());
        } else if let Some(elem) = numeric_slice(ty) {
            // `(ptr, len)` in *elements*, matching `slice::len()`; the shim
            // makes a typed-array view of that many elements, so neither side
            // has to know the element size.
            let (ptr_ty, ptr) = if is_mut_ref(ty) {
                (
                    quote! { *mut u8 },
                    quote! { #pname.as_mut_ptr() as *mut u8 },
                )
            } else {
                (
                    quote! { *const u8 },
                    quote! { #pname.as_ptr() as *const u8 },
                )
            };
            extern_params.push(quote! { _: #ptr_ty });
            extern_params.push(quote! { _: usize });
            call_args.push(ptr);
            call_args.push(quote! { #pname.len() });
            arg_tags.push(format!("slice:{elem}"));
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

    let has_receiver = matches!(
        f.params.first(),
        Some((n, t)) if n == "this" && is_ref_jsvalue(t)
    );

    // An explicit attribute picks the kind; otherwise it is a call, `m` if the
    // first parameter is `this: &JsValue` and `f` if not.
    let kind = match f.kind {
        None => {
            if has_receiver {
                "m"
            } else {
                "f"
            }
        }
        Some(k) => {
            if k.receiver && !has_receiver {
                return Err(Error::new_spanned(
                    name,
                    format!(
                        "import!: #[{}] operates on a JS object, so its first parameter \
                         must be the receiver `this: &JsValue`",
                        k.spelled
                    ),
                ));
            }
            if k.arity != usize::MAX && f.params.len() != k.arity {
                return Err(Error::new_spanned(
                    name,
                    format!(
                        "import!: #[{}] takes exactly {} parameter(s) (including \
                         `this`), found {}",
                        k.spelled,
                        k.arity,
                        f.params.len()
                    ),
                ));
            }
            if k.returns == Some(true) && f.ret.is_none() {
                return Err(Error::new_spanned(
                    name,
                    format!("import!: #[{}] must have a return type", k.spelled),
                ));
            }
            if k.returns == Some(false) && f.ret.is_some() {
                return Err(Error::new_spanned(
                    name,
                    format!("import!: #[{}] must not have a return type", k.spelled),
                ));
            }
            k.tag
        }
    };
    if f.variadic && !matches!(kind, "f" | "m" | "n") {
        return Err(Error::new_spanned(
            name,
            "import!: #[variadic] is only supported on functions, methods, and constructors",
        ));
    }

    // Default JS name: the Rust name minus any `r#` (the raw prefix is
    // Rust-only; `fn r#type(...)` must call the JS property `type`). The wasm
    // import symbol keeps the raw string — it only has to match the descriptor,
    // which uses the same `fname_str`.
    let js_name = f.js.clone().unwrap_or_else(|| unraw(name));
    let ret = build_return(krate, name, ns, &extern_params, &call_args, f.ret.as_ref())?;

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
                #[used] static __WL_KEEP_MALLOC: extern "C" fn(usize) -> *mut u8 = #krate::__wl_malloc;
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
    let variadic_tag = if f.variadic { "1" } else { "" };
    let frag = quote! {
        concat!(
            #kind, "|", #ns, "|", concat!(module_path!(), "::", #fname_str), "|",
            #js_name, "|", #arg_tags, "|", #ret_tag, "|", #variadic_tag, "\n"
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
    krate: &Path,
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

    // `-> ()` is spelled out by generated bindings and means the same as no
    // return type at all.
    let ret = ret.filter(|t| !matches!(t, Type::Tuple(t) if t.elems.is_empty()));
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
            wrapper_ret: quote! { -> #krate::JsValue },
            extern_decl: scalar_extern(quote! { -> u32 }),
            body: quote! { #krate::JsValue::__wl_from_abi(unsafe { #call }) },
            ret_tag: "handle".into(),
            needs_malloc: false,
        });
    }
    if is_ident(ty, "String") {
        return Ok(Return {
            wrapper_ret: quote! { -> #krate::__String },
            extern_decl: scalar_extern(quote! { -> i64 }),
            body: unpack_buffer(&call, quote! { #krate::__String::from_raw_parts }),
            ret_tag: "str".into(),
            needs_malloc: true,
        });
    }
    if vec_u8(ty) {
        return Ok(Return {
            wrapper_ret: quote! { -> #krate::__Vec<u8> },
            extern_decl: scalar_extern(quote! { -> i64 }),
            body: unpack_buffer(&call, quote! { #krate::__Vec::from_raw_parts }),
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
                #krate::__Option::Some(unsafe { <#inner as #krate::FromSretPayload>::__wl_read(__buf.as_ptr()) })
            } else {
                #krate::__Option::None
            }
        };
        return Ok(Return {
            wrapper_ret: quote! { -> #krate::__Option<#inner> },
            extern_decl: sret_extern,
            body,
            ret_tag: format!("opt:{tag}"),
            needs_malloc: true,
        });
    }
    // `Result<Option<T>, E>`: three outcomes in one discriminant word.
    if let Some((ok_ty, err_ty)) = generic2(ty, "Result")
        && let Some(inner) = generic1(ok_ty, "Option")
    {
        let ok_tag = payload_tag(inner).ok_or_else(|| {
            Error::new_spanned(
                inner,
                format!(
                    "import!: unsupported Result<Option<_>, _> Ok type `{}`",
                    type_string(inner)
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
            match u32::from_le_bytes([__buf[0], __buf[1], __buf[2], __buf[3]]) {
                0 => #krate::__Result::Ok(#krate::__Option::Some(unsafe {
                    <#inner as #krate::FromSretPayload>::__wl_read(__buf.as_ptr())
                })),
                2 => #krate::__Result::Ok(#krate::__Option::None),
                _ => #krate::__Result::Err(unsafe {
                    <#err_ty as #krate::FromSretPayload>::__wl_read(__buf.as_ptr())
                }),
            }
        };
        return Ok(Return {
            wrapper_ret: quote! { -> #krate::__Result<#ok_ty, #err_ty> },
            extern_decl: sret_extern,
            body,
            ret_tag: format!("resopt:{ok_tag}:{err_tag}"),
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
                #krate::__Result::Ok(unsafe { <#ok_ty as #krate::FromSretPayload>::__wl_read(__buf.as_ptr()) })
            } else {
                #krate::__Result::Err(unsafe { <#err_ty as #krate::FromSretPayload>::__wl_read(__buf.as_ptr()) })
            }
        };
        return Ok(Return {
            wrapper_ret: quote! { -> #krate::__Result<#ok_ty, #err_ty> },
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
/// The flattened parameters, call arguments and descriptor tag for an
/// `Option<T>` argument.
///
/// The string and slice cases go through `as_deref` rather than consuming the
/// option: they contribute three arguments (discriminant, pointer, length), and
/// `Option<&mut [u8]>` is not `Copy`, so using it directly moves it twice over.
fn option_arg(
    krate: &Path,
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
                quote! { #pname.as_deref().map_or(#krate::__null(), |__s| __s.as_ptr()) },
                quote! { #pname.as_deref().map_or(0, |__s| __s.len()) },
            ],
            "str".into(),
        ));
    }
    if is_byte_slice(inner) {
        if is_mut_ref(inner) {
            return Ok((
                vec![
                    quote! { _: i32 },
                    quote! { _: *mut u8 },
                    quote! { _: usize },
                ],
                vec![
                    quote! { #pname.is_some() as i32 },
                    quote! { #pname.as_deref_mut().map_or(::core::ptr::null_mut(), |__s| __s.as_mut_ptr()) },
                    quote! { #pname.as_deref().map_or(0, |__s| __s.len()) },
                ],
                "bytes".into(),
            ));
        }
        return Ok((
            vec![
                quote! { _: i32 },
                quote! { _: *const u8 },
                quote! { _: usize },
            ],
            vec![
                quote! { #pname.is_some() as i32 },
                quote! { #pname.as_deref().map_or(#krate::__null(), |__s| __s.as_ptr()) },
                quote! { #pname.as_deref().map_or(0, |__s| __s.len()) },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn expansion(input: TokenStream2) -> String {
        build(input).unwrap().to_string()
    }

    #[test]
    fn mutable_byte_slice_preserves_mutable_pointer_provenance() {
        let output = expansion(quote! {
            "Buffer" { fn fill(bytes: &mut [u8]); }
        });
        assert!(output.contains("* mut u8"), "{output}");
        assert!(output.contains("bytes . as_mut_ptr ()"), "{output}");
    }

    #[test]
    fn optional_mutable_byte_slice_uses_a_mutable_reborrow() {
        let output = expansion(quote! {
            "Buffer" { fn fill(bytes: Option<&mut [u8]>); }
        });
        assert!(
            output.contains("mut bytes : Option < & mut [u8] >"),
            "{output}"
        );
        assert!(output.contains("bytes . as_deref_mut ()"), "{output}");
        assert!(output.contains("null_mut"), "{output}");
    }

    #[test]
    fn mutable_numeric_slice_preserves_mutable_pointer_provenance() {
        let output = expansion(quote! {
            "Buffer" { fn fill(values: &mut [f32]); }
        });
        assert!(
            output.contains("values . as_mut_ptr () as * mut u8"),
            "{output}"
        );
    }

    #[test]
    fn rejects_variadic_on_non_call_bindings() {
        let result = build(quote! {
            "Element" {
                #[setter]
                #[variadic]
                fn set_values(this: &JsValue, values: &[JsValue]);
            }
        });
        assert!(
            result.is_err(),
            "#[variadic] on a setter must be rejected before it reaches JavaScript codegen"
        );
        let error = result.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("#[variadic] is only supported on functions, methods, and constructors"),
            "{error}"
        );
    }
}
