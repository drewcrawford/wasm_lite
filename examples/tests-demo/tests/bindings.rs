// SPDX-License-Identifier: MIT OR Apache-2.0
//! The non-call binding kinds, exercised against real browser objects.
//!
//! `import!` can express a JS *call* two ways (namespaced function, method on a
//! handle). These are the operations that are not calls — property read and
//! write, `new`, and computed indexing — which a binding surface like web-sys
//! needs for the majority of its API. They cannot be inferred from a Rust
//! signature, so each is requested with an attribute.
//!
//! Everything here runs against `URL` and `Array`, so a passing run means the
//! generated glue is right in a real engine rather than just matching a string
//! in a codegen unit test.

use wasm_lite::{JsValue, wasm_lite_test};

wasm_lite::import! {
    "URL" {
        /// `new URL(spec)`.
        #[constructor]
        fn new_url(spec: &str) -> JsValue as "URL";

        /// `url.pathname`
        #[getter]
        fn pathname(this: &JsValue) -> String;

        /// `url.hash`
        #[getter]
        fn hash(this: &JsValue) -> String;

        /// `url.hash = value`
        #[setter]
        fn set_hash(this: &JsValue, value: &str) as "hash";

        /// `url.toString()` — a plain method, for contrast with the getters.
        fn href(this: &JsValue) -> String as "toString";
    }

    "JSON" {
        fn parse(text: &str) -> JsValue;
    }

    "Array" {
        /// `arr[i]`
        #[indexing_getter]
        fn at(this: &JsValue, i: u32) -> f64;

        /// `arr[i] = value`
        #[indexing_setter]
        fn put(this: &JsValue, i: u32, value: f64);

        /// `arr.length` — the case that motivates getters: as a method call
        /// this would throw, because `length` is a number, not a function.
        #[getter]
        fn length(this: &JsValue) -> f64;

        /// `x instanceof Array`
        #[instanceof]
        fn is_array(this: &JsValue) -> bool as "Array";

        /// `Array.from(view)` — a static method, i.e. a namespaced function
        /// with the class as the namespace. Used here to read a typed-array
        /// view back out as ordinary elements.
        fn from_f32(v: &[f32]) -> JsValue as "from";
        fn from_u32(v: &[u32]) -> JsValue as "from";
        fn from_i32(v: &[i32]) -> JsValue as "from";

        /// `Array.of(v)`, once per narrow scalar type, to check each reaches JS
        /// as the value Rust had.
        fn of_i8(v: i8) -> JsValue as "of";
        fn of_u8(v: u8) -> JsValue as "of";
        fn of_i16(v: i16) -> JsValue as "of";
        fn of_u16(v: u16) -> JsValue as "of";
        fn of_f32(v: f32) -> JsValue as "of";
        fn of_usize(v: usize) -> JsValue as "of";
        fn of_isize(v: isize) -> JsValue as "of";
    }

    "globalThis" {
        /// `String(v)` — the global `String` function, reached as a member of
        /// `globalThis` (which is its own property, so `globalThis.globalThis`
        /// is the global object). Rendering to text is the only way to read a
        /// 64-bit value back without an f64 losing the low bits on the way.
        fn text_of_i64(v: i64) -> String as "String";
        fn text_of_u64(v: u64) -> String as "String";
        /// `BigInt(text)` — back the other way, so the round trip is closed.
        fn parse_i64(text: &str) -> i64 as "BigInt";
        fn parse_u64(text: &str) -> u64 as "BigInt";
    }

    "Object" {
        /// `Object.freeze(o)` bound as a fallible *void* operation. It does
        /// return the object in JS; binding it as `Result<(), _>` discards that
        /// while keeping the throw-to-`Err` mapping.
        fn freeze_it(o: &JsValue) -> Result<(), JsValue> as "freeze";
        fn is_frozen(o: &JsValue) -> bool as "isFrozen";
        /// Throws on a frozen object, which is the `Err` half.
        fn define_prop(o: &JsValue, k: &str, d: &JsValue) -> Result<(), JsValue>
            as "defineProperty";
    }

    "URLTest" {
        /// `x instanceof URL`
        #[instanceof]
        fn is_url(this: &JsValue) -> bool as "URL";

        /// A class no engine defines, to check the guard.
        #[instanceof]
        fn is_nonexistent(this: &JsValue) -> bool as "NoSuchClassAnywhere";
    }
}

