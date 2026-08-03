// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opaque handles to JavaScript values.
//!
//! Rust can't hold a JS object directly, so the host keeps a *value table* (an
//! array of live JS values) and hands Rust an integer index. A [`JsValue`] wraps
//! that index. When a JS function returns an object, the generated shim stores
//! it in the table and returns the index; when Rust passes a `&JsValue` back,
//! the shim looks the object up. Dropping a `JsValue` frees its table slot via
//! the `__wasm_lite.__wl_drop` runtime import.

use core::fmt;
use core::marker::PhantomData;

/// A handle to a JavaScript value living in the host's value table.
///
/// The table belongs to one JS realm, so a handle is only meaningful on the
/// thread (worker) that created it — sending it elsewhere would index a
/// *different* table. The `PhantomData<*const ()>` makes `JsValue` `!Send` and
/// `!Sync` so the type system forbids that.
///
/// `repr(transparent)` is load-bearing, not decoration: it is what lets a
/// `&[JsValue]` be handed to JS as a run of table indices without copying.
#[repr(transparent)]
pub struct JsValue {
    idx: u32,
    _not_thread_safe: PhantomData<*const ()>,
}

impl JsValue {
    /// Lower a borrowed handle to its ABI form (the table index).
    #[doc(hidden)]
    pub fn __wl_abi(&self) -> u32 {
        self.idx
    }

    /// Wrap a table index received across the ABI into an owned handle.
    #[doc(hidden)]
    pub fn __wl_from_abi(idx: u32) -> JsValue {
        JsValue {
            idx,
            _not_thread_safe: PhantomData,
        }
    }
}

// Runtime support imports; the generated glue always provides them.
#[link(wasm_import_module = "__wasm_lite")]
unsafe extern "C" {
    #[link_name = "__wl_num"]
    fn make_num(v: f64) -> u32;
    #[link_name = "__wl_str_val"]
    fn make_str(ptr: *const u8, len: usize) -> u32;
    #[link_name = "__wl_bigint"]
    fn make_bigint(v: i64) -> u32;
    #[link_name = "__wl_ubigint"]
    fn make_ubigint(v: i64) -> u32;
    #[link_name = "__wl_bigint_str"]
    fn make_bigint_str(ptr: *const u8, len: usize) -> u32;
    #[link_name = "__wl_as_f64"]
    fn as_f64_out(idx: u32, out: *mut f64) -> i32;
    #[link_name = "__wl_as_bool"]
    fn as_bool_out(idx: u32, out: *mut i32) -> i32;
    #[link_name = "__wl_as_str"]
    fn as_str_out(idx: u32, out: *mut u32) -> i32;
    #[link_name = "__wl_eq"]
    fn strict_eq(a: u32, b: u32) -> i32;
    #[link_name = "__wl_binop"]
    fn binop(op: u32, a: u32, b: u32) -> u32;
    #[link_name = "__wl_unop"]
    fn unop(op: u32, a: u32) -> u32;
    #[link_name = "__wl_cmp"]
    fn cmp3(a: u32, b: u32) -> i32;
    #[link_name = "__wl_is"]
    fn is_kind(kind: u32, a: u32) -> i32;
}

/// JavaScript's relational operators.
///
/// `None` where JS says neither `<`, `>` nor `==` holds — `NaN` against
/// anything, and values whose coercion does not compare.
impl PartialOrd for JsValue {
    fn partial_cmp(&self, other: &JsValue) -> Option<core::cmp::Ordering> {
        match unsafe { cmp3(self.idx, other.idx) } {
            -1 => Some(core::cmp::Ordering::Less),
            0 => Some(core::cmp::Ordering::Equal),
            1 => Some(core::cmp::Ordering::Greater),
            _ => None,
        }
    }
}

/// The JS operators, applied to the values two handles denote.
///
/// These are JavaScript's semantics, not Rust's: `+` concatenates strings,
/// `/` yields `Infinity` rather than panicking, and the bitwise operators
/// coerce to 32-bit integers. A binding surface needs them because the JS
/// types it wraps (`BigInt`, `Number`) are used with operators on the JS side.
macro_rules! __js_binop {
    ($($trait:ident, $method:ident, $op:expr;)*) => { $(
        impl core::ops::$trait for &JsValue {
            type Output = JsValue;
            fn $method(self, rhs: &JsValue) -> JsValue {
                JsValue::__wl_from_abi(unsafe { binop($op, self.idx, rhs.idx) })
            }
        }
    )* };
}
__js_binop! {
    Add, add, 0;
    Sub, sub, 1;
    Mul, mul, 2;
    Div, div, 3;
    Rem, rem, 4;
    BitAnd, bitand, 5;
    BitOr, bitor, 6;
    BitXor, bitxor, 7;
    Shl, shl, 8;
    Shr, shr, 9;
}

impl core::ops::Neg for &JsValue {
    type Output = JsValue;
    fn neg(self) -> JsValue {
        JsValue::__wl_from_abi(unsafe { unop(0, self.idx) })
    }
}

