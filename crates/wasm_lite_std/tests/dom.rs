// SPDX-License-Identifier: MIT OR Apache-2.0
//! Browser suite for `wasm_lite::dom`, run via the wasm_lite runner.
//!
//! Here rather than in `wasm_lite` for the same reason the `fetch` and
//! `websocket` suites are: the worker tests need `wasm_lite_std::spawn`, and a
//! dev-dependency pointing the other way would link two copies of a crate with
//! `#[no_mangle]` exports.
//!
//! The peer is the test page itself. The runner serves a real HTML document, so
//! a DOM test needs no fixture beyond what is already there — and the parts
//! that cannot be driven from Rust (a real mouse click, a real key press) are
//! covered by *synthesizing* events, which exercises the same accessors the
//! real ones would reach.
//!
//! ```text
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=$PWD/target/debug/runner \
//! cargo +nightly test -p wasm_lite_std --test dom \
//!   --target wasm32-unknown-unknown -Z build-std=std,panic_abort
//! ```

#[cfg(target_arch = "wasm32")]
wasm_lite::test_main!();

#[cfg(not(target_arch = "wasm32"))]
fn main() {}

#[cfg(target_arch = "wasm32")]
mod suite {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_lite::dom::{
        DeltaMode, KeyboardEvent, MouseEvent, WheelEvent, is_main_thread, window,
    };
    use wasm_lite::{Closure, JsValue};

    /// Synthesize and dispatch events, so the accessor bindings are exercised
    /// against events the browser constructed.
    ///
    /// A test cannot produce a real mouse click, but a `new MouseEvent(...)`
    /// dispatched at an element goes through the same event plumbing and yields
    /// the same object shape — which is what the bindings read.
    mod synth {
        use wasm_lite::JsValue;

        wasm_lite::import! {
            "Object" {
                #[constructor] fn new_object() -> JsValue as "Object";
                #[indexing_setter] fn set_f64(this: &JsValue, key: &str, v: f64);
                #[indexing_setter] fn set_str(this: &JsValue, key: &str, v: &str);
                #[indexing_setter] fn set_bool(this: &JsValue, key: &str, v: bool);
            }
            "MouseEvent" {
                #[constructor] fn new_mouse_event(kind: &str, init: &JsValue) -> JsValue
                    as "MouseEvent";
            }
            "WheelEvent" {
                #[constructor] fn new_wheel_event(kind: &str, init: &JsValue) -> JsValue
                    as "WheelEvent";
            }
            "KeyboardEvent" {
                #[constructor] fn new_keyboard_event(kind: &str, init: &JsValue) -> JsValue
                    as "KeyboardEvent";
            }
            "EventTarget" {
                fn dispatch_event(this: &JsValue, event: &JsValue) -> bool as "dispatchEvent";
            }
        }

        /// An event-init dictionary with the given number/string/bool fields.
        pub struct Init(JsValue);

        impl Init {
            pub fn new() -> Init {
                Init(new_object())
            }
            pub fn num(self, key: &str, v: f64) -> Init {
                set_f64(&self.0, key, v);
                self
            }
            pub fn text(self, key: &str, v: &str) -> Init {
                set_str(&self.0, key, v);
                self
            }
            pub fn flag(self, key: &str, v: bool) -> Init {
                set_bool(&self.0, key, v);
                self
            }
            pub fn mouse(&self, kind: &str) -> JsValue {
                new_mouse_event(kind, &self.0)
            }
            pub fn wheel(&self, kind: &str) -> JsValue {
                new_wheel_event(kind, &self.0)
            }
            pub fn keyboard(&self, kind: &str) -> JsValue {
                new_keyboard_event(kind, &self.0)
            }
        }

        pub fn dispatch(target: &JsValue, event: &JsValue) -> bool {
            dispatch_event(target, event)
        }
    }

    use synth::Init;

    // --- the globals --------------------------------------------------------

    #[wasm_lite::wasm_lite_test]
    fn the_main_thread_has_a_window() {
        wasm_lite::set_panic_hook();
        assert!(is_main_thread());
        let window = window().expect("the main thread has a window");

        // The runner serves a real page, so these are the viewport's actual
        // size rather than a default.
        assert!(window.inner_width() > 0.0, "no width");
        assert!(window.inner_height() > 0.0, "no height");
        assert!(window.device_pixel_ratio() > 0.0, "no pixel ratio");
    }

    #[wasm_lite::wasm_lite_test(worker)]
    fn a_worker_has_no_window() {
        wasm_lite::set_panic_hook();
        // The distinction web-sys makes you discover with a downcast. `None` is
        // the answer, not an error, and `is_main_thread` asks it directly.
        assert!(!is_main_thread());
        assert!(window().is_none(), "a worker global is not a Window");
    }

