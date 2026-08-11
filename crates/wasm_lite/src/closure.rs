// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rust closures callable from JavaScript.
//!
//! Private module; the public docs live on [`Closure`], which is re-exported at
//! the crate root.

use crate::JsValue;
use core::cell::RefCell;

// Runtime support import; the generated glue always provides it.
#[link(wasm_import_module = "__wasm_lite")]
unsafe extern "C" {
    #[link_name = "__wl_closure_new"]
    fn closure_new(id: u32, arity: u32) -> u32;
}

/// One registry entry.
///
/// `Borrowed` and `Dropped` exist to make "the closure was dropped while it was
/// running" well defined: the slot cannot be reused until the call returns,
/// and the value must not be resurrected afterwards.
enum Slot<T> {
    /// Free, and on the free list.
    Empty,
    Filled(T),
    /// Currently executing; the value is on the stack in the trampoline.
    Borrowed,
    /// Dropped mid-call; release the slot when the trampoline returns.
    Dropped,
}

/// A slab of closures with free-list reuse, so ids stay small and dense.
struct Registry<T> {
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
}

impl<T> Registry<T> {
    const fn new() -> Self {
        Registry {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    fn insert(&mut self, value: T) -> u32 {
        let idx = match self.free.pop() {
            Some(i) => {
                self.slots[i] = Slot::Filled(value);
                i
            }
            None => {
                self.slots.push(Slot::Filled(value));
                self.slots.len() - 1
            }
        };
        idx as u32
    }

    /// Take the closure out for the duration of a call, if it is callable.
    fn borrow_out(&mut self, id: u32) -> Option<T> {
        let slot = self.slots.get_mut(id as usize)?;
        match core::mem::replace(slot, Slot::Borrowed) {
            Slot::Filled(v) => Some(v),
            // Re-entrant call, or already gone: restore and report nothing to
            // call. Restoring matters — `replace` just overwrote the state.
            other => {
                *slot = other;
                None
            }
        }
    }

    /// Put the closure back after a call, unless it was dropped meanwhile.
    fn give_back(&mut self, id: u32, value: T) {
        let Some(slot) = self.slots.get_mut(id as usize) else {
            return;
        };
        match slot {
            Slot::Dropped => {
                *slot = Slot::Empty;
                self.free.push(id as usize);
                drop(value);
            }
            _ => *slot = Slot::Filled(value),
        }
    }

    fn remove(&mut self, id: u32) {
        let Some(slot) = self.slots.get_mut(id as usize) else {
            return;
        };
        match slot {
            // Mid-call: defer, so the trampoline's stack copy stays valid and
            // the id is not reused under it.
            Slot::Borrowed => *slot = Slot::Dropped,
            Slot::Empty | Slot::Dropped => {}
            Slot::Filled(_) => {
                *slot = Slot::Empty;
                self.free.push(id as usize);
            }
        }
    }
}

type Fn0 = Box<dyn FnMut()>;
type Fn1 = Box<dyn FnMut(JsValue)>;
type FnN = Box<dyn FnMut(&[JsValue]) -> Result<Option<JsValue>, JsValue>>;

thread_local! {
    static ZERO_ARG: RefCell<Registry<Fn0>> = const { RefCell::new(Registry::new()) };
    static ONE_ARG: RefCell<Registry<Fn1>> = const { RefCell::new(Registry::new()) };
    static ANY_ARGS: RefCell<Registry<FnN>> = const { RefCell::new(Registry::new()) };
}

/// Run `id`'s closure, with the registry unborrowed for the duration of the
/// call so the closure may itself touch the registry.
macro_rules! invoke {
    ($reg:ident, $id:expr, $call:expr) => {{
        let taken = $reg.with(|r| r.borrow_mut().borrow_out($id));
        if let Some(mut f) = taken {
            #[allow(clippy::redundant_closure_call)]
            $call(&mut f);
            $reg.with(|r| r.borrow_mut().give_back($id, f));
        }
    }};
}

/// Trampoline for a zero-argument closure. Called by the generated glue.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_closure_call_0(id: u32) {
    invoke!(ZERO_ARG, id, |f: &mut Fn0| f());
}

/// Trampoline for a one-argument closure. The handle is created by the glue and
/// ownership passes to Rust, matching the export convention for objects.
///
/// # Safety
///
/// `arg` must name a live value-table slot whose ownership is transferred to
/// this call.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wl_closure_call_1(id: u32, arg: u32) {
    let arg = unsafe { JsValue::__wl_from_abi(arg) };
    // Bound so `arg` is moved into the call exactly once, and still dropped
    // (freeing its table slot) if the closure has already gone away.
    let mut arg = Some(arg);
    invoke!(ONE_ARG, id, |f: &mut Fn1| f(arg.take().expect(
        "the closure is called at most once per trampoline entry"
    )));
    drop(arg);
}

