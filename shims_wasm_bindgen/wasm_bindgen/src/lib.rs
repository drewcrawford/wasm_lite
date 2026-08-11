// SPDX-License-Identifier: MIT OR Apache-2.0
//! `wasm-bindgen`'s API, lowered onto [`wasm_lite`].
//!
//! This is the **fake-wasm-bindgen** shim from `docs/design-notes.md`: rather
//! than reconciling two binding systems or rewriting `wgpu` by hand, replace
//! wasm-bindgen itself. An application substitutes this crate graph-wide with
//!
//! ```toml
//! [patch.crates-io]
//! wasm-bindgen = { path = ".../shims_wasm_bindgen/wasm_bindgen" }
//! ```
//!
//! and because `js-sys`, `web-sys`, `wasm-bindgen-futures` and `wgpu` are all
//! written *against* wasm-bindgen, the **unmodified upstream crates** then
//! compile on wasm_lite and emit wasm_lite descriptors. One `wasm-lite` codegen
//! pass covers the whole module, with no second binding system to subordinate.
//!
//! It is the mirror of `shims/`, which lowers wasm_lite's API onto
//! wasm-bindgen for an app that stays on wasm-bindgen.
//!
//! # Status
//!
//! This is a bounded compatibility shim, not a complete wasm-bindgen
//! reimplementation. Its browser suite runs unmodified `js-sys` 0.3.85 and DOM
//! `web-sys` 0.3.85; all 135 web-sys WebGPU features compile; and wgpu 28
//! constructs an `Instance` and reaches `navigator.gpu` through
//! `request_adapter`. See the workspace
//! [coverage table](https://github.com/drewcrawford/wasm_lite/blob/main/shims_wasm_bindgen/README.md#what-works)
//! for the version-pinning and test details.
//!
//! [`macro@wasm_bindgen`] translates the attribute grammar those crates use,
//! including property/indexing forms, constructors, namespaces, `catch`,
//! variadics, string enums, generic extern types, `is_type_of`, and thread-local
//! statics.
//!
//! [`JsCast`] is generated per type, so `dyn_into`/`dyn_ref`/`is_instance_of`
//! work.
//!
//! Not yet handled — each an explicit error rather than silently wrong glue:
//! block-level `module`/`raw_module`/`inline_js`/`js_namespace`, `start`, and
//! closures taking `&T`. `final` is accepted as a lookup hint but cannot enforce
//! wasm-bindgen's prototype-only method lookup if an object shadows the method.

#![deny(missing_docs)]

pub use wasm_bindgen_macro_wl::wasm_bindgen;
pub use wasm_lite::JsValue;

pub mod closure;
pub mod error;
pub mod snippet;
pub mod views;
pub use closure::Closure;
pub use error::JsError;

/// Marks a byte buffer as JavaScript's *clamped* kind — `Uint8ClampedArray`
/// rather than `Uint8Array`.
///
/// A transparent wrapper: the bytes cross exactly as they would without it, and
/// the distinction is only about which typed array JS sees. `web-sys` uses it
/// for `ImageData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct Clamped<T>(pub T);

impl<T> core::ops::Deref for Clamped<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> core::ops::DerefMut for Clamped<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// The handle behind a generated extern-type newtype.
///
/// `#[wasm_bindgen] extern "C" { pub type Element; }` produces a
/// `#[repr(transparent)]` struct wrapping a [`JsValue`], and this is how the
/// generated wrappers get at it — a trait rather than field access, so the
/// conversion works across the module boundary each binding is generated into.
pub trait JsObject {
    /// The underlying handle, to lend to an import.
    fn as_js(&self) -> &JsValue;
    /// Wrap a handle returned by an import.
    fn from_js(obj: JsValue) -> Self
    where
        Self: Sized;
    /// Take the handle out, consuming the wrapper.
    fn into_js(self) -> JsValue
    where
        Self: Sized;
}