#[wasm_lite_test]
fn constructor_builds_an_object() {
    let url = new_url("https://example.com/a/b?q=1");
    assert_eq!(href(&url), "https://example.com/a/b?q=1");
}

#[wasm_lite_test]
fn getter_reads_a_property() {
    let url = new_url("https://example.com/a/b");
    assert_eq!(pathname(&url), "/a/b");
}

#[wasm_lite_test]
fn setter_writes_a_property() {
    let url = new_url("https://example.com/a/b");
    assert_eq!(hash(&url), "", "no fragment to start with");
    set_hash(&url, "section-2");
    assert_eq!(hash(&url), "#section-2");
    // The write must be visible through an unrelated binding too, i.e. it
    // landed on the object rather than on a copy.
    assert_eq!(href(&url), "https://example.com/a/b#section-2");
}

#[wasm_lite_test]
fn getter_on_a_non_function_property() {
    let arr = parse("[10, 20, 30]");
    assert_eq!(length(&arr), 3.0);
}

#[wasm_lite_test]
fn indexing_reads_and_writes_elements() {
    let arr = parse("[10, 20, 30]");
    assert_eq!(at(&arr, 1), 20.0);
    put(&arr, 1, 99.0);
    assert_eq!(at(&arr, 1), 99.0);
    // Untouched neighbours stay put, so the index really was computed.
    assert_eq!(at(&arr, 0), 10.0);
    assert_eq!(at(&arr, 2), 30.0);
}

#[wasm_lite_test]
fn instanceof_distinguishes_classes() {
    let url = new_url("https://example.com/");
    let arr = parse("[1, 2]");

    assert!(is_url(&url));
    assert!(!is_url(&arr));
    assert!(is_array(&arr));
    assert!(!is_array(&url));
}

#[wasm_lite_test]
fn instanceof_of_a_missing_class_is_false_not_a_trap() {
    // Bare `x instanceof undefined` is a TypeError. A downcast test against a
    // class this engine does not have must answer "no" and leave the instance
    // usable — so the assertion after it has to run at all.
    let arr = parse("[1, 2]");
    assert!(!is_nonexistent(&arr));
    assert_eq!(length(&arr), 2.0);
}

#[wasm_lite_test]
fn numeric_slices_cross_as_typed_array_views() {
    // Values, not just length: a wrong view type or a byte/element length mixup
    // still produces *an* array, so only reading the contents back catches it.
    let floats = from_f32(&[1.5f32, 2.5, 3.5]);
    assert_eq!(length(&floats), 3.0);
    assert_eq!(at(&floats, 0), 1.5);
    assert_eq!(at(&floats, 1), 2.5);
    assert_eq!(at(&floats, 2), 3.5);

    // u32 must survive the top bit — the wasm param is an i32, so a value above
    // 2^31 is where a missing reinterpretation would show up.
    let uints = from_u32(&[1u32, 4_000_000_000, 7]);
    assert_eq!(length(&uints), 3.0);
    assert_eq!(at(&uints, 1), 4_000_000_000.0);

    // ...and i32 must keep its sign, which is the same bits read the other way.
    let ints = from_i32(&[-5i32, 0, 5]);
    assert_eq!(at(&ints, 0), -5.0);
    assert_eq!(at(&ints, 2), 5.0);
}

#[wasm_lite_test]
fn a_heap_slice_views_at_a_nonzero_offset() {
    // A Vec is allocated wherever the allocator put it, so this exercises a
    // non-zero byteOffset — the case where an element/byte length confusion
    // reads past the end.
    let v: Vec<f32> = (0..64).map(|i| i as f32 * 0.5).collect();
    let arr = from_f32(&v);
    assert_eq!(length(&arr), 64.0);
    assert_eq!(at(&arr, 0), 0.0);
    assert_eq!(at(&arr, 63), 31.5);
}

