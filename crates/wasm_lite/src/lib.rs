// SPDX-License-Identifier: MIT OR Apache-2.0
//! # wasm_lite
//!
//! ![logo](https://github.com/drewcrawford/wasm_lite/raw/main/art/wasm_lite.png)
//!
//! Browser-first Rust/JavaScript bindings for `wasm32-unknown-unknown`, with
//! real-browser tests/doctests, first-class threads, and zero runtime dependencies.
//!
//! One tool owns the whole browser loop: it generates the JS glue from your compiled
//! `.wasm`, serves it, launches a real browser over WebDriver, and drives
//! `cargo run`, `cargo test`, `cargo bench`, and rustdoc doctests through it —
//! including shared-memory `+atomics` builds, where Rust threads land on Web
//! Workers and [`wasm_lite_std`] fills in the `std::thread`/`std::sync`/`std::time`
//! slice the browser is missing.
//!
//! **Coming from wasm-bindgen?** The [migration guide](https://github.com/drewcrawford/wasm_lite/blob/main/MIGRATION.md) has a
//! side-by-side rosetta stone, an honest pros/cons list, and the gotchas.
//!
//! ## A taste
//!
//! ```rust
//! use wasm_lite::{JsValue, export, import, js_class, wasm_lite_test};
//!
//! // Import JavaScript into Rust. Strings, bytes, numbers, Option, and JS object
//! // handles all cross the boundary; `as "..."` decouples the Rust name from the
//! // JS one.
//! import! {
//!     "JSON" {
//!         fn parse(text: &str) -> JsValue;
//!         fn stringify(value: &JsValue) -> String;
//!     }
//!     "Number" {
//!         fn parse_int(s: &str, radix: Option<f64>) -> f64 as "parseInt";
//!     }
//! }
//!
//! // Give a JS object a typed Rust wrapper.
//! js_class! {
//!     type JsArray;
//!     impl JsArray {
//!         fn push(&self, value: f64) -> f64;
//!         fn join(&self, sep: &str) -> String;
//!     }
//! }
//!
//! // Export Rust to JavaScript. `Err` becomes a thrown exception on the JS side.
//! #[export]
//! pub fn divide(a: f64, b: f64) -> Result<f64, String> {
//!     if b == 0.0 { Err("division by zero".into()) } else { Ok(a / b) }
//! }
//!
//! // Tests run in a real browser under `cargo test` (and as ordinary libtest tests
//! // off wasm32). An `async fn` body is driven fail-closed: it can't pass by
//! // being dropped.
//! #[wasm_lite_test]
//! fn round_trips_through_json() {
//!     let arr = JsArray::from_js(parse("[1, 2]"));
//!     assert_eq!(arr.push(3.0), 3.0);
//!     assert_eq!(arr.join(","), "1,2,3");
//!     assert_eq!(parse_int("ff", Some(16.0)), 255.0);
//! }
//! # fn main() {}
//! ```
//!
//! No JavaScript is hand-written for any of that. Each binding leaves a small
//! descriptor in a custom section of the `.wasm`; the `wasm_lite` CLI reads those
//! back out and emits the matching glue.
//!
//! ## Quickstart
//!
//! ```bash
//! rustup target add wasm32-unknown-unknown
//! cargo install wasm_lite_cli          # provides the `wasm_lite` command
//! ```
//!
//! ```toml
//! # Cargo.toml
//! [dependencies]
//! wasm_lite = "0.1"
//! ```
//!
//! ```toml
//! # .cargo/config.toml — make wasm32 the default target and hand cargo the runner
//! [build]
//! target = "wasm32-unknown-unknown"
//!
//! [target.wasm32-unknown-unknown]
//! runner = ["wasm_lite", "run"]
//! ```
//!
//! Then `cargo run` opens your bin in a browser, and `cargo test` drives your
//! [`#[wasm_lite_test]`](wasm_lite_test)s and doctests headless and exits with the verdict.
//! (`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="wasm_lite run"` in the
//! environment works instead of the config file.)
//!
//! You need a WebDriver-capable browser on `PATH`: Firefox + `geckodriver` (the
//! default) or Chrome + `chromedriver` (`WASM_LITE_BROWSER=chrome`; also what
//! WebGPU needs, with `WASM_LITE_GPU=1`, since headless Firefox has none). The
//! other knobs — timeouts, reusing one browser across runs, serving extra assets,
//! running a bin without opening a browser — are in
//! [configure the runner](https://github.com/drewcrawford/wasm_lite/blob/main/docs/testing.md#configure-the-runner).
//!
//! **From a checkout**, build the CLI once and point the runner at it; the
//! examples already default `--target` to wasm32:
//!
//! ```bash
//! cargo build -p wasm_lite_cli
//! export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/wasm_lite run"
//! cd examples/hello-rust && cargo run && cargo test
//! ```
//!
//! `examples/` holds one standalone crate per feature — imports and handles,
//! exports, tests, doctests, benchmarks, wasm-bindgen interop, and the nightly
//! atomics/threads family. Threads and shared memory need **nightly +
//! `-Z build-std`** and the link flags in
//! [threads & async](https://github.com/drewcrawford/wasm_lite/blob/main/docs/threads-and-async.md); async work that stays on one
//! thread (`spawn_local`, [`JsFuture`], `sleep_async`) is fine on stable.
//!
//! **Without the runner**, the CLI generates the glue directly:
//!
//! ```bash
//! cargo build --target wasm32-unknown-unknown
//! wasm_lite build target/wasm32-unknown-unknown/debug/app.wasm -o glue.js
//! # import { instantiate, divide } from "./glue.js"
//! ```
//!
//! ## What you get
//!
//! * **Bindings.** [`import!`] for JS functions grouped by namespace, [`#[export]`](export)
//!   for Rust functions, [`js_class!`] for typed [`JsValue`] wrappers, [`Closure`] to
//!   hand Rust callbacks to JS, [`JsFuture`] to await JS promises. Strings, bytes,
//!   numbers, `Option`, `Result`, and live JS handles marshal in both directions
//!   over a small, auditable ABI. Focused [`console`], [`dom`], [`event`], [`fetch`],
//!   [`websocket`], [`timer`], [`date`], and [`performance`] modules cover the common
//!   browser calls. → [binding model](https://github.com/drewcrawford/wasm_lite/blob/main/docs/binding-model.md)
//! * **Testing in a real browser.** [`#[wasm_lite_test]`](wasm_lite_test) runs under `cargo test`
//!   on wasm32 and as a plain libtest test elsewhere; `#[should_panic]` and
//!   `#[ignore]` keep their meaning. Rustdoc doctests run in the browser too.
//!   `async fn` tests are fail-closed, `(worker)` moves a body onto a Web Worker
//!   where it may block, and [`#[wasm_lite_bench]`](wasm_lite_bench) measures in-browser.
//!   → [testing and benchmarking](https://github.com/drewcrawford/wasm_lite/blob/main/docs/testing.md)
//! * **Threads and shared memory.** `+atomics` builds get a shared
//!   `WebAssembly.Memory`, a module-worker bootstrap, and COOP/COEP serving out
//!   of the box. [`wasm_lite_std`] provides `spawn`, `JoinHandle`, `Mutex`,
//!   `RwLock`, `Condvar`, `mpsc`, `Instant`, and `SystemTime`, with sync and
//!   async variants because the browser main thread cannot block; on stable,
//!   non-atomic wasm it degrades to a local event-loop executor.
//!   → [threads, async & shared memory](https://github.com/drewcrawford/wasm_lite/blob/main/docs/threads-and-async.md)
//! * **Panics and logs where you can see them.** Console output — from workers
//!   too — is bridged to the CLI under `cargo test` *and* `cargo run`, so a browser
//!   program prints on the terminal that started it, and a panic prints its message
//!   instead of a bare `unreachable` trap. Misconfigured threaded builds are
//!   diagnosed at build time rather than as runtime mysteries.
//! * **wasm-bindgen interop, both ways.** The `wasm-bindgen` feature converts
//!   between the two [`JsValue`]s when `wasm_lite` owns final codegen. Two package
//!   substitution shims cover the host directions:
//!   [`backend-wasm-lite/`](https://github.com/drewcrawford/wasm_lite/tree/main/backend-wasm-lite) lowers wasm-bindgen's API onto
//!   wasm_lite so unmodified `js-sys`/`web-sys`/`wgpu` compile here, and
//!   [`backend-wasm-bindgen/`](https://github.com/drewcrawford/wasm_lite/tree/main/backend-wasm-bindgen) runs wasm_lite-authored
//!   code on real wasm-bindgen. → [interop](https://github.com/drewcrawford/wasm_lite/blob/main/docs/interop.md),
//!   [running wgpu on wasm_lite](https://github.com/drewcrawford/wasm_lite/blob/main/backend-wasm-lite/README.md)
//! * **Zero runtime dependencies.** The core crate and codegen depend on nothing.
//!   The proc-macros use `syn`/`quote` at build time, which adds zero bytes to the
//!   `.wasm`; [`wasm_lite_std`] adds `atomic-waker`.
//!
//! The scope is deliberate: the target is **modern browsers**, and the generated
//! glue is one ES module. There is no Node CommonJS, no-module, or bundler-specific
//! output, and no TypeScript declarations. Giving those up is what lets one loader,
//! one worker bootstrap, one server, and one test harness cover everything above.
//!
//! ## How it works
//!
//! There is no `#[wasm_bindgen]`-style all-in-one macro.
//!
//! 1. **Rust side.** [`import!`], [`#[export]`](export), and [`js_class!`] emit ordinary wasm
//!    imports/exports *plus* a text descriptor of each binding's ABI into a custom
//!    wasm section. [`#[wasm_lite_test]`](wasm_lite_test) and [`#[wasm_lite_bench]`](wasm_lite_bench) register
//!    themselves the same way.
//! 2. **Codegen.** `wasm_lite build` reads those sections out of the compiled
//!    `.wasm` — a dependency-free wasm reader and text parser — and writes the JS
//!    glue: the import object the module expects, one wrapper per export, and the
//!    worker bootstrap when memory is shared.
//! 3. **Run.** `wasm_lite run` does that on the fly, serves the result with the
//!    headers shared memory needs, opens it in a browser over WebDriver, and
//!    collects the console and test verdicts. `cargo run` and `cargo test` are told
//!    apart by the artifact path, so one runner serves both.
//!
//! The full ABI — how strings, bytes, `Option`, `Result`, and handles cross the
//! boundary — is in [binding model](https://github.com/drewcrawford/wasm_lite/blob/main/docs/binding-model.md).
//!
//! ## When to use something else
//!
//! `wasm_lite` does not try to replace the breadth of the wasm-bindgen ecosystem:
//! there is no generated `js-sys`/`web-sys`-scale surface, no TypeScript output,
//! and no serde-style marshalling.
//!
//! | tool | best fit |
//! |---|---|
//! | [`wasm-bindgen`](https://wasm-bindgen.github.io/wasm-bindgen/) | the mature Rust/JS binding ecosystem: rich JS types, `js-sys`/`web-sys`, TypeScript output, many packaging targets |
//! | [`wasm-pack`](https://wasm-bindgen.github.io/wasm-pack/) | packaging and publishing Rust-generated wasm into npm-oriented JavaScript workflows |
//! | [Component Model / WIT](https://component-model.bytecodealliance.org/) | language-neutral component interfaces, WASI, composition, and tooling such as `wit-bindgen` and `jco` |
//! | raw `WebAssembly.instantiate` | tiny ABIs that only need numeric imports/exports and handwritten JavaScript |
//!
//! Prefer `wasm-bindgen` when you need its ecosystem surface today. Prefer
//! `wasm_lite` when the browser path itself — atomics, workers, tests, doctests,
//! logs, panics, small explicit bindings — is what you want the tooling to own.
//! The [migration guide](https://github.com/drewcrawford/wasm_lite/blob/main/MIGRATION.md) goes through this in detail.
//!
//! ## Documentation
//!
//! | doc | covers |
//! |---|---|
//! | [Binding model](https://github.com/drewcrawford/wasm_lite/blob/main/docs/binding-model.md) | [`import!`], [`#[export]`](export), [`js_class!`], [`JsValue`], type marshalling (`Option`/`Result`, strings, bytes, handles) |
//! | [Testing and benchmarking](https://github.com/drewcrawford/wasm_lite/blob/main/docs/testing.md) | [`#[wasm_lite_test]`](wasm_lite_test), [`#[wasm_lite_bench]`](wasm_lite_bench), `(worker)`, `cargo test`/`cargo bench` in-browser, doctests, configuring the runner |
//! | [Threads, async & shared memory](https://github.com/drewcrawford/wasm_lite/blob/main/docs/threads-and-async.md) | `+atomics` builds, `thread::spawn`, [`wasm_lite_std`], the `spawn_local` executor, panic surfacing, the `std::time` veneer |
//! | [wasm-bindgen interop](https://github.com/drewcrawford/wasm_lite/blob/main/docs/interop.md) | the `wasm-bindgen` feature and `.to_wasm_bindgen()` / `.to_wasm_lite()` conversions |
//! | [Running wgpu / unmodified wasm-bindgen crates](https://github.com/drewcrawford/wasm_lite/blob/main/backend-wasm-lite/README.md) | the wasm_lite backend for wasm-bindgen code: substitute it graph-wide and `js-sys`/`web-sys`/`wgpu` compile on wasm_lite |
//! | [Migration guide](https://github.com/drewcrawford/wasm_lite/blob/main/MIGRATION.md) | moving from wasm-bindgen: pros/cons, rosetta stone, gotchas |
//! | [Roadmap & status](https://github.com/drewcrawford/wasm_lite/blob/main/docs/roadmap.md) | what is done, what is planned, crate layering, known gaps |
//! | [Design notes](https://github.com/drewcrawford/wasm_lite/blob/main/docs/design-notes.md) | coexistence strategies for wasm_lite and wasm-bindgen in one binary, and which have shipped |
//!
//! The workspace itself is described in [`AGENTS.md`](https://github.com/drewcrawford/wasm_lite/blob/main/AGENTS.md).
//!
//! ## License
//!
//! MIT OR Apache-2.0, at your option.
//!
//! [`wasm_lite_std`]: https://crates.io/crates/wasm_lite_std
//!

