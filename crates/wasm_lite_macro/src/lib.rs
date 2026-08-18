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
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Error, Ident, ItemFn, Meta, ReturnType, Token, Type, parse_macro_input};

mod export;
mod import;
mod js_class;
mod ty;

use crate::export::build_export;
use crate::js_class::{JsClass, build_js_class};

/// Mark a function as a test, on every target.
///
/// On wasm32 this generates an exported `__wl_test_<module_path>::<name>` entry
/// point (which installs the panic hook and drives the test) and records the
/// test's Rust path in the `__wasm_lite_tests` section, so the runner discovers
/// it and drives it in a fresh browser page. Everywhere else it registers an
/// ordinary libtest `#[test]`, so `cargo test` runs the same suite on the host:
///
/// ```
/// #[wasm_lite::wasm_lite_test]
/// fn two_plus_two() {
///     assert_eq!(2 + 2, 4);
/// }
/// # fn main() {}
/// ```
///
/// One attribute therefore covers both targets. The
/// `#[cfg_attr(not(target_arch = "wasm32"), test)]` pairing this documentation
/// used to teach still works and needs no rewrite, since off wasm32 the
/// `cfg_attr` holding this macro is false and it never runs.
///
/// A *literal* `#[test]` alongside is different: written below this attribute it
/// is detected and no second registration is made, but written above it rustc
/// expands and consumes it first, so this macro cannot see it and the case is
/// registered twice. Drop the `#[test]`; it is redundant now.
///
/// # `async fn`
///
/// An `async fn` body is driven to completion rather than called:
/// `wasm_lite_std::block_on` off wasm32, and on the event loop with a deferred
/// verdict in the browser. It needs `wasm_lite_std` in scope.
///
/// ```
/// #[wasm_lite::wasm_lite_test]
/// async fn awaits_a_value() {
///     assert_eq!(async { 42 }.await, 42);
/// }
/// # fn main() {}
/// ```
///
/// The verdict is deferred for the reason `async_doctest!` exists: on wasm32
/// the runner decides pass/fail by polling a still-live page, not by the entry
/// point returning. A body that panics after an await, hangs, or is dropped
/// therefore **fails**. That guarantee is what this macro used to secure by
/// rejecting `async fn` outright, so it is checked from the failing side by the
/// fixtures in `examples/must-fail-demo`; a passing suite cannot show it.
///
/// A body that returns a value is still rejected: the value would be discarded,
/// so an `Err` would pass silently.
///
/// # `(worker)`
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
///
/// `(worker)` combines with `async fn`, for a body that awaits *and* blocks:
/// the future is driven by `block_on` on the worker — the one place it can
/// block — while the main thread awaits that worker's join.
///
/// # Pointing at a re-export
///
/// A wrapper crate that re-exports the runtimes points each half at its own
/// re-export, since its users depend on neither by name. `crate =` covers
/// `wasm_lite` and `std_crate =` covers `wasm_lite_std`; the latter matters for
/// the expansions that mention the veneer — `(worker)` and any `async fn`:
///
/// ```text
/// #[wasm_lite_test(worker, crate = ::mycrate::wl, std_crate = ::mycrate::wls)]
/// ```
#[proc_macro_attribute]
pub fn wasm_lite_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as TestArgs);
    let krate = &args.krate;
    let std_krate = &args.std_krate;
    let mut func = parse_macro_input!(item as ItemFn);

    // Fail closed: a returned value (e.g. a `Result`) would be discarded by
    // every arm below, so an `Err` would silently pass. An `async fn` body is
    // *driven* rather than rejected — see `drive` — but a value it resolves to
    // would be dropped just the same, so the rule is the same for both.
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

    let verdict = match Verdict::read(&func.attrs) {
        Ok(verdict) => verdict,
        Err(err) => return err.to_compile_error().into(),
    };
    let fields = verdict.fields();

    let name = func.sig.ident.clone();
    let entry = format_ident!("__wl_test_{}", name);
    let is_async = func.sig.asyncness.is_some();

    // Don't hand a second `#[test]` to a function that already carries one.
    //
    // This only catches a `#[test]` written *below* this attribute. Above it,
    // rustc expands the outer attribute first and libtest consumes it, so by the
    // time this macro runs there is nothing left in `attrs` to find — and the
    // case registers twice. It is not detectable from here, so the docs say so
    // rather than the code pretending otherwise.
    //
    // The pairing that actually matters is the one this macro's documentation
    // used to teach,
    // `#[cfg_attr(not(target_arch = "wasm32"), test)] #[cfg_attr(…, wasm_lite_test)]`,
    // and it is unaffected in either order: off wasm32 the `cfg_attr` holding
    // *this* macro is false, so it never runs at all. `crates/wasm_lite_std/src/sync_tests.rs`
    // is a suite full of them, and `scripts/native/tests` runs it.
    let already_test = func.attrs.iter().any(|attr| attr.path().is_ident("test"));

    // Off wasm32 the case is an ordinary libtest test, so that one attribute
    // covers both targets and `cargo test` runs the same suite natively. What
    // `#[test]` attaches to differs by shape:
    //
    // * A sync body *is* the test, so the attribute goes on it directly. The
    //   reported name is the function's own, and `#[should_panic]`/`#[ignore]`
    //   already sit where libtest reads them.
    // * libtest rejects an `async fn`, so the body moves under a hidden name and
    //   a generated sync test of the original name drives it. The verdict
    //   attributes move with the *test*, not the body: they describe the case,
    //   and libtest reads them off the function it runs.
    let body_ident = if is_async {
        format_ident!("__wl_async_body_{}", name)
    } else {
        name.clone()
    };

    let native_test = if is_async && !already_test {
        let verdict_attrs: Vec<_> = func
            .attrs
            .iter()
            .filter(|attr| is_verdict_attr(attr))
            .cloned()
            .collect();
        func.attrs.retain(|attr| !is_verdict_attr(attr));
        func.sig.ident = body_ident.clone();
        quote! {
            #[cfg(not(target_arch = "wasm32"))]
            #[test]
            #(#verdict_attrs)*
            fn #name() {
                #std_krate::block_on(#body_ident());
            }
        }
    } else {
        if is_async {
            func.sig.ident = body_ident.clone();
        } else if !already_test {
            func.attrs
                .push(syn::parse_quote!(#[cfg_attr(not(target_arch = "wasm32"), test)]));
        }
        quote! {}
    };

    // Worker tests defer the verdict: mark pending, run the body on a worker, and
    // pass only once its join resolves (an awaited worker panic propagates through
    // `.unwrap()` and fails the test). Main-thread tests just call the body.
    //
    // An async body is deferred for the same reason and by the same mechanism as
    // `async_doctest!`: `main` returning is not the verdict, so the page is marked
    // pending and passes only once the future settles. A body that panics, hangs,
    // or is dropped therefore fails, rather than passing because the entry point
    // returned.
    //
    // The entry is emitted for wasm32 only (see below), so these arms use the
    // browser-runner integration (`set_panic_hook`, the `__rt` pending/pass
    // verdict hooks) unconditionally.
    //
    // `std_crate =` for the same reason `crate =` exists: a wrapper crate's users
    // have never heard of wasm_lite_std and do not depend on it, so an absolute
    // `::wasm_lite_std` path in the expansion cannot resolve there.
    let entry_body = match (is_async, args.worker) {
        (false, false) => quote! {
            #krate::set_panic_hook();
            #body_ident();
        },
        (false, true) => quote! {
            #krate::set_panic_hook();
            #std_krate::__rt::test_pending();
            #std_krate::spawn_local(async {
                #std_krate::spawn(#body_ident).join_async().await.unwrap();
                #std_krate::__rt::test_pass();
            });
        },
        (true, false) => quote! {
            #krate::set_panic_hook();
            #std_krate::__rt::test_pending();
            #std_krate::spawn_local(async {
                #body_ident().await;
                #std_krate::__rt::test_pass();
            });
        },
        // An async body that also blocks. The future cannot be driven on the
        // main thread at all — `block_on` traps there — so a worker drives it
        // and the main thread awaits that worker's join.
        (true, true) => quote! {
            #krate::set_panic_hook();
            #std_krate::__rt::test_pending();
            #std_krate::spawn_local(async {
                #std_krate::spawn(|| #std_krate::block_on(#body_ident()))
                    .join_async()
                    .await
                    .unwrap();
                #std_krate::__rt::test_pass();
            });
        },
    };

    // The entry point and its registration exist for the browser runner only.
    // Off wasm32 they must not be emitted: a sync body carries `#[test]`, which
    // strips it from any non-test build (a doctest, a lib's non-`cfg(test)`
    // module), and an entry that still called it would then fail to compile.
    quote! {
        #func
        #native_test
        #[cfg(target_arch = "wasm32")]
        #[unsafe(export_name = concat!("__wl_test_", module_path!(), "::", stringify!(#name)))]
        pub extern "C" fn #entry() {
            #entry_body
        }
        #[cfg(target_arch = "wasm32")]
        const _: () = {
            const __WL_TEST_NAME_LEN: usize = concat!(module_path!(), "::", stringify!(#name), #fields, "\n").len();
            #[used]
            #[unsafe(link_section = "__wasm_lite_tests")]
            static __WL_TEST_NAME: [u8; __WL_TEST_NAME_LEN] = {
                let bytes = concat!(module_path!(), "::", stringify!(#name), #fields, "\n").as_bytes();
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

    let verdict = match Verdict::read(&func.attrs) {
        Ok(verdict) => verdict,
        Err(err) => return err.to_compile_error().into(),
    };
    // `#[ignore]` means the same thing for a benchmark as for a test, but
    // `#[should_panic]` does not: a benchmark that panicked produced no
    // measurement, so there is no result to invert into a pass.
    if verdict.should_panic.is_some() {
        return Error::new_spanned(
            &func.sig.ident,
            "#[wasm_lite_bench] does not support `#[should_panic]`: a benchmark \
             that panics has recorded no measurement, so there is nothing to \
             report. Use #[wasm_lite_test] to assert that something panics.",
        )
        .to_compile_error()
        .into();
    }
    let fields = verdict.fields();

    let name = &func.sig.ident;
    let entry = format_ident!("__wl_bench_{}", name);

    // A sync benchmark measures and records inline. An async one marks the run
    // pending, drives the body on the event loop, records when it settles, and
    // only then passes — so "never completed" fails via the runner's timeout
    // instead of reporting whatever the exports happened to hold.
    let body = if is_async {
        // `std_crate =` for the same reason `#[wasm_lite_test(worker)]` takes
        // one: this is the only bench expansion that names the std veneer, and a
        // wrapper crate's users do not depend on it under that name.
        let std_krate = &args.std_krate;
        quote! {
            #std_krate::__rt::test_pending();
            #std_krate::spawn_local(async {
                let mut __wl_b = #krate::Bencher::new();
                #name(&mut __wl_b).await;
                __wl_b.__wl_record();
                #std_krate::__rt::test_pass();
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
            const __WL_BENCH_NAME_LEN: usize = concat!(module_path!(), "::", stringify!(#name), #fields, "\n").len();
            #[used]
            #[cfg_attr(target_arch = "wasm32", unsafe(link_section = "__wasm_lite_benches"))]
            static __WL_BENCH_NAME: [u8; __WL_BENCH_NAME_LEN] = {
                let bytes = concat!(module_path!(), "::", stringify!(#name), #fields, "\n").as_bytes();
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

/// Arguments to `#[wasm_lite_bench]`: nothing, `crate = <path>`, or
/// `std_crate = <path>` (which only an `async fn` benchmark's expansion uses).
struct BenchArgs {
    /// Where the generated code finds the runtime; see `import!`'s `crate =`.
    krate: syn::Path,
    /// Where an `async fn` benchmark finds the std veneer; see [`TestArgs`].
    std_krate: syn::Path,
}

impl Parse for BenchArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = BenchArgs {
            krate: syn::parse_quote!(::wasm_lite),
            std_krate: syn::parse_quote!(::wasm_lite_std),
        };
        while !input.is_empty() {
            if input.peek(Token![crate]) {
                input.parse::<Token![crate]>()?;
                input.parse::<Token![=]>()?;
                args.krate = input.parse()?;
            } else {
                let ident: Ident = input.parse()?;
                if ident != "std_crate" {
                    return Err(Error::new_spanned(
                        &ident,
                        "expected `crate = <path>`, `std_crate = <path>`, or no argument",
                    ));
                }
                input.parse::<Token![=]>()?;
                args.std_krate = input.parse()?;
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
    }
}

/// The libtest attributes that change how a case's result is judged.
///
/// Read off the function rather than taken as macro arguments, and deliberately
/// *left on* the emitted function. The usual shape in this workspace is
///
/// ```ignore
/// # // ignore because: this attribute combination is illustrative documentation,
/// # // not a complete test function.
/// #[cfg_attr(target_arch = "wasm32", wasm_lite::wasm_lite_test)]
/// #[cfg_attr(not(target_arch = "wasm32"), test)]
/// #[should_panic(expected = "boom")]
/// ```
///
/// where the same `#[should_panic]` has to keep meaning what it says under
/// libtest on the host. Consuming it here would break the native half; ignoring
/// it — which is what this macro used to do — made the wasm32 half report a
/// correct test as failing.
#[derive(Default)]
struct Verdict {
    ignored: bool,
    /// `None` absent; `Some(None)` bare; `Some(Some(msg))` an expected message.
    should_panic: Option<Option<String>>,
}

/// Is this one of the libtest attributes [`Verdict`] reads?
///
/// Only used for an `async fn`, whose body is not itself the libtest test: these
/// have to move onto the generated one, or `#[ignore]` would run the case anyway
/// and `#[should_panic]` would report a correct test as failing — the exact pair
/// of bugs `Verdict` exists to prevent on wasm32.
fn is_verdict_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("ignore") || attr.path().is_ident("should_panic")
}

impl Verdict {
    fn read(attrs: &[Attribute]) -> syn::Result<Verdict> {
        let mut verdict = Verdict::default();
        for attr in attrs {
            if attr.path().is_ident("ignore") {
                verdict.ignored = true;
            } else if attr.path().is_ident("should_panic") {
                verdict.should_panic = Some(expected_message(attr)?);
            }
        }
        Ok(verdict)
    }

    /// The trailing tab-separated fields for this case's harness-section record.
    fn fields(&self) -> String {
        let mut fields = String::new();
        if self.ignored {
            fields.push_str("\tignore");
        }
        match &self.should_panic {
            None => {}
            Some(None) => fields.push_str("\tshould_panic"),
            Some(Some(message)) => {
                fields.push_str("\tshould_panic=");
                fields.push_str(message);
            }
        }
        fields
    }
}

/// The `expected = "…"` of a `#[should_panic]`, if it carries one.
///
/// Accepts both spellings libtest does: `#[should_panic(expected = "…")]` and
/// the older `#[should_panic = "…"]`.
fn expected_message(attr: &Attribute) -> syn::Result<Option<String>> {
    let literal = match &attr.meta {
        Meta::Path(_) => return Ok(None),
        Meta::NameValue(nv) => match &nv.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => s.value(),
            other => {
                return Err(Error::new_spanned(
                    other,
                    "#[should_panic = ...] expects a string literal",
                ));
            }
        },
        Meta::List(_) => {
            let nv: syn::MetaNameValue = attr.parse_args()?;
            if !nv.path.is_ident("expected") {
                return Err(Error::new_spanned(
                    &nv.path,
                    "#[should_panic(...)] only accepts `expected = \"...\"`",
                ));
            }
            match &nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => s.value(),
                other => {
                    return Err(Error::new_spanned(
                        other,
                        "#[should_panic(expected = ...)] expects a string literal",
                    ));
                }
            }
        }
    };
    // The harness section is one record per line, fields split on tabs, so a
    // message carrying either would be read back as a different record.
    if literal.contains('\t') || literal.contains('\n') {
        return Err(Error::new_spanned(
            attr,
            "#[should_panic] expected message must not contain a tab or newline: \
             it is recorded in a line-oriented custom wasm section",
        ));
    }
    Ok(Some(literal))
}

/// Arguments to `#[wasm_lite_test]`: nothing (main thread) or `(worker)`.
struct TestArgs {
    worker: bool,
    /// Where the generated code finds the runtime; see `import!`'s `crate =`.
    krate: syn::Path,
    /// Where the `(worker)` form finds the std veneer.
    ///
    /// Separate from `krate` rather than derived from it: a wrapper crate is
    /// free to re-export `wasm_lite` and `wasm_lite_std` under names with no
    /// relationship to each other, so there is nothing to derive.
    std_krate: syn::Path,
}

impl Parse for TestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = TestArgs {
            worker: false,
            krate: syn::parse_quote!(::wasm_lite),
            std_krate: syn::parse_quote!(::wasm_lite_std),
        };
        while !input.is_empty() {
            if input.peek(Token![crate]) {
                input.parse::<Token![crate]>()?;
                input.parse::<Token![=]>()?;
                args.krate = input.parse()?;
            } else {
                let ident: Ident = input.parse()?;
                if ident == "std_crate" {
                    input.parse::<Token![=]>()?;
                    args.std_krate = input.parse()?;
                } else if ident == "worker" {
                    args.worker = true;
                } else {
                    return Err(Error::new_spanned(
                        &ident,
                        "expected `worker`, `crate = <path>`, `std_crate = <path>`, \
                         or no argument",
                    ));
                }
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
