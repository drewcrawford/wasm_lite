// SPDX-License-Identifier: MIT OR Apache-2.0
// `async fn` tests in wasm-bindgen's idiom, on wasm_lite's harness.
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

mod js {
    use wasm_bindgen::JsValue;
    wasm_bindgen::__rt::import! {
        crate = ::wasm_bindgen::__rt;
        "Promise" {
            /// `Promise.resolve(v)`
            fn resolve(v: &JsValue) -> JsValue;
            /// `Promise.reject(v)`
            fn reject(v: &JsValue) -> JsValue;
        }
    }
}

#[wasm_bindgen_test]
async fn an_async_test_runs_past_its_await() {
    let v = wasm_lite::JsFuture::new(&js::resolve(&JsValue::from(41u32)))
        .await
        .expect("a resolved promise");
    assert_eq!(v.as_f64(), Some(41.0));
}

#[wasm_bindgen_test]
async fn a_rejected_promise_is_an_err() {
    let e = wasm_lite::JsFuture::new(&js::reject(&JsValue::from_str("nope")))
        .await
        .expect_err("a rejected promise");
    assert_eq!(e.as_string().as_deref(), Some("nope"));
}

#[wasm_bindgen_test]
fn sync_tests_still_work_alongside() {
    assert_eq!(1 + 1, 2);
}

// The property that matters here is *fail-closed*: an async test that fails
// after an await point must be reported, not silently pass, because dropping
// the future unpolled is exactly what a naive harness would do. That was
// verified with a deliberately failing async test run by hand — the runner
// reported the assertion and exited non-zero. It cannot live in the suite for
// the obvious reason.
