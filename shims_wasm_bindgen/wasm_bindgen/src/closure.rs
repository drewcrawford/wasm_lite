// SPDX-License-Identifier: MIT OR Apache-2.0
//! wasm-bindgen's generic `Closure<T>`, over wasm_lite's concrete one.

use crate::JsValue;
use core::marker::PhantomData;

/// A Rust closure exposed to JavaScript.
///
/// The type parameter carries the closure's signature — `Closure<dyn FnMut()>`,
/// `Closure<dyn FnMut(JsValue)>` — which is how upstream code spells it, and
/// why this is generic even though the runtime underneath
/// ([`wasm_lite::Closure`]) is not. The parameter is phantom: the signature is
/// fixed at construction, where the matching trampoline is chosen.
pub struct Closure<T: ?Sized> {
    inner: wasm_lite::Closure,
    _signature: PhantomData<Box<T>>,
}

impl<T: ?Sized> Closure<T> {
    fn from_inner(inner: wasm_lite::Closure) -> Self {
        Closure {
            inner,
            _signature: PhantomData,
        }
    }

    /// The JS function value, for passing to an import.
    pub fn as_js_value(&self) -> &JsValue {
        self.inner.as_js_value()
    }

    /// Hand the closure to JavaScript for the life of the realm.
    ///
    /// A deliberate leak — the registry entry and table slot are never
    /// released — and the right answer for a listener that outlives any Rust
    /// binding.
    pub fn forget(self) {
        self.inner.forget();
    }
}

/// A `Closure` is a handle like any other, so it can be passed wherever a
/// binding takes one.
impl<T: ?Sized> crate::JsArg for Closure<T> {
    fn js_arg(&self) -> crate::JsArgRef<'_> {
        crate::JsArgRef::Borrowed(self.inner.as_js_value())
    }
}

impl<T: ?Sized> crate::JsObject for Closure<T> {
    fn as_js(&self) -> &JsValue {
        self.inner.as_js_value()
    }
    fn from_js(_obj: JsValue) -> Self {
        // A JS function is not a Rust closure: there is no captured state to
        // recover, and inventing one would make `forget` and `drop` lie.
        panic!("a Closure cannot be reconstructed from an arbitrary JS value")
    }
    fn into_js(self) -> JsValue {
        // The JS function outlives this wrapper, so the registry entry has to
        // stay: dropping it here would hand back an inert function.
        let handle = self.inner.as_js_value().clone();
        self.inner.forget();
        handle
    }
}

impl<T: ?Sized> AsRef<JsValue> for Closure<T> {
    fn as_ref(&self) -> &JsValue {
        self.inner.as_js_value()
    }
}

/// A Rust closure that can become a `Closure<T>`.
///
/// This is what lets `Closure::new` be a *single* generic constructor, as it is
/// in wasm-bindgen. Separate inherent `new`s per signature look equivalent
/// until a call site leaves the signature to inference — `Closure::new(move |_|
/// ..)` — and then they are ambiguous.
pub trait IntoWasmClosure<T: ?Sized> {
    /// Register the closure and return the runtime handle.
    fn into_wasm_closure(self) -> wasm_lite::Closure;
}

impl<F: FnMut() + 'static> IntoWasmClosure<dyn FnMut()> for F {
    fn into_wasm_closure(self) -> wasm_lite::Closure {
        wasm_lite::Closure::new(self)
    }
}

/// Generic over the *argument* type, not just `JsValue`: a callback in a
/// generated binding takes the type it was declared with
/// (`GpuUncapturedErrorEvent`, not `JsValue`), and the handle is converted on
/// the way in. `JsValue` itself is covered because it is `FromJs`, where the
/// conversion is the identity.
impl<T, F> IntoWasmClosure<dyn FnMut(T)> for F
where
    T: crate::FromJs + 'static,
    F: FnMut(T) + 'static,
{
    fn into_wasm_closure(self) -> wasm_lite::Closure {
        let mut f = self;
        wasm_lite::Closure::new_with_arg(move |v| f(T::from_js_value(v)))
    }
}

