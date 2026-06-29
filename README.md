# wasm_lite

A "smol"-style rewrite of wasm-bindgen: bind JavaScript and Rust to each other
on `wasm32-unknown-unknown`, with no *runtime* dependencies and a small
host-side codegen tool. The core (`import!`, `JsValue`, runtime) and the codegen
are dependency-free; the proc-macros (`wasm_lite_macro`) use `syn`/`quote`, which
are build-time-only (zero bytes in the `.wasm`).

A checkout of wasm-bindgen is available in the `wasm-bindgen/` folder for reference.

## Design goals

* Runner for major web browsers — **done** (WebDriver: Firefox/Chrome/Safari).
* Support with and without +atomics — **done**: shared-memory `+atomics` builds
  run, and threads spawn onto Web Workers (`wasm_lite::thread::spawn`). A
  `std::thread`-like layer (`wasm_lite_std`) is the next step.
* Unit test support — **done** (`#[wasm_lite_test]`, `cargo test` via a custom runner).
* Bind JS APIs to Rust and vice versa — **done** (`import!` / `#[export]`).
* Doctest support — **done** (runs rustdoc doctests in a browser).
* Simple, clean architecture — ongoing.
* Avoid dependencies — **mostly held**: zero *runtime* deps; the core crate and
  codegen pull nothing. The proc-macro crate uses `syn`/`quote` (build-time only)
  — a deliberate trade for typed parsing + hygienic codegen over hand-rolled
  token wrangling.

Nice to have:
* Interop with wasm-bindgen crates — **done** behind the `wasm-bindgen` feature.

## How it works

There is no `#[wasm_bindgen]`-style all-in-one macro. Instead the flow is:

1. **Rust side.** `import!`, `#[export]`, and `js_class!` emit the usual wasm
   imports/exports *plus* a descriptor line into a custom wasm section
   (`__wasm_lite_imports`, `__wl_exports`) describing each binding's ABI.
2. **Codegen.** The `wasm-lite` CLI reads those sections from the compiled
   `.wasm` and generates a matching JavaScript glue module — the import object
   the wasm expects, plus one wrapper per `#[export]`. No JS is hand-written
   per binding.
3. **Run.** The `runner` launches the module in a real browser over WebDriver,
   and doubles as a `cargo` test/run runner.

```
cargo build --target wasm32-unknown-unknown      # produces app.wasm
wasm-lite app.wasm -o glue.js                     # generates the JS glue
# import { instantiate, <your exports> } from "./glue.js"
```

## Workspace layout

| crate | role |
|---|---|
| `crates/wasm_lite` | core: `import!`, `#[export]`, `js_class!`, `JsValue`, runtime (`__wl_malloc`/`__wl_free`, panic hook), `thread::spawn`, `console`/`performance` bindings |
| `crates/wasm_lite_macro` | proc-macros (`syn`/`quote`): `import!`, `#[export]`, `#[wasm_lite_test]`, `js_class!` (shared type→ABI dispatch in `ty`) |
| `crates/wasm_lite_codegen` | host-side: read descriptor sections, generate JS glue |
| `crates/wasm_lite_cli` | the `wasm-lite` binary wrapping codegen |
| `crates/wasm_lite_std` | std-like veneer (`std::thread`/`std::sync`); ported from `wasm_safe_thread`, sync path retargeted onto `wasm_lite` |
| `runner` | WebDriver runner; serves a bin interactively, or drives tests/doctests headless and exits |

Examples (each standalone, builds to `wasm32-unknown-unknown`):
`examples/hello-rust` (imports, handles, strings, bytes, `js_class!`),
`exports-demo` (Rust→JS exports), `tests-demo` (`#[wasm_lite_test]`),
`doctest-demo` (doctests), `interop` (wasm-bindgen bridge),
`atomics-demo` (shared memory + atomics; nightly),
`threads-demo` (`thread::spawn` over Web Workers; nightly),
`std-threads-demo` (`wasm_lite_std::spawn`, the std-like API; nightly),
`async-demo` (non-blocking `join_async` on the main thread; nightly),
`async-mutex-demo` (main `lock_async` woken cross-thread by a worker; nightly).