impl JsObject for JsValue {
    fn as_js(&self) -> &JsValue {
        self
    }
    fn from_js(obj: JsValue) -> JsValue {
        obj
    }
    fn into_js(self) -> JsValue {
        self
    }
}

/// A handle to lend to an import, borrowed when possible.
///
/// Most binding types *are* a handle and can lend theirs directly. A string
/// enum is not — it has to make one — so the argument path returns this rather
/// than a plain `&JsValue`, and pays for a table slot only in the case that
/// needs it.
pub enum JsArgRef<'a> {
    /// The value already holds a handle.
    Borrowed(&'a JsValue),
    /// The value had to be converted, and owns the result for the call.
    Owned(JsValue),
}

impl core::ops::Deref for JsArgRef<'_> {
    type Target = JsValue;
    fn deref(&self) -> &JsValue {
        match self {
            JsArgRef::Borrowed(v) => v,
            JsArgRef::Owned(v) => v,
        }
    }
}

/// Something a generated binding can pass to JS.
pub trait JsArg {
    /// The handle to lend for the duration of a call.
    fn js_arg(&self) -> JsArgRef<'_>;
}

impl JsArg for JsValue {
    fn js_arg(&self) -> JsArgRef<'_> {
        JsArgRef::Borrowed(self)
    }
}

/// Something a generated binding can build from a value JS returned.
pub trait FromJs {
    /// Build from the handle an import returned.
    fn from_js_value(v: JsValue) -> Self;
}

impl FromJs for JsValue {
    fn from_js_value(v: JsValue) -> JsValue {
        v
    }
}

/// Scalars, so a callback declared `FnMut(u32)` works.
///
/// Deliberately *not* a blanket over `JsObject`: that would overlap these, and
/// coherence rejects the pair. The macro emits the per-type impls instead.
macro_rules! scalar_from_js {
    ($($t:ty),*) => { $(
        impl FromJs for $t {
            fn from_js_value(v: JsValue) -> $t {
                v.as_f64().unwrap_or_default() as $t
            }
        }

        impl JsArg for $t {
            fn js_arg(&self) -> JsArgRef<'_> {
                JsArgRef::Owned(JsValue::from_f64(*self as f64))
            }
        }
    )* };
}
scalar_from_js!(i8, i16, i32, isize, u8, u16, u32, usize, f32, f64);

impl FromJs for bool {
    fn from_js_value(v: JsValue) -> bool {
        v.as_bool().unwrap_or_default()
    }
}

impl FromJs for String {
    fn from_js_value(v: JsValue) -> String {
        v.as_string().unwrap_or_default()
    }
}

/// Checked and unchecked downcasting between JS types.
///
/// The same shape as wasm-bindgen's trait of this name, because the point is
/// that upstream code calling `dyn_into`/`dyn_ref`/`is_instance_of` compiles
/// unchanged. `instanceof` is generated per type from the `#[instanceof]`
/// binding kind.
///
/// # Invariant
///
/// The reference conversions ([`JsCast::dyn_ref`], [`JsCast::unchecked_ref`])
/// reinterpret `&Self` as `&T`. That is sound only because every implementor is
/// `#[repr(transparent)]` over the same [`JsValue`] — which holds for the types
/// `#[wasm_bindgen]` generates, and is the same bargain wasm-bindgen makes.
/// Hand-writing an implementation for a type of a different shape breaks it.
pub trait JsCast: JsObject + Sized {
    /// Is this handle an instance of `Self`'s JS class?
    fn instanceof(val: &JsValue) -> bool;

    /// Wrap without checking.
    fn unchecked_from_js(val: JsValue) -> Self;

    /// Checked downcast, giving the original back on failure.
    fn dyn_into<T: JsCast>(self) -> Result<T, Self> {
        if T::instanceof(self.as_js()) {
            Ok(T::unchecked_from_js(self.into_js()))
        } else {
            Err(self)
        }
    }

