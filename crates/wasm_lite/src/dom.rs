// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings to the DOM: the window, the document, elements, and input events.
//!
//! This is the `wasm_lite` answer to the `web_sys` slice a windowing layer
//! needs — `Window`, `Document`, `Element`, `HtmlElement`,
//! `HtmlCanvasElement`, `CssStyleDeclaration`, `MouseEvent`, `WheelEvent`,
//! `KeyboardEvent` — plus the `js_sys::global`/`Reflect` calls that go with
//! them.
//!
//! ```
//! # #[cfg(target_arch = "wasm32")]
//! # fn run() -> Result<(), wasm_lite::JsValue> {
//! let window = wasm_lite::dom::window().expect("not on the main thread");
//! let document = window.document().expect("no document");
//!
//! let canvas = document.create_element("canvas")?;
//! canvas.style().set_property("width", "100vw")?;
//! document.body().expect("no body").append_child(&canvas)?;
//! # Ok(()) }
//! # #[cfg(target_arch = "wasm32")]
//! # fn main() { wasm_lite::set_panic_hook(); run().unwrap(); }
//! # #[cfg(not(target_arch = "wasm32"))]
//! # fn main() {}
//! ```
//!
//! # Differences from web-sys
//!
//! * **One [`Element`] type**, not `Element`/`HtmlElement`/`HtmlCanvasElement`.
//!   web-sys splits them because it mirrors the IDL hierarchy in the type
//!   system; the practical effect on calling code was a chain of unchecked
//!   casts between types with no runtime distinction. Bind the methods you
//!   need on one handle instead.
//!
//!   The cost is that nothing checks a method exists on the element you have.
//!   [`Element::style`] is the one to watch: it is defined on `HTMLElement` and
//!   `SVGElement` but not on a bare `Element`, so on, say, a MathML node it
//!   reads `undefined` and the next [`CssStyleDeclaration`] call throws. web-sys
//!   would have refused at compile time. Everything else here is on `Element`,
//!   `Document`, `Window` or `EventTarget` proper.
//! * **[`window`] answers `None` on a worker** rather than requiring a
//!   `dyn_into::<WorkerGlobalScope>()` to find out. [`is_main_thread`] is the
//!   same question asked directly.
//! * **Event handlers are [`Closure`](crate::Closure)s the caller owns.**
//!   Dropping one unregisters it, and the listener then calls a no-op — safe,
//!   but silent. Keep it alive or `forget()` it.

use crate::JsValue;
use crate::macros::js_handle;

pub use crate::event::Event;

mod imp {
    use crate::JsValue;

