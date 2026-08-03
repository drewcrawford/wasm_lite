// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed-array *views* over Rust memory.

use crate::{JsObject, JsValue};

mod imp {
    // `import!` names `JsValue` by bare ident.
    #[allow(unused_imports)]
    use wasm_lite::JsValue;

    // `import!` already hands a `&[T]` to JS as a typed-array view over wasm
    // memory. All that is needed is a JS function that returns its argument,
    // and `Object(x)` is exactly that for anything already an object.
    //
    // The namespace is `globalThis`, which is its own property, so this
    // resolves to `globalThis.Object(view)`.
    wasm_lite::import! {
        "globalThis" {
            fn view_i8(v: &[i8]) -> JsValue as "Object";
            fn view_i16(v: &[i16]) -> JsValue as "Object";
            fn view_i32(v: &[i32]) -> JsValue as "Object";
            fn view_i64(v: &[i64]) -> JsValue as "Object";
            fn view_u8(v: &[u8]) -> JsValue as "Object";
            fn view_u16(v: &[u16]) -> JsValue as "Object";
            fn view_u32(v: &[u32]) -> JsValue as "Object";
            fn view_u64(v: &[u64]) -> JsValue as "Object";
            fn view_f32(v: &[f32]) -> JsValue as "Object";
            fn view_f64(v: &[f64]) -> JsValue as "Object";
        }
    }
}

/// A slice that can be presented to JS as a typed-array view.
pub trait SliceView {
    /// A JS typed array over *this* memory — a view, not a copy.
    fn wbg_view(&self) -> JsValue;
}

macro_rules! views {
    ($($t:ty => $f:ident),* $(,)?) => { $(
        impl SliceView for [$t] {
            fn wbg_view(&self) -> JsValue {
                imp::$f(self)
            }
        }
    )* };
}
views! {
    i8 => view_i8, i16 => view_i16, i32 => view_i32, i64 => view_i64,
    u8 => view_u8, u16 => view_u16, u32 => view_u32, u64 => view_u64,
    f32 => view_f32, f64 => view_f64,
}

/// Present a Rust slice to JS as a typed array of the corresponding kind.
///
/// wasm-bindgen's internal spelling, which `js-sys` calls from its
/// `TypedArray::view` constructors.
///
/// # Safety
///
/// The result is a view into wasm's linear memory, so it is invalidated by
/// anything that grows or reallocates that memory — and it does not borrow the
/// slice, so nothing stops it outliving the data. Both hazards are inherited
/// from the API being matched; `js-sys` documents them on `view` itself.
pub unsafe fn wbg_cast<T, U>(slice: &T) -> U
where
    T: SliceView + ?Sized,
    U: JsObject,
{
    U::from_js(slice.wbg_view())
}
