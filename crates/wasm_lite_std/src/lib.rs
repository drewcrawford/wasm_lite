// SPDX-License-Identifier: MIT OR Apache-2.0
// `nightly_rustc` alone is too broad: the only caller of `set_output_capture` is
// in `mod wasm`, which is `cfg(target_arch = "wasm32")`. Enabling the feature on
// a nightly *host* build declares it without using it, which `unused_features`
// reports — a hard error under `-D warnings`.
#![cfg_attr(
    all(nightly_rustc, target_arch = "wasm32"),
    feature(internal_output_capture)
)]
#![cfg_attr(
    all(target_arch = "wasm32", target_feature = "atomics"),
    feature(stdarch_wasm_atomic_wait)
)]
//! Cross-platform `std`-shaped APIs for native + wasm32.
//!
//! ![logo](https://github.com/drewcrawford/wasm_lite/raw/main/art/wasm_lite.png)
//!
//! # Scope
//!
//! This crate absorbs functionality that is **`std`-shaped** and whose **native
//! implementation is a thin veneer over `std`** — that is the test for whether
//! something belongs here rather than in a crate above. `std::thread` and
//! `std::sync` qualified; `std::fs` would. A crate solving a problem `std` never
//! addressed does not, even when this crate wants what it has. See
//! [the absorption rule](https://github.com/drewcrawford/wasm_lite/blob/main/docs/roadmap.md#what-belongs-in-wasm_lite_std--the-absorption-rule).
//!
//! The optional `fs` feature adds `wasm_lite_std::fs`, an asynchronous, read-only
//! `std::fs`-shaped API. Native operations use a blocking-I/O pool; browser
//! wasm reads same-origin or CORS-enabled resources with HTTP range requests.
//!
//! This crate provides a unified threading API and synchronization primitives that work across both
//! WebAssembly and native platforms. In practice, you can treat it as a cross-platform replacement
//! for much of `std::thread` plus key `std::sync` primitives. Unlike similar crates, it's designed
//! from the ground up to handle the async realities of browser environments.
//!
//! # Synchronization primitives
//!
//! Alongside thread APIs, this crate includes WebAssembly-safe synchronization primitives:
//!
//! - [`Mutex`]
//! - [`rwlock::RwLock`]
//! - [`condvar::Condvar`]
//! - [`spinlock::Spinlock`]
//! - [`mpsc`] channels
//!
//! These APIs are usable on their own; you do not need to spawn threads with this crate to use
//! [`Mutex`], [`RwLock`](rwlock::RwLock), [`Condvar`](condvar::Condvar), or [`mpsc`].
//!
//! These primitives adapt their behavior to the runtime:
//!
//! - **Native**: uses thread parking for efficient blocking
//! - **WASM worker**: uses `Atomics.wait`-based blocking when available
//! - **WASM main thread**: falls back to non-blocking/spin strategies to avoid panics
//!
//! ## Sync Example
//!
//! ```
//! use wasm_lite_std::Mutex;
//!
//! let data = Mutex::new(41);
//! *data.lock_sync() += 1;
//! assert_eq!(*data.lock_sync(), 42);
//! ```
//!
//! ## Channel Example
//!
//! ```
//! use wasm_lite_std::mpsc::channel;
//!
//! let (tx, rx) = channel();
//! tx.send_sync(5).unwrap();
//! assert_eq!(rx.recv_sync().unwrap(), 5);
//! ```
//!
//! # Time
//!
//! The [`time`] module is a cross-platform [`std::time`] veneer: on native it
//! re-exports the real types; on wasm32 it provides drop-in [`Instant`](time::Instant)
//! and [`SystemTime`](time::SystemTime) backed by the browser clocks, with no
//! `wasm-bindgen` dependency.
//!
//! ```
//! use wasm_lite_std::time::{Duration, Instant};
//!
//! let start = Instant::now();
//! let _elapsed: Duration = start.elapsed();
//! ```
//!
//! # Threading primitives
//!
//! In addition to synchronization primitives, this crate provides a `std::thread`-like API:
//! [`spawn()`], [`Builder`], [`JoinHandle`], [`park()`], [`Thread::unpark()`], thread locals,
//! and spawn hooks.
//!
//! Worker spawning needs shared-memory `+atomics` wasm and therefore nightly
//! `-Z build-std`. Async that stays in one realm does not: `spawn_local`,
//! [`sleep_async`], and the non-blocking synchronization paths run in an
//! ordinary stable, non-atomic wasm build too. In that mode, attempting to
//! spawn a worker reports [`std::io::ErrorKind::Unsupported`].
//!
//! # Comparison with wasm_thread
//!
//! [wasm_thread](https://crates.io/crates/wasm_thread) is a popular crate that aims to closely
//! replicate `std::thread` on wasm targets. This section compares design goals and practical tradeoffs.
//!
//! ## Design goals
//!
//! - `wasm_lite_std`: async-first, unified API that works identically on native and wasm32,
//!   playing well with the browser event loop.
//! - `wasm_thread`: high `std::thread` compatibility with minimal changes to existing codebases
//!   (wasm32 only; native uses `std::thread` directly).
//!
//! ## Feature comparison
//!
//! | Feature | wasm_lite_std | wasm_thread |
//! |---------|------------------|-------------|
//! | **Native support** | Unified API (same code runs on native and wasm) | Re-exports `std::thread::*` on native |
//! | **Toolchain** | `wasm_lite` codegen + runner; **no wasm-bindgen / wasm-pack** | wasm-bindgen + wasm-pack |
//! | **Worker bootstrap** | Codegen-emitted (`wl_worker.js`); no JS to write or ship | External JS files; `es_modules` feature for module workers |
//! | **Event loop integration** | [`yield_to_event_loop_async()`] for cooperative scheduling | No equivalent |
//! | **Driving a future** | [`block_on()`] on any thread that may block; `spawn_local()` to hand it to the event loop (wasm32) | No equivalent |
//! | **Spawn hooks** | Global hooks that run at thread start | Not available |
//! | **Parking primitives** | [`park()`]/[`Thread::unpark()`] on wasm workers | Not implemented |
//! | **Scoped threads** | Not implemented | `scope()` allows borrowing non-`'static` data |
//! | **std compatibility** | Custom [`Thread`]/[`ThreadId`] (similar API) | Re-exports `std::thread::{Thread, ThreadId}` |
//! | **Dependencies** | `wasm_lite` + `atomic-waker` (zero wasm-bindgen) | web-sys (many features), futures crate |
//! | **Thread handle** | [`JoinHandle::thread()`] returns `&Thread` | `thread()` is unimplemented (panics) |
//!
//! ## Shared capabilities
//!
//! Both crates provide:
//! - [`spawn()`] and [`Builder`] for thread creation
//! - [`JoinHandle::join()`] (blocking) and [`JoinHandle::join_async()`] (async) for waiting on threads
//! - [`JoinHandle::is_finished()`] for non-blocking completion checks
//! - Thread naming via [`Builder::name()`]
//!
//! ## Behavioral differences to know
//!
//! - **Main-thread blocking:** both crates must avoid blocking APIs on the browser main thread;
//!   [`JoinHandle::join_async()`] is the safe path.
//! - **Spawn timing:** wasm workers only run after the main thread yields back to the event loop.
//! - **Worker spawning model:** `wasm_thread` proxies worker spawning through the main thread;
//!   `wasm_lite_std` spawns directly (simpler, but different model).
//!
//! ## Implementation differences (for maintainers)
//!
//! **Result passing:**
//! - `wasm_lite_std` uses its built-in `mpsc` channels with async `recv_async()`
//! - `wasm_thread` uses `Arc<Packet<UnsafeCell>>` with a custom `Signal` primitive and `Waker` list
//!
//! **Async waiting:**
//! - `wasm_lite_std` runs a small event-loop executor (`spawn_local`): stable
//!   non-atomic wasm schedules another host turn, while shared-memory builds
//!   sleep on `Atomics.waitAsync` and wake cross-thread via
//!   `memory.atomic.notify`
//! - `wasm_thread` implements `futures::future::poll_fn` with manual `Waker` tracking
//!
//! ## When to use which
//!
//! **Choose wasm_lite_std when:**
//! - You want to stay off wasm-bindgen entirely (no `wasm-bindgen`/`js-sys`/`web-sys` in the build)
//! - You want identical behavior on native and wasm (e.g., for testing)
//! - You need park/unpark synchronization primitives
//! - You need spawn hooks for initialization (logging, tracing, etc.)
//! - You prefer fewer dependencies and no external JS files
//!
//! **Choose wasm_thread when:**
//! - You need scoped threads for borrowing non-`'static` data
//! - You want maximum compatibility with `std::thread` types
//! - You need `no-modules` wasm-pack target support
//!
//! # Usage
//!
//! Replace `use std::thread` with `use wasm_lite_std as thread`:
//!
//! ```
//! # if cfg!(target_arch="wasm32") { return; } //join() not reliable here
//! use wasm_lite_std as thread;
//!
//! // Spawn a thread
//! let handle = thread::spawn(|| {
//!     println!("Hello from a worker!");
//!     42
//! });
//!
//! // Wait for the thread to complete
//! // Synchronous join (works on native and some browser context - but not reliably!)
//! let result = handle.join().unwrap();
//! assert_eq!(result, 42);
//! ```
//!
//! # API
//!
//! ## Thread spawning
//!
//! ```
//! use wasm_lite_std::{spawn, spawn_named, Builder};
//!
//! // Simple spawn
//! let handle = spawn(|| "result");
//!
//! // Convenience function for named threads
//! let handle = spawn_named("my-worker", || "result").unwrap();
//!
//! // Builder pattern for more options
//! let handle = Builder::new()
//!     .name("my-worker".to_string())
//!     .spawn(|| "result")
//!     .unwrap();
//! ```
//!
//! ## Joining threads
//!
//! The portable way to wait for a thread's result is `join_async().await` — it
//! behaves identically on native, on wasm workers, and on the wasm main thread,
//! and returns the same `Result<T, Box<String>>` everywhere. (The `async_doctest!`
//! wrapper just drives the future to completion on whichever platform is running.)
//!
//! ```
//! # #[cfg(target_arch = "wasm32")] wasm_lite::set_panic_hook();
//! use wasm_lite_std::spawn;
//!
//! wasm_lite_std::async_doctest!(async {
//!     let result = spawn(|| 42).join_async().await.unwrap();
//!     assert_eq!(result, 42);
//! });
//! ```
//!
//! For a non-blocking completion check, use `is_finished`:
//!
//! ```
//! use wasm_lite_std::spawn;
//! let handle = spawn(|| 42);
//! if handle.is_finished() {
//!     // Thread completed
//! }
//! # drop(handle);
//! ```
//!
//! A synchronous `join()` also exists. It blocks the calling thread, so the only
//! place it can't be used is the browser main thread (which must never block) —
//! there it returns an error instead. Off the main thread it's fully portable:
//! inside a worker, `Atomics.wait` is available and blocking is fine, exactly as
//! on a native thread. So the joining just has to happen on a worker:
//!
//! ```
//! # #[cfg(target_arch = "wasm32")] wasm_lite::set_panic_hook();
//! use wasm_lite_std::spawn;
//!
//! wasm_lite_std::async_doctest!(async {
//!     // Do the blocking join on a worker, never on the main thread:
//!     spawn(|| {
//!         let result = spawn(|| 42).join().unwrap();
//!         assert_eq!(result, 42);
//!     })
//!     .join_async()
//!     .await
//!     .unwrap();
//! });
//! ```
//!
//! Still, prefer `join_async` on the main thread, where blocking isn't an option.
//!
//! ## Thread operations
//!
//! ```
//! use wasm_lite_std::{current, sleep, yield_now};
//! use std::time::Duration;
//!
//! // Get current thread
//! let thread = current();
//! println!("Thread: {:?}", thread.name());
//!
//! // Sleep
//! sleep(Duration::from_millis(10));
//!
//! // Yield to scheduler
//! yield_now();
//! ```
//!
//! Park/unpark works from background threads:
//!
//! ```
//! if cfg!(target_arch="wasm32") { return } //join not reliable on wasm
//! use wasm_lite_std::{spawn, park, park_timeout};
//! use std::time::Duration;
//!
//! let handle = spawn(|| {
//!     // Park/unpark (from background threads)
//!     park_timeout(Duration::from_millis(10)); // Wait with timeout
//! });
//! handle.thread().unpark();  // Wake parked thread
//! handle.join().unwrap();  // join() is not reliable on wasm and should be avoided
//! ```
//!
//! ## Event loop integration
//!
//! ```
//! # #[cfg(not(target_arch = "wasm32"))]
//! # fn main() {
//! use wasm_lite_std::yield_to_event_loop_async;
//!
//! // Yield to browser event loop (works on native too)
//! # wasm_lite_std::test_executor::spawn(async {
//! yield_to_event_loop_async().await;
//! # });
//! # }
//! # #[cfg(target_arch = "wasm32")]
//! # fn main() {} // the wasm future is !Send; the wasm path is covered by tests/browser.rs
//! ```
//!
//! ## Thread local storage
//!
//! ```
//! use wasm_lite_std::thread_local;
//! use std::cell::RefCell;
//!
//! thread_local! {
//!     static COUNTER: RefCell<u32> = RefCell::new(0);
//! }
//!
//! COUNTER.with(|c| {
//!     *c.borrow_mut() += 1;
//! });
//! ```
//!
//! ## Spawn hooks
//!
//! Register callbacks that run when any thread starts:
//!
//! ```
//! use wasm_lite_std::{register_spawn_hook, remove_spawn_hook, clear_spawn_hooks};
//!
//! // Register a hook
//! register_spawn_hook("my-hook", || {
//!     println!("Thread starting!");
//! });
//!
//! // Hooks run in registration order, before the thread's main function
//!
//! // Remove specific hook
//! remove_spawn_hook("my-hook");
//!
//! // Clear all hooks
//! clear_spawn_hooks();
//! ```
//!
//! ## Async tasks on a worker (WASM)
//!
//! Async work runs on the built-in event-loop executor: `wasm_lite_std::spawn_local(fut)`
//! queues a future on the current thread. A worker drains its `spawn_local` queue before
//! tearing down (the bootstrap polls the exported `__wl_executor_idle`), so tasks spawned
//! this way are awaited automatically — no manual bookkeeping:
//!
//! ```
//! # #[cfg(target_arch = "wasm32")]
//! # {
//! wasm_lite_std::spawn_local(async {
//!     // ... async work; the worker won't close until this completes ...
//! });
//! # }
//! ```
//!
//! For async work driven by some *other* mechanism (not the built-in executor), bracket it
//! with [`task_begin`]/[`task_finished`] so the worker still waits for it before exiting.
//! Both are no-ops on native, so they're safe to call unconditionally in cross-platform code.
//!
//! # WASM Limitations
//!
//! ## Main thread restrictions
//!
//! The browser main thread cannot use blocking APIs:
//!
//! - [`JoinHandle::join()`] - Use [`JoinHandle::join_async()`] instead
//! - [`park()`] / [`park_timeout()`] - Only works from background threads
//! - `Mutex::lock()` from std - Use `wasm_lite_std::Mutex` instead
//!
//! ## SharedArrayBuffer requirements
//!
//! Threading requires `SharedArrayBuffer`, which needs these HTTP headers:
//!
//! ```text
//! Cross-Origin-Opener-Policy: same-origin
//! Cross-Origin-Embedder-Policy: require-corp
//! ```
//!
//! See [Mozilla's documentation](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/SharedArrayBuffer) for details.
//!
//! ## Environment support
//!
//! - **Browser**: Web Workers over shared memory, driven by the `wasm_lite` runner.
//!   (There is no Node.js backend today — the runner is browser-only.)
//!
//! # Building for WASM
//!
//! To spawn workers, the standard library must be rebuilt with atomics support.
//! Stable, non-atomic builds can use the local executor and timers without this
//! configuration:
//!
//! ```bash
//! # Install nightly and components
//! rustup toolchain install nightly
//! rustup component add rust-src --toolchain nightly
//!
//! # Build with atomics. The link args are not optional: `+atomics` alone
//! # compiles in the spawning path but leaves memory unshared, so every spawn
//! # fails at runtime. The three exports are what the worker bootstrap reads.
//! RUSTFLAGS='-C target-feature=+atomics,+bulk-memory,+mutable-globals \
//!   -C link-arg=--shared-memory -C link-arg=--import-memory \
//!   -C link-arg=--max-memory=1073741824 \
//!   -C link-arg=--export=__stack_pointer \
//!   -C link-arg=--export=__tls_size \
//!   -C link-arg=--export=__wasm_init_tls' \
//! cargo +nightly build -Z build-std=std,panic_abort \
//!     --target wasm32-unknown-unknown
//! ```
//!
//! For doctests the same flags must be repeated under `rustdocflags`, keyed by
//! the exact triple. `docs/threads-and-async.md` has the whole configuration as
//! a `.cargo/config.toml` to copy.
//!

