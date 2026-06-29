# Migrating from wasm-bindgen to wasm_lite

This guide is for people who already ship Rust on `wasm32-unknown-unknown` with
[`wasm-bindgen`](https://github.com/rustwasm/wasm-bindgen) and are evaluating
`wasm_lite`. It assumes you know the wasm-bindgen flow (`#[wasm_bindgen]`,
`wasm-pack`, `js-sys`/`web-sys`, `wasm-bindgen-futures`). The
[README](./README.md) is the feature baseline; this document goes deeper on the
trade-offs, gives a side-by-side "rosetta stone", and walks through the gotchas
you will actually hit.

> **TL;DR.** `wasm_lite` is a smaller, dependency-light reimplementation of the
> *core idea* of wasm-bindgen (describe a binding's ABI in a custom wasm section,
> generate JS glue from the compiled module). It is **not** a drop-in replacement
> and it does **not** yet have the binding ecosystem (`js-sys`/`web-sys`),
> Promise interop, or closures-into-JS. What it *does* have that wasm-bindgen
> does not: a built-in browser test/run/doctest runner, a wasm-bindgen-free
> threading + async story (`wasm_lite_std`), and zero runtime dependencies /
> zero bytes of proc-macro output in the `.wasm`.

---

## 1. Should you migrate? Detailed pros and cons

The README has a short "design goals" list. Here is the longer, honest version.

### Where wasm_lite wins

| | wasm-bindgen | wasm_lite |
|---|---|---|
| **Runtime deps in your `.wasm`** | pulls `wasm-bindgen` (and usually `js-sys`/`web-sys`) into the dependency graph | **zero** runtime deps; core crate + codegen pull nothing. Proc-macros use `syn`/`quote` but those are build-time only (0 bytes in `.wasm`) |
| **Toolchain** | needs the external `wasm-bindgen` CLI *and* (for `wasm-pack`) a Node toolchain | one host binary, `wasm-lite`, that reads the `.wasm` and emits a single ES module. No Node, no bundler required |
| **Testing** | `wasm-bindgen-test` + `wasm-bindgen-test-runner` (separate crates) | built in: `#[wasm_lite_test]`, `cargo test` via a custom runner, **and doctests run in a real browser**. `cargo run --example foo` serves the bin interactively |
| **Threads** | not in wasm-bindgen proper; you reach for `wasm_thread`/`wasm-bindgen-rayon`, which depend on wasm-bindgen + js-sys | `wasm_lite::thread::spawn` over Web Workers with **no wasm-bindgen/js-sys/web-sys**, plus a `std::thread`-shaped layer (`wasm_lite_std`) with `Mutex`/`RwLock`/`Condvar`/`mpsc`/`JoinHandle` |
| **Async on the main thread** | `wasm-bindgen-futures` (great for awaiting JS Promises) | an event-loop executor that `Atomics.waitAsync`-sleeps instead of busy-polling, designed for **cross-thread** coordination (`join_async`, `lock_async`) |
| **`std::time` on wasm** | typically `web-time` (depends on wasm-bindgen/js-sys) | `wasm_lite_std::time` — drop-in `Instant`/`SystemTime`, **no** wasm-bindgen/js-sys |
| **Architecture surface** | large, mature, lots of moving parts | small enough to read end to end; the whole ABI is "pack `(ptr<<32 | len)` into an `i64`, objects are `u32` table indices" |

### Where wasm-bindgen wins (today)

| | Status in wasm_lite |
|---|---|
| **`js-sys` (ECMAScript built-ins)** | **Not yet.** Planned as `wasm_lite_js`, gated on `js_class!` constructors + property accessors. You bind what you need by hand with `import!`/`js_class!`. |
| **`web-sys` (DOM, `fetch`, `WebGL`, …)** | **Not yet.** Planned as `wasm_lite_web`. Same story — hand-bind for now. |
| **Awaiting a JS `Promise` from Rust** (`JsFuture`, `wasm-bindgen-futures`) | **Not supported** *(on the [roadmap](./docs/roadmap.md))*. wasm_lite's async is for *thread* coordination, not Promise interop. You cannot `fetch(...).await` today. See [§4 Gotchas](#4-design-trade-offs-and-gotchas). |
| **Passing a Rust closure to JS as a callback** (`Closure<dyn FnMut(...)>`) | **Not supported** *(on the [roadmap](./docs/roadmap.md))*. `import!` takes scalars/strings/bytes/handles, not function pointers. Event listeners / callbacks must be structured around exports or polled state. |
| **Rich type marshalling** (`serde-wasm-bindgen`, `Vec<T>` of structs, tuples, enums with data) | Manual *(a serde bridge is on the [roadmap](./docs/roadmap.md))*. The ABI carries numbers, `bool`, strings, bytes, and opaque `JsValue` handles, plus `Option`/`Result` as *returns*. Anything richer you encode yourself (e.g. JSON through a `&str`). |
| **TypeScript `.d.ts` generation** | Not generated. The codegen emits plain JS glue. |
| **`getrandom`/`rand`/`uuid`, `HashMap` with strong seed** | **Not yet.** `getrandom`'s `js` feature pulls in wasm-bindgen; a wasm-bindgen-free `crypto.getRandomValues` backend is on the roadmap. Until then, randomness-dependent crates need work. |
| **Ecosystem & docs** | wasm-bindgen is years mature with enormous community coverage; wasm_lite is new. |
| **Stable toolchain for threads** | Threaded/atomics builds need **nightly** + `-Z build-std` (this is a wasm/LLVM constraint, not unique to wasm_lite, but wasm-bindgen's non-threaded path runs on stable). |

### Who should migrate now

- **Good fit:** compute-in-wasm libraries, CLI-shaped or worker-heavy workloads,
  anything where you want threads without dragging in the wasm-bindgen stack, and
  projects that value a tiny dependency graph and a built-in test runner.
- **Wait for now:** DOM-heavy front ends that lean on `web-sys`, anything that
  `await`s `fetch`/Promises, code that passes closures to JS event listeners, or
  crates whose dependencies assume `getrandom`'s `js` feature.
- **Hybrid is supported:** you don't have to choose all-or-nothing. The
  `wasm-bindgen` feature lets a wasm_lite module link a crate that itself uses
  wasm-bindgen, with explicit `.to_wasm_bindgen()` / `.to_wasm_lite()` value
  conversions. See [§3 Interop](#interop-keep-a-wasm-bindgen-crate-in-the-mix).

---

## 2. Rosetta stone

Each row links to a real, building example in this repo. Build any example with
`cargo build --target wasm32-unknown-unknown` (threaded ones need
`cargo +nightly run`), then `wasm-lite app.wasm -o glue.js`.

### Project setup

**wasm-bindgen**
```toml
# Cargo.toml
[lib]
crate-type = ["cdylib"]
[dependencies]
wasm-bindgen = "0.2"
```
```bash
wasm-pack build --target web      # or: cargo build + wasm-bindgen CLI
```

**wasm_lite**
```toml
# Cargo.toml
[dependencies]
wasm_lite = { path = "…/crates/wasm_lite" }
```
```bash
cargo build --target wasm32-unknown-unknown
wasm-lite target/wasm32-unknown-unknown/debug/app.wasm -o glue.js
# import { instantiate, <exports> } from "./glue.js"
```
There is **no all-in-one `#[wasm_bindgen]` macro** and no `wasm-pack`. The flow is
*compile → run `wasm-lite` over the `.wasm` → import the generated glue*. The
codegen reads custom sections the macros wrote and emits the import object plus
one wrapper per export.

### Import a JS function into Rust

**wasm-bindgen**
```rust
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Math)]
    fn random() -> f64;
    #[wasm_bindgen(js_namespace = Math, js_name = max)]
    fn max2(a: f64, b: f64) -> f64;
}
```

**wasm_lite** — see [`examples/hello-rust/src/main.rs`](./examples/hello-rust/src/main.rs)
```rust
wasm_lite::import! {
    "Math" {
        fn random() -> f64;
        fn max2(a: f64, b: f64) -> f64 as "max";   // `as` decouples the JS name
    }
}
```
Imports are grouped by JS namespace (the string). `as "name"` plays the role of
`js_name` (and lets you bind one JS function under several Rust names — handy for
overloads like `Math.max`). Each binding gets a unique wasm import symbol derived
from `module_path!()`, so the same JS function can be bound from many modules
without link conflicts.

### Call a method on a JS object / work with handles

**wasm-bindgen** — `this` is implicit on `js-sys`/`web-sys` types, or:
```rust
#[wasm_bindgen(method)]
fn push(this: &Array, value: f64) -> f64;
```

**wasm_lite** — a first `this: &JsValue` parameter marks a method; see
[`examples/hello-rust/src/main.rs`](./examples/hello-rust/src/main.rs)
```rust
wasm_lite::import! {
    "JSON"  { fn parse(text: &str) -> JsValue; }            // returns an object handle
    "Array" { fn push(this: &JsValue, value: f64) -> f64; } // method on that handle
}
```

### Typed object wrappers (`js-sys`-style)

**wasm-bindgen** gives you `js_sys::Array`, `js_sys::Object`, etc. for free.

**wasm_lite** has no `js-sys` yet, so you declare the wrapper with `js_class!` —
a newtype over `JsValue` whose methods lower to `receiver[name](args)`. See
[`examples/hello-rust/src/main.rs`](./examples/hello-rust/src/main.rs)
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
This is also the forward path: `wasm_lite_js`/`wasm_lite_web` will be built out of
`js_class!`, so anything you hand-bind today is the same shape the future crates
will ship.

### Export a Rust function to JS

**wasm-bindgen**
```rust
#[wasm_bindgen]
pub fn greet(name: &str) -> String { format!("hello, {name}!") }
```

**wasm_lite** — see [`examples/exports-demo/src/lib.rs`](./examples/exports-demo/src/lib.rs)
```rust
#[wasm_lite::export]
pub fn greet(name: &str) -> String { format!("hello, {name}!") }
// JS: import { greet } from "./glue.js"; greet("world")
```
Marshalling is symmetric with imports: numbers/`bool`, `&str`/`String`,
`&[u8]`/`Vec<u8>`, and `JsValue` handles all cross both directions.

### Fallible / optional values

**wasm-bindgen** maps `Result<T, JsValue>` to a thrown JS exception and
`Option<T>` to `null`/`undefined`.

**wasm_lite** does the same, as **return** types; see
[`examples/exports-demo/src/lib.rs`](./examples/exports-demo/src/lib.rs)
```rust
#[wasm_lite::export]
pub fn checked_sqrt(x: f64) -> Option<f64> {           // None -> JS null
    if x >= 0.0 { Some(x.sqrt()) } else { None }
}
#[wasm_lite::export]
pub fn divide(a: f64, b: f64) -> Result<f64, String> { // Err -> JS throw
    if b == 0.0 { Err("division by zero".into()) } else { Ok(a / b) }
}
#[wasm_lite::export]
pub fn greet_opt(name: Option<&str>) -> String {       // Option also works as an *arg*
    match name { Some(n) => format!("hi, {n}!"), None => "hi, anonymous!".into() }
}
```
On imports, `Result<T, JsValue>` turns a thrown JS exception into `Err`. Note:
`Result` is **return-only** (JS has no `Result` to pass *in* — matching
wasm-bindgen); `Option` works in both directions. Deeply nested generics are
limited (single-level `Option<Vec<u8>>`/`Result<…>` work; `Option<Result<…>>`
does not).

### The entry point

**wasm-bindgen** uses `#[wasm_bindgen(start)]` for init-on-load.

**wasm_lite**: a `bin`'s `main()` runs on instantiation (the LLD `start`
function also sets up TLS / passive data segments). Libraries just expose
`#[export]` functions; there is no separate `start` attribute.

### Testing

**wasm-bindgen**
```rust
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);
#[wasm_bindgen_test]
fn it_works() { assert_eq!(2 + 2, 4); }
```

**wasm_lite**, normal unit-test layout — see
[`examples/tests-demo/src/lib.rs`](./examples/tests-demo/src/lib.rs)
```rust
#[cfg(test)]
mod tests {
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_lite::wasm_lite_test)]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
```

For a standalone wasm integration-test target, use `harness = false` and
`test_main!()` — see [`examples/tests-demo/tests/suite.rs`](./examples/tests-demo/tests/suite.rs):
```rust
use wasm_lite::wasm_lite_test;
#[wasm_lite_test]
fn passes() { assert_eq!(2 + 2, 4); }

#[wasm_lite_test(worker)]            // run on a dedicated Web Worker so blocking
fn blocking_ok() {                   // APIs (lock_block, recv_block, park, join)
    /* … synchronous blocking code … */ // don't trap on the main thread
}
wasm_lite::test_main!();             // once per test binary
```
Wire the runner up once in `.cargo/config.toml`:
```toml
[target.wasm32-unknown-unknown]
runner = "path/to/runner"
```
Then `cargo test` runs headless in a browser; `cargo run --example foo` serves it
interactively (the runner distinguishes them by path). **Doctests run too** —
see [`examples/doctest-demo/src/lib.rs`](./examples/doctest-demo/src/lib.rs); call
`wasm_lite::set_panic_hook()` at the top of a doctest so a failure reports its
panic message.

### Threads

There is no threading in wasm-bindgen proper; the usual answer is `wasm_thread`
or `wasm-bindgen-rayon`, both of which depend on wasm-bindgen + js-sys.

**wasm_lite (core, detached):** see [`examples/threads-demo/src/main.rs`](./examples/threads-demo/src/main.rs)
```rust
use wasm_lite::thread;
thread::spawn(move || {
    SUM.fetch_add(i + 1, Ordering::SeqCst);   // coordinate via core::sync::atomic
});
```

**wasm_lite_std (`std::thread`-shaped, with `JoinHandle`):** see
[`examples/std-threads-demo/src/main.rs`](./examples/std-threads-demo/src/main.rs)
```rust
let handle = wasm_lite_std::spawn(move || i * 10);   // -> JoinHandle<i32>
```
`wasm_lite_std` mirrors `std`: `spawn`/`JoinHandle`, `park`/`unpark`,
`Mutex`/`RwLock`/`Condvar`/`mpsc`, `thread_local!`, plus
`wasm_lite_std::time` (`Instant`/`SystemTime`). Lock and channel methods come in
`_spin` / `_block` / `_sync` / `_async` flavours (plus `_timeout`) so you pick the
right waiting strategy for the thread you're on — see [§4](#blocking-traps-on-the-main-thread).

### Async (cross-thread coordination)

**wasm-bindgen** uses `wasm-bindgen-futures` mainly to *await JS Promises*
(`JsFuture::from(promise).await`).

**wasm_lite** has a different goal: a `spawn_local` event-loop executor for
non-blocking coordination *between threads* (it sleeps on `Atomics.waitAsync`,
not a busy loop). See [`examples/async-demo/src/main.rs`](./examples/async-demo/src/main.rs)
```rust
let handles: Vec<_> = (1..=3).map(|i| wasm_lite_std::spawn(move || i * 10)).collect();
wasm_lite_std::spawn_local(async move {
    for h in handles {
        let r = h.join_async().await.expect("worker panicked");  // non-blocking on main
        /* … */
    }
});
```
and the cross-thread `Mutex` in
[`examples/async-mutex-demo/src/main.rs`](./examples/async-mutex-demo/src/main.rs)
(`lock_sync()` on a worker, `lock_async().await` on main). It does **not** give
you `fetch(...).await` — see the gotchas.

### `std::time`

Replace `web-time` with `wasm_lite_std::time` — same `Instant`/`SystemTime`/
`Duration` API, no wasm-bindgen/js-sys dependency. On native it re-exports the
real `std::time`.

### Interop: keep a wasm-bindgen crate in the mix

If you depend on a crate that uses wasm-bindgen, enable the `wasm-bindgen`
feature. The codegen runs the (version-matched) wasm-bindgen CLI, merges its
loader with the wasm_lite glue, and gives you explicit conversions. See
[`examples/interop/src/main.rs`](./examples/interop/src/main.rs)
```rust
use wasm_lite::interop::ToWasmLite;
let wb_value = wb_widget::make_message();   // wasm_bindgen::JsValue
let wl_value = wb_value.to_wasm_lite();      // -> wasm_lite::JsValue
wasm_lite::console::log_value(&wl_value);
```
This is the recommended **incremental** migration path: move modules over one at
a time while leaving wasm-bindgen-dependent code in place.

**Direction matters.** This works because `wasm-lite` is the *outer* tool. The
reverse — you migrate a leaf crate to wasm_lite but your downstream consumers keep
a `wasm-bindgen`/`wasm-pack` pipeline — is **not** supported today: their
toolchain never runs the wasm_lite codegen pass, so the leaf's imports go
unsatisfied and the module won't instantiate. Your options are to have the app
make `wasm-lite` its final codegen step (its `#[wasm_bindgen]` code keeps working)
or ship the leaf dual-backend. See
[docs/interop.md](./docs/interop.md#direction-matters-wasm-lite-is-the-outer-tool)
and the [roadmap](./docs/roadmap.md).

---

## 3. Cargo / build configuration cheat-sheet

### Non-threaded build (stable)
```toml
# .cargo/config.toml
[build]
target = "wasm32-unknown-unknown"
[target.wasm32-unknown-unknown]
runner = "path/to/runner"   # only if you want `cargo test`/`cargo run` in-browser
```

### Threaded / atomics build (nightly)
Copied from [`examples/threads-demo/.cargo/config.toml`](./examples/threads-demo/.cargo/config.toml):
```toml
[build]
target = "wasm32-unknown-unknown"
rustflags = [
    "-C", "target-feature=+atomics,+bulk-memory,+mutable-globals",
    "-C", "link-arg=--shared-memory",
    "-C", "link-arg=--max-memory=1073741824",
    "-C", "link-arg=--import-memory",
    # thread symbols the worker bootstrap needs:
    "-C", "link-arg=--export=__stack_pointer",
    "-C", "link-arg=--export=__tls_base",
    "-C", "link-arg=--export=__tls_size",
    "-C", "link-arg=--export=__tls_align",
    "-C", "link-arg=--export=__wasm_init_tls",
]
[unstable]
build-std = ["std", "panic_abort"]
```
Build with `cargo +nightly run`. `--import-memory` makes JS own the single
`WebAssembly.Memory`; the codegen emits `makeMemory()` + `instantiate(url,
memory?)` so every worker shares it. **Async doctests** additionally need the same
link args repeated under `[build] rustdocflags` (rustdoc links doctests with
`rustdocflags`, not `rustflags`) — see
[`examples/async-doctest-demo`](./examples/async-doctest-demo).

### Cross-origin isolation
`SharedArrayBuffer` requires a cross-origin-isolated page. The runner already
serves `Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`. **In production you must set these
headers yourself**, or threaded modules won't get shared memory.

---

## 4. Design trade-offs and gotchas

### The ABI is deliberately narrow
Everything crosses the boundary as one of: a number/`bool`, a string, a byte
slice, or an opaque `JsValue` handle (a `u32` index into a host-side value
table). Strings/bytes are passed by allocating in wasm memory (`__wl_malloc`,
align 1) and handing over a packed `(ptr<<32 | len)` `i64`; ownership transfers to
whichever side allocated last. There is **no automatic struct/enum/tuple
marshalling and no serde bridge** — if you need to move a rich value, serialize it
yourself (e.g. `serde_json` to a `String`, or build a JS object via `js_class!`).
The import/export asymmetry for objects is intentional: an import *lends* Rust's
handle (`&JsValue`), an export *takes* ownership from JS (`JsValue` by value).

### `JsValue` is `!Send`/`!Sync` — and that's load-bearing
A handle indexes a *per-realm* value table, so it is only meaningful on the
worker that created it. The type system forbids sending one across threads. This
matches wasm-bindgen's `JsValue` (also `!Send`/`!Sync`), but it interacts with the
threading layer: you cannot capture a `JsValue` into a `thread::spawn` closure.
Move data across threads as plain Rust values (atomics, channels, `Mutex<T>`),
and re-acquire JS handles on the thread that needs them.

### You cannot `await` a JS Promise (yet)
This is the single biggest behavioral difference from wasm-bindgen. wasm_lite's
async executor exists to coordinate **threads** (`join_async`, `lock_async`),
*not* to await host async APIs. There is no `JsFuture`/Promise→Future bridge in
the ABI. A thrown JS exception maps to `Err` on a `Result`-returning import, but a
**resolved Promise does not map to anything** — so `fetch(url).await` is not
expressible today. Workarounds: do the async on the JS side and call a Rust
`#[export]` with the result, or hand-roll polling. If your app is built around
`fetch`/`IndexedDB`/streaming, stay on wasm-bindgen for that part (and consider
the interop feature for the rest).

### No closures-into-JS / callbacks
wasm-bindgen's `Closure<dyn FnMut(...)>` lets JS call back into Rust (event
listeners, `setTimeout`, Promise `.then`). wasm_lite's `import!` only accepts
scalar/string/bytes/handle arguments — **not** function pointers. Structure
callbacks around `#[export]`ed functions that JS invokes, or around polled shared
state. (An async `setTimeout`-backed timer is on the roadmap, which will give a
main-thread `sleep_async`, but it is not a general callback mechanism.)

### Blocking traps on the main thread
The browser main thread cannot run `Atomics.wait`, so any **blocking** primitive
(`lock_block`, `recv_block`, `park`, synchronous `JoinHandle::join`) *traps* there.
This is why `wasm_lite_std` exposes `_spin` / `_block` / `_sync` / `_async`
variants:
- On the **main thread**, use the `_async` variants (`lock_async().await`,
  `join_async().await`) or `_spin` for very short critical sections.
- On a **worker**, blocking (`_sync`/`_block`) is fine.
- `wasm_lite_std::is_main_thread()` tells you which you're on.
- To unit-test a blocking path, use `#[wasm_lite_test(worker)]` so it runs off the
  main thread.

### Panics are per-thread traps under `panic = "abort"`
`panic = "abort"` is the supported model. On wasm a panic is an `unreachable`
trap **local to one instance**: a panicking worker takes down only itself; the
main thread, sibling workers, and shared memory survive (unlike native, where
`abort()` kills the whole process). The residual surprise is a **lock the dead
thread held** — with no unwind there's no `Drop` and no poison, so it stays
locked. That's why the `*_timeout` lock/recv APIs exist: a dead holder surfaces as
a timeout, not a hang. A *detached* worker's panic is logged (with thread
attribution) but doesn't fail a test, matching `std`; an **awaited** panic
propagates to `join_async().await` as `Err(message)`, and re-panics on unwrap.

### Async failures must be fail-closed in tests
Unlike libtest/rustdoc, where `main` returning *is* the verdict, a deferred async
failure can't be seen that way. Wrap async test bodies in
`wasm_lite_std::async_doctest!(async { … })` (usable in doctests,
`#[wasm_lite_test]` bodies, and `main`). The *only* thing that records success is
the body reaching its end; a panic, dropped task, or deadlock cannot masquerade as
a pass. See [`examples/async-doctest-demo`](./examples/async-doctest-demo) and the
pass/fail pair [`examples/async-pass-demo`](./examples/async-pass-demo) /
[`examples/async-fail-demo`](./examples/async-fail-demo).

### Worker teardown drains async tasks
A wasm-bindgen worker `close()`s the moment its entry returns, silently abandoning
any `spawn_local`'d task (and freeing the TLS its queue lives in). wasm_lite's
worker bootstrap instead polls `__wl_executor_idle()` and only frees TLS/stack +
`close()`s once the executor has drained — so `spawn_local` is correct on *any*
thread, not just main (see
[`examples/worker-spawn-local-demo`](./examples/worker-spawn-local-demo)). Residual
hazard: a worker task that *never completes* keeps the worker alive forever. The
intended fix is **cooperative cancellation** (a token tasks check), not a hard
`terminate()` that would strand held locks — it's designed but not yet built.

### Custom panic hooks must be installed before the first spawn
`wasm_lite_std` installs one canonical panic hook (once, on first spawn) that logs
each panic with thread attribution and routes it to the join channel. If you want
your own hook (`set_panic_hook`), install it **before** the first `spawn`.

### Nightly is required for threads
`+atomics` means `std` must be recompiled with it, which needs nightly +
`-Z build-std`. The non-threaded path runs on stable. This is a wasm/LLVM reality,
not a wasm_lite choice, but plan your CI accordingly.

### No `getrandom` / weak `HashMap` seed
On `wasm32-unknown-unknown`, `HashMap` falls back to a weak fixed seed and the
`getrandom`/`rand`/`uuid` ecosystem needs `getrandom`'s `js` feature — which pulls
in wasm-bindgen. A wasm-bindgen-free `crypto.getRandomValues` backend is on the
roadmap; until then, audit your dependency tree for randomness.

### No TypeScript types, no bundler integration
The codegen emits a single plain-JS ES module (`glue.js`) plus, for threaded
builds, a `wl_worker.js` bootstrap. There's no `.d.ts` generation and no
`wasm-pack`-style `--target bundler|web|nodejs` matrix — you `import` the glue
directly. The runner is browser-only today (no Node test path).

---

## 5. A suggested migration order

1. **Start hybrid.** Turn on the `wasm-bindgen` feature and move *leaf* modules
   (pure compute, no DOM/Promise) to `import!`/`#[export]` first, bridging values
   with `.to_wasm_lite()` / `.to_wasm_bindgen()` at the seams.
2. **Swap the test harness.** Replace `wasm_bindgen_test` with `#[wasm_lite_test]`
   and wire the runner into `.cargo/config.toml`. You get doctests-in-browser for
   free.
3. **Replace `web-time`** with `wasm_lite_std::time` and any ad-hoc `js-sys` time
   calls.
4. **Move threading** off `wasm_thread`/`wasm-bindgen-rayon` onto
   `wasm_lite::thread` / `wasm_lite_std` (this is where the dependency-graph win is
   largest). Mind the main-thread blocking rule.
5. **Hand-bind the host APIs you actually use** with `import!`/`js_class!`,
   keeping wasm-bindgen only for the parts that need Promises/closures/`web-sys`
   until `wasm_lite_js`/`wasm_lite_web` land.

See the [roadmap](./docs/roadmap.md) for the order things are expected to fill in.