    crate::import! {
        "globalThis" {
            /// `globalThis.window` — `undefined` on a worker, hence `Option`.
            #[static_getter] fn window() -> Option<JsValue>;
            /// `globalThis instanceof Window`.
            ///
            /// Guarded by the codegen, so on a worker — where `Window` is not
            /// defined at all — this answers `false` rather than throwing a
            /// `TypeError`.
            #[static_getter] fn global_this() -> JsValue as "globalThis";
        }
        "Window" {
            #[instanceof] fn is_window(this: &JsValue) -> bool as "Window";

            #[getter] fn document(this: &JsValue) -> Option<JsValue>;
            #[getter] fn inner_width(this: &JsValue) -> f64 as "innerWidth";
            #[getter] fn inner_height(this: &JsValue) -> f64 as "innerHeight";
            #[getter] fn device_pixel_ratio(this: &JsValue) -> f64 as "devicePixelRatio";
            #[setter] fn set_onresize(this: &JsValue, v: Option<&JsValue>) as "onresize";
            fn alert(this: &JsValue, message: &str) -> Result<(), JsValue>;
        }
        "EventTarget" {
            fn add_event_listener(this: &JsValue, kind: &str, handler: &JsValue)
                -> Result<(), JsValue> as "addEventListener";
            fn remove_event_listener(this: &JsValue, kind: &str, handler: &JsValue)
                -> Result<(), JsValue> as "removeEventListener";
        }
        "Document" {
            #[getter] fn title(this: &JsValue) -> String;
            #[setter] fn set_title(this: &JsValue, v: &str) as "title";
            #[getter] fn body(this: &JsValue) -> Option<JsValue>;
            #[getter] fn document_element(this: &JsValue) -> Option<JsValue> as "documentElement";
            #[getter] fn fullscreen_element(this: &JsValue) -> Option<JsValue>
                as "fullscreenElement";
            fn create_element(this: &JsValue, tag: &str) -> Result<JsValue, JsValue>
                as "createElement";
            fn get_element_by_id(this: &JsValue, id: &str) -> Option<JsValue>
                as "getElementById";
            fn query_selector(this: &JsValue, selectors: &str) -> Result<Option<JsValue>, JsValue>
                as "querySelector";
            fn exit_fullscreen(this: &JsValue) -> JsValue as "exitFullscreen";
        }
        "Element" {
            #[getter] fn style(this: &JsValue) -> JsValue;
            #[getter] fn tag_name(this: &JsValue) -> String as "tagName";
            #[getter] fn client_width(this: &JsValue) -> i32 as "clientWidth";
            #[getter] fn client_height(this: &JsValue) -> i32 as "clientHeight";
            #[setter] fn set_text_content(this: &JsValue, v: &str) as "textContent";
            fn set_attribute(this: &JsValue, name: &str, value: &str) -> Result<(), JsValue>
                as "setAttribute";
            fn get_attribute(this: &JsValue, name: &str) -> Option<String> as "getAttribute";
            fn append_child(this: &JsValue, child: &JsValue) -> Result<(), JsValue>
                as "appendChild";
            fn remove(this: &JsValue);
            /// `Promise<undefined>` — resolves when the element is fullscreen.
            fn request_fullscreen(this: &JsValue) -> Result<JsValue, JsValue>
                as "requestFullscreen";
        }
        "CSSStyleDeclaration" {
            fn set_property(this: &JsValue, name: &str, value: &str) -> Result<(), JsValue>
                as "setProperty";
            fn get_property_value(this: &JsValue, name: &str) -> String as "getPropertyValue";
        }
        "MouseEvent" {
            #[getter] fn client_x(this: &JsValue) -> f64 as "clientX";
            #[getter] fn client_y(this: &JsValue) -> f64 as "clientY";
            #[getter] fn offset_x(this: &JsValue) -> f64 as "offsetX";
            #[getter] fn offset_y(this: &JsValue) -> f64 as "offsetY";
            #[getter] fn movement_x(this: &JsValue) -> f64 as "movementX";
            #[getter] fn movement_y(this: &JsValue) -> f64 as "movementY";
            #[getter] fn button(this: &JsValue) -> i16;
            #[getter] fn buttons(this: &JsValue) -> u16;
            #[getter] fn shift_key(this: &JsValue) -> bool as "shiftKey";
            #[getter] fn ctrl_key(this: &JsValue) -> bool as "ctrlKey";
            #[getter] fn alt_key(this: &JsValue) -> bool as "altKey";
            #[getter] fn meta_key(this: &JsValue) -> bool as "metaKey";
        }
        "WheelEvent" {
            #[getter] fn delta_x(this: &JsValue) -> f64 as "deltaX";
            #[getter] fn delta_y(this: &JsValue) -> f64 as "deltaY";
            #[getter] fn delta_z(this: &JsValue) -> f64 as "deltaZ";
            #[getter] fn delta_mode(this: &JsValue) -> u32 as "deltaMode";
        }
    }

    /// `KeyboardEvent`'s own module, because it shares four property names with
    /// `MouseEvent` (`shiftKey` and friends) and one `import!` module cannot
    /// define the same Rust name twice.
    pub mod kbd {
        use crate::JsValue;

        crate::import! {
            "KeyboardEvent" {
                /// The physical key — layout-independent, so `"KeyZ"` is the
                /// same key on QWERTY and AZERTY.
                #[getter] fn code(this: &JsValue) -> String;
                /// The character the key produces, which *is* layout-dependent.
                #[getter] fn key(this: &JsValue) -> String;
                #[getter] fn repeat(this: &JsValue) -> bool;
                #[getter] fn shift_key(this: &JsValue) -> bool as "shiftKey";
                #[getter] fn ctrl_key(this: &JsValue) -> bool as "ctrlKey";
                #[getter] fn alt_key(this: &JsValue) -> bool as "altKey";
                #[getter] fn meta_key(this: &JsValue) -> bool as "metaKey";
            }
        }
    }
}

js_handle! {
    /// The browser [`Window`](https://developer.mozilla.org/docs/Web/API/Window).
    ///
    /// Only exists on the main thread; a worker's global is a
    /// `WorkerGlobalScope` and has no DOM. Reach one with [`window`].
    Window;

    /// The [`Document`](https://developer.mozilla.org/docs/Web/API/Document).
    Document;

    /// A DOM [`Element`](https://developer.mozilla.org/docs/Web/API/Element).
    ///
    /// One type for the whole element hierarchy — see the module docs.
    Element;

    /// An element's [inline style](https://developer.mozilla.org/docs/Web/API/CSSStyleDeclaration).
    CssStyleDeclaration;

    /// A [mouse event](https://developer.mozilla.org/docs/Web/API/MouseEvent).
    MouseEvent;

    /// A [wheel event](https://developer.mozilla.org/docs/Web/API/WheelEvent).
    ///
    /// A `WheelEvent` *is* a `MouseEvent`, so [`WheelEvent::as_mouse_event`]
    /// reaches the pointer position without a cast.
    WheelEvent;

    /// A [keyboard event](https://developer.mozilla.org/docs/Web/API/KeyboardEvent).
    KeyboardEvent;
}

