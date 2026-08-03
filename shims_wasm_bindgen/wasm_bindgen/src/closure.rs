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