extern crate alloc;

mod animation;
mod async_wait;
mod block_on;
pub mod condvar;
#[cfg(feature = "fs")]
pub mod fs;
pub mod guard;
mod hooks;
pub mod mpsc;
pub mod mutex;
pub mod rwlock;
mod sleep;
pub mod spinlock;
#[cfg(not(target_arch = "wasm32"))]
mod stdlib;
// `#[wasm_lite_test(worker)]` emits absolute `wasm_lite_std::` paths to reach the
// worker spawn helper, which do not resolve inside wasm_lite_std itself. The
// alias makes our own name mean us, so the generated code compiles here too.
#[cfg(test)]
extern crate self as wasm_lite_std;

/// Runs an async test body on whichever executor the target has.
///
/// This crate cannot use `#[test_executors::async_test]`, even though that is
/// what it expands to elsewhere: `test_executors` depends on `wasm_lite_std`, so
/// patching it to the local path to get the wasm_lite spelling makes the
/// lib-test binary link two copies of this crate and collide on its
/// `#[no_mangle]` executor exports. See the note in the workspace `Cargo.toml`.
#[cfg(test)]
macro_rules! async_test_body {
    ($body:expr) => {{
        #[cfg(target_arch = "wasm32")]
        $crate::async_doctest!($body);
        #[cfg(not(target_arch = "wasm32"))]
        $crate::test_executor::spawn($body);
    }};
}
#[cfg(test)]
pub(crate) use async_test_body;

