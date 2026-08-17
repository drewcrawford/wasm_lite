// SPDX-License-Identifier: MIT OR Apache-2.0
//! Small declarative macros.
//!
//! `import!` used to live here as a `macro_rules!` tt-muncher; it is now a
//! proc-macro in `wasm_lite_macro` (re-exported from the crate root). What
//! remains is the tiny `test_main!` helper.

/// Supply the entry point for a `harness = false` test target.
///
/// `harness = false` test binaries need a `fn main`, but the runner drives each
/// test via its `#[wasm_lite_test]`-generated export, so `main` is a no-op. Call
/// once per test file alongside your [`wasm_lite_test`](crate::wasm_lite_test)
/// functions.
///
/// Using this also tells the runner not to bother running `main`. Where it is
/// *absent* the runner runs `main` after the suite, because then `main` is
/// libtest's entry point and owns any plain `#[test]` in the same binary — see
/// [`__wl_noop_main`](crate::__wl_noop_main).
///
/// [`wasm_lite_test`]: crate::wasm_lite_test
#[macro_export]
macro_rules! test_main {
    () => {
        fn main() {}
        $crate::__wl_noop_main!();
    };
}

/// Record that this binary's `main` does nothing, so the runner can skip it.
///
/// The runner cannot tell `fn main() {}` from libtest's entry point by looking
/// at the module: both are just a `main` export. Guessing the wrong way either
/// costs a page load per suite or silently drops every `#[test]` and doctest in
/// the binary, so the answer is recorded here rather than inferred — the same
/// custom-section channel the test and benchmark names travel over.
///
/// Emitted by [`test_main!`](crate::test_main) and
/// [`bench_main!`](crate::bench_main); there is no reason to call it directly.
#[doc(hidden)]
#[macro_export]
macro_rules! __wl_noop_main {
    () => {
        const _: () = {
            #[used]
            #[cfg_attr(target_arch = "wasm32", unsafe(link_section = "__wl_noop_main"))]
            static __WL_NOOP_MAIN: [u8; 1] = [1];
        };
    };
}

/// Supply the entry point for a `harness = false` bench target.
///
/// The benchmark counterpart of [`test_main!`](crate::test_main): the runner
/// drives each benchmark through its `#[wasm_lite_bench]`-generated export, so
/// `main` has nothing to do.
///
/// A `[[bench]]` target needs `harness = false` in `Cargo.toml`, exactly as a
/// test target does — libtest's own bench harness would otherwise claim `main`.
#[macro_export]
macro_rules! bench_main {
    () => {
        fn main() {}
        $crate::__wl_noop_main!();
    };
}

/// Declare a newtype over a [`JsValue`](crate::JsValue) handle.
///
/// The binding modules ([`fetch`](crate::fetch), [`websocket`](crate::websocket),
/// [`dom`](crate::dom)) are each a set of typed wrappers around one handle, and
/// each wants the same four things: `from_js`/`as_js`/`into_js`,
/// [`AsJsValue`](crate::AsJsValue), `Debug`, and `Clone`.
///
/// Distinct from [`js_class!`](crate::js_class), which also *generates the
/// methods* by lowering each declaration to a `receiver[name](args)` call. That
/// covers plain method calls only; a real binding surface needs constructors,
/// getters and setters, which are written as `import!` declarations by hand.
/// This macro supplies just the wrapper those hand-written bindings hang off.
///
/// `Clone` is a second *reference*, the way a JS variable is: both handles
/// denote the same object and each frees only its own table slot.
macro_rules! js_handle {
    ($($(#[$m:meta])* $name:ident;)*) => { $(
        $(#[$m])*
        ///
        /// A handle wrapper: `Clone` yields a second reference to the *same* JS
        /// object, not a copy of it.
        #[derive(Debug, Clone)]
        pub struct $name($crate::JsValue);

        impl $name {
            /// Wrap a handle as this type. Unchecked — no `instanceof` test.
            pub fn from_js(v: $crate::JsValue) -> Self {
                $name(v)
            }
            /// Borrow the underlying handle.
            pub fn as_js(&self) -> &$crate::JsValue {
                &self.0
            }
            /// Unwrap into the underlying handle.
            pub fn into_js(self) -> $crate::JsValue {
                self.0
            }
        }

        impl $crate::AsJsValue for $name {
            fn as_js_value(&self) -> &$crate::JsValue {
                &self.0
            }
        }
    )* };
}

// Crate-internal: the binding modules use it, but it is not part of the public
// API. `pub(crate)` visibility for a `macro_rules!` is spelled this way.
pub(crate) use js_handle;