// The proc-macros (`import!`, `#[export]`, `js_class!`) emit absolute
// `::wasm_lite::…` paths (a proc-macro can't use `$crate`). This self-alias lets
// those paths resolve when the macros are used *inside* this crate too.
extern crate self as wasm_lite;

mod bench;
mod closure;
mod future;
mod macros;
mod value;

pub mod console;
pub mod date;
pub mod dom;
pub mod event;
pub mod fetch;
pub mod performance;
pub mod thread;
pub mod timer;
pub mod websocket;

/// Bridge to `wasm_bindgen::JsValue` (enable the `wasm-bindgen` feature).
///
/// Enabling the feature puts wasm-bindgen's schema section in your module, so
/// the runner finalizes it with the `wasm-bindgen` CLI and serves a merged
/// loader over both glues. That applies to `cargo run`, doctests, **and**
/// `cargo test` alike, so turning the feature on does not cost you a test
/// suite — but it does mean the CLI must be installed and version-matched to
/// your `wasm-bindgen` dependency, or the build fails when the runner reaches
/// for it.
#[cfg(feature = "wasm-bindgen")]
pub mod interop;

pub use bench::{Bencher, CALIBRATION_MS, SAMPLES};
pub use closure::Closure;
pub use future::JsFuture;
pub use value::{AsJsValue, JsValue};

