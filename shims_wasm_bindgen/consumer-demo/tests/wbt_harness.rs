// A suite written the way an existing wasm-bindgen crate writes one, running
// on wasm_lite's harness. The only concession is `harness = false` in the
// manifest — no source change.
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn a_wasm_bindgen_test_runs() {
    assert_eq!(2 + 2, 4);
}

#[wasm_bindgen_test]
fn it_can_reach_javascript() {
    // Through the `wasm-bindgen` shim, which is the pairing this exists for.
    let v = wasm_bindgen::JsValue::from_str("hello");
    assert_eq!(v.as_string().as_deref(), Some("hello"));
}

// A harness that passes everything is worse than none, so that was checked —
// but not from inside a test. These builds are `panic = "abort"`, so
// `catch_unwind` cannot observe a failure; the check was a deliberately failing
// test run by hand, which the runner reported with its message and location and
// exited non-zero on.