/// Narrow scalars all share a wasm `i32` parameter, so each one's edge value is
/// where a missing sign-extension or reinterpretation shows up — and nowhere
/// else, since mid-range values look identical either way.
#[wasm_lite_test]
fn narrow_scalars_keep_their_value() {
    assert_eq!(at(&of_i8(-128), 0), -128.0, "i8 arrives sign-extended");
    assert_eq!(at(&of_u8(255), 0), 255.0, "u8 255 must not read as -1");
    assert_eq!(at(&of_i16(-32768), 0), -32768.0);
    assert_eq!(at(&of_u16(65535), 0), 65535.0, "u16 must not read as -1");
    assert_eq!(at(&of_f32(1.5), 0), 1.5);
    assert_eq!(at(&of_isize(-5), 0), -5.0);
}

/// `usize` is 32-bit unsigned on wasm32, so it needs the same reinterpretation
/// as `u32`: the wasm param is an i32 and surfaces in JS as a *signed* Number.
#[wasm_lite_test]
fn usize_survives_the_top_bit() {
    assert_eq!(at(&of_usize(7), 0), 7.0);
    assert_eq!(at(&of_usize(4_000_000_000), 0), 4_000_000_000.0);
}

/// 64-bit integers cross as wasm `i64`, which the WebAssembly JS API surfaces
/// as a `BigInt`. The values here are chosen to fail if anything on the path
/// were a Number: 2^53+1 is the first integer an f64 cannot represent, and
/// `i64::MIN`/`u64::MAX` sit at the ends of the range.
#[wasm_lite_test]
fn sixty_four_bit_integers_cross_as_bigint() {
    assert_eq!(text_of_i64(9_007_199_254_740_993), "9007199254740993");
    assert_eq!(text_of_i64(i64::MIN), "-9223372036854775808");
    assert_eq!(text_of_i64(-1), "-1");
}

/// `u64` shares the signed wasm `i64` param, so the top half of the range is
/// where a missing reinterpretation shows up — `u64::MAX` reads as `-1`.
#[wasm_lite_test]
fn u64_is_read_unsigned() {
    assert_eq!(text_of_u64(u64::MAX), "18446744073709551615");
    assert_eq!(text_of_u64(1 << 63), "9223372036854775808");
    assert_eq!(text_of_u64(7), "7");
}

#[wasm_lite_test]
fn sixty_four_bit_integers_come_back() {
    assert_eq!(parse_i64("-9223372036854775808"), i64::MIN);
    assert_eq!(parse_i64("9007199254740993"), 9_007_199_254_740_993);
    assert_eq!(parse_u64("18446744073709551615"), u64::MAX);
}

/// `Result<(), E>` — a fallible operation with nothing to hand back. The sret
/// buffer carries only the discriminant.
#[wasm_lite_test]
fn a_fallible_void_call_reports_ok_and_still_runs() {
    let o = parse("{\"a\": 1}");
    assert!(!is_frozen(&o));

    freeze_it(&o).expect("freezing a plain object succeeds");

    // The point of the assertion: with no payload to store there is nothing
    // forcing the call to be evaluated, so this is what catches the glue
    // optimising the side effect away.
    assert!(is_frozen(&o), "the call must still have happened");
}

#[wasm_lite_test]
fn a_fallible_void_call_maps_a_throw_to_err() {
    let o = parse("{}");
    freeze_it(&o).expect("freeze succeeds");

    // Defining a property on a frozen object throws; the generated module is
    // strict, so it is a TypeError rather than a silent no-op.
    let descriptor = parse("{\"value\": 1}");
    assert!(
        define_prop(&o, "x", &descriptor).is_err(),
        "a throw must become Err, not a trap"
    );

    // ...and the instance is still alive to say so.
    assert!(is_frozen(&o));
}

wasm_lite::test_main!();
