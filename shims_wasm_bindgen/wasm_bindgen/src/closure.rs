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

impl Closure<dyn FnMut()> {
    /// Wrap a zero-argument closure.
    pub fn new<F: FnMut() + 'static>(f: F) -> Self {
        Self::from_inner(wasm_lite::Closure::new(f))
    }

    /// wasm-bindgen's spelling for the same thing, taking an already-boxed
    /// trait object.
    pub fn wrap(mut f: Box<dyn FnMut()>) -> Self {
        Self::from_inner(wasm_lite::Closure::new(move || f()))
    }
}

impl Closure<dyn FnMut(JsValue)> {
    /// Wrap a closure taking one JavaScript argument.
    pub fn new<F: FnMut(JsValue) + 'static>(f: F) -> Self {
        Self::from_inner(wasm_lite::Closure::new_with_arg(f))
    }

    /// As [`Closure::wrap`], for the one-argument signature.
    pub fn wrap(mut f: Box<dyn FnMut(JsValue)>) -> Self {
        Self::from_inner(wasm_lite::Closure::new_with_arg(move |v| f(v)))
    }
}