/// Like [`async_test_body`], but for a body that also *blocks* — a
/// `recv()` on a std channel, a `join()`, a spin wait.
///
/// Those trap on the browser's main thread, so the test must carry
/// `#[wasm_lite_test(worker)]` and drive the future with the blocking poll loop
/// rather than handing it to the main-thread event loop.
#[cfg(test)]
macro_rules! blocking_async_test_body {
    ($body:expr) => {{
        $crate::test_executor::spawn($body);
    }};
}
#[cfg(test)]
pub(crate) use blocking_async_test_body;

#[cfg(test)]
mod sync_tests;
#[doc(hidden)]
pub mod test_executor;
pub mod time;
#[cfg(target_arch = "wasm32")]
mod wasm;
mod wasm_support;

#[cfg(not(target_arch = "wasm32"))]
use stdlib as backend;
#[cfg(target_arch = "wasm32")]
use wasm as backend;

use std::io;
use std::num::NonZeroUsize;
use std::time::Duration;

pub use animation::request_animation_frame;
/// Run a future to completion on the current thread's event loop (wasm only).
#[cfg(target_arch = "wasm32")]
pub use backend::spawn_local;
pub use backend::yield_to_event_loop_async;
pub use backend::{AccessError, Builder, JoinHandle, LocalKey, Thread, ThreadId};
pub use backend::{task_begin, task_finished};
pub use block_on::block_on;
pub use guard::Guard;
pub use hooks::{clear_spawn_hooks, register_spawn_hook, remove_spawn_hook};
pub use mutex::{Mutex, NotAvailable};
pub use sleep::{MAX_TIMEOUT, SleepAsync, sleep_async};