/// The [`Window`], or `None` when there is not one — i.e. on a worker.
///
/// Replaces web-sys' `window()` and the `dyn_into::<WorkerGlobalScope>()` that
/// usually follows it: `None` *is* the "we are on a worker" answer.
pub fn window() -> Option<Window> {
    imp::window().map(Window)
}

/// Whether this is the browser main thread.
///
/// Asks `globalThis instanceof Window` directly. The `instanceof` is guarded,
/// so on a worker — where `Window` is not defined at all — it answers `false`
/// rather than throwing.
pub fn is_main_thread() -> bool {
    imp::is_window(&imp::global_this())
}

impl Window {
    /// The document, or `None` in a context that has none.
    pub fn document(&self) -> Option<Document> {
        imp::document(&self.0).map(Document)
    }

    /// The viewport width in CSS pixels, including any scrollbar.
    ///
    /// This is the *logical* size. A canvas's `width`/`height` attributes are
    /// its backing-buffer size and are a different number entirely — reading
    /// those to learn the window size reports whatever they were last set to,
    /// or the 300×150 default if nobody set them.
    pub fn inner_width(&self) -> f64 {
        imp::inner_width(&self.0)
    }

    /// The viewport height in CSS pixels. See [`inner_width`](Self::inner_width).
    pub fn inner_height(&self) -> f64 {
        imp::inner_height(&self.0)
    }

    /// Physical pixels per CSS pixel.
    pub fn device_pixel_ratio(&self) -> f64 {
        imp::device_pixel_ratio(&self.0)
    }

    /// Set the `resize` handler, replacing any previous one.
    ///
    /// One handler only; [`add_event_listener`](Self::add_event_listener) is
    /// the additive form.
    pub fn set_onresize(&self, handler: Option<&JsValue>) {
        imp::set_onresize(&self.0, handler);
    }

    /// Register an additional listener for `kind` (`"blur"`, `"resize"`, …).
    pub fn add_event_listener(&self, kind: &str, handler: &JsValue) -> Result<(), JsValue> {
        imp::add_event_listener(&self.0, kind, handler)
    }

    /// Remove a listener previously registered with the *same* handler value.
    pub fn remove_event_listener(&self, kind: &str, handler: &JsValue) -> Result<(), JsValue> {
        imp::remove_event_listener(&self.0, kind, handler)
    }

    /// Show a modal alert. Blocks the page until dismissed.
    pub fn alert(&self, message: &str) -> Result<(), JsValue> {
        imp::alert(&self.0, message)
    }
}

impl Document {
    /// The document title — what the browser tab shows.
    pub fn title(&self) -> String {
        imp::title(&self.0)
    }

    /// Set the document title.
    pub fn set_title(&self, title: &str) {
        imp::set_title(&self.0, title);
    }

    /// The `<body>`, or `None` before it has been parsed.
    pub fn body(&self) -> Option<Element> {
        imp::body(&self.0).map(Element)
    }

    /// The root element (`<html>`).
    pub fn document_element(&self) -> Option<Element> {
        imp::document_element(&self.0).map(Element)
    }

    /// The element currently displayed fullscreen, if any.
    pub fn fullscreen_element(&self) -> Option<Element> {
        imp::fullscreen_element(&self.0).map(Element)
    }

    /// Create an element. Fails for a name that is not a valid tag.
    pub fn create_element(&self, tag: &str) -> Result<Element, JsValue> {
        Ok(Element(imp::create_element(&self.0, tag)?))
    }

    /// Look an element up by `id`.
    pub fn get_element_by_id(&self, id: &str) -> Option<Element> {
        imp::get_element_by_id(&self.0, id).map(Element)
    }

    /// The first element matching a CSS selector. Fails for an invalid
    /// selector, which is distinct from "matched nothing" (`Ok(None)`).
    pub fn query_selector(&self, selectors: &str) -> Result<Option<Element>, JsValue> {
        Ok(imp::query_selector(&self.0, selectors)?.map(Element))
    }

    /// Register a listener for `kind` (`"keydown"`, `"mousemove"`, …).
    pub fn add_event_listener(&self, kind: &str, handler: &JsValue) -> Result<(), JsValue> {
        imp::add_event_listener(&self.0, kind, handler)
    }