/// The standard-library items generated code needs, re-exported.
///
/// Emitted code cannot name these absolutely. `::std::string::String` fails in
/// a `no_std` binding crate (web-sys is one), and `::core::option::Option`
/// fails in an *edition 2015* crate, where `::core` needs an explicit
/// `extern crate` — `console_error_panic_hook` is one, and it is in this
/// graph. Routing through the configured crate path means the generated code
/// names something that always resolves, whichever crate it lands in.
#[doc(hidden)]
pub use core::option::Option as __Option;
#[doc(hidden)]
pub use core::ptr::null as __null;
#[doc(hidden)]
pub use core::result::Result as __Result;
#[doc(hidden)]
pub use std::string::String as __String;
#[doc(hidden)]
pub use std::vec::Vec as __Vec;
pub use wasm_lite_macro::{export, import, js_class, wasm_lite_bench, wasm_lite_test};

/// Install a panic hook that reports the panic message via `console.error`.
///
/// On `wasm32-unknown-unknown` a panic aborts (a trap) and the default hook has
/// nowhere to write — so without this, a failure surfaces only as
/// "unreachable", losing the message. [`wasm_lite_test`] installs it
/// automatically; call it yourself at the top of a **doctest** so its failures
/// report the panic message too:
///
/// ```
/// #[cfg(target_arch = "wasm32")]
/// wasm_lite::set_panic_hook();
/// assert_eq!(2 + 2, 4);
/// ```
///
/// [`wasm_lite_test`]: crate::wasm_lite_test
pub fn set_panic_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prior = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            crate::console::error(&format!("{info}"));
            prior(info);
        }));
    });
}

