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

        /// Sparse reads: past the end is `undefined`, i.e. `None`. Each width
        /// takes a different path through the sret buffer.
        #[indexing_getter]
        fn at_opt_i64(this: &JsValue, i: u32) -> Option<i64>;
        #[indexing_getter]
        fn at_opt_u8(this: &JsValue, i: u32) -> Option<u8>;
        #[indexing_getter]
        fn at_opt_f32(this: &JsValue, i: u32) -> Option<f32>;
        #[indexing_getter]
        fn at_opt_i16(this: &JsValue, i: u32) -> Option<i16>;

        /// `delete arr[i]`
        #[indexing_deleter]
        fn remove_at(this: &JsValue, i: u32) -> bool;

        /// The same, discarding the boolean — which is the usual way to write
        /// it, so the kind must not insist on a return type.
        #[indexing_deleter]
        fn drop_at(this: &JsValue, i: u32);

        /// `arr.length` — the case that motivates getters: as a method call
        /// this would throw, because `length` is a number, not a function.
        #[getter]
        fn length(this: &JsValue) -> f64;

        /// `Array.of(...values)` — the slice is *spread* into the call, so a
        /// three-element slice becomes three arguments rather than one.
        #[variadic]
        fn of_many(values: &[JsValue]) -> JsValue as "of";

        /// The same call without the spread, for contrast: one argument that
        /// happens to be an array.
        fn of_one_array(values: &[JsValue]) -> JsValue as "of";

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

    "globalThis" {
        /// `String(v)` — renders any handle, so a test can see what it holds.
        fn render(v: &JsValue) -> String as "String";
    }

    "Uint8Array" {
        /// `Uint8Array.prototype.set.call(dst, src)` — a *dotted* JS name, and
        /// a destination JS writes into. `set` is borrowed off the prototype
        /// so it can be applied to a view Rust owns.
        fn set_into(dst: &mut [u8], src: &JsValue) as "prototype.set.call";
    }

    "Math" {
        /// `Math.PI` — a namespaced property, not a call. `Math.PI()` throws.
        #[static_getter]
        fn pi() -> f64 as "PI";
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

/// A cloned handle denotes the *same* JS value, not a copy of it — the table
/// holds references. Mutating through one must be visible through the other,
/// and each must free only its own slot.
#[wasm_lite_test]
fn cloning_a_handle_shares_the_object() {
    let url = new_url("https://example.com/a");
    let alias = url.clone();

    set_hash(&url, "one");
    assert_eq!(hash(&alias), "#one", "the clone sees the write");

    set_hash(&alias, "two");
    assert_eq!(
        hash(&url),
        "#two",
        "and the original sees the clone's write"
    );

    // Dropping one leaves the other perfectly usable; a shared slot would make
    // this a use-after-free.
    drop(alias);
    assert_eq!(pathname(&url), "/a");
    assert_eq!(hash(&url), "#two");
}

/// Handles to primitive JS values, which is what `JsValue::from` needs to
/// produce for a binding to pass a number or a string where an object is
/// expected.
#[wasm_lite_test]
fn primitive_handles_hold_the_right_values() {
    assert_eq!(render(&JsValue::from_f64(1.5)), "1.5");
    assert_eq!(render(&JsValue::from_f64(-0.25)), "-0.25");
    assert_eq!(render(&JsValue::from(42u32)), "42");
    assert_eq!(render(&JsValue::from(-7i32)), "-7");

    assert_eq!(render(&JsValue::from_bool(true)), "true");
    assert_eq!(render(&JsValue::from_bool(false)), "false");

    assert_eq!(render(&JsValue::from_str("hi there")), "hi there");
    assert_eq!(render(&JsValue::from("")), "");

    assert_eq!(render(&JsValue::null()), "null");
    assert_eq!(render(&JsValue::undefined()), "undefined");
}

/// `false` and `undefined` must not collapse into the same slot: the four
/// singletons share one import, keyed by a discriminant, which is exactly where
/// an off-by-one would show.
#[wasm_lite_test]
fn the_primitive_singletons_stay_distinct() {
    let values = [
        (JsValue::null(), "null"),
        (JsValue::undefined(), "undefined"),
        (JsValue::from_bool(true), "true"),
        (JsValue::from_bool(false), "false"),
    ];
    for (v, expected) in &values {
        assert_eq!(render(v), *expected);
    }
}

/// Reading a primitive back out of a handle. The presence flag is separate from
/// the value, which is what keeps a genuine `NaN` or `""` distinguishable from
/// "wrong type".
#[wasm_lite_test]
fn primitives_read_back_out_of_handles() {
    assert_eq!(JsValue::from_f64(1.5).as_f64(), Some(1.5));
    assert_eq!(JsValue::from(-7i32).as_f64(), Some(-7.0));
    assert_eq!(JsValue::from_bool(true).as_bool(), Some(true));
    assert_eq!(JsValue::from_bool(false).as_bool(), Some(false));
    assert_eq!(JsValue::from_str("hi").as_string().as_deref(), Some("hi"));

    // Values parsed from JS, not just ones Rust made.
    assert_eq!(parse("41").as_f64(), Some(41.0));
    assert_eq!(parse("true").as_bool(), Some(true));
    assert_eq!(parse("\"text\"").as_string().as_deref(), Some("text"));
}

#[wasm_lite_test]
fn reading_the_wrong_type_is_none_not_a_wrong_answer() {
    let s = JsValue::from_str("not a number");
    assert_eq!(s.as_f64(), None);
    assert_eq!(s.as_bool(), None);

    let n = JsValue::from_f64(1.0);
    assert_eq!(n.as_string(), None);
    assert_eq!(n.as_bool(), None, "1 is truthy but it is not a boolean");

    assert_eq!(JsValue::null().as_f64(), None);
    assert_eq!(JsValue::undefined().as_bool(), None);

    // The edge cases the presence flag exists for.
    assert!(
        JsValue::from_f64(f64::NAN)
            .as_f64()
            .is_some_and(f64::is_nan)
    );
    assert_eq!(JsValue::from_str("").as_string().as_deref(), Some(""));
}

/// A namespaced property read. `Math.PI` is the canonical case for why this
/// cannot be a `Kind::Function`: calling it throws.
#[wasm_lite_test]
fn a_namespaced_property_reads_without_calling() {
    assert!((pi() - core::f64::consts::PI).abs() < 1e-12);
}

/// `delete arr[i]` leaves a hole rather than shortening the array — which is
/// exactly what distinguishes it from a `splice`, and what proves the operator
/// ran.
#[wasm_lite_test]
fn indexing_deleter_removes_the_element() {
    let arr = parse("[10, 20, 30]");
    assert!(remove_at(&arr, 1));

    assert_eq!(length(&arr), 3.0, "delete leaves the length alone");
    assert_eq!(at(&arr, 0), 10.0);
    assert_eq!(at(&arr, 2), 30.0);
    assert_eq!(render(&arr), "10,,30", "the middle slot is now a hole");
}

/// A `&[JsValue]` crosses as a run of table indices, which JS reads back as the
/// objects they denote.
#[wasm_lite_test]
fn a_slice_of_handles_becomes_an_array_of_objects() {
    let items = [parse("1"), parse("\"two\""), parse("[3]")];
    let arr = of_one_array(&items);

    // Not spread: one argument, so `Array.of` wraps it once.
    assert_eq!(length(&arr), 1.0);
    assert_eq!(render(&arr), "1,two,3");
}

/// `#[variadic]` spreads that slice into the call. The distinction is visible
/// in the result's length, which is what makes this testable at all.
#[wasm_lite_test]
fn variadic_spreads_the_final_argument() {
    let items = [parse("1"), parse("2"), parse("3")];
    let arr = of_many(&items);

    assert_eq!(length(&arr), 3.0, "three arguments, not one array");
    assert_eq!(at(&arr, 0), 1.0);
    assert_eq!(at(&arr, 2), 3.0);

    // An empty slice spreads to no arguments at all.
    let empty: [JsValue; 0] = [];
    assert_eq!(length(&of_many(&empty)), 0.0);
}

/// The four JS singletons live in reserved table slots, so they are constants
/// rather than calls — and must never be freed or reallocated.
#[wasm_lite_test]
fn the_singletons_are_stable_constants() {
    assert_eq!(render(&JsValue::UNDEFINED), "undefined");
    assert_eq!(render(&JsValue::NULL), "null");
    assert_eq!(render(&JsValue::TRUE), "true");
    assert_eq!(render(&JsValue::FALSE), "false");

    // Dropping one must not release its slot: a later use would otherwise find
    // whatever got allocated there instead.
    for _ in 0..32 {
        drop(JsValue::UNDEFINED);
        drop(JsValue::NULL);
    }
    let churn: Vec<JsValue> = (0..32).map(JsValue::from).collect();
    assert_eq!(churn.len(), 32);

    assert_eq!(render(&JsValue::UNDEFINED), "undefined");
    assert_eq!(render(&JsValue::NULL), "null");
    assert_eq!(JsValue::TRUE.as_bool(), Some(true));
    assert_eq!(JsValue::FALSE.as_bool(), Some(false));
}

/// Strict equality between handles, which is `===` on what they denote — so
/// two different handles to the same object compare equal.
#[wasm_lite_test]
fn handles_compare_by_javascript_equality() {
    let a = parse("[1]");
    let alias = a.clone();
    assert_eq!(a, alias, "a clone denotes the same object");

    let b = parse("[1]");
    assert_ne!(a, b, "structurally equal but different objects");

    assert_eq!(JsValue::from_f64(1.5), JsValue::from_f64(1.5));
    assert_ne!(JsValue::NULL, JsValue::UNDEFINED, "=== distinguishes these");
}

/// The operators are *JavaScript's*, not Rust's. That is the point: a binding
/// wraps JS types, and they have to behave the way JS does.
#[wasm_lite_test]
fn operators_follow_javascript_semantics() {
    let two = JsValue::from_f64(2.0);
    let three = JsValue::from_f64(3.0);

    assert_eq!((&two + &three).as_f64(), Some(5.0));
    assert_eq!((&three - &two).as_f64(), Some(1.0));
    assert_eq!((&two * &three).as_f64(), Some(6.0));
    assert_eq!((&three % &two).as_f64(), Some(1.0));
    assert_eq!((-&two).as_f64(), Some(-2.0));

    // `+` on strings concatenates — Rust would not compile this at all.
    let hello = JsValue::from_str("he");
    let rest = JsValue::from_str("llo");
    assert_eq!((&hello + &rest).as_string().as_deref(), Some("hello"));

    // Division by zero is Infinity, not a panic.
    let zero = JsValue::from_f64(0.0);
    assert_eq!((&two / &zero).as_f64(), Some(f64::INFINITY));
}

#[wasm_lite_test]
fn bitwise_operators_coerce_to_32_bit_integers() {
    let a = JsValue::from_f64(12.0); // 1100
    let b = JsValue::from_f64(10.0); // 1010

    assert_eq!((&a & &b).as_f64(), Some(8.0));
    assert_eq!((&a | &b).as_f64(), Some(14.0));
    assert_eq!((&a ^ &b).as_f64(), Some(6.0));
    assert_eq!((&a << &JsValue::from_f64(1.0)).as_f64(), Some(24.0));
    assert_eq!((&a >> &JsValue::from_f64(2.0)).as_f64(), Some(3.0));

    // `>>` keeps the sign, `>>>` does not — the distinction Rust has no
    // operator for.
    let minus_eight = JsValue::from_f64(-8.0);
    let one = JsValue::from_f64(1.0);
    assert_eq!((&minus_eight >> &one).as_f64(), Some(-4.0));
    assert_eq!(
        minus_eight.unsigned_shr(&one).as_f64(),
        Some(2147483644.0),
        ">>> fills with zeros"
    );

    // `!` is logical and `~` is bitwise; Rust spells both `!`.
    assert_eq!((!&JsValue::from_f64(0.0)).as_bool(), Some(true));
    assert_eq!(a.bit_not().as_f64(), Some(-13.0));
}

/// `Option<T>` returns put `T` in an sret buffer, and each width reads and
/// writes it differently — 64-bit through BigInt, the narrow ones through
/// sized DataView accessors.
#[wasm_lite_test]
fn option_returns_carry_every_scalar_width() {
    let arr = parse("[7, 255, -32768, 1.5]");

    assert_eq!(at_opt_i64(&arr, 0), Some(7));
    assert_eq!(at_opt_u8(&arr, 1), Some(255), "u8 at its maximum");
    assert_eq!(at_opt_i16(&arr, 2), Some(-32768), "i16 at its minimum");
    assert_eq!(at_opt_f32(&arr, 3), Some(1.5));

    // Past the end is `undefined`, which is `None` — and must not be confused
    // with a zero payload.
    assert_eq!(at_opt_i64(&arr, 9), None);
    assert_eq!(at_opt_u8(&arr, 9), None);
    assert_eq!(at_opt_f32(&arr, 9), None);
}

/// Zero is a value, not an absence — the case a sentinel-based encoding would
/// get wrong.
#[wasm_lite_test]
fn a_zero_payload_is_some_not_none() {
    let zeros = parse("[0, 0, 0]");
    assert_eq!(at_opt_i64(&zeros, 0), Some(0));
    assert_eq!(at_opt_u8(&zeros, 1), Some(0));
    assert_eq!(at_opt_f32(&zeros, 2), Some(0.0));
}

/// JS relational operators. `None` is the answer JS gives for `NaN`, and it is
/// the reason this is `PartialOrd` rather than `Ord`.
#[wasm_lite_test]
fn javascript_ordering_including_the_unordered_case() {
    let one = JsValue::from_f64(1.0);
    let two = JsValue::from_f64(2.0);
    let nan = JsValue::from_f64(f64::NAN);

    assert!(one < two);
    assert!(two > one);
    assert!(one <= JsValue::from_f64(1.0));
    assert_eq!(one.partial_cmp(&two), Some(core::cmp::Ordering::Less));

    // NaN is unordered against everything, itself included.
    assert_eq!(nan.partial_cmp(&one), None);
    assert_eq!(nan.partial_cmp(&nan), None);

    // Strings compare lexicographically, as in JS.
    assert!(JsValue::from_str("a") < JsValue::from_str("b"));
}

#[wasm_lite_test]
fn type_predicates_and_loose_equality() {
    assert!(parse("{}").is_object());
    assert!(!parse("1").is_object());
    assert!(!JsValue::NULL.is_object(), "null is not an object here");

    assert!(JsValue::from_str("s").is_string());
    assert!(JsValue::NULL.is_null());
    assert!(JsValue::UNDEFINED.is_undefined());

    assert!(JsValue::from_f64(1.0).is_truthy());
    assert!(JsValue::from_f64(0.0).is_falsy());
    assert!(JsValue::from_str("").is_falsy());

    // `==` coerces where `===` does not — the whole reason both exist.
    let one = JsValue::from_f64(1.0);
    let one_str = JsValue::from_str("1");
    assert!(one.loose_eq(&one_str));
    assert_ne!(one, one_str, "strict equality says no");
}

#[wasm_lite_test]
fn exponentiation() {
    let two = JsValue::from_f64(2.0);
    assert_eq!(two.pow(&JsValue::from_f64(10.0)).as_f64(), Some(1024.0));
}

/// A dotted `js_name` is a *path*, not a property whose name contains dots —
/// and the destination slice is a view JS writes back through.
#[wasm_lite_test]
fn a_dotted_js_name_resolves_and_writes_back() {
    let mut buf = [0u8; 3];
    set_into(&mut buf, &parse("[10, 20, 30]"));
    assert_eq!(buf, [10, 20, 30], "JS wrote into Rust's buffer");

    // A second call overwrites, so the view is fresh each time rather than a
    // snapshot taken once.
    set_into(&mut buf, &parse("[1, 2, 3]"));
    assert_eq!(buf, [1, 2, 3]);
}

/// 64-bit and wider integers become `BigInt`, not numbers: a JS number is a
/// double and loses precision above 2^53, which is well inside these ranges.
#[wasm_lite_test]
fn wide_integers_convert_to_bigint() {
    let big = JsValue::from(9_007_199_254_740_993i64); // 2^53 + 1
    assert!(big.is_bigint());
    assert_eq!(render(&big), "9007199254740993");

    assert_eq!(render(&JsValue::from(u64::MAX)), "18446744073709551615");
    assert_eq!(render(&JsValue::from(i64::MIN)), "-9223372036854775808");

    // Wider than any wasm parameter, so it travels as decimal text.
    assert_eq!(
        render(&JsValue::from(i128::MIN)),
        "-170141183460469231731687303715884105728"
    );
    assert_eq!(
        render(&JsValue::from(u128::MAX)),
        "340282366920938463463374607431768211455"
    );

    // ...while 32-bit and under stay ordinary numbers.
    let small = JsValue::from(42u32);
    assert!(!small.is_bigint());
    assert_eq!(small.as_f64(), Some(42.0));
}

/// `delete` yields a boolean, and discarding it is the normal spelling.
#[wasm_lite_test]
fn indexing_deleter_may_discard_its_result() {
    let arr = parse("[1, 2, 3]");
    drop_at(&arr, 0);
    assert_eq!(length(&arr), 3.0, "delete leaves a hole");
    assert_eq!(render(&arr), ",2,3");
}

/// `BigInt` division by zero raises a `RangeError`. `checked_div` hands the
/// error back as a value rather than letting it kill the instance.
#[wasm_lite_test]
fn checked_div_returns_the_error_instead_of_throwing() {
    let ten = JsValue::from(10i64);
    let two = JsValue::from(2i64);
    assert_eq!(render(&ten.checked_div(&two)), "5", "ordinary division");

    let zero = JsValue::from(0i64);
    let failed = ten.checked_div(&zero);
    assert!(failed.is_object(), "an Error object, not a number");
    assert!(
        render(&failed).contains("RangeError"),
        "got {}",
        render(&failed)
    );

    // The instance survived, which is the point of catching it.
    assert_eq!(render(&ten.checked_div(&two)), "5");
}

wasm_lite::test_main!();