## Binding model

**Import JS into Rust** — `import!`, grouped by JS namespace:

```rust
wasm_lite::import! {
    "Math" {
        fn random() -> f64;
        fn max2(a: f64, b: f64) -> f64 as "max";   // `as` decouples JS name -> overloads
    }
    "JSON" { fn parse(text: &str) -> JsValue; }     // returns an object handle
    "Array" { fn push(this: &JsValue, value: f64) -> f64; }  // method on a handle
}
```

Each binding gets a unique wasm import symbol (via `module_path!()`), so the
same JS function can be bound from many crates/modules without link conflicts.

**Export Rust to JS** — `#[export]`:

```rust
#[wasm_lite::export]
pub fn greet(name: &str) -> String { format!("hello, {name}!") }
// JS: import { greet } from "./glue.js"; greet("world")
```

**Typed object wrappers** — `js_class!` (a newtype over `JsValue`; methods lower
to `receiver[name](args)`, delegating all ABI work to `import!`):

```rust
wasm_lite::js_class! {
    type JsArray;
    impl JsArray {
        fn push(&self, value: f64) -> f64;
        fn join(&self, sep: &str) -> String;
        fn concat(&self, other: &JsArray) -> JsArray;  // typed arg + typed return
    }
}
```

**`JsValue`** is an opaque handle into a host-side value table; it is `!Send`/
`!Sync` (a handle is only meaningful in the realm that created it) and frees its
table slot on `Drop`.

### Type marshalling

Symmetric across imports and exports:

| type | import arg | import return | export arg | export return |
|---|---|---|---|---|
| numbers / `bool` | ✓ | ✓ | ✓ | ✓ |
| strings | `&str` | `String` | `&str` | `String` |
| bytes | `&[u8]` | `Vec<u8>` | `&[u8]` | `Vec<u8>` |
| JS objects | `&JsValue` | `JsValue` | `JsValue` | `JsValue` |

Strings/bytes are passed by allocating in wasm memory (`__wl_malloc`, align 1)
and handing over a packed `(ptr<<32 | len)` `i64`; ownership transfers to
whichever side allocated last. Objects cross as `u32` value-table indices.
The import/export asymmetry for objects is deliberate: an import *lends* Rust's
handle (`&JsValue`), an export *takes* ownership from JS (`JsValue` by value).

`Option<T>` and `Result<T, E>` are supported as **return** types (imports and
exports), where the scalar return ABI can't carry a discriminant. They use a
return pointer (sret): a 16-byte buffer holds a discriminant word plus the
payload at offset 8. `None` ↔ JS `null`; `Err(e)` ↔ a **thrown** JS exception
(`Ok`/`Some` carry the value). Inner types may be any scalar/string/bytes/handle.

`Option<T>` is also supported as an **argument** (a nullable parameter): it
flattens to a discriminant `i32` plus T's normal parameters. On exports JS
`null`/`undefined` → `None`; on imports `None` → JS `undefined` (so a JS default
parameter applies). `Result` arguments are *not* supported — JS has no `Result`
type, so there is no natural value to pass (this matches wasm-bindgen).

```rust
#[wasm_lite::export]
pub fn divide(a: f64, b: f64) -> Result<f64, String> {       // Err -> JS throw
    if b == 0.0 { Err("division by zero".into()) } else { Ok(a / b) }
}

wasm_lite::import! {
    "JSON" { fn try_parse(text: &str) -> Result<f64, JsValue> as "parse"; }  // JS throw -> Err
}
```

## Testing

```toml
# .cargo/config.toml
[target.wasm32-unknown-unknown]
runner = "path/to/runner"
```

* `#[wasm_lite_test]` marks a test; it is recorded in `__wasm_lite_tests` and
  the runner discovers and drives each one in a browser (pass / fail / panic).
* Plain `cargo run --example foo` serves the bin interactively in the browser;
  `cargo test` runs headless and exits — the runner distinguishes them by path.