/// Trampoline for a closure of any arity.
///
/// JS packs one value-table index per argument into a buffer it allocated with
/// `__wl_malloc`, and frees it after the call. Ownership of each handle
/// transfers to Rust here, matching the one-argument trampoline.
///
/// The result encodes three outcomes in one `u32`, because table index 0 is a
/// real index and cannot double as "nothing":
///
/// * `0` — returned nothing;
/// * `1 ..= 0x7FFF_FFFF` — the handle, plus one;
/// * high bit set — *throw* the handle in the low bits, minus one.
///
/// The high bit is safe to steal: table indices are allocated densely from
/// zero, so a live one never approaches 2^31.
///
/// # Safety
/// `args_ptr` must point at `argc` consecutive `u32` table indices.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wl_closure_call_n(id: u32, argc: u32, args_ptr: u32) -> u32 {
    let raw = unsafe { core::slice::from_raw_parts(args_ptr as *const u32, argc as usize) };
    // Wrapped up front so the handles are freed on drop whatever happens next
    // — including the closure having already gone away.
    let args: Vec<JsValue> = raw
        .iter()
        .map(|i| unsafe { JsValue::__wl_from_abi(*i) })
        .collect();

    let taken = ANY_ARGS.with(|r| r.borrow_mut().borrow_out(id));
    let Some(mut f) = taken else { return 0 };
    let result = f(&args);
    ANY_ARGS.with(|r| r.borrow_mut().give_back(id, f));

    let (value, throwing) = match result {
        Ok(Some(v)) => (Some(v), false),
        Ok(None) => (None, false),
        Err(e) => (Some(e), true),
    };
    match value {
        // `+ 1` so 0 stays free to mean "returned nothing".
        Some(v) => {
            let idx = v.__wl_abi();
            // JS takes ownership of the slot from here.
            core::mem::forget(v);
            if throwing {
                (idx + 1) | 0x8000_0000
            } else {
                idx + 1
            }
        }
        None => 0,
    }
}

/// A Rust closure exposed to JavaScript as a function value.
///
/// [`export`](crate::export) exports a *static* function. `Closure` is the
/// dynamic counterpart: it wraps a closure — captured state and all — as a real
/// JS function value, which is what an event listener, a `Promise`
/// continuation, or a `wgpu` device-lost callback needs.
///
/// ```no_run
/// use wasm_lite::Closure;
///
/// let mut count = 0;
/// let cb = Closure::new(move || { count += 1; });
/// # let _ = cb.as_js_value();
/// // pass `cb.as_js_value()` to an import, then either keep `cb` alive or
/// // hand it to JS for good with `cb.forget()`.
/// ```
///
/// # How the handle stays sound
///
/// JS is handed a small integer **id**, not a pointer. The closure lives in a
/// thread-local registry that the id indexes. Dropping the `Closure` removes
/// the entry, so a JS caller still holding the function object — an event
/// listener nobody unregistered, say — invokes a **no-op** rather than reading
/// freed memory. A raw `Box` pointer would make that same sequence a
/// use-after-free.
///
/// The registry is thread-local because a [`JsValue`] is only meaningful in the
/// realm that created it; a worker has its own table and its own registry.
///
/// # Re-entrancy
///
/// Calling a closure takes it out of the registry for the duration of the call,
/// so a closure that (directly or indirectly) causes *itself* to be called
/// again sees an empty slot and the inner call is a no-op. This is deliberate:
/// the alternative is aliasing `&mut` to the captured state. A closure dropped
/// *during* its own call is honoured when the call returns.
///
/// # Keeping it alive
///
/// A `Closure` owns the JS function; dropping it makes the function inert. When
/// JS should keep calling it for the remaining life of the realm, hand
/// ownership over with [`Closure::forget`].
#[derive(Debug)]
pub struct Closure {
    handle: JsValue,
    id: u32,
    arity: Arity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arity {
    Zero,
    One,
    Any,
}

impl Closure {
    /// Wrap a zero-argument closure.
    ///
    /// JS arguments, if the caller passes any, are ignored — the same as a JS
    /// function that declares no parameters.
    pub fn new<F>(f: F) -> Closure
    where
        F: FnMut() + 'static,
    {
        // The trampoline is only referenced from JS, so nothing in the Rust
        // call graph keeps it: without this wasm-ld garbage-collects it and the
        // glue calls a missing export. wasm32-only — forcing it to be emitted
        // on the host would pull the wasm-only runtime imports into a native
        // link that has no definitions for them.
        #[cfg(target_arch = "wasm32")]
        #[used]
        static KEEP: extern "C" fn(u32) = __wl_closure_call_0;

        let id = ZERO_ARG.with(|r| r.borrow_mut().insert(Box::new(f)));
        Closure {
            handle: unsafe { JsValue::__wl_from_abi(closure_new(id, 0)) },
            id,
            arity: Arity::Zero,
        }
    }