    /// Checked downcast by reference.
    fn dyn_ref<T: JsCast>(&self) -> Option<&T> {
        if T::instanceof(self.as_js()) {
            // SAFETY: see the trait's invariant.
            Some(unsafe { &*(self as *const Self as *const T) })
        } else {
            None
        }
    }

    /// Is this value an instance of `T`?
    fn is_instance_of<T: JsCast>(&self) -> bool {
        T::instanceof(self.as_js())
    }

    /// wasm-bindgen's other spelling of [`JsCast::is_instance_of`].
    fn has_type<T: JsCast>(&self) -> bool {
        T::instanceof(self.as_js())
    }

    /// Unchecked downcast by value.
    fn unchecked_into<T: JsCast>(self) -> T {
        T::unchecked_from_js(self.into_js())
    }

    /// Unchecked downcast by reference.
    fn unchecked_ref<T: JsCast>(&self) -> &T {
        // SAFETY: see the trait's invariant.
        unsafe { &*(self as *const Self as *const T) }
    }
}

impl JsCast for JsValue {
    /// Every JS value is a `JsValue`, so this is the one type the check is
    /// unconditionally true for.
    fn instanceof(_val: &JsValue) -> bool {
        true
    }
    fn unchecked_from_js(val: JsValue) -> JsValue {
        val
    }
}

/// The wasm_lite runtime, re-exported for generated code.
///
/// This module is why the shim can work at all.
///
/// `wasm_lite`'s macros emit absolute paths — `::wasm_lite::JsValue`,
/// `::wasm_lite::descriptor_bytes`, and so on — which is right when the calling
/// crate depends on wasm_lite, and fatal here: `js-sys`, `web-sys` and `wgpu`
/// depend on `wasm-bindgen` and have never heard of wasm_lite. Requiring them
/// to add a dependency would defeat the entire premise, which is that
/// *unmodified* upstream crates compile.
///
/// So `import!` takes an optional `crate = <path>;` prefix, and generated code
/// reaches the runtime through here instead:
///
/// ```
/// wasm_bindgen::__rt::import! {
///     crate = ::wasm_bindgen::__rt;
///     "Math" { fn random() -> f64; }
/// }
/// # #[cfg(target_arch = "wasm32")]
/// # fn main() {
/// # wasm_bindgen::__rt::set_panic_hook();
/// # let value = random();
/// # assert!((0.0..1.0).contains(&value));
/// # }
/// # #[cfg(not(target_arch = "wasm32"))]
/// # fn main() {}
/// ```
///
/// Every path in that expansion resolves through `wasm-bindgen`, so a crate
/// that depends only on this shim can host wasm_lite's lowering.
///
/// Not a stable API.
#[doc(hidden)]
pub mod __rt {
    /// Normalise a binding's return into `Result<JsValue, JsValue>`.
    ///
    /// wasm-bindgen's internal, but reachable — `app_window` imports it
    /// directly — so it is reproduced with the same signature and impls.
    pub trait IntoJsResult {
        /// Convert, treating a non-`Result` value as success.
        fn into_js_result(self) -> Result<JsValue, JsValue>;
    }

    // Only the blanket impl. wasm-bindgen also has ones for `()` and
    // `Result<T, E>`, which it can because `JsValue` is local to it; here it is
    // foreign, so coherence cannot rule out `(): Into<JsValue>` and the pair
    // overlaps. `JsValue: From<()>` covers the unit case instead.
    impl<T: Into<JsValue>> IntoJsResult for T {
        fn into_js_result(self) -> Result<JsValue, JsValue> {
            Ok(self.into())
        }
    }

    pub use crate::views::wbg_cast;
    /// `core` itself, re-exported.
    ///
    /// Generated code cannot say `::core::…`: in an **edition 2015** crate that
    /// names an extern crate which is not linked unless declared, and
    /// `console_error_panic_hook` — in this very graph — is edition 2015.
    /// Reaching it through the shim, which such a crate has already declared in
    /// order to use `#[wasm_bindgen]`, always resolves.
    pub use core;
    pub use wasm_lite::{
        __Option, __Result, __String, __Vec, __null, __wl_free, __wl_malloc, AsJsValue, Closure,
        FromSretPayload, JsFuture, JsValue, descriptor_bytes, export, import, js_class,
        set_panic_hook,
    };
}

