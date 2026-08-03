// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rust closures called from JavaScript.
//!
//! Driven through real JS call sites rather than by calling the trampoline
//! directly, so these also prove the generated glue wires `__wl_closure_new`
//! and that the trampolines survive linking — a `#[no_mangle]` function reached
//! only from JS has nothing in the Rust call graph to keep it.

use std::cell::RefCell;
use std::rc::Rc;
use wasm_lite::{Closure, JsValue, wasm_lite_test};

wasm_lite::import! {
    "Reflect" {
        /// `f()` — call a JS function value with no arguments.
        fn apply0(f: &JsValue, this: &JsValue, args: &JsValue) -> JsValue as "apply";
    }
    "JSON" {
        fn parse(text: &str) -> JsValue;
        /// Reads a handle's *value* back as text, so a test can assert on it.
        fn stringify(v: &JsValue) -> String;
    }
    "globalThis" {
        /// `Number(v)` — reached as a member of `globalThis`, which is its own
        /// property. Produces a handle holding a JS *number*, which is what a
        /// comparator has to return.
        fn number_of(v: f64) -> JsValue as "Number";
    }
    "JSON" {
        /// `JSON.stringify(v)` bound as fallible, so a callback's throw can be
        /// observed as an `Err` rather than killing the instance.
        fn try_stringify(v: &JsValue) -> Result<String, JsValue> as "stringify";
    }
    "Array" {
        /// `arr.sort(cmp)` — a callback taking *two* arguments, which is the
        /// shape a per-arity trampoline cannot cover.
        fn sort_with(this: &JsValue, cmp: &JsValue) -> JsValue as "sort";
        /// `arr.map(f)` — the callback gets (element, index, array).
        fn map_with(this: &JsValue, f: &JsValue) -> JsValue as "map";
        #[getter]
        fn length_of(this: &JsValue) -> f64 as "length";
        /// `obj.toJSON = f`
        #[setter]
        fn set_to_json(this: &JsValue, f: &JsValue) as "toJSON";
        /// `Array.of(v)` — a one-element array, used both to build argument
        /// lists and to park a JS value somewhere Rust does not own it.
        fn of1(v: f64) -> JsValue as "of";
        fn of_handle(v: &JsValue) -> JsValue as "of";
        #[indexing_getter]
        fn at_handle(this: &JsValue, i: u32) -> JsValue;
    }
}

/// `f()` via `Reflect.apply(f, null, [])`.
fn call0(f: &JsValue) {
    let empty = parse("[]");
    let null = parse("null");
    apply0(f, &null, &empty);
}

/// `f(arg)` via `Reflect.apply(f, null, [arg])`.
fn call1_with_number(f: &JsValue, arg: f64) {
    let args = of1(arg);
    let null = parse("null");
    apply0(f, &null, &args);
}

#[wasm_lite_test]
fn a_closure_runs_when_javascript_calls_it() {
    let hits = Rc::new(RefCell::new(0));
    let counter = hits.clone();
    let cb = Closure::new(move || *counter.borrow_mut() += 1);

    call0(cb.as_js_value());
    call0(cb.as_js_value());

    assert_eq!(*hits.borrow(), 2, "JS should have run the closure twice");
}

#[wasm_lite_test]
fn a_closure_keeps_its_captured_state() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    let mut n = 0;
    let cb = Closure::new(move || {
        n += 1;
        sink.borrow_mut().push(n);
    });

    call0(cb.as_js_value());
    call0(cb.as_js_value());
    call0(cb.as_js_value());

    // Mutation persists across calls, so it is one closure, not a fresh copy.
    assert_eq!(*log.borrow(), vec![1, 2, 3]);
}

#[wasm_lite_test]
fn a_closure_receives_its_javascript_argument() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let cb = Closure::new_with_arg(move |v: JsValue| {
        sink.borrow_mut().push(stringify(&v));
    });

    call1_with_number(cb.as_js_value(), 41.0);
    call1_with_number(cb.as_js_value(), 42.0);

    // The values, not just the call count: a handle pointing at the wrong table
    // slot would still arrive as *something*.
    assert_eq!(*seen.borrow(), vec!["41".to_string(), "42".to_string()]);
}

/// The whole reason the registry holds ids rather than `Box` pointers: a JS
/// reference that outlives the `Closure` must not read freed memory.
///
/// The function value is parked inside a JS array first, so dropping the
/// `Closure` releases Rust's handle while JS still holds the function — the
/// exact shape of an event listener nobody unregistered.
#[wasm_lite_test]
fn a_dropped_closure_becomes_inert() {
    let hits = Rc::new(RefCell::new(0));
    let counter = hits.clone();
    let cb = Closure::new(move || *counter.borrow_mut() += 1);

    let parked = of_handle(cb.as_js_value());
    let still_reachable = at_handle(&parked, 0);

    call0(&still_reachable);
    assert_eq!(*hits.borrow(), 1, "reachable through the array");

    drop(cb);

    // The JS function object is still perfectly alive and callable; its Rust
    // closure is gone. It must no-op rather than run, or trap.
    call0(&still_reachable);
    call0(&still_reachable);

    assert_eq!(*hits.borrow(), 1, "a dropped closure must not run again");
}