    // --- document and elements ----------------------------------------------

    #[wasm_lite::wasm_lite_test]
    fn the_document_has_a_body_and_a_title() {
        wasm_lite::set_panic_hook();
        let document = window().unwrap().document().expect("no document");

        document.set_title("a title from Rust");
        assert_eq!(document.title(), "a title from Rust");

        let body = document.body().expect("no body");
        assert_eq!(body.tag_name(), "BODY", "tagName is upper-case for HTML");
    }

    #[wasm_lite::wasm_lite_test]
    fn an_element_can_be_created_styled_and_attached() {
        wasm_lite::set_panic_hook();
        let document = window().unwrap().document().expect("no document");
        let body = document.body().expect("no body");

        let canvas = document.create_element("canvas").expect("create");
        assert_eq!(canvas.tag_name(), "CANVAS");

        canvas
            .style()
            .set_property("width", "100vw")
            .expect("style");
        canvas
            .style()
            .set_property("height", "100vh")
            .expect("style");
        assert_eq!(canvas.style().get_property_value("width"), "100vw");

        canvas.set_attribute("id", "the-canvas").expect("attribute");
        assert_eq!(canvas.get_attribute("id").as_deref(), Some("the-canvas"));
        assert_eq!(canvas.get_attribute("nope"), None, "unset reads as None");

        // Not in the document yet, so no lookup finds it and it has no layout.
        assert!(document.get_element_by_id("the-canvas").is_none());
        body.append_child(&canvas).expect("append");

        let found = document
            .get_element_by_id("the-canvas")
            .expect("attached, so findable");
        assert_eq!(found.tag_name(), "CANVAS");
        // `100vw` resolved against the viewport, which only happens once the
        // element is laid out.
        assert!(found.client_width() > 0, "the canvas has no layout");

        canvas.remove();
        assert!(document.get_element_by_id("the-canvas").is_none());
    }

    #[wasm_lite::wasm_lite_test]
    fn a_bad_tag_name_is_an_error() {
        wasm_lite::set_panic_hook();
        let document = window().unwrap().document().expect("no document");
        // A space is not valid in a tag name. This is the `Result` earning its
        // place: `create_element` is otherwise infallible-looking.
        assert!(document.create_element("not a tag").is_err());
        // ...and an invalid selector is distinct from "matched nothing".
        assert!(document.query_selector("###").is_err());
        assert!(
            document
                .query_selector("nosuchelement")
                .expect("valid selector")
                .is_none()
        );
    }

    // --- events -------------------------------------------------------------

    /// Somewhere to record what a handler saw, from the handler.
    type Seen<T> = Rc<RefCell<Option<T>>>;

    #[wasm_lite::wasm_lite_test]
    fn a_mouse_event_reports_its_position_and_button() {
        wasm_lite::set_panic_hook();
        let document = window().unwrap().document().expect("no document");
        let body = document.body().expect("no body");

        let seen: Seen<(f64, f64, i16, bool)> = Rc::new(RefCell::new(None));
        let record = seen.clone();
        let handler = Closure::new_with_arg(move |v: JsValue| {
            let event = MouseEvent::from_js(v);
            *record.borrow_mut() = Some((
                event.client_x(),
                event.client_y(),
                event.button(),
                event.shift_key(),
            ));
        });
        body.add_event_listener("mousedown", handler.as_js_value())
            .expect("listen");

        let event = Init::new()
            .num("clientX", 12.0)
            .num("clientY", 34.0)
            .num("button", 2.0)
            .flag("shiftKey", true)
            .flag("bubbles", true)
            .mouse("mousedown");
        synth::dispatch(body.as_js(), &event);

        assert_eq!(
            seen.borrow().expect("handler did not run"),
            (12.0, 34.0, 2, true)
        );

        body.remove_event_listener("mousedown", handler.as_js_value())
            .expect("unlisten");
        *seen.borrow_mut() = None;
        synth::dispatch(body.as_js(), &event);
        assert!(
            seen.borrow().is_none(),
            "a removed listener should not fire"
        );
    }

