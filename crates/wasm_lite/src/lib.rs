//! # wasm_lite
//!
//! ![logo](https://github.com/drewcrawford/wasm_lite/raw/main/art/logo.png)
//!
//! A dependency-light rewrite of wasm-bindgen: bind JavaScript and Rust to each
//! other on `wasm32-unknown-unknown`, with no *runtime* dependencies and a small
//! host-side codegen tool.
//!
//! The core crate ([`import!`], [`JsValue`], runtime helpers) and the host
//! codegen are dependency-free. The proc-macros (`wasm_lite_macro`) use
//! `syn`/`quote`, which are build-time-only and add zero bytes to the final
//! `.wasm`.
//!
//! **Coming from wasm-bindgen?** See the
//! [migration guide](https://github.com/drewcrawford/wasm_lite/blob/main/MIGRATION.md)
//! for a detailed pros/cons comparison, a side-by-side "rosetta stone" of how
//! to do X in each, and the design trade-offs and gotchas to expect.
//!
//! ## Example
//!
//! Declare JavaScript imports with [`import!`], and export Rust functions with
//! [`export`]:
//!
//! ```
//! wasm_lite::import! {
//!     "Math" { fn random() -> f64; }
//! }
//!
//! #[wasm_lite::export]
//! pub fn greet(name: &str) -> String { format!("hello, {name}!") }
//! ```
//!
//! No per-binding JavaScript is hand-written. The compiled `.wasm` carries
//! binding descriptors in custom sections, and the host-side codegen emits
//! matching JS glue.
//!
//! ## Quickstart
//!
//! ### Prerequisites
//!
//! * A Rust toolchain and the wasm target:
//!   `rustup target add wasm32-unknown-unknown`.
//! * A WebDriver-capable browser on `PATH`: Firefox + `geckodriver`, or Chrome +
//!   `chromedriver`. The runner drives a *real* browser.
//!
//! ### Run an example
//!
//! The `runner` is a `cargo` runner: it reads descriptor sections from your
//! compiled `.wasm`, generates the JS glue, serves it, and opens it in a browser
//! for `cargo run` or drives it headless for `cargo test`.
//!
//! ```bash
//! # 1. Build the runner once from the workspace root.
//! cargo build -p runner
//!
//! # 2. Point the wasm target at it. The examples already ship a .cargo/config.toml
//! #    that defaults `--target` to wasm32; you just supply the runner path.
//! export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/runner"
//!
//! # 3. Run an example in the browser.
//! cd examples/hello-rust
//! cargo run
//! cargo test
//! ```
//!
//! `examples/hello-rust` covers imports, handles, strings, bytes, and
//! [`js_class!`]. The other examples build the same way, except the
//! threaded/async examples need **nightly + `-Z build-std`** and atomics link
//! flags. See
//! [Threads, async & shared memory](https://github.com/drewcrawford/wasm_lite/blob/main/docs/threads-and-async.md)
//! and `crates/wasm_lite_std/run-browser-tests.sh`.
//!
//! ### Add it to a crate
//!
//! ```toml
//! # Cargo.toml
//! [dependencies]
//! wasm_lite = "0.1"
//! ```
//!
//! ```toml
//! # .cargo/config.toml
//! [build]
//! target = "wasm32-unknown-unknown"
//!
//! [target.wasm32-unknown-unknown]
//! runner = "/abs/path/to/wasm_lite/target/debug/runner"
//! ```
//!
//! You can also set `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER` instead of
//! putting the runner path in `.cargo/config.toml`.
//!
//! ### Generate glue manually
//!
//! The runner automates this, but the `wasm-lite` CLI can generate the JS glue
//! directly:
//!
//! ```bash
//! cargo install --path crates/wasm_lite_cli
//! cargo build --target wasm32-unknown-unknown
//! wasm-lite app.wasm -o glue.js
//! # import { instantiate, <your exports> } from "./glue.js"
//! ```
//!
//! ## How It Works
//!
//! There is no `#[wasm_bindgen]`-style all-in-one macro. Instead:
//!
//! 1. **Rust side.** [`import!`], [`export`], and [`js_class!`] emit normal
//!    wasm imports/exports *plus* a descriptor line into a custom wasm section
//!    (`__wasm_lite_imports`, `__wl_exports`) describing each binding's ABI.
//! 2. **Codegen.** The `wasm-lite` CLI reads those sections from the compiled
//!    `.wasm` and generates a matching JavaScript glue module: the import object
//!    the wasm expects, plus one wrapper per [`export`].
//! 3. **Run.** The `runner` launches the module in a real browser over
//!    WebDriver, and doubles as a `cargo` test/run runner.
//!
//! ```bash
//! cargo build --target wasm32-unknown-unknown
//! wasm-lite app.wasm -o glue.js
//! ```
//!
//! See the
//! [binding model](https://github.com/drewcrawford/wasm_lite/blob/main/docs/binding-model.md)
//! docs for the full ABI story.
//!
//! ## Crate API
//!
//! The [`wasm_lite`](crate) crate provides the core binding surface:
//!
//! | item | role |
//! |---|---|
//! | [`import!`] | declare imported JavaScript functions grouped by namespace |
//! | [`export`] | export Rust functions to JavaScript callers |
//! | [`js_class!`] | define typed [`JsValue`] wrappers |
//! | [`wasm_lite_test`] | register browser-driven wasm tests; `(worker)` runs the body on a Web Worker |
//! | [`JsValue`] | opaque handle to a JavaScript value owned by the host value table |
//! | [`set_panic_hook`] | report wasm panic messages through `console.error` |
//!
//! The core crate also exposes these modules:
//!
//! | module | role |
//! |---|---|
//! | [`console`] | `console.log` / `console.error` bindings |
//! | [`date`] | `Date.now()` binding |
//! | [`performance`] | `performance.now()` binding |
//! | [`thread`] | raw cross-thread primitives; prefer [`wasm_lite_std`] for the full `std::thread` + `std::sync` surface |
//! | `interop` | optional `wasm-bindgen` feature: conversions to/from `wasm_bindgen::JsValue` |
//!
//! ## Documentation
//!
//! | doc | covers |
//! |---|---|
//! | [Binding model](https://github.com/drewcrawford/wasm_lite/blob/main/docs/binding-model.md) | [`import!`], [`export`], [`js_class!`], [`JsValue`], type marshalling (`Option`/`Result`, strings, bytes, handles) |
//! | [Testing](https://github.com/drewcrawford/wasm_lite/blob/main/docs/testing.md) | [`wasm_lite_test`] and `(worker)`, `cargo test`/`cargo run` in-browser, doctests, the [`wasm_lite_std`] browser suite |
//! | [Threads, async & shared memory](https://github.com/drewcrawford/wasm_lite/blob/main/docs/threads-and-async.md) | `+atomics` builds, [`thread::spawn`], [`wasm_lite_std`] (`Mutex`/`RwLock`/`Condvar`/`mpsc`, sync + async), the `spawn_local` executor, panic surfacing, the `std::time` veneer |
//! | [wasm-bindgen interop](https://github.com/drewcrawford/wasm_lite/blob/main/docs/interop.md) | the `wasm-bindgen` feature and `.to_wasm_bindgen()` / `.to_wasm_lite()` conversions |
//! | [Crate layering & roadmap](https://github.com/drewcrawford/wasm_lite/blob/main/docs/roadmap.md) | planned `wasm_lite_js`/`wasm_lite_web` split and known gaps |
//! | [Design notes](https://github.com/drewcrawford/wasm_lite/blob/main/docs/design-notes.md) | forward-looking strategy for running wasm_lite and wasm-bindgen, including wgpu, in one binary |
//! | [wasm-bindgen thread-ownership census](https://github.com/drewcrawford/wasm_lite/blob/main/docs/wasm-thread-ownership-census.md) | db-dump data: about 1% of the wasm-bindgen ecosystem owns wasm threads; backs the interop strategy |
//! | [Migration guide](https://github.com/drewcrawford/wasm_lite/blob/main/MIGRATION.md) | moving from wasm-bindgen: pros/cons, rosetta stone, gotchas |
//!
//! ## Workspace
//!
//! | crate | role |
//! |---|---|
//! | `crates/wasm_lite` | core: [`import!`], [`export`], [`js_class!`], [`JsValue`], runtime (`__wl_malloc`/`__wl_free`, panic hook), [`thread::spawn`], [`console`]/[`performance`]/[`date`] bindings |
//! | `crates/wasm_lite_macro` | proc-macros (`syn`/`quote`): [`import!`], [`export`], [`wasm_lite_test`], [`js_class!`]; shared type-to-ABI dispatch lives in `ty` |
//! | `crates/wasm_lite_codegen` | host-side: read descriptor sections, generate JS glue |
//! | `crates/wasm_lite_cli` | the `wasm-lite` binary wrapping codegen |
//! | `crates/wasm_lite_std` | std-like veneer (`std::thread`/`std::sync`/`std::time`, sync + async); ported from `wasm_safe_thread`, retargeted off wasm-bindgen onto [`wasm_lite`](crate) + a `spawn_local` event-loop executor |
//! | `runner` | WebDriver runner; serves a bin interactively, or drives tests/doctests headless and exits |
//!
//! ## Examples
//!
//! Each example is a standalone crate that builds to `wasm32-unknown-unknown`:
//!
//! | example | covers |
//! |---|---|
//! | `examples/hello-rust` | imports, handles, strings, bytes, [`js_class!`] |
//! | `examples/exports-demo` | Rust-to-JS exports |
//! | `examples/tests-demo` | [`wasm_lite_test`] |
//! | `examples/doctest-demo` | browser-driven doctests |
//! | `examples/interop` | wasm-bindgen bridge |
//! | `examples/atomics-demo` | shared memory + atomics; nightly |
//! | `examples/threads-demo` | [`thread::spawn`] over Web Workers; nightly |
//! | `examples/std-threads-demo` | `wasm_lite_std::spawn`, the std-like API; nightly |
//! | `examples/async-demo` | non-blocking `join_async` on the main thread; nightly |
//! | `examples/async-mutex-demo` | main-thread `lock_async` woken cross-thread by a worker; nightly |
//! | `examples/async-doctest-demo` | fail-closed async doctest; nightly |
//! | `examples/async-fail-demo` / `examples/async-pass-demo` | fail-closed async-test verdict; nightly |
//! | `examples/panic-demo` | worker panic surfaced on the CLI; nightly |
//! | `examples/worker-spawn-local-demo` | a worker that itself `spawn_local`s async work; nightly |
//!
//! ## Design Goals
//!
//! * Runner for major web browsers: **done** (WebDriver: Firefox/Chrome/Safari).
//! * Support with and without `+atomics`: **done**. Shared-memory `+atomics`
//!   builds run, threads spawn onto Web Workers ([`thread::spawn`]), and the
//!   `std::thread`-like layer [`wasm_lite_std`] (sync **and** async) sits on top.
//! * Unit test support: **done** ([`wasm_lite_test`], `cargo test` via a custom runner).
//! * Bind JS APIs to Rust and vice versa: **done** ([`import!`] / [`export`]).
//! * Doctest support: **done** (runs rustdoc doctests in a browser).
//! * Simple, clean architecture: ongoing.
//! * Avoid dependencies: **mostly held**. The core crate and codegen have zero
//!   runtime dependencies. The proc-macro crate uses `syn`/`quote` at build time
//!   for typed parsing and hygienic codegen.
//! * Interop with wasm-bindgen crates: **done** behind the `wasm-bindgen` feature.
//!
//! [`wasm_lite_std`]: https://crates.io/crates/wasm_lite_std