#[wasm_lite_test]
fn many_closures_get_distinct_identities() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut callbacks = Vec::new();
    for i in 0..8 {
        let sink = log.clone();
        callbacks.push(Closure::new(move || sink.borrow_mut().push(i)));
    }
    for cb in &callbacks {
        call0(cb.as_js_value());
    }
    assert_eq!(*log.borrow(), (0..8).collect::<Vec<i32>>());
}

/// Ids are reused from a free list, so a stale handle must not reach whatever
/// took its slot.
#[wasm_lite_test]
fn a_reused_id_does_not_resurrect_the_old_closure() {
    let log = Rc::new(RefCell::new(Vec::new()));

    let first_sink = log.clone();
    let first = Closure::new(move || first_sink.borrow_mut().push("first"));
    call0(first.as_js_value());
    drop(first);

    let second_sink = log.clone();
    let second = Closure::new(move || second_sink.borrow_mut().push("second"));
    call0(second.as_js_value());

    assert_eq!(*log.borrow(), vec!["first", "second"]);
}

/// A variadic closure receives however many arguments JS passes.
#[wasm_lite_test]
fn a_variadic_closure_sees_every_argument() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let cb = Closure::new_variadic(move |args: &[JsValue]| {
        sink.borrow_mut().push(args.len());
        None
    });

    let arr = parse("[3, 1, 2]");
    let _sorted = map_with(&arr, cb.as_js_value());

    // `map` passes (element, index, array) for each of the three elements.
    assert_eq!(*seen.borrow(), vec![3, 3, 3]);
}

/// The callback's *return value* has to reach JS, which is what makes a
/// comparator work at all.
#[wasm_lite_test]
fn a_variadic_closure_returns_a_value_to_javascript() {
    // Descending sort: return b - a. The arguments arrive as handles, so read
    // them back through JSON to compare.
    let cmp = Closure::new_variadic(move |args: &[JsValue]| {
        let a: f64 = stringify(&args[0]).parse().expect("a number");
        let b: f64 = stringify(&args[1]).parse().expect("a number");
        Some(number_of(b - a))
    });

    let arr = parse("[1, 3, 2]");
    let sorted = sort_with(&arr, cmp.as_js_value());
    assert_eq!(
        stringify(&sorted),
        "[3,2,1]",
        "the comparator drove the sort"
    );
}

/// Returning `None` must reach JS as `undefined`, not as table index 0 — which
/// is why the trampoline returns the handle plus one.
#[wasm_lite_test]
fn returning_none_is_undefined_not_handle_zero() {
    let cb = Closure::new_variadic(move |_args: &[JsValue]| None);
    let arr = parse("[1, 2]");
    let mapped = map_with(&arr, cb.as_js_value());
    assert_eq!(
        stringify(&mapped),
        "[null,null]",
        "undefined maps to null in JSON"
    );
    assert_eq!(length_of(&mapped), 2.0);
}

/// A callback's `Err` has to become a **thrown** JS exception, since that is
/// how a JS API reports failure — and a Rust closure cannot throw by itself.
#[wasm_lite_test]
fn a_fallible_closure_throws_into_javascript() {
    // `JSON.stringify` calls `toJSON` on the value; a throwing replacer is the
    // easiest way to observe the exception crossing back out.
    let boom = Closure::new_variadic_fallible(move |_args: &[JsValue]| {
        Err(JsValue::from_str("callback said no"))
    });
    let obj = parse("{}");
    // Attach the throwing closure as `toJSON`, so stringify calls it.
    set_to_json(&obj, boom.as_js_value());

    let r = try_stringify(&obj);
    let err = r.expect_err("the closure's Err must surface as a throw");
    assert_eq!(err.as_string().as_deref(), Some("callback said no"));
}

/// ...and the instance must survive it, which is the difference between a
/// thrown exception the binding catches and a trap.
#[wasm_lite_test]
fn throwing_from_a_closure_leaves_the_instance_usable() {
    let boom =
        Closure::new_variadic_fallible(move |_args: &[JsValue]| Err(JsValue::from_str("nope")));
    let obj = parse("{}");
    set_to_json(&obj, boom.as_js_value());
    assert!(try_stringify(&obj).is_err());

    // Still alive.
    assert_eq!(stringify(&parse("[1,2]")), "[1,2]");
}

wasm_lite::test_main!();