    /// Remove a listener previously registered with the *same* handler value.
    pub fn remove_event_listener(&self, kind: &str, handler: &JsValue) -> Result<(), JsValue> {
        imp::remove_event_listener(&self.0, kind, handler)
    }

    /// Leave fullscreen. The returned handle is a `Promise`.
    pub fn exit_fullscreen(&self) -> JsValue {
        imp::exit_fullscreen(&self.0)
    }
}

impl Element {
    /// The tag name, upper-case for an HTML element (`"CANVAS"`).
    pub fn tag_name(&self) -> String {
        imp::tag_name(&self.0)
    }

    /// The inline style declaration.
    ///
    /// Defined on `HTMLElement` and `SVGElement`, **not** on a bare `Element` —
    /// see the module docs. For anything `create_element` returns for an HTML
    /// tag this is always present.
    pub fn style(&self) -> CssStyleDeclaration {
        CssStyleDeclaration(imp::style(&self.0))
    }

    /// The laid-out content width in CSS pixels, rounded to an integer.
    pub fn client_width(&self) -> i32 {
        imp::client_width(&self.0)
    }

    /// The laid-out content height in CSS pixels, rounded to an integer.
    pub fn client_height(&self) -> i32 {
        imp::client_height(&self.0)
    }

    /// Set an attribute. Fails for a name that is not valid.
    pub fn set_attribute(&self, name: &str, value: &str) -> Result<(), JsValue> {
        imp::set_attribute(&self.0, name, value)
    }

    /// An attribute's value, or `None` if it is not set.
    pub fn get_attribute(&self, name: &str) -> Option<String> {
        imp::get_attribute(&self.0, name)
    }

    /// Replace the element's contents with text.
    pub fn set_text_content(&self, text: &str) {
        imp::set_text_content(&self.0, text);
    }

    /// Append `child` as the last child. Moves it if it is already in the tree.
    pub fn append_child(&self, child: &Element) -> Result<(), JsValue> {
        imp::append_child(&self.0, &child.0)
    }

    /// Remove the element from the tree.
    pub fn remove(&self) {
        imp::remove(&self.0);
    }

    /// Ask for fullscreen. The `Ok` handle is a `Promise`.
    ///
    /// Fails synchronously where fullscreen is disallowed outright, and the
    /// promise rejects when the request is refused — which it is unless it was
    /// made from a user gesture. Neither failure is a bug in the caller's
    /// element handling.
    pub fn request_fullscreen(&self) -> Result<JsValue, JsValue> {
        imp::request_fullscreen(&self.0)
    }

    /// Register a listener for `kind`.
    pub fn add_event_listener(&self, kind: &str, handler: &JsValue) -> Result<(), JsValue> {
        imp::add_event_listener(&self.0, kind, handler)
    }

    /// Remove a listener previously registered with the *same* handler value.
    pub fn remove_event_listener(&self, kind: &str, handler: &JsValue) -> Result<(), JsValue> {
        imp::remove_event_listener(&self.0, kind, handler)
    }
}

impl CssStyleDeclaration {
    /// Set a CSS property (`"width"`, `"background-color"`, …).
    ///
    /// A value the browser cannot parse is *ignored*, not reported — the `Err`
    /// here is for a malformed property name, so a silently-unapplied style is
    /// a wrong value rather than a failed call.
    pub fn set_property(&self, name: &str, value: &str) -> Result<(), JsValue> {
        imp::set_property(&self.0, name, value)
    }

    /// A property's value, or `""` if it is not set inline.
    pub fn get_property_value(&self, name: &str) -> String {
        imp::get_property_value(&self.0, name)
    }
}

impl MouseEvent {
    /// This event as its base [`Event`].
    pub fn as_event(&self) -> Event {
        Event::from_js(self.0.clone())
    }

    /// X relative to the viewport, in CSS pixels — the same frame
    /// [`Window::inner_width`] reports.
    pub fn client_x(&self) -> f64 {
        imp::client_x(&self.0)
    }

    /// Y relative to the viewport, in CSS pixels.
    pub fn client_y(&self) -> f64 {
        imp::client_y(&self.0)
    }

    /// X relative to the *target element's* padding box.
    ///
    /// Which element that is depends on what the pointer is over, so this is
    /// not interchangeable with [`client_x`](Self::client_x) even when a canvas
    /// fills the viewport.
    pub fn offset_x(&self) -> f64 {
        imp::offset_x(&self.0)
    }

    /// Y relative to the target element's padding box.
    pub fn offset_y(&self) -> f64 {
        imp::offset_y(&self.0)
    }