/// `Fn`, not just `FnMut`: upstream code spells both.
impl<F: Fn() + 'static> IntoWasmClosure<dyn Fn()> for F {
    fn into_wasm_closure(self) -> wasm_lite::Closure {
        wasm_lite::Closure::new(self)
    }
}

impl<T, F> IntoWasmClosure<dyn Fn(T)> for F
where
    T: crate::FromJs + 'static,
    F: Fn(T) + 'static,
{
    fn into_wasm_closure(self) -> wasm_lite::Closure {
        wasm_lite::Closure::new_with_arg(move |v| self(T::from_js_value(v)))
    }
}

/// As [`IntoWasmClosure`], for a closure that runs at most once.
pub trait IntoWasmClosureOnce<T: ?Sized> {
    /// Register the closure and return the runtime handle.
    fn into_wasm_closure_once(self) -> wasm_lite::Closure;
}

impl<F: FnOnce() + 'static> IntoWasmClosureOnce<dyn FnMut()> for F {
    fn into_wasm_closure_once(self) -> wasm_lite::Closure {
        // The registry holds `FnMut`, so a one-shot closure is stored in an
        // `Option` and taken on first call. A second call is a no-op rather
        // than a panic: JS may hold the function after it has fired, and the
        // whole point of a `Closure` here is that a stale reference is inert.
        let mut f = Some(self);
        wasm_lite::Closure::new(move || {
            if let Some(f) = f.take() {
                f();
            }
        })
    }
}

impl<T, F> IntoWasmClosureOnce<dyn FnMut(T)> for F
where
    T: crate::FromJs + 'static,
    F: FnOnce(T) + 'static,
{
    fn into_wasm_closure_once(self) -> wasm_lite::Closure {
        let mut f = Some(self);
        wasm_lite::Closure::new_with_arg(move |v| {
            if let Some(f) = f.take() {
                f(T::from_js_value(v));
            }
        })
    }
}

impl<T: ?Sized> Closure<T> {
    /// Wrap a Rust closure whose signature is `T`.
    pub fn new<F: IntoWasmClosure<T>>(f: F) -> Self {
        Self::from_inner(f.into_wasm_closure())
    }

    /// wasm-bindgen's spelling for the same thing, taking an already-boxed
    /// trait object.
    pub fn wrap(f: Box<T>) -> Self
    where
        Box<T>: IntoWasmClosure<T>,
    {
        Self::from_inner(f.into_wasm_closure())
    }

    /// Wrap a closure that runs at most once.
    pub fn once<F: IntoWasmClosureOnce<T>>(f: F) -> Self {
        Self::from_inner(f.into_wasm_closure_once())
    }

    /// A one-shot closure handed straight to JS, with nothing kept on the Rust
    /// side.
    ///
    /// The registry entry is never released — the JS function has to stay
    /// callable and nothing here knows when it has fired.
    pub fn once_into_js<F: IntoWasmClosureOnce<T>>(f: F) -> JsValue {
        Self::once(f).into_js_value()
    }

    /// The JS function value, leaving the closure alive for the realm's life.
    ///
    /// wasm-bindgen's spelling for "hand this to JS and stop tracking it".
    pub fn into_js_value(self) -> JsValue {
        crate::JsObject::into_js(self)
    }
}

// There is deliberately no `IntoWasmClosure<dyn FnMut(&T)>`. It would overlap
// the by-value impl above — `dyn FnMut(X)` with `X = &Y` matches both — and
// rustc only tolerates that under a future-compatibility lint, so it is not
// something to keep. Ruling it out properly needs negative reasoning
// (`&Y: !FromJs`), which stable Rust does not have.
//
// The only crate in this graph that wanted it, `wasm_safe_thread`, is refused
// earlier anyway for using `inline_js`.
