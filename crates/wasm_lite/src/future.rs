// SPDX-License-Identifier: MIT OR Apache-2.0
//! Awaiting JavaScript promises from Rust.
//!
//! Private module; the public docs live on [`JsFuture`], which is re-exported
//! at the crate root.

use crate::{Closure, JsValue};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::cell::RefCell;
use std::rc::Rc;

mod imp {
    use crate::JsValue;

    crate::import! {
        "Promise" {
            /// `promise.then(onFulfilled, onRejected)`.
            fn then2(this: &JsValue, on_ok: &JsValue, on_err: &JsValue) -> JsValue as "then";
        }
    }
}

/// What the settle callbacks hand to the poller.
#[derive(Default)]
struct Shared {
    outcome: Option<Result<JsValue, JsValue>>,
    waker: Option<Waker>,
}

/// Record the outcome and wake the task, if one is waiting.
///
/// The `RefCell` borrow is released *before* waking: a waker may poll
/// re-entrantly, and that poll borrows the same cell.
fn settle(shared: &Rc<RefCell<Shared>>, outcome: Result<JsValue, JsValue>) {
    let waker = {
        let mut s = shared.borrow_mut();
        // A promise settles once, but the callbacks are ordinary JS functions
        // and nothing here depends on the engine to enforce that.
        if s.outcome.is_some() {
            return;
        }
        s.outcome = Some(outcome);
        s.waker.take()
    };
    if let Some(w) = waker {
        w.wake();
    }
}

/// A JavaScript `Promise`, awaitable from Rust.
///
/// Resolves to `Ok` with the fulfilled value, or `Err` with the rejection
/// reason — the same split as binding a throwing import as
/// `Result<_, JsValue>`.
///
/// ```no_run
/// # // no_run because: awaiting a Promise needs wasm_lite_std's executor, which cannot depend back from this core crate without linking duplicate runtime exports
/// # async fn f(promise: &wasm_lite::JsValue) {
/// let value = wasm_lite::JsFuture::new(promise).await;
/// match value {
///     Ok(v) => { /* fulfilled */ }
///     Err(e) => { /* rejected */ }
/// }
/// # }
/// ```
///
/// Awaiting one needs an executor to drive it; on wasm that is
/// `wasm_lite_std::spawn_local`.
///
/// # Dropping before it settles
///
/// Cancelling is safe and does not leak: the `JsFuture` owns the two
/// [`Closure`]s registered with `then`, so dropping it removes them from the
/// closure registry and the promise's eventual callbacks become no-ops. The
/// promise itself still settles — JS has no cancellation — it just has nowhere
/// to report to.
pub struct JsFuture {
    shared: Rc<RefCell<Shared>>,
    /// Owned so the callbacks stay live exactly as long as this future does.
    _on_ok: Closure,
    _on_err: Closure,
    finished: bool,
}

impl JsFuture {
    /// Attach to a JS promise.
    ///
    /// `promise` is anything thenable; `then` is looked up on the value itself.
    pub fn new(promise: &JsValue) -> JsFuture {
        let shared = Rc::new(RefCell::new(Shared::default()));

        let ok_shared = shared.clone();
        let on_ok = Closure::new_with_arg(move |v| settle(&ok_shared, Ok(v)));
        let err_shared = shared.clone();
        let on_err = Closure::new_with_arg(move |e| settle(&err_shared, Err(e)));

        // The returned promise is not needed: the callbacks are the channel.
        let _chained = imp::then2(promise, on_ok.as_js_value(), on_err.as_js_value());

        JsFuture {
            shared,
            _on_ok: on_ok,
            _on_err: on_err,
            finished: false,
        }
    }
}

impl Future for JsFuture {
    type Output = Result<JsValue, JsValue>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Every field is Unpin, so there is nothing to project.
        let this = self.get_mut();
        assert!(
            !this.finished,
            "JsFuture polled after it completed; a promise settles once, so the \
             outcome has already been taken"
        );

        let mut s = this.shared.borrow_mut();
        match s.outcome.take() {
            Some(outcome) => {
                drop(s);
                this.finished = true;
                Poll::Ready(outcome)
            }
            None => {
                // Re-register every poll: the task may have been moved to a
                // different waker since last time.
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl core::fmt::Debug for JsFuture {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JsFuture")
            .field("settled", &self.shared.borrow().outcome.is_some())
            .field("finished", &self.finished)
            .finish()
    }
}
