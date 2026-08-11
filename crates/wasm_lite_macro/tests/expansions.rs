// SPDX-License-Identifier: MIT OR Apache-2.0
//! Compile-time regression coverage for generated code. Unit tests can inspect
//! tokens, but this target makes rustc type-check the expansion itself.

mod imports {
    wasm_lite::import! {
        "Buffer" {
            fn fill_bytes(bytes: &mut [u8]);
            fn fill_optional_bytes(bytes: Option<&mut [u8]>);
            fn fill_floats(values: &mut [f32]);
        }
    }
}

#[wasm_lite::export]
pub fn option_i8(value: i32) -> Option<i8> {
    i8::try_from(value).ok()
}

#[wasm_lite::export]
pub fn option_i64(value: i32) -> Option<i64> {
    Some(i64::from(value))
}

#[wasm_lite::export]
pub fn option_f32(value: i32) -> Option<f32> {
    Some(value as f32)
}

#[wasm_lite::export]
pub fn fallible_unit(fail: bool) -> Result<(), String> {
    if fail {
        Err("failed".to_string())
    } else {
        Ok(())
    }
}

#[wasm_lite::export]
#[allow(clippy::unused_unit)]
pub fn explicit_unit() -> () {}

#[test]
fn generated_items_compile() {
    // Taking the function items also prevents accidental dead-code-only
    // diagnostics from obscuring whether the generated signatures exist.
    let _ = imports::fill_bytes as fn(&mut [u8]);
    let _ = imports::fill_optional_bytes as fn(Option<&mut [u8]>);
    let _ = imports::fill_floats as fn(&mut [f32]);
}