* Doctests run too (rustdoc's doctest binaries are detected and driven headless).
  Call `wasm_lite::set_panic_hook()` at the top of a doctest so failures report
  the panic message.

## Shared memory & atomics

wasm_lite runs modules built with the threads-related wasm features
(`+atomics,+bulk-memory,+mutable-globals`) on a **shared** linear memory (a
`SharedArrayBuffer`). This is the foundation for threads; actually spawning work
onto Web Workers is not implemented yet, but everything below is in place:

* **Toolchain.** `+atomics` means `std` must be recompiled with it, so these
  builds need **nightly** and `-Z build-std`. See
  `examples/atomics-demo/.cargo/config.toml`: it sets the target features, links
  with `--shared-memory --max-memory=… --import-memory`, and adds
  `build-std = ["std", "panic_abort"]`. Build with `cargo +nightly run`.
* **Imported memory.** `--import-memory` makes the module import its memory
  rather than define it, so JS owns the one `WebAssembly.Memory` object (the same
  object every future worker will share). The codegen reads the module's imported
  memory limits and emits `makeMemory()` plus an `instantiate(url, memory?)` that
  creates the shared memory (or accepts one) and supplies it as an import.
* **Cross-origin isolation.** Browsers only hand out `SharedArrayBuffer` to
  cross-origin-isolated pages, so the runner serves
  `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Embedder-Policy: require-corp` on every response.
* **Init.** LLD emits a `start` function that sets up the main thread's TLS and
  initializes passive data segments on first instantiation — so single-threaded
  atomic code and `thread_local!` work with no manual setup.

`JsValue` is already `!Send`/`!Sync`: a handle indexes a per-realm value table,
so it is only valid on the worker that created it — the type system forbids
sending one across threads.

### Spawning threads

`wasm_lite::thread::spawn(move || { … })` runs a closure on a new Web Worker
sharing this module's compiled `WebAssembly.Module` and shared memory — **no
wasm-bindgen, js-sys, or web-sys**. The `Worker` lives entirely in the generated
glue behind a single `__wl_spawn` import:

* `spawn` boxes the closure (double-boxed to a thin `u32` pointer) and calls
  `__wl_spawn`.
* The glue allocates a fresh stack + TLS block (`__wl_thread_alloc`) and starts a
  worker, postMessaging `{ module, memory, work, stackTop, tlsPtr }`.
* The worker (a codegen-emitted bootstrap, `wl_worker.js`) instantiates the same
  module on the same memory, points `__stack_pointer` at the new stack, calls
  `__wasm_init_tls`, then `__wl_thread_entry` — which reconstitutes the closure
  and runs it. Threads coordinate via `core::sync::atomic`.

This requires the worker bootstrap to import the glue *without* re-running
`main`, so the runner serves the glue (`program.js`) separately from a small
bootstrap module. A threaded build must export the linker's thread symbols;
`examples/threads-demo/.cargo/config.toml` shows the flags
(`--export=__stack_pointer`, `__tls_size`, `__wasm_init_tls`, …).

The core `spawn` is **detached** (no `JoinHandle`). The std-like layer with
`spawn -> JoinHandle`, `park`/`unpark`, `Mutex`/`Condvar`/`RwLock`/`mpsc` lives in
`wasm_lite_std` (a port of `wasm_safe_thread`, retargeted off wasm-bindgen onto
this primitive + `core::arch::wasm32` atomics).

Both the **sync** and **async** paths work. Since the main thread can't block,
`wasm_lite_std` ships a small event-loop executor: `spawn_local(future)` runs a
task on the event loop, and while it's pending the executor *sleeps* on
`Atomics.waitAsync` (the `__wl_wait_async` runtime import) rather than busy-polling.
Wakes are edge-triggered and cross-thread: each executor owns a wake atom, and a
task's `Waker` bumps it and issues `memory.atomic.notify` — which resolves the
owning realm's `waitAsync` Promise even when the notify comes from another worker.
So `JoinHandle::join_async().await`, `Mutex::lock_async().await`, etc. run
non-blocking on the main thread and are woken the instant a worker releases.

### Async lifecycle & failures — two fixes for wasm-bindgen footguns

`spawn_local` is meant to be the **uniform** primitive: the same
`spawn_local(a); spawn_local(b); …` works on any thread, and "wait for my tasks"
is implicit — the event loop on the main thread, a drain refcount on a worker.
`block_on` is the niche tool (a worker that truly needs a *synchronous* result);
it is the one that must know it isn't the main thread, and deadlocks if it's
wrong. For that uniformity to hold, two things have to be true:

* **Threads drain their async tasks before teardown** *(planned)*. A wasm-bindgen
  worker `close()`s when its entry returns, so a `spawn_local`'d task is silently
  abandoned — "the thread shut down and my futures mysteriously stopped." Here the
  worker bootstrap will instead poll an exported `__wl_executor_idle()` and only
  free its TLS/stack + `close()` once the executor has drained (it must not free
  the TLS its task queue lives in). Explicit termination stays rare; the right
  tool for it is *cooperative cancellation* (a token the tasks check), not a hard
  `terminate()` that strands held locks.

* **Async tests are fail-closed** *(planned)*. `#[wasm_lite_async_test]` /
  `async_doctest!` never pass by default — unlike `rustdoc`/`libtest`, where
  `main` returning *is* the verdict (so a deferred async failure can't be seen).
  The only thing that records success is the async body reaching its end; a panic,
  dropped task, or deadlock therefore cannot masquerade as a pass. Panics report
  *fast and attributed* (the panic hook writes the verdict + message before the
  `abort` trap, with a `try/catch` around the executor tick as a backstop); a true
  deadlock degrades to a diagnostic timeout. (The verdict is rendered by the
  runner polling a still-live browser page, not by `main` returning — which is
  what makes deferring it past `main` possible.)

On panics: `panic = "abort"` is the supported model. On wasm a panic is an
`unreachable` **trap local to one instance** — verified: a panicking worker traps
only itself; the main thread and other workers keep running and shared memory
persists (unlike native, where `abort()` kills the whole process). So in a
multithreaded executor a panic takes down only *that* worker (its futures die with
it); siblings are unaffected. The one residual surprise is a lock the dead thread
held — with no unwind there's no `Drop` and no poison, so it stays locked — which
is why the `*_timeout` lock APIs exist: a dead holder surfaces as a timeout, not a
hang. (Our own runtime holds no cross-thread lock across a poll — the executor's
queue is thread-local and its wake is a lock-free atom — and the shared allocator
survives a panic, so a no-user-lock hang is not something we introduce. A future
`panic = "unwind"` mode could `catch_unwind` per poll, drop just the failed task,
and poison its locks.)

### How a panic surfaces (browser vs CLI)

The default `wasm32` panic prints nothing, so `wasm_lite_std`'s worker hook
**always** logs the panic to the console with thread attribution
(`[wasm_lite_std ThreadId(N)] panicked at …`) — never silent — *in addition to*
routing it to the join channel. That covers the interactive/browser case fully.

The CLI (`cargo test` / doctests via the runner) is only partly there today:

| Panic site | Browser console | CLI (terminal) |
|---|---|---|
| main thread, hook installed | ✓ message | ✓ message + `FAILED` (runner prints the captured console on failure) |
| main thread, no hook | trap only | trap only — install `set_panic_hook()` |
| **joined** worker | ✓ message | ✓ via the channel → the joiner re-panics on main → captured |
| **detached** worker | ✓ message | ✗ **not surfaced** — the worker is a separate JS realm, and the runner only captures the *main* realm's console |

Worker console output is **bridged to the main realm** so it reaches the CLI: a
worker forwards each console line up the spawn chain via `postMessage`, and the
runner prints any worker-panic lines (even on a passing test). So a detached
worker panic now shows on the terminal as a warning, e.g.
`[wasm_lite_std ThreadId(0)] panicked at …`, while the test still passes.

**Detached vs. awaited.** A *detached* (never-joined) worker panic is logged but
doesn't fail the test — matching `std`, where an unjoined thread's panic prints
without failing. An **awaited** panic *propagates*: the worker's panic is
delivered to `join_async().await` as `Err(message)` (sent through the channel
before the worker aborts), so a wrapper returning `T` unwraps it and re-panics on
the awaiter — failing the test, exactly like `std::thread::join` /
`tokio::JoinHandle` (which hand you a `Result` you unwrap). *Caveat:* on the wasm
main thread the await runs on the `spawn_local` executor, and turning that
propagated panic into a hard CLI **failure** (rather than a passing-with-warning)
needs the fail-closed async-test verdict (defer the verdict past `main`; wrap the
executor tick in `try/catch`) — *planned*, and the same machinery that makes async
doctests trustworthy.

Doctests go through the same path, so they inherit all of the above. A failing
*sync* doctest with `set_panic_hook()` reports the full message + `FAILED` on the
CLI. Note: with Rust 2024 *merged* doctests, the first `panic = "abort"` aborts
the whole bundle, so later doctests in the crate don't run.

## wasm-bindgen interop

Enable the `wasm-bindgen` feature to link a crate that itself uses wasm-bindgen.
`wasm_lite_codegen` runs the (version-matched) wasm-bindgen CLI, merges its
loader with our glue, and provides explicit `.to_wasm_bindgen()` /
`.to_wasm_lite()` conversions between the two `JsValue` types.

## Planned crate layering

Following the wasm-bindgen ecosystem split (language vs browser):

* `wasm_lite` — core (above). *Like `wasm-bindgen`.*
* `wasm_lite_js` *(future)* — ECMAScript built-ins (`Object`, `Array`, `Map`,
  `JSON`, `Date`, …) bound with `js_class!`. *Like `js-sys`.*
* `wasm_lite_web` *(future)* — Web/host APIs (DOM, `fetch`, …). *Like `web-sys`.*
* `wasm_lite_std` *(in progress)* — std-like veneer: a `std::thread` + `std::sync`
  port of [`wasm_safe_thread`](https://crates.io/crates/wasm_safe_thread) with its
  wasm backend retargeted off wasm-bindgen onto `wasm_lite::thread::spawn` +
  `core::arch::wasm32` atomics. Both **sync** and **async** paths work:
  `spawn`/`JoinHandle` (`join`/`join_async`), `park`/`unpark`, `Mutex`/`Condvar`/
  `RwLock`/`mpsc` (sync + async), and a `spawn_local` event-loop executor for
  non-blocking async on the main thread. Its only runtime dep is `continue`
  (a wasm-bindgen-free continuation primitive).

Bindings stay out of core so it remains small; `js_class!` is the primitive all
upper layers build on.

## Known gaps / roadmap

* `js_class!`: constructors (`new Foo()`), property get/set (`el.textContent`),
  owned-object args, and `instanceof`-checked downcasting — each needs a new
  codegen shim kind. Constructors + properties are the prerequisite for starting
  `wasm_lite_js` / `wasm_lite_web`.
* `wasm_lite_std`: a `std::thread` + `std::sync` veneer (`spawn -> JoinHandle`,
  `park`/`unpark`, `Mutex`/`RwLock`/`Condvar`/`mpsc`) built on the detached
  `thread::spawn` primitive — modelled on (and likely a port of)
  `wasm_safe_thread`, with its wasm backend retargeted off wasm-bindgen onto
  `__wl_spawn`. (Shared memory, atomics, and detached spawn: done, above.)
* Nested generics like `Option<Vec<u8>>` on the import side (the macro grammar
  takes single-ident inner types today; the proc-macro export side already
  allows nesting). `Option<&[u8]>` arguments on the import side.
* `js_class!` constructors (`new Foo()`) + property get/set — the prerequisite
  for standing up `wasm_lite_js` / `wasm_lite_web`.
* Deployment niceties: a `wasm-lite bundle` command, session pooling/idle reaper
  for the persistent browser, test filtering (`cargo test NAME`).