impl core::ops::Not for &JsValue {
    type Output = JsValue;
    fn not(self) -> JsValue {
        JsValue::__wl_from_abi(unsafe { unop(1, self.idx) })
    }
}

impl JsValue {
    /// JS `>>>` — the unsigned right shift, which has no Rust operator.
    pub fn unsigned_shr(&self, rhs: &JsValue) -> JsValue {
        JsValue::__wl_from_abi(unsafe { binop(10, self.idx, rhs.idx) })
    }

    /// JS `~` — bitwise complement, distinct from `Not`'s logical `!`.
    pub fn bit_not(&self) -> JsValue {
        JsValue::__wl_from_abi(unsafe { unop(2, self.idx) })
    }

    /// JS `**`.
    pub fn pow(&self, exp: &JsValue) -> JsValue {
        JsValue::__wl_from_abi(unsafe { binop(11, self.idx, exp.idx) })
    }

    /// JS `==` — *loose* equality, which coerces. [`PartialEq`] is `===`.
    pub fn loose_eq(&self, other: &JsValue) -> bool {
        // The three-way comparison reports 0 for `==`, so loose equality falls
        // out of it without a second import.
        unsafe { cmp3(self.idx, other.idx) == 0 }
    }

    /// `typeof x === "object"` and not `null`.
    pub fn is_object(&self) -> bool {
        unsafe { is_kind(0, self.idx) != 0 }
    }
    /// `typeof x === "function"`.
    pub fn is_function(&self) -> bool {
        unsafe { is_kind(1, self.idx) != 0 }
    }
    /// `typeof x === "string"`.
    pub fn is_string(&self) -> bool {
        unsafe { is_kind(2, self.idx) != 0 }
    }
    /// `x === null`.
    pub fn is_null(&self) -> bool {
        unsafe { is_kind(3, self.idx) != 0 }
    }
    /// `x === undefined`.
    pub fn is_undefined(&self) -> bool {
        unsafe { is_kind(4, self.idx) != 0 }
    }
    /// Whether JS considers the value truthy.
    pub fn is_truthy(&self) -> bool {
        unsafe { is_kind(5, self.idx) != 0 }
    }
    /// Whether JS considers the value falsy.
    pub fn is_falsy(&self) -> bool {
        !self.is_truthy()
    }
    /// `typeof x === "bigint"`.
    pub fn is_bigint(&self) -> bool {
        unsafe { is_kind(6, self.idx) != 0 }
    }
}

/// So that a `JsValue` can stand in wherever a binding takes
/// `impl AsRef<JsValue>`, which is how generated code accepts either a handle
/// or one of the newtypes wrapping one.
impl AsRef<JsValue> for JsValue {
    fn as_ref(&self) -> &JsValue {
        self
    }
}

/// JavaScript strict equality (`===`) between the values two handles denote.
///
/// Two *different* handles to the same object compare equal, which is the
/// useful reading — a handle is a reference, not an identity.
impl PartialEq for JsValue {
    fn eq(&self, other: &JsValue) -> bool {
        unsafe { strict_eq(self.idx, other.idx) != 0 }
    }
}

/// Follows wasm-bindgen, whose `JsValue` is `Eq` too, so that binding crates
/// deriving `Eq` on their types compile.
///
/// Strictly this is a lie: `NaN !== NaN`, so equality is not reflexive. The
/// alternative — omitting `Eq` — breaks every generated type that derives it,
/// for a case no binding relies on.
impl Eq for JsValue {}

