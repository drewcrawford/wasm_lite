// SPDX-License-Identifier: MIT OR Apache-2.0
//! Procedural macros for wasm_lite, built on `syn` + `quote`.
//!
//! Five macros: `import!` (JavaScript imports), `#[export]` (Rust→JS exports),
//! `#[wasm_lite_test]` and `#[wasm_lite_bench]` (browser harness entries), and
//! `js_class!` (typed `JsValue` wrappers). Each parses the input into a typed AST
//! and emits the matching wasm import/export or custom-section descriptor with
//! `quote!`. The descriptor formats and flattened ABI are what
//! `wasm_lite_codegen` reads back to generate the JavaScript glue.

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
    let krate = &args.krate;
    let func = parse_macro_input!(item as ItemFn);

    // Fail closed: an `async fn` body would be constructed and dropped unpolled
    // by the generated entry (`#name()`), so a failing async test would silently
    // pass. Likewise a returned value (e.g. `Result`) would be discarded, hiding
    // an `Err`. Reject both instead of generating a test that can't fail.
    if let Some(asyncness) = &func.sig.asyncness {
        return Error::new_spanned(
            asyncness,
            "#[wasm_lite_test] does not support `async fn`: the future would be \
             dropped without being polled and the test would always pass. Use a \
             sync body that drives the future to completion instead.",
        )
        .to_compile_error()
        .into();
    }
    if let ReturnType::Type(_, ty) = &func.sig.output
        && !matches!(ty.as_ref(), Type::Tuple(t) if t.elems.is_empty())
    {
        return Error::new_spanned(
            ty,
            "#[wasm_lite_test] functions must not return a value: the return \
             value (e.g. a `Result`) would be discarded, so an `Err` would \
             silently pass. Return `()` and assert/unwrap inside the test.",
        )
        .to_compile_error()
        .into();
    }

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
                #krate::set_panic_hook();
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
            #krate::set_panic_hook();
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