    /// Wrap a closure taking one JavaScript argument — an event, a resolved
    /// promise value, an error object.
    pub fn new_with_arg<F>(f: F) -> Closure
    where
        F: FnMut(JsValue) + 'static,
    {
        #[cfg(target_arch = "wasm32")]
        #[used]
        static KEEP: unsafe extern "C" fn(u32, u32) = __wl_closure_call_1;

        let id = ONE_ARG.with(|r| r.borrow_mut().insert(Box::new(f)));
        Closure {
            handle: unsafe { JsValue::__wl_from_abi(closure_new(id, 1)) },
            id,
            arity: Arity::One,
        }
    }

    /// Wrap a closure taking however many arguments JavaScript passes.
    ///
    /// The callback receives them as a slice of handles and may return one.
    /// This is the shape a general binding layer needs: `Array.prototype.sort`
    /// passes two elements, `find` passes three, and adding a trampoline per
    /// shape would multiply arity by return type.
    ///
    /// Arguments are owned — dropping the slice frees their table slots — so a
    /// callback that wants to keep one should move it out.
    ///
    /// Use [`Closure::new_variadic_fallible`] for a callback that needs to
    /// raise a JS exception.
    pub fn new_variadic<F>(mut f: F) -> Closure
    where
        F: FnMut(&[JsValue]) -> Option<JsValue> + 'static,
    {
        Closure::new_variadic_fallible(move |args| Ok(f(args)))
    }

    /// As [`Closure::new_variadic`], but the callback may fail — and its `Err`
    /// becomes a **thrown** JS exception at the call site.
    ///
    /// This is what a binding needs to be faithful to a JS API that reports
    /// failure by throwing, since a Rust closure cannot throw by itself.
    pub fn new_variadic_fallible<F>(f: F) -> Closure
    where
        F: FnMut(&[JsValue]) -> Result<Option<JsValue>, JsValue> + 'static,
    {
        #[cfg(target_arch = "wasm32")]
        #[used]
        static KEEP: unsafe extern "C" fn(u32, u32, u32) -> u32 = __wl_closure_call_n;

        let id = ANY_ARGS.with(|r| r.borrow_mut().insert(Box::new(f)));
        Closure {
            handle: unsafe { JsValue::__wl_from_abi(closure_new(id, 2)) },
            id,
            arity: Arity::Any,
        }
    }

    /// The JS function value, for passing to an import.
    pub fn as_js_value(&self) -> &JsValue {
        &self.handle
    }

    /// Give the closure to JavaScript for the remaining life of the realm.
    ///
    /// The registry entry and the value-table slot are never released, so the
    /// function stays callable. This is a deliberate leak, and it is the right
    /// answer for a listener that outlives any Rust binding — but it *is* a
    /// leak, so prefer keeping the [`Closure`] alive where you can.
    pub fn forget(self) {
        core::mem::forget(self);
    }
}

impl Drop for Closure {
    fn drop(&mut self) {
        match self.arity {
            Arity::Zero => ZERO_ARG.with(|r| r.borrow_mut().remove(self.id)),
            Arity::One => ONE_ARG.with(|r| r.borrow_mut().remove(self.id)),
            Arity::Any => ANY_ARGS.with(|r| r.borrow_mut().remove(self.id)),
        }
        // `handle` drops itself, freeing the value-table slot. Any JS reference
        // that outlives this now points at a function whose registry entry is
        // gone, which the trampoline treats as a no-op.
    }
}