    /// X movement since the previous event — what pointer lock reports.
    pub fn movement_x(&self) -> f64 {
        imp::movement_x(&self.0)
    }

    /// Y movement since the previous event.
    pub fn movement_y(&self) -> f64 {
        imp::movement_y(&self.0)
    }

    /// Which button changed state: 0 main, 1 auxiliary (middle), 2 secondary
    /// (right).
    ///
    /// Note that this is **not** the order most native APIs use, where middle
    /// and right are usually swapped.
    pub fn button(&self) -> i16 {
        imp::button(&self.0)
    }

    /// A bitmask of the buttons currently held: 1 main, 2 secondary, 4
    /// auxiliary. A different numbering from [`button`](Self::button).
    pub fn buttons(&self) -> u16 {
        imp::buttons(&self.0)
    }

    /// Whether shift was held.
    pub fn shift_key(&self) -> bool {
        imp::shift_key(&self.0)
    }

    /// Whether control was held.
    pub fn ctrl_key(&self) -> bool {
        imp::ctrl_key(&self.0)
    }

    /// Whether alt/option was held.
    pub fn alt_key(&self) -> bool {
        imp::alt_key(&self.0)
    }

    /// Whether command/super was held.
    pub fn meta_key(&self) -> bool {
        imp::meta_key(&self.0)
    }
}

/// The unit a [`WheelEvent`]'s deltas are expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaMode {
    /// Pixels.
    Pixel,
    /// Lines — multiply by a line height to get pixels.
    Line,
    /// Pages — multiply by a viewport height.
    Page,
    /// A value this binding does not know.
    Other(u32),
}

impl WheelEvent {
    /// This event as a [`MouseEvent`] — a `WheelEvent` is one, so the pointer
    /// position and modifier keys are all available.
    pub fn as_mouse_event(&self) -> MouseEvent {
        MouseEvent::from_js(self.0.clone())
    }

    /// This event as its base [`Event`].
    pub fn as_event(&self) -> Event {
        Event::from_js(self.0.clone())
    }

    /// Horizontal scroll amount, in [`delta_mode`](Self::delta_mode) units.
    pub fn delta_x(&self) -> f64 {
        imp::delta_x(&self.0)
    }

    /// Vertical scroll amount, in [`delta_mode`](Self::delta_mode) units.
    pub fn delta_y(&self) -> f64 {
        imp::delta_y(&self.0)
    }

    /// Z scroll amount, for the rare device that has one.
    pub fn delta_z(&self) -> f64 {
        imp::delta_z(&self.0)
    }

    /// What unit the deltas are in.
    ///
    /// **Always check this.** The same physical gesture reports pixels in one
    /// browser and lines in another, so treating the delta as pixels
    /// unconditionally makes scrolling ~10× too slow wherever it is lines.
    pub fn delta_mode(&self) -> DeltaMode {
        match imp::delta_mode(&self.0) {
            0 => DeltaMode::Pixel,
            1 => DeltaMode::Line,
            2 => DeltaMode::Page,
            other => DeltaMode::Other(other),
        }
    }
}

impl KeyboardEvent {
    /// This event as its base [`Event`].
    pub fn as_event(&self) -> Event {
        Event::from_js(self.0.clone())
    }

    /// The **physical** key: `"KeyZ"`, `"ArrowLeft"`, `"Space"`.
    ///
    /// Layout-independent, which is what a game's WASD binding wants — on
    /// AZERTY the same physical keys still report `KeyW`/`KeyA`/`KeyS`/`KeyD`.
    pub fn code(&self) -> String {
        imp::kbd::code(&self.0)
    }

    /// The **character** the key produces: `"z"`, `"ArrowLeft"`, `" "`.
    ///
    /// Layout- and modifier-dependent, which is what text input wants.
    pub fn key(&self) -> String {
        imp::kbd::key(&self.0)
    }

    /// Whether this is an auto-repeat rather than a fresh press.
    pub fn repeat(&self) -> bool {
        imp::kbd::repeat(&self.0)
    }

    /// Whether shift was held.
    pub fn shift_key(&self) -> bool {
        imp::kbd::shift_key(&self.0)
    }

    /// Whether control was held.
    pub fn ctrl_key(&self) -> bool {
        imp::kbd::ctrl_key(&self.0)
    }

    /// Whether alt/option was held.
    pub fn alt_key(&self) -> bool {
        imp::kbd::alt_key(&self.0)
    }

    /// Whether command/super was held.
    pub fn meta_key(&self) -> bool {
        imp::kbd::meta_key(&self.0)
    }
}
