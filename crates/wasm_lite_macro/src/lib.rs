//! Procedural macros for wasm_lite, built on `syn` + `quote`.
//!
//! Three macros: `#[export]` (Rust→JS exports), `#[wasm_lite_test]` (test
//! harness entries), and `js_class!` (typed `JsValue` wrappers). Each parses the
//! input into a typed AST and emits the matching wasm export / descriptor with
//! `quote!`. The descriptor format (`name|argtags|rettag`) and the flattened ABI
//! are what `wasm_lite_codegen` reads back to generate the JS glue.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{
    Error, FnArg, Ident, ItemFn, LitStr, Pat, ReturnType, Token, Type, braced, parenthesized,
    parse_macro_input,
};

mod import;
mod ty;
use crate::ty::*;

/// Mark a function as a wasm_lite test (analogous to `#[test]`).
///
/// Generates an exported `__wl_test_<module_path>::<name>` entry point (which
/// installs the panic hook and calls the test) and records the test's Rust path
/// in the `__wasm_lite_tests` section so the runner discovers and drives it.
///
/// By default the test body runs on the browser **main thread**, where
/// `Atomics.wait`-based blocking APIs are unavailable. Pass `(worker)` to run the
/// body on a dedicated Web Worker instead — there blocking primitives
/// (`lock_block`, `recv_block`, `park`, …) work:
///
/// ```
/// #[wasm_lite::wasm_lite_test(worker)]
/// fn blocking_recv() {
///     let (tx, rx) = wasm_lite_std::mpsc::channel();
///     tx.send_block(1).unwrap();
///     assert_eq!(rx.recv_block(), Ok(1));
/// }
/// # fn main() {}
/// ```
///
/// The `(worker)` form expands to a fail-closed async harness (spawn the body on
/// a worker, await its join, propagate panics), so it requires `wasm_lite_std`
/// in scope.
#[proc_macro_attribute]
pub fn wasm_lite_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as TestArgs);
    let func = parse_macro_input!(item as ItemFn);
    let name = &func.sig.ident;
    let entry = format_ident!("__wl_test_{}", name);

    // Worker tests defer the verdict: mark pending, run the body on a worker, and
    // pass only once its join resolves (an awaited worker panic propagates through
    // `.unwrap()` and fails the test). Main-thread tests just call the body.
    //
    // The wasm32 arms carry the browser-runner integration (`set_panic_hook`, the
    // `__rt` pending/pass verdict hooks). Those symbols only exist on wasm32, so on
    // other targets we emit a plain host-runnable fallback instead: a worker test
    // spawns the body on a real thread and blocks on the join; a main-thread test
    // just calls it. This keeps the generated entry compilable/linkable on the host
    // (e.g. as a doctest) without changing the wasm32 expansion at all.
    let entry_body = if args.worker {
        quote! {
            #[cfg(target_arch = "wasm32")]
            {
                ::wasm_lite::set_panic_hook();
                ::wasm_lite_std::__rt::test_pending();
                ::wasm_lite_std::spawn_local(async {
                    ::wasm_lite_std::spawn(#name).join_async().await.unwrap();
                    ::wasm_lite_std::__rt::test_pass();
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                ::wasm_lite_std::spawn(#name).join().unwrap();
            }
        }
    } else {
        quote! {
            #[cfg(target_arch = "wasm32")]
            ::wasm_lite::set_panic_hook();
            #name();
        }
    };

    quote! {
        #func
        #[unsafe(export_name = concat!("__wl_test_", module_path!(), "::", stringify!(#name)))]
        pub extern "C" fn #entry() {
            #entry_body
        }
        const _: () = {
            const __WL_TEST_NAME_LEN: usize = concat!(module_path!(), "::", stringify!(#name), "\n").len();
            #[used]
            #[cfg_attr(target_arch = "wasm32", unsafe(link_section = "__wasm_lite_tests"))]
            static __WL_TEST_NAME: [u8; __WL_TEST_NAME_LEN] = {
                let bytes = concat!(module_path!(), "::", stringify!(#name), "\n").as_bytes();
                let mut out = [0u8; __WL_TEST_NAME_LEN];
                let mut i = 0;
                while i < __WL_TEST_NAME_LEN {
                    out[i] = bytes[i];
                    i += 1;
                }
                out
            };
        };
    }
    .into()
}

/// Arguments to `#[wasm_lite_test]`: nothing (main thread) or `(worker)`.
struct TestArgs {
    worker: bool,
}

impl Parse for TestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(TestArgs { worker: false });
        }
        let ident: Ident = input.parse()?;
        if ident != "worker" {
            return Err(Error::new_spanned(
                &ident,
                "expected `worker` or no argument",
            ));
        }
        Ok(TestArgs { worker: true })
    }
}

/// Export a Rust function to JavaScript callers.
///
/// ```
/// #[wasm_lite::export]
/// pub fn add(a: i32, b: i32) -> i32 { a + b }
/// # fn main() {}
/// ```
///
/// Generates a wasm export (`__wl_export_add`) with a flattened ABI and records
/// the signature in the `__wl_exports` section, so the codegen emits a matching
/// JS wrapper: `import { add } from "./glue.js"; add(2, 3) // 5`.
///
/// Supported arguments: numeric (`i32`/`u32`/`f64`), `bool`, `&str`, `&[u8]`,
/// `JsValue`, and `Option<T>` of those. Supported returns: those, plus `String`,
/// `Vec<u8>`, `JsValue`, and `Option<T>`/`Result<T, E>` (via a return pointer).
#[proc_macro_attribute]
pub fn export(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    match build_export(&func) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Declare imported JavaScript functions grouped by JS namespace.
///
/// ```
/// use wasm_lite::JsValue;
/// wasm_lite::import! {
///     "console" { fn log(msg: &str); }
///     "Math" { fn max2(a: f64, b: f64) -> f64 as "max"; }   // `as` for overloads
///     "Array" { fn push(this: &JsValue, value: f64) -> f64; } // method on a handle
/// }
/// # fn main() {}
/// ```
///
/// For each function, emits a safe Rust wrapper, a function-local wasm import
/// with a flattened ABI, and a line in the `__wasm_lite_imports` section. Each
/// import symbol is `module_path!()`-qualified, so the same JS function can be
/// bound from many crates/modules without link conflicts.
#[proc_macro]
pub fn import(input: TokenStream) -> TokenStream {
    match import::build(input.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn build_export(func: &ItemFn) -> syn::Result<TokenStream2> {
    let name = &func.sig.ident;
    let export_ident = format_ident!("__wl_export_{}", name);

    let mut flat_params: Vec<TokenStream2> = Vec::new(); // shim parameter declarations
    let mut pre: Vec<TokenStream2> = Vec::new(); // statements reconstructing args
    let mut call_args: Vec<TokenStream2> = Vec::new(); // arguments to the user fn
    let mut arg_tags: Vec<String> = Vec::new(); // descriptor tags

    for input in &func.sig.inputs {
        let (pat, ty) = fn_arg(input)?;

        // `Option<T>` arg: a discriminant param plus T's normal flattening.
        if let Some(inner) = generic1(ty, "Option") {
            let (flat, recon, tag) = option_arg(&pat, inner)?;
            flat_params.extend(flat);
            pre.push(recon);
            call_args.push(quote! { #pat });
            arg_tags.push(format!("opt:{tag}"));
            continue;
        }

        if is_str(ty) {
            let (p, l) = (format_ident!("{pat}_ptr"), format_ident!("{pat}_len"));
            flat_params.push(quote! { #p: *const u8 });
            flat_params.push(quote! { #l: usize });
            pre.push(quote! { let #pat = unsafe { ::core::str::from_utf8_unchecked(::core::slice::from_raw_parts(#p, #l)) }; });
            call_args.push(quote! { #pat });
            arg_tags.push("str".into());
        } else if is_byte_slice(ty) {
            let (p, l) = (format_ident!("{pat}_ptr"), format_ident!("{pat}_len"));
            flat_params.push(quote! { #p: *const u8 });
            flat_params.push(quote! { #l: usize });
            pre.push(quote! { let #pat = unsafe { ::core::slice::from_raw_parts(#p, #l) }; });
            call_args.push(quote! { #pat });
            arg_tags.push("bytes".into());
        } else if is_jsvalue(ty) {
            // JS registers the object and passes its index; Rust takes ownership.
            flat_params.push(quote! { #pat: u32 });
            pre.push(quote! { let #pat = ::wasm_lite::JsValue::__wl_from_abi(#pat); });
            call_args.push(quote! { #pat });
            arg_tags.push("handle".into());
        } else if let Some(scalar) = numeric(ty) {
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
    let (ret_decl, ret_tag, ret_expr, is_sret) = build_return(&call, &func.sig.output)?;

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
                #[used] static __WL_KEEP_MALLOC: extern "C" fn(usize) -> *mut u8 = ::wasm_lite::__wl_malloc;
                #[used] static __WL_KEEP_FREE: extern "C" fn(*mut u8, usize) = ::wasm_lite::__wl_free;
            };
        }
    } else {
        quote! {}
    };

    let descriptor = format!("{name}|{}|{ret_tag}", arg_tags.join(","));
    let section = section_literal(&descriptor);
    let len = descriptor.len() + 1;

    Ok(quote! {
        #func
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        #[unsafe(no_mangle)]
        pub extern "C" fn #export_ident( #(#flat_params),* ) #ret_decl {
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
    call: &TokenStream2,
    output: &ReturnType,
) -> syn::Result<(TokenStream2, String, TokenStream2, bool)> {
    let ty = match output {
        ReturnType::Default => return Ok((quote! {}, String::new(), quote! { #call; }, false)),
        ReturnType::Type(_, ty) => ty.as_ref(),
    };

    if let Some(inner) = generic1(ty, "Option") {
        let (tag, write) = payload(inner, &format_ident!("__x"))?;
        let body = quote! {
            let __v: ::core::option::Option<#inner> = #call;
            match __v {
                ::core::option::Option::Some(__x) => {
                    unsafe { ::core::ptr::write_unaligned(__ret as *mut u32, 1u32); }
                    #write
                }
                ::core::option::Option::None => unsafe { ::core::ptr::write_unaligned(__ret as *mut u32, 0u32); },
            }
        };
        return Ok((quote! {}, format!("opt:{tag}"), body, true));
    }

    if let Some((ok_ty, err_ty)) = generic2(ty, "Result") {
        let (ok_tag, ok_write) = payload(ok_ty, &format_ident!("__x"))?;
        let (err_tag, err_write) = payload(err_ty, &format_ident!("__e"))?;
        let body = quote! {
            let __v: ::core::result::Result<#ok_ty, #err_ty> = #call;
            match __v {
                ::core::result::Result::Ok(__x) => {
                    unsafe { ::core::ptr::write_unaligned(__ret as *mut u32, 0u32); }
                    #ok_write
                }
                ::core::result::Result::Err(__e) => {
                    unsafe { ::core::ptr::write_unaligned(__ret as *mut u32, 1u32); }
                    #err_write
                }
            }
        };
        return Ok((quote! {}, format!("res:{ok_tag}:{err_tag}"), body, true));
    }

    if let Some(scalar) = numeric(ty) {
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
            pack_buffer(call, quote! { ::std::string::String }),
            false,
        ));
    }
    if vec_u8(ty) {
        return Ok((
            quote! { -> i64 },
            "bytes".into(),
            pack_buffer(call, quote! { ::std::vec::Vec<u8> }),
            false,
        ));
    }
    if is_ident(ty, "JsValue") {
        // Hand the table slot to JS: take the index, then forget so Drop doesn't
        // free it — ownership transfers across the boundary.
        let expr = quote! {
            let __r: ::wasm_lite::JsValue = #call;
            let __idx = ::wasm_lite::JsValue::__wl_abi(&__r);
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
fn pack_buffer(call: &TokenStream2, ty: TokenStream2) -> TokenStream2 {
    quote! {
        let __r: #ty = #call;
        let __len = __r.len();
        let __ptr = ::wasm_lite::__wl_malloc(__len);
        unsafe { ::core::ptr::copy_nonoverlapping(__r.as_ptr(), __ptr, __len); }
        (((__ptr as usize as u64) << 32) | (__len as u64)) as i64
    }
}

/// Code to write `binding` (of type `ty`) into an sret buffer at `__ret + 8`
/// (str/bytes also use `__ret + 12` for the length). Returns the descriptor tag
/// and the code. Writes are unaligned (the buffer is align-1).
fn payload(ty: &Type, binding: &Ident) -> syn::Result<(String, TokenStream2)> {
    let off8 = quote! { (__ret as *mut u8).add(8) };
    let off12 = quote! { (__ret as *mut u8).add(12) };

    if let Some(scalar) = numeric(ty) {
        let write = match scalar.as_str() {
            "i32" => {
                quote! { unsafe { ::core::ptr::write_unaligned(#off8 as *mut i32, #binding); } }
            }
            "u32" => {
                quote! { unsafe { ::core::ptr::write_unaligned(#off8 as *mut u32, #binding); } }
            }
            _ => quote! { unsafe { ::core::ptr::write_unaligned(#off8 as *mut f64, #binding); } },
        };
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
                    let __h = ::wasm_lite::JsValue::__wl_abi(&#binding);
                    ::core::mem::forget(#binding);
                    unsafe { ::core::ptr::write_unaligned(#off8 as *mut u32, __h); }
                }
            },
        ));
    }
    let buf = quote! {
        {
            let __len = #binding.len();
            let __ptr = ::wasm_lite::__wl_malloc(__len);
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
fn option_arg(pat: &Ident, inner: &Type) -> syn::Result<(Vec<TokenStream2>, TokenStream2, String)> {
    let some = format_ident!("{pat}_some");

    if let Some(scalar) = numeric(inner) {
        let val = format_ident!("{pat}_val");
        return Ok((
            vec![quote! { #some: i32 }, quote! { #val: #inner }],
            quote! { let #pat = if #some != 0 { ::core::option::Option::Some(#val) } else { ::core::option::Option::None }; },
            scalar,
        ));
    }
    if is_ident(inner, "bool") {
        let val = format_ident!("{pat}_val");
        return Ok((
            vec![quote! { #some: i32 }, quote! { #val: i32 }],
            quote! { let #pat = if #some != 0 { ::core::option::Option::Some(#val != 0) } else { ::core::option::Option::None }; },
            "bool".into(),
        ));
    }
    if is_jsvalue(inner) {
        let h = format_ident!("{pat}_h");
        return Ok((
            vec![quote! { #some: i32 }, quote! { #h: u32 }],
            quote! { let #pat = if #some != 0 { ::core::option::Option::Some(::wasm_lite::JsValue::__wl_from_abi(#h)) } else { ::core::option::Option::None }; },
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
            quote! { let #pat = if #some != 0 { ::core::option::Option::Some(unsafe { ::core::str::from_utf8_unchecked(::core::slice::from_raw_parts(#p, #l)) }) } else { ::core::option::Option::None }; },
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
            quote! { let #pat = if #some != 0 { ::core::option::Option::Some(unsafe { ::core::slice::from_raw_parts(#p, #l) }) } else { ::core::option::Option::None }; },
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
// ---------------------------------------------------------------------------

/// Declare a typed handle wrapper over a JS object.
///
/// ```
/// wasm_lite::js_class! {
///     type JsArray;
///     impl JsArray {
///         fn push(&self, value: f64) -> f64;            // method
///         fn join(&self, sep: &str) -> String;          // &str arg, String return
///         fn concat(&self, other: &JsArray) -> JsArray; // typed arg + typed return
///     }
/// }
/// # fn main() {}
/// ```
///
/// Generates a newtype `struct JsArray(JsValue)` with `from_js`/`as_js`/`into_js`
/// and one method per declaration. Each lowers to a `receiver[jsName](args)` call
/// by delegating the ABI to [`import!`], adding only the typed veneer — object
/// types (`&JsArray`, `-> JsArray`) cross as value-table handles and are
/// wrapped/unwrapped automatically. Use `as "jsName"` to bind a differing JS name.
///
/// [`import!`]: macro@import
#[proc_macro]
pub fn js_class(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as JsClass);
    match build_js_class(&parsed) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// A parsed `js_class!`: `type Class; impl Class { <methods> }`.
struct JsClass {
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

fn build_js_class(class_def: &JsClass) -> syn::Result<TokenStream2> {
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

        let js = m.js.clone().unwrap_or_else(|| mname.to_string());
        let js_lit = LitStr::new(&js, Span::call_site());
        import_decls.push(quote! { fn #mname( #(#imp_args),* ) #imp_ret as #js_lit; });
    }

    Ok(quote! {
        pub struct #class(::wasm_lite::JsValue);
        impl #class {
            /// Wrap a `JsValue` as this type (unchecked — no runtime type test).
            pub fn from_js(v: ::wasm_lite::JsValue) -> Self { #class(v) }
            /// Borrow the underlying handle.
            pub fn as_js(&self) -> &::wasm_lite::JsValue { &self.0 }
            /// Unwrap into the underlying handle.
            pub fn into_js(self) -> ::wasm_lite::JsValue { self.0 }
            #(#wrappers)*
        }
        impl ::core::convert::From<#class> for ::wasm_lite::JsValue {
            fn from(v: #class) -> Self { v.0 }
        }
        mod #module {
            use ::wasm_lite::JsValue;
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
