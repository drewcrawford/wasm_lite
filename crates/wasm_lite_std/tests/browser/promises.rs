// SPDX-License-Identifier: MIT OR Apache-2.0
//! `JsFuture`: awaiting JavaScript promises.

use wasm_lite::{JsFuture, JsValue};

wasm_lite::import! {
    "Promise" {
        /// `Promise.resolve(v)`
        fn resolve(v: &JsValue) -> JsValue;
        /// `Promise.reject(v)`
        fn reject(v: &JsValue) -> JsValue;
    }
    "JSON" {
        fn parse(text: &str) -> JsValue;
        fn stringify(v: &JsValue) -> String;
    }
}

/// A fulfilled promise resolves to `Ok` with its value.
#[wasm_lite::wasm_lite_test]
fn js_future_resolves() {
    wasm_lite_std::async_doctest!(async {
        let value = parse("41");
        let out = JsFuture::new(&resolve(&value)).await;
        let ok = out.expect("promise was fulfilled, so this must be Ok");
        assert_eq!(stringify(&ok), "41");
    });
}

/// A rejected promise resolves to `Err`, rather than trapping the way an
/// uncaught JS exception from a non-Result import would.
#[wasm_lite::wasm_lite_test]
fn js_future_rejects() {
    wasm_lite_std::async_doctest!(async {
        let reason = parse("\"boom\"");
        let out = JsFuture::new(&reject(&reason)).await;
        let err = out.expect_err("promise was rejected, so this must be Err");
        assert_eq!(stringify(&err), "\"boom\"");
    });
}

/// Several promises in flight at once must not cross their outcomes —
/// each `JsFuture` owns its own callbacks and its own shared cell.
#[wasm_lite::wasm_lite_test]
fn js_futures_do_not_cross_outcomes() {
    wasm_lite_std::async_doctest!(async {
        let a = JsFuture::new(&resolve(&parse("1")));
        let b = JsFuture::new(&resolve(&parse("2")));
        let c = JsFuture::new(&reject(&parse("3")));

        // Awaited out of creation order, so a shared or mis-keyed slot
        // would show up as the wrong value rather than a hang.
        let cv = c.await.expect_err("c was rejected");
        let bv = b.await.expect("b was fulfilled");
        let av = a.await.expect("a was fulfilled");

        assert_eq!(stringify(&av), "1");
        assert_eq!(stringify(&bv), "2");
        assert_eq!(stringify(&cv), "3");
    });
}

/// Dropping before the promise settles must not leak or trap: the
/// callbacks go away with the future, and the promise still settles
/// with nowhere to report.
#[wasm_lite::wasm_lite_test]
fn dropping_a_pending_js_future_is_safe() {
    wasm_lite_std::async_doctest!(async {
        for _ in 0..16 {
            let pending = JsFuture::new(&resolve(&parse("7")));
            drop(pending);
        }
        // The instance must still be healthy afterwards.
        let out = JsFuture::new(&resolve(&parse("7"))).await;
        assert_eq!(stringify(&out.expect("fulfilled")), "7");
    });
}