/// Mark a function as a wasm_lite benchmark (analogous to `#[bench]`).
///
/// The function takes a `&mut Bencher` and hands it the work to measure:
///
/// ```
/// #[wasm_lite::wasm_lite_bench]
/// fn sum_to_1000(b: &mut wasm_lite::Bencher) {
///     b.iter(|| (0u64..1000).sum::<u64>());
/// }
/// # fn main() {}
/// ```
///
/// Generates an exported `__wl_bench_<module_path>::<name>` entry point and
/// records the benchmark's Rust path in the `__wasm_lite_benches` section, so
/// the runner discovers and drives it the same way it does tests. Each
/// benchmark gets its own page load, so one that traps cannot take the rest of
/// the suite with it.
///
/// Unlike `#[wasm_lite_test]` there is no `(worker)` form. A benchmark timed on
/// a worker would be measuring a thread the browser is free to deprioritize,
/// and `performance.now()` on a worker is coarsened independently of the main
/// thread's — two ways for the number to be wrong that are hard to see in the
/// output. Benchmark the work on the main thread; if what you want to measure
/// *is* the threading, measure it end-to-end from the main thread.
#[proc_macro_attribute]
pub fn wasm_lite_bench(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as BenchArgs);
    let krate = &args.krate;
    let func = parse_macro_input!(item as ItemFn);

    // An `async fn` benchmark is supported, but it cannot be *called* like a sync
    // one — awaiting is the whole point, and a future dropped unpolled would
    // measure nothing while reporting success. The async arm below therefore
    // spawns it on the event loop and defers the verdict, exactly as
    // `async_doctest!` does for tests.
    let is_async = func.sig.asyncness.is_some();
    if let ReturnType::Type(_, ty) = &func.sig.output
        && !matches!(ty.as_ref(), Type::Tuple(t) if t.elems.is_empty())
    {
        return Error::new_spanned(
            ty,
            "#[wasm_lite_bench] functions must not return a value: it would be \
             discarded. Return `()` and measure inside `b.iter(..)`.",
        )
        .to_compile_error()
        .into();
    }
    // A benchmark that takes no `Bencher` has nothing to measure with, and the
    // generated call would fail with an arity error pointing at generated code.
    if func.sig.inputs.len() != 1 {
        return Error::new_spanned(
            &func.sig,
            "#[wasm_lite_bench] functions take exactly one argument, \
             `b: &mut Bencher`",
        )
        .to_compile_error()
        .into();
    }

    let name = &func.sig.ident;
    let entry = format_ident!("__wl_bench_{}", name);

    // A sync benchmark measures and records inline. An async one marks the run
    // pending, drives the body on the event loop, records when it settles, and
    // only then passes — so "never completed" fails via the runner's timeout
    // instead of reporting whatever the exports happened to hold.
    let body = if is_async {
        quote! {
            ::wasm_lite_std::__rt::test_pending();
            ::wasm_lite_std::spawn_local(async {
                let mut __wl_b = #krate::Bencher::new();
                #name(&mut __wl_b).await;
                __wl_b.__wl_record();
                ::wasm_lite_std::__rt::test_pass();
            });
        }
    } else {
        quote! {
            let mut __wl_b = #krate::Bencher::new();
            #name(&mut __wl_b);
            __wl_b.__wl_record();
        }
    };

    quote! {
        #func
        #[unsafe(export_name = concat!("__wl_bench_", module_path!(), "::", stringify!(#name)))]
        pub extern "C" fn #entry() {
            // The hook turns a panic inside the benchmark into a console
            // message the runner can report, rather than a bare trap.
            #[cfg(target_arch = "wasm32")]
            #krate::set_panic_hook();
            #body
        }
        const _: () = {
            const __WL_BENCH_NAME_LEN: usize = concat!(module_path!(), "::", stringify!(#name), "\n").len();
            #[used]
            #[cfg_attr(target_arch = "wasm32", unsafe(link_section = "__wasm_lite_benches"))]
            static __WL_BENCH_NAME: [u8; __WL_BENCH_NAME_LEN] = {
                let bytes = concat!(module_path!(), "::", stringify!(#name), "\n").as_bytes();
                let mut out = [0u8; __WL_BENCH_NAME_LEN];
                let mut i = 0;
                while i < __WL_BENCH_NAME_LEN {
                    out[i] = bytes[i];
                    i += 1;
                }
                out
            };
        };
    }
    .into()
}

/// Arguments to `#[wasm_lite_bench]`: nothing, or `crate = <path>`.
struct BenchArgs {
    /// Where the generated code finds the runtime; see `import!`'s `crate =`.
    krate: syn::Path,
}

impl Parse for BenchArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = BenchArgs {
            krate: syn::parse_quote!(::wasm_lite),
        };
        while !input.is_empty() {
            if input.peek(Token![crate]) {
                input.parse::<Token![crate]>()?;
                input.parse::<Token![=]>()?;
                args.krate = input.parse()?;
            } else {
                return Err(input.error("expected `crate = <path>` or no argument"));
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
    }
}

/// Arguments to `#[wasm_lite_test]`: nothing (main thread) or `(worker)`.
struct TestArgs {
    worker: bool,
    /// Where the generated code finds the runtime; see `import!`'s `crate =`.
    krate: syn::Path,
}

impl Parse for TestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = TestArgs {
            worker: false,
            krate: syn::parse_quote!(::wasm_lite),
        };
        while !input.is_empty() {
            if input.peek(Token![crate]) {
                input.parse::<Token![crate]>()?;
                input.parse::<Token![=]>()?;
                args.krate = input.parse()?;
            } else {
                let ident: Ident = input.parse()?;
                if ident != "worker" {
                    return Err(Error::new_spanned(
                        &ident,
                        "expected `worker`, `crate = <path>`, or no argument",
                    ));
                }
                args.worker = true;
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
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
pub fn export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    // `#[export(crate = ::some::path)]` points the generated code at a
    // re-export of the runtime, for the same reason `import!` takes one: a
    // shim's users have never heard of wasm_lite.
    let krate: syn::Path = if attr.is_empty() {
        syn::parse_quote!(::wasm_lite)
    } else {
        match syn::parse::<CratePath>(attr) {
            Ok(c) => c.path,
            Err(e) => return e.to_compile_error().into(),
        }
    };
    match build_export(&krate, &func) {
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

/// `crate = <path>` as an attribute argument.
struct CratePath {
    path: syn::Path,
}

impl syn::parse::Parse for CratePath {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        input.parse::<Token![crate]>()?;
        input.parse::<Token![=]>()?;
        Ok(CratePath {
            path: input.parse()?,
        })
    }
}

fn build_export(krate: &syn::Path, func: &ItemFn) -> syn::Result<TokenStream2> {
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
    if let Some(unsafety) = &func.sig.unsafety {
        return Err(Error::new_spanned(
            unsafety,
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
