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
    "Array" {
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

wasm_lite::test_main!();