impl JsValue {
    /// A handle to the JS number `v`.
    ///
    /// Every JS number is a double, so this is the one numeric constructor;
    /// the `From` impls below funnel into it.
    pub fn from_f64(v: f64) -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_num(v) })
    }

    /// JS `undefined`.
    pub const UNDEFINED: JsValue = JsValue::__wl_const(0);
    /// JS `null`.
    pub const NULL: JsValue = JsValue::__wl_const(1);
    /// JS `true`.
    pub const TRUE: JsValue = JsValue::__wl_const(2);
    /// JS `false`.
    pub const FALSE: JsValue = JsValue::__wl_const(3);

    /// The first four table slots are permanently the JS singletons, so a
    /// handle to one is a constant rather than a call into JS.
    const fn __wl_const(idx: u32) -> JsValue {
        JsValue {
            idx,
            _not_thread_safe: PhantomData,
        }
    }

    /// True for one of the reserved singleton slots, which are never freed and
    /// never cloned into a new slot.
    const fn is_reserved(&self) -> bool {
        self.idx < 4
    }

    /// A handle to a JS boolean.
    pub fn from_bool(v: bool) -> JsValue {
        if v { JsValue::TRUE } else { JsValue::FALSE }
    }

    /// A handle to a JS `BigInt`.
    ///
    /// 64-bit and wider integers become `BigInt` rather than a number: a JS
    /// number is a double and silently loses precision above 2^53, which is
    /// well inside the range of the types that reach here.
    pub fn from_i64(v: i64) -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_bigint(v) })
    }

    /// A handle to an unsigned JS `BigInt`.
    pub fn from_u64(v: u64) -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_ubigint(v as i64) })
    }

    /// A handle to a `BigInt` wider than 64 bits.
    ///
    /// Goes through decimal text, because no wasm parameter can carry the
    /// value in one piece.
    pub fn from_i128(v: i128) -> JsValue {
        let s = v.to_string();
        JsValue::__wl_from_abi(unsafe { make_bigint_str(s.as_ptr(), s.len()) })
    }

    /// As [`JsValue::from_i128`], unsigned.
    pub fn from_u128(v: u128) -> JsValue {
        let s = v.to_string();
        JsValue::__wl_from_abi(unsafe { make_bigint_str(s.as_ptr(), s.len()) })
    }

    /// A handle to a JS string, copied out of wasm memory.
    // Named to match wasm-bindgen's `JsValue::from_str`, which the shim
    // re-exports, so upstream call sites compile unchanged. It is infallible,
    // so it is not `FromStr` and never could be.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> JsValue {
        JsValue::__wl_from_abi(unsafe { make_str(s.as_ptr(), s.len()) })
    }

    /// A handle to JS `null`.
    pub fn null() -> JsValue {
        JsValue::NULL
    }

    /// A handle to JS `undefined`.
    pub fn undefined() -> JsValue {
        JsValue::UNDEFINED
    }

    /// The number this handle holds, or `None` if it is not a JS number.
    ///
    /// The presence flag is separate from the value, so a genuine `NaN` is
    /// `Some(NaN)` rather than indistinguishable from "not a number".
    pub fn as_f64(&self) -> Option<f64> {
        let mut out = 0.0f64;
        let ok = unsafe { as_f64_out(self.idx, &mut out) };
        (ok != 0).then_some(out)
    }

    /// The boolean this handle holds, or `None` if it is not a JS boolean.
    pub fn as_bool(&self) -> Option<bool> {
        let mut out = 0i32;
        let ok = unsafe { as_bool_out(self.idx, &mut out) };
        (ok != 0).then_some(out != 0)
    }

    /// The string this handle holds, or `None` if it is not a JS string.
    ///
    /// The empty string is `Some("")`; only a non-string is `None`.
    pub fn as_string(&self) -> Option<String> {
        let mut out = [0u32; 2];
        let ok = unsafe { as_str_out(self.idx, out.as_mut_ptr()) };
        if ok == 0 {
            return None;
        }
        // The host allocated with `__wl_malloc` and transferred ownership.
        let (ptr, len) = (out[0] as usize as *mut u8, out[1] as usize);
        Some(unsafe { String::from_utf8_unchecked(Vec::from_raw_parts(ptr, len, len)) })
    }
}

macro_rules! __from_number {
    ($($t:ty),*) => { $(
        impl From<$t> for JsValue {
            fn from(v: $t) -> JsValue { JsValue::from_f64(v as f64) }
        }
    )* };
}
// 32 bits and under are exact as doubles; `usize`/`isize` are 32-bit on
// wasm32, so they belong here too.
__from_number!(i8, i16, i32, isize, u8, u16, u32, usize, f32, f64);

macro_rules! __from_bigint {
    ($($t:ty => $ctor:ident),*) => { $(
        impl From<$t> for JsValue {
            fn from(v: $t) -> JsValue { JsValue::$ctor(v as _) }
        }
    )* };
}
__from_bigint!(i64 => from_i64, u64 => from_u64, i128 => from_i128, u128 => from_u128);

impl From<bool> for JsValue {
    fn from(v: bool) -> JsValue {
        JsValue::from_bool(v)
    }
}

impl From<&str> for JsValue {
    fn from(v: &str) -> JsValue {
        JsValue::from_str(v)
    }
}

impl Clone for JsValue {
    /// A second handle to the same JavaScript value.
    ///
    /// The value table holds references, so this allocates a table slot rather
    /// than copying the object: both handles denote the *same* JS value, and
    /// each frees its own slot on drop. That matches how JS references behave,
    /// and it is what lets generated bindings derive `Clone` on their newtypes.
    fn clone(&self) -> JsValue {
        // A singleton is the same slot for everyone; there is nothing to
        // allocate and nothing to free.
        if self.is_reserved() {
            return JsValue::__wl_const(self.idx);
        }
        // Runtime support import; the generated glue always provides it.
        #[link(wasm_import_module = "__wasm_lite")]
        unsafe extern "C" {
            #[link_name = "__wl_clone"]
            fn clone_handle(idx: u32) -> u32;
        }
        JsValue::__wl_from_abi(unsafe { clone_handle(self.idx) })
    }
}

impl fmt::Debug for JsValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsValue").field("idx", &self.idx).finish()
    }
}

impl Drop for JsValue {
    fn drop(&mut self) {
        if self.is_reserved() {
            return;
        }
        // Runtime support import; the generated glue always provides it.
        #[link(wasm_import_module = "__wasm_lite")]
        unsafe extern "C" {
            #[link_name = "__wl_drop"]
            fn drop_handle(idx: u32);
        }
        unsafe { drop_handle(self.idx) }
    }
}