// The proc-macros (`import!`, `#[export]`, `js_class!`) emit absolute
// `::wasm_lite::…` paths (a proc-macro can't use `$crate`). This self-alias lets
// those paths resolve when the macros are used *inside* this crate too.
extern crate self as wasm_lite;

mod macros;
mod value;

pub mod console;
pub mod date;
pub mod performance;
pub mod thread;

/// Bridge to `wasm_bindgen::JsValue` (enable the `wasm-bindgen` feature).
#[cfg(feature = "wasm-bindgen")]
pub mod interop;

pub use value::JsValue;
pub use wasm_lite_macro::{export, import, js_class, wasm_lite_test};

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
    std::panic::set_hook(Box::new(|info| {
        crate::console::error(&format!("{info}"));
    }));
}

/// Allocate `len` bytes (align 1) for string/byte marshalling across the JS
/// boundary. Exported for the generated glue; freed with [`__wl_free`].
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_malloc(len: usize) -> *mut u8 {
    if len == 0 {
        return core::ptr::NonNull::<u8>::dangling().as_ptr();
    }
    match std::alloc::Layout::from_size_align(len, 1) {
        Ok(layout) => unsafe { std::alloc::alloc(layout) },
        Err(_) => core::ptr::null_mut(),
    }
}

/// Free a buffer from [`__wl_malloc`] (`len` must match the allocation).
#[doc(hidden)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "C" fn __wl_free(ptr: *mut u8, len: usize) {
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
__impl_sret_scalar!(i32, u32, f64);

impl FromSretPayload for bool {
    unsafe fn __wl_read(base: *const u8) -> Self {
        unsafe { core::ptr::read_unaligned(base.add(8) as *const i32) != 0 }
    }
}

impl FromSretPayload for JsValue {
    unsafe fn __wl_read(base: *const u8) -> Self {
        let idx = unsafe { core::ptr::read_unaligned(base.add(8) as *const u32) };
        JsValue::__wl_from_abi(idx)
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