    #[wasm_lite::wasm_lite_test]
    fn a_wheel_event_reports_its_delta_and_unit() {
        wasm_lite::set_panic_hook();
        let document = window().unwrap().document().expect("no document");
        let body = document.body().expect("no body");

        let seen: Seen<(f64, f64, DeltaMode, f64)> = Rc::new(RefCell::new(None));
        let record = seen.clone();
        let handler = Closure::new_with_arg(move |v: JsValue| {
            let event = WheelEvent::from_js(v);
            *record.borrow_mut() = Some((
                event.delta_x(),
                event.delta_y(),
                event.delta_mode(),
                // A WheelEvent *is* a MouseEvent, so this reaches the pointer
                // position with no cast.
                event.as_mouse_event().client_x(),
            ));
        });
        body.add_event_listener("wheel", handler.as_js_value())
            .expect("listen");

        // deltaMode 1 is lines, the mode that makes a pixel-assuming caller
        // scroll about ten times too slowly.
        let event = Init::new()
            .num("deltaX", -3.0)
            .num("deltaY", 7.5)
            .num("deltaMode", 1.0)
            .num("clientX", 99.0)
            .flag("bubbles", true)
            .wheel("wheel");
        synth::dispatch(body.as_js(), &event);

        assert_eq!(
            seen.borrow().expect("handler did not run"),
            (-3.0, 7.5, DeltaMode::Line, 99.0)
        );
    }

    #[wasm_lite::wasm_lite_test]
    fn a_keyboard_event_reports_code_and_key_separately() {
        wasm_lite::set_panic_hook();
        let document = window().unwrap().document().expect("no document");

        let seen: Seen<(String, String, bool, bool)> = Rc::new(RefCell::new(None));
        let record = seen.clone();
        let handler = Closure::new_with_arg(move |v: JsValue| {
            let event = KeyboardEvent::from_js(v);
            *record.borrow_mut() =
                Some((event.code(), event.key(), event.repeat(), event.ctrl_key()));
        });
        document
            .add_event_listener("keydown", handler.as_js_value())
            .expect("listen");

        // The two differ on purpose: `code` is the physical key a WASD binding
        // wants, `key` is the character text input wants. A test that set them
        // to the same value would not notice the two getters being swapped.
        let event = Init::new()
            .text("code", "KeyW")
            .text("key", "z")
            .flag("repeat", true)
            .flag("ctrlKey", true)
            .flag("bubbles", true)
            .keyboard("keydown");
        synth::dispatch(document.as_js(), &event);

        assert_eq!(
            seen.borrow().clone().expect("handler did not run"),
            ("KeyW".to_string(), "z".to_string(), true, true)
        );
    }

    #[wasm_lite::wasm_lite_test]
    fn an_event_knows_its_type_and_target() {
        wasm_lite::set_panic_hook();
        let document = window().unwrap().document().expect("no document");
        let body = document.body().expect("no body");

        let seen: Seen<(String, bool)> = Rc::new(RefCell::new(None));
        let record = seen.clone();
        let handler = Closure::new_with_arg(move |v: JsValue| {
            let event = MouseEvent::from_js(v).as_event();
            event.prevent_default();
            *record.borrow_mut() = Some((event.event_type(), event.default_prevented()));
        });
        body.add_event_listener("click", handler.as_js_value())
            .expect("listen");

        // `cancelable` is what makes preventDefault observable; without it the
        // call is a no-op and `defaultPrevented` stays false.
        let event = Init::new()
            .flag("bubbles", true)
            .flag("cancelable", true)
            .mouse("click");
        // dispatchEvent returns false exactly when the default was prevented.
        let not_prevented = synth::dispatch(body.as_js(), &event);
        assert!(
            !not_prevented,
            "preventDefault should be visible to the caller"
        );

        assert_eq!(
            seen.borrow().clone().expect("handler did not run"),
            ("click".to_string(), true)
        );
    }

    #[wasm_lite::wasm_lite_test]
    fn dropping_a_handler_unregisters_it() {
        wasm_lite::set_panic_hook();
        let document = window().unwrap().document().expect("no document");
        let body = document.body().expect("no body");

        let count = Rc::new(RefCell::new(0u32));
        let record = count.clone();
        let handler = Closure::new_with_arg(move |_v: JsValue| {
            *record.borrow_mut() += 1;
        });
        body.add_event_listener("click", handler.as_js_value())
            .expect("listen");

        let event = Init::new().flag("bubbles", true).mouse("click");
        synth::dispatch(body.as_js(), &event);
        assert_eq!(*count.borrow(), 1);

        // The trap this documents: JS still holds the listener, but the Rust
        // side is gone, so the call becomes a no-op instead of reading freed
        // memory. Safe, and silent — which is why it is worth a test.
        drop(handler);
        synth::dispatch(body.as_js(), &event);
        assert_eq!(
            *count.borrow(),
            1,
            "a dropped Closure should no longer count"
        );
    }
}
