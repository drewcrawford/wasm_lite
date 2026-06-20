//! A wasm_lite test suite. Built with `harness = false`, so `wasm_lite::tests!`
//! supplies the entry point and the runner drives each test in its own page.

use wasm_lite::console;

fn arithmetic_works() {
    assert_eq!(2 + 2, 4);
}

fn can_log() {
    console::log("can_log ran inside the test harness");
}

fn deliberately_fails() {
    assert_eq!(2 + 2, 5, "math is definitely broken");
}

wasm_lite::tests! {
    arithmetic_works,
    can_log,
    deliberately_fails,
}