/// Allocate `len` bytes (align 1) for string/byte marshalling across the JS
/// boundary. Exported for the generated glue; freed with [`__wl_free`].
///
/// Aborts (traps) on allocation failure rather than returning null: no caller —
/// neither the JS glue nor the generated Rust shims — checks the result, and a
/// null return would otherwise be written through as address 0, silently
/// corrupting low linear memory.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_malloc(len: usize) -> *mut u8 {
    if len == 0 {
        return core::ptr::NonNull::<u8>::dangling().as_ptr();
    }
    match std::alloc::Layout::from_size_align(len, 1) {
        Ok(layout) => {
            let ptr = unsafe { std::alloc::alloc(layout) };
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            ptr
        }
        Err(_) => std::process::abort(),
    }
}

/// Free a buffer from [`__wl_malloc`].
///
/// # Safety
///
/// `ptr` must have been returned by [`__wl_malloc`] for the same non-zero
/// `len`, and the allocation must not have been freed already. For `len == 0`,
/// `ptr` is ignored.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wl_free(ptr: *mut u8, len: usize) {
    if len == 0 {
        return;
    }
    if let Ok(layout) = std::alloc::Layout::from_size_align(len, 1) {
        unsafe { std::alloc::dealloc(ptr, layout) }
    }
}

