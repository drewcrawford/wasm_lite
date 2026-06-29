# Crate layering & roadmap

*(Part of the [wasm_lite](../README.md) docs. See also: [binding model](./binding-model.md),
[testing](./testing.md), [threads & async](./threads-and-async.md),
[interop](./interop.md), [migration guide](../MIGRATION.md).)*

## Planned crate layering

Following the wasm-bindgen ecosystem split (language vs browser):

* `wasm_lite` — core (above). *Like `wasm-bindgen`.*
* `wasm_lite_std` — **done**: std-like veneer over `wasm_lite`, a port of
  [`wasm_safe_thread`](https://crates.io/crates/wasm_safe_thread) with its wasm
  backend retargeted off wasm-bindgen onto `wasm_lite::thread::spawn` +
  `core::arch::wasm32` atomics. Both **sync** and **async** paths work:
  `spawn`/`JoinHandle` (`join`/`join_async`), `park`/`unpark`, `Mutex`/`Condvar`/
  `RwLock`/`mpsc` (sync + async), and a `spawn_local` event-loop executor for
  non-blocking async on the main thread. Runtime deps are `wasm_lite` and a
  small internal `atomic-waker`-backed async wait primitive. Browser-validated (see
  [testing](./testing.md)). *Like `std` (the `std::thread`/`std::sync` slice).*
* `wasm_lite_js` *(future)* — ECMAScript built-ins (`Object`, `Array`, `Map`,
  `JSON`, `Date`, …) bound with `js_class!`. *Like `js-sys`.*
* `wasm_lite_web` *(future)* — Web/host APIs (DOM, `fetch`, …). *Like `web-sys`.*

Bindings stay out of core so it remains small; `js_class!` is the primitive all
upper layers build on (so its constructors + property get/set are the gate for
`wasm_lite_js`/`wasm_lite_web`).

## Known gaps / roadmap

Roughly in priority order. The threading/async/testing layer is complete and
browser-validated; the next frontier is the binding crates. Items marked
*designed* have a worked-out plan but no implementation yet.

### Async & runtime

* **Cooperative cancellation** — graceful shutdown for a worker whose `spawn_local`
  task never completes (drain-before-teardown is done, but such a worker lingers
  forever). Plan: a `CancelToken` plus a `run_until_cancelled(token, fut)`
  combinator, reusing the executor wake path. Library-only. *Designed.* The
  `*_timeout` APIs give a crude poll-based version today.
* **Async timer / `sleep_async`** — a `setTimeout`-backed timer giving an
  `async sleep` usable on the main thread (`thread::sleep` uses `Atomics.wait`,
  which traps there). Also replaces the current `*_timeout` implementation, which
  spawns a whole Web Worker per timeout.
* **No-atomics `queueMicrotask` executor** — today `spawn_local` is built only on
  the shared-memory path, so single-threaded async still needs a `+atomics` build
  (nightly + `-Z build-std` + COOP/COEP). Plan: a `queueMicrotask`-backed
  `spawn_local` selected when the module is not a shared-memory build, so
  single-threaded async needs no `SharedArrayBuffer`. The two coexist behind one
  `spawn_local` surface.
* **Promise interop (`await` a JS `Promise` from Rust)** — the analogue of
  `wasm-bindgen-futures`' `JsFuture`. There is no ABI path turning a resolved JS
  `Promise` into a Rust `Future`, so host async APIs like `fetch(url).await` are
  not expressible. This is the prerequisite for any `fetch`/streaming/`async` host
  API in `wasm_lite_web`, and the single biggest behavioral gap vs wasm-bindgen
  (see the [migration guide](../MIGRATION.md)). *Designed.*

### Bindings & marshalling

* **Closures into JS (`Closure<dyn FnMut(...)>` analogue)** — let JS call back into
  Rust (event listeners, `setTimeout`, Promise `.then`). Today `import!` accepts
  only scalar/string/bytes/handle arguments, not function pointers. Needs a new
  codegen shim kind. With Promise interop, this unblocks idiomatic event-driven
  `wasm_lite_web` APIs.
* **`js_class!` constructors (`new Foo()`) + property get/set** (`el.textContent`),
  plus owned-object args and `instanceof`-checked downcasting — each a new codegen
  shim kind. The prerequisite for `wasm_lite_js`/`wasm_lite_web`.
* **`wasm_lite_js` / `wasm_lite_web`** — the binding crates (ECMAScript built-ins,
  then DOM/host APIs), gated on the `js_class!` work above.
* **Entropy (`crypto.getRandomValues`)** — a wasm-bindgen-free `getrandom` backend.
  Today the `getrandom`/`rand`/`uuid` ecosystem needs `getrandom`'s `js` feature,
  which pulls in wasm-bindgen.
* **Richer type marshalling (a `serde-wasm-bindgen` analogue)** — a `serde`
  `Serializer`/`Deserializer` pair so `#[derive(Serialize, Deserialize)]` types
  cross the boundary directly, instead of hand-encoding (e.g. JSON through a
  `&str`). Gated on the `js_class!` get/set work.

### Threading parity

* **More `std::sync` / `std::thread` parity** — `Once`/`OnceLock`/`LazyLock`
  (worker-safe, async-aware init), `Barrier`, and `thread::scope` (scoped threads).
  Library-only additions to the `wasm_lite_std` veneer. (Browser-shaped `std` APIs
  — `std::net`/`std::fs`/`std::env` — belong to `wasm_lite_web`, not a `std`
  drop-in.)
* **Worker pool** — one Web Worker is created per `spawn` today; a persistent pool
  would cut spawn cost and enable a synchronous `block_on` against pre-warmed
  workers. Pairs with cooperative cancellation for teardown.

### wasm-bindgen coexistence

* **Running wasm_lite and wasm-bindgen in one binary** — the hardest open problem,
  driven by `wgpu` (whose web backend can't leave wasm-bindgen in the near term).
  Several strategies are under design — reverse interop, subordinating wgpu's glue,
  and `[patch]`-based shims in both directions. These are detailed separately in
  the [design notes](./design-notes.md).

### Tooling & tests

* **Broaden the wasm test suite** — `crates/wasm_lite_std/tests/browser.rs` ports
  the bulk of the native unit suite (~46 tests across spin/block/sync/async +
  timeouts). Remaining: multi-reader `RwLock` and `park`/`unpark`.
* **Deployment niceties** — a `wasm-lite bundle` command, session pooling/idle
  reaper for the persistent browser, and test filtering (`cargo test NAME`).
* **Smaller items** — deeply nested generics on imports (`Option<Result<…>>` does
  not work yet, though single-level `Option<Vec<u8>>`/`Result<…>` do), and a
  `panic = "unwind"` mode (catch-unwind per poll, drop just the failed task, poison
  its locks — vs `abort`'s per-thread trap).
