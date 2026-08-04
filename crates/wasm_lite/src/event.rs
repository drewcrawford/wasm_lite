// SPDX-License-Identifier: MIT OR Apache-2.0
//! The DOM [`Event`] base type.
//!
//! Its own module because more than one binding surface hands one back:
//! [`websocket`](crate::websocket) fires `open`/`error` events, and
//! [`dom`](crate::dom) fires everything else. Both re-export [`Event`], so a
//! caller of either needs only that one.

use crate::JsValue;
use crate::macros::js_handle;

mod imp {
    use crate::JsValue;

    crate::import! {
        "Event" {
            #[getter] fn event_type(this: &JsValue) -> String as "type";
            /// The object the event was dispatched on.
            #[getter] fn target(this: &JsValue) -> Option<JsValue>;
            /// The object the listener is attached to — often *not* the target,
            /// which is what makes delegated listeners work.
            #[getter] fn current_target(this: &JsValue) -> Option<JsValue> as "currentTarget";
            #[getter] fn default_prevented(this: &JsValue) -> bool as "defaultPrevented";
            fn prevent_default(this: &JsValue) as "preventDefault";
            fn stop_propagation(this: &JsValue) as "stopPropagation";
        }
    }
}

js_handle! {
    /// A DOM [`Event`](https://developer.mozilla.org/docs/Web/API/Event).
    ///
    /// The base of every event type. A more specific binding —
    /// [`MouseEvent`](crate::dom::MouseEvent),
    /// [`KeyboardEvent`](crate::dom::KeyboardEvent) — offers `as_event` to
    /// reach these.
    Event;
}

impl Event {
    /// The event's type (`"click"`, `"keydown"`, …).
    pub fn event_type(&self) -> String {
        imp::event_type(&self.0)
    }

    /// The object the event was dispatched on.
    pub fn target(&self) -> Option<JsValue> {
        imp::target(&self.0)
    }

    /// The object whose listener is running.
    ///
    /// Not the same as [`target`](Self::target) for a listener on an ancestor,
    /// which is the whole basis of event delegation. It is also `null` once
    /// dispatch has finished, so a handler that stashes the event and reads
    /// this later gets `None`.
    pub fn current_target(&self) -> Option<JsValue> {
        imp::current_target(&self.0)
    }

    /// Suppress the browser's default action — following a link, scrolling on
    /// a wheel event, typing a character.
    ///
    /// Only has an effect for a cancelable event, and only during dispatch.
    pub fn prevent_default(&self) {
        imp::prevent_default(&self.0);
    }

    /// Whether [`prevent_default`](Self::prevent_default) has been called.
    pub fn default_prevented(&self) -> bool {
        imp::default_prevented(&self.0)
    }

    /// Stop the event reaching listeners on ancestors.
    pub fn stop_propagation(&self) {
        imp::stop_propagation(&self.0);
    }
}