/// Read one value out of an `Option`/`Result` sret buffer.
///
/// The host writes a discriminant at `base` and a payload at `base + 8`; this
/// reads that payload back into Rust. Implemented for every type usable as an
/// `Option`/`Result` inner type, so [`import!`](crate::import) needs only one
/// terminal rule per `Option`/`Result` (the per-type read dispatches here, in
/// Rust, instead of in the macro).
#[doc(hidden)]
pub trait FromSretPayload {
    /// # Safety
    /// The host must have written a payload of exactly this type at `base + 8`
    /// (and transferred ownership, for `String`/`Vec<u8>`/`JsValue`).
    unsafe fn __wl_read(base: *const u8) -> Self;
}

macro_rules! __impl_sret_scalar {
    ($($t:ty),*) => { $(
        impl FromSretPayload for $t {
            unsafe fn __wl_read(base: *const u8) -> Self {
                unsafe { core::ptr::read_unaligned(base.add(8) as *const $t) }
            }
        }
    )* };
}
__impl_sret_scalar!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64);

impl FromSretPayload for () {
    unsafe fn __wl_read(_base: *const u8) -> Self {}
}

impl FromSretPayload for bool {
    unsafe fn __wl_read(base: *const u8) -> Self {
        unsafe { core::ptr::read_unaligned(base.add(8) as *const i32) != 0 }
    }
}

impl FromSretPayload for JsValue {
    unsafe fn __wl_read(base: *const u8) -> Self {
        let idx = unsafe { core::ptr::read_unaligned(base.add(8) as *const u32) };
        unsafe { JsValue::__wl_from_abi(idx) }
    }
}

impl FromSretPayload for String {
    unsafe fn __wl_read(base: *const u8) -> Self {
        unsafe {
            let ptr = core::ptr::read_unaligned(base.add(8) as *const u32) as usize as *mut u8;
            let len = core::ptr::read_unaligned(base.add(12) as *const u32) as usize;
            String::from_raw_parts(ptr, len, len)
        }
    }
}

impl FromSretPayload for Vec<u8> {
    unsafe fn __wl_read(base: *const u8) -> Self {
        unsafe {
            let ptr = core::ptr::read_unaligned(base.add(8) as *const u32) as usize as *mut u8;
            let len = core::ptr::read_unaligned(base.add(12) as *const u32) as usize;
            Vec::from_raw_parts(ptr, len, len)
        }
    }
}

/// Copy a `&str`'s bytes into a fixed-size array at compile time.
///
/// Used by [`import!`] to place its descriptor text into a `#[link_section]`
/// static (which must be an array by value, not a reference).
#[doc(hidden)]
pub const fn descriptor_bytes<const N: usize>(s: &str) -> [u8; N] {
    let src = s.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = src[i];
        i += 1;
    }
    out
}