/// Unwrap by *throwing* a JS exception rather than panicking.
///
/// # Caveat
///
/// The exception unwinds *through* the wasm frames without running Rust
/// destructors, so the instance is left in the same suspect state a panic
/// leaves it in — wasm_lite's glue marks it unusable. This matches
/// wasm-bindgen, where `unwrap_throw` has the same hazard. It exists so that
/// upstream code naming it compiles and reports a JS `Error` rather than an
/// opaque abort, not because it is a recoverable path.
pub trait UnwrapThrowExt<T> {
    /// Unwrap, throwing a JS exception on failure.
    fn unwrap_throw(self) -> T;
    /// Unwrap, throwing `message` on failure.
    fn expect_throw(self, message: &str) -> T;
}

impl<T> UnwrapThrowExt<T> for Option<T> {
    fn unwrap_throw(self) -> T {
        self.expect_throw("called `unwrap_throw` on a `None` value")
    }
    fn expect_throw(self, message: &str) -> T {
        match self {
            Some(v) => v,
            None => throw_str(message),
        }
    }
}

impl<T, E: core::fmt::Debug> UnwrapThrowExt<T> for Result<T, E> {
    fn unwrap_throw(self) -> T {
        self.expect_throw("called `unwrap_throw` on an `Err` value")
    }
    fn expect_throw(self, message: &str) -> T {
        match self {
            Ok(v) => v,
            Err(_) => throw_str(message),
        }
    }
}

/// The module's `WebAssembly.Memory`, as wasm-bindgen spells it.
pub fn memory() -> JsValue {
    JsValue::wasm_memory()
}

/// The compiled `WebAssembly.Module`, as wasm-bindgen spells it.
pub fn module() -> JsValue {
    JsValue::wasm_module()
}

/// Throw a JS value, never returning.
pub fn throw_val(value: JsValue) -> ! {
    let thrower =
        wasm_lite::Closure::new_variadic_fallible(
            move |_args| Err(JsObject::as_js(&value).clone()),
        );
    call_thrower(thrower.as_js_value());
    unreachable!("the thrower always raises")
}

/// Throw a JS `Error` with this message, never returning.
///
/// The throw happens inside a closure JS invokes, which is the only way Rust
/// can raise a JS exception; the `unreachable!` is never reached because the
/// exception unwinds the JS frame instead of returning.
pub fn throw_str(message: &str) -> ! {
    let err = JsError::new(message);
    let thrower =
        wasm_lite::Closure::new_variadic_fallible(move |_args| Err(JsObject::as_js(&err).clone()));
    call_thrower(thrower.as_js_value());
    unreachable!("the thrower always raises")
}

mod js {
    use wasm_lite::JsValue;
    wasm_lite::import! {
        "Reflect" {
            /// `Reflect.apply(f, this, args)`.
            fn apply(f: &JsValue, this: &JsValue, args: &JsValue) -> JsValue;
        }
        "Array" {
            /// `Array.of()` — an empty argument list.
            fn of() -> JsValue;
        }
    }
}

fn call_thrower(f: &JsValue) {
    // `Reflect.apply(f, null, [])` through a *non*-`Result` binding, so the
    // closure's exception escapes as a real JS throw rather than being caught
    // and handed back.
    js::apply(f, &JsValue::null(), &js::of());
}

/// The names wasm-bindgen users expect from `wasm_bindgen::prelude::*`.
pub mod prelude {
    pub use crate::{
        Clamped, Closure, FromJs, JsArg, JsCast, JsError, JsObject, JsValue, UnwrapThrowExt,
        wasm_bindgen,
    };
}