const CONSOLE_REDIRECT_HOOK_NAME: &str = "wasm_lite_std::println_eprintln_console_redirect";

/// Declare a new thread local storage key of type [`LocalKey`].
///
/// # Examples
///
/// ```
/// use wasm_lite_std::thread_local;
/// use std::cell::RefCell;
///
/// thread_local! {
///     static FOO: RefCell<u32> = RefCell::new(1);
/// }
///
/// FOO.with(|f| {
///     assert_eq!(*f.borrow(), 1);
///     *f.borrow_mut() = 2;
/// });
/// ```
#[macro_export]
#[cfg(not(target_arch = "wasm32"))]
macro_rules! thread_local {
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr; $($rest:tt)*) => {
        std::thread_local! {
            $(#[$attr])* static INNER: $t = $init;
        }
        $(#[$attr])* $vis static $name: $crate::LocalKey<$t> = $crate::LocalKey::new(&INNER);
        $crate::thread_local!($($rest)*);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr) => {
        std::thread_local! {
            $(#[$attr])* static INNER: $t = $init;
        }
        $(#[$attr])* $vis static $name: $crate::LocalKey<$t> = $crate::LocalKey::new(&INNER);
    };
    () => {};
}

/// Declare a new thread local storage key of type [`LocalKey`].
#[macro_export]
#[cfg(target_arch = "wasm32")]
macro_rules! thread_local {
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr; $($rest:tt)*) => {
        std::thread_local! {
            $(#[$attr])* static INNER: $t = $init;
        }
        $(#[$attr])* $vis static $name: $crate::LocalKey<$t> = $crate::LocalKey::new(&INNER);
        $crate::thread_local!($($rest)*);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr) => {
        std::thread_local! {
            $(#[$attr])* static INNER: $t = $init;
        }
        $(#[$attr])* $vis static $name: $crate::LocalKey<$t> = $crate::LocalKey::new(&INNER);
    };
    () => {};
}

/// Spawns a new thread, returning a JoinHandle for it.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    backend::spawn(f)
}

/// Gets a handle to the thread that invokes it.
pub fn current() -> Thread {
    backend::current()
}

/// Returns `true` unless the current thread was spawned by this crate.
///
/// - **Native**: `true` on the process's initial thread (and any thread not
///   created via [`spawn`]/[`Builder`]); `false` on threads this crate spawned.
/// - **WASM**: `true` on the browser main thread, `false` on a spawned Web Worker.
///   This is the thread where `Atomics.wait` is unavailable, so blocking APIs
///   ([`park`], blocking locks) must not be used there.
pub fn is_main_thread() -> bool {
    backend::is_main_thread()
}

/// Puts the current thread to sleep for at least the specified duration.
pub fn sleep(dur: Duration) {
    backend::sleep(dur)
}

/// Cooperatively gives up a timeslice to the OS scheduler.
pub fn yield_now() {
    backend::yield_now()
}

/// Blocks unless or until the current thread's token is made available.
pub fn park() {
    backend::park()
}

/// Blocks unless or until the current thread's token is made available
/// or the specified duration has been reached.
pub fn park_timeout(dur: Duration) {
    backend::park_timeout(dur)
}

/// Returns an estimate of the default amount of parallelism a program should use.
pub fn available_parallelism() -> io::Result<NonZeroUsize> {
    backend::available_parallelism()
}

/// A convenience function for spawning a thread with a name.
pub fn spawn_named<F, T>(name: impl Into<String>, f: F) -> io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Builder::new().name(name.into()).spawn(f)
}

/// Redirects `println!`/`eprintln!` for the current thread to JavaScript console output.
///
/// On `wasm32` + nightly this installs redirection for the calling thread.
/// On other targets/toolchains this is a no-op.
pub fn redirect_println_eprintln_to_console_current_thread() {
    backend::redirect_println_eprintln_to_console_current_thread_impl();
}

/// Installs a global spawn hook that redirects `println!`/`eprintln!` to JavaScript console output.
///
/// On `wasm32` + nightly this causes each newly spawned thread to install redirection.
/// On other targets/toolchains this hook has no effect.
pub fn install_println_eprintln_console_hook() {
    register_spawn_hook(CONSOLE_REDIRECT_HOOK_NAME, || {
        redirect_println_eprintln_to_console_current_thread();
    });
}

/// Run a closure on a **dedicated worker**, fail-closed, for doctests.
///
/// The counterpart of [`async_doctest!`](crate::async_doctest) for *blocking*
/// code. A doctest runs as a plain `bin` on the browser main thread, where
/// `Atomics.wait` traps — so anything that calls `join`, `park`, `recv_block` or
/// a `lock_block` cannot be demonstrated there at all. This spawns the body on a
/// worker, where those are legal, and defers the verdict until the worker joins,
/// so a panic inside it fails the doctest rather than passing silently.
///
/// It is what `#[wasm_lite_test(worker)]` does, in a form a doctest can use.
/// On native it just calls the closure.
///
/// This replaces wasm-bindgen's `wasm_bindgen_test_configure!(run_in_dedicated_worker)`,
/// which was a file-level switch; this is per-doctest, because a file usually
/// has both kinds.
///
/// ```
/// # #[cfg(target_arch = "wasm32")] wasm_lite::set_panic_hook();
/// wasm_lite_std::worker_doctest!(|| {
///     // `join` blocks, which is fine here and would trap on the main thread.
///     let v = wasm_lite_std::spawn(|| 2 + 2).join().unwrap();
///     assert_eq!(v, 4);
/// });
/// ```
#[macro_export]
macro_rules! worker_doctest {
    ($body:expr) => {{
        #[cfg(target_arch = "wasm32")]
        {
            $crate::__rt::test_pending();
            $crate::spawn_local(async {
                // Inside the body, not before it: see `__rt::set_panic_hook`.
                $crate::__rt::set_panic_hook();
                $crate::spawn($body).join_async().await.unwrap();
                $crate::__rt::test_pass();
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let f = $body;
            f();
        }
    }};
}

/// Run a future as a **fail-closed** async test (doctests, tests, `main`) — works in doctests, tests, and
/// `main`.
///
/// The wasm test runner decides pass/fail by polling a still-live browser page,
/// not by `main` returning. So this drives the future on the event-loop executor
/// and **defers the verdict** until the future completes: a panic inside it
/// (including an awaited worker panic propagated by `.unwrap()`) fails the test,
/// and a hang fails via the runner's timeout. An async test can never pass by
/// default. On native it simply blocks on the future, so a panic propagates to
/// the normal harness.
///
/// ```
/// # #[cfg(target_arch = "wasm32")] wasm_lite::set_panic_hook();
/// wasm_lite_std::async_doctest!(async {
///     let v = wasm_lite_std::spawn(|| 2 + 2).join_async().await.unwrap();
///     assert_eq!(v, 4);
/// });
/// ```
#[macro_export]
macro_rules! async_doctest {
    ($fut:expr) => {{
        #[cfg(target_arch = "wasm32")]
        {
            $crate::__rt::test_pending();
            $crate::spawn_local(async move {
                // Inside the body, not before it: see `__rt::set_panic_hook`.
                $crate::__rt::set_panic_hook();
                let _ = $fut.await;
                $crate::__rt::test_pass();
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = $crate::block_on($fut);
        }
    }};
}

/// Runtime hooks for [`async_doctest!`](crate::async_doctest). Not a stable API.
#[doc(hidden)]
pub mod __rt {
    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "__wasm_lite")]
    unsafe extern "C" {
        #[link_name = "__wl_test_pending"]
        fn pending();
        #[link_name = "__wl_test_pass"]
        fn pass();
    }

    /// Mark the test as having pending async work, so the runner defers its verdict.
    #[cfg(target_arch = "wasm32")]
    pub fn test_pending() {
        unsafe { pending() }
    }

    /// Signal that the async test body completed successfully.
    #[cfg(target_arch = "wasm32")]
    pub fn test_pass() {
        unsafe { pass() }
    }

    /// Install wasm_lite's panic hook from *inside* the deferred body.
    ///
    /// A doctest that calls `set_panic_hook()` in its own `main` does not get it
    /// here. In an edition-2024 merged bundle rustdoc runs each doctest as a
    /// libtest `#[test]`, and libtest takes the current hook before a test and
    /// restores it afterwards — so a hook installed *during* the doctest is
    /// dropped the moment its `main` returns. The deferred body runs later, off
    /// the event loop, by which time the hook is gone and a panic reports a bare
    /// wasm stack trace with no message.
    ///
    /// Installing it at the top of the body puts it in force at the only moment
    /// that matters: when the body can actually panic. Called by
    /// [`async_doctest!`](crate::async_doctest) and
    /// [`worker_doctest!`](crate::worker_doctest), so their documented promise —
    /// that a failure reports its message — holds without the caller doing
    /// anything.
    #[cfg(target_arch = "wasm32")]
    pub fn set_panic_hook() {
        wasm_lite::set_panic_hook();
    }
}

#[cfg(test)]
mod tests;
