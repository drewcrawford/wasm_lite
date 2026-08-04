# Crate layering & roadmap

*(Part of the [wasm_lite](../README.md) docs. See also: [binding model](./binding-model.md),
[testing](./testing.md), [threads & async](./threads-and-async.md),
[interop](./interop.md), [migration guide](../MIGRATION.md).)*

## Goals that shape the roadmap

The roadmap is not "rebuild all of
[wasm-bindgen](https://wasm-bindgen.github.io/wasm-bindgen/)." It is guided by a
smaller set of browser-first goals:

* **Modern browsers are the primary backend.** wasm_lite intentionally emits a
  modern ES-module browser loader and serves it through the runner. It does not
  currently generate Node CommonJS, IE-era script loading, legacy no-module
  scripts, or a matrix of bundler-specific outputs. wasm-bindgen's broader
  target matrix is valuable, but it creates target-specific behavior: its
  [CLI](https://wasm-bindgen.github.io/wasm-bindgen/reference/cli.html) has
  `bundler`, `web`, `nodejs`, `no-modules`, Deno, and module variants; its docs
  call out [JS snippet](https://wasm-bindgen.github.io/wasm-bindgen/reference/js-snippets.html)
  support and [threaded wasm](https://wasm-bindgen.github.io/wasm-bindgen/examples/raytrace.html)
  caveats that vary by target. wasm_lite spends that complexity budget on the
  browser path instead. The concrete payoff is one ES-module loader, module
  workers, consistent COOP/COEP serving, and one implementation of test/log/panic
  capture rather than a compatibility layer for every old script model.
* **Atomics and threads are first-class, not an example-only afterthought.** The
  existing implementation already reads imported shared memory, creates the
  shared `WebAssembly.Memory`, emits `wl_worker.js`, starts module workers, and
  serves COOP/COEP headers. Roadmap items like a worker pool, cooperative
  cancellation, `sleep_async`, and broader `std::sync` parity are extensions of
  that same path.
* **Std-like APIs should exist where the browser can support them.** `Mutex`,
  `RwLock`, `Condvar`, `mpsc`, `JoinHandle`, `Instant`, and `SystemTime` are not
  just demos; they are the compatibility layer many Rust crates want. Roadmap
  items such as `Once`, `Barrier`, scoped threads, and a no-atomics
  `queueMicrotask` executor continue that theme.
* **Testing and logging are product features.** The runner already detects
  rustdoc doctest artifacts, runs wasm tests in real browsers, captures console
  output, and surfaces main-thread and worker panics to the CLI. Roadmap items
  such as test filtering, browser session pooling, and deployment bundling are
  runner work because one server/test runner owns the local feedback loop.
* **Interop is a migration tool, not the destination.** The supported direction
  is currently "wasm-lite is the final codegen step." Reverse interop and shim
  strategies exist because real applications may need wgpu/web-sys today, but
  the long-term browser-first design still wants wasm_lite's runner, atomics,
  logging, and test semantics to remain coherent.

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
  **`fetch` landed first, in core** (`wasm_lite::fetch`), because a consumer
  needed it before the split was worth doing. It moves here when the crate
  exists; nothing about its API depends on where it lives.

Bindings stay out of core so it remains small; `js_class!` is the primitive all
upper layers build on (so its constructors + property get/set are the gate for
`wasm_lite_js`/`wasm_lite_web`).

### What belongs in `wasm_lite_std` — the absorption rule

`wasm_lite_std` grew by absorbing crates (`wasm_safe_thread`, `wasm_safe_mutex`),
and there is standing pressure to absorb more. The test that has held up:

> **Absorb a crate when it is `std`-shaped *and* its native implementation is a
> thin veneer over `std`.**

Both halves matter. The first says the API has a `std` counterpart to be judged
against; the second says the crate exists *because* `std` does not work on wasm —
which is exactly `wasm_lite_std`'s remit — rather than because it is solving a
problem `std` never addressed.

| candidate | `std`-shaped? | native impl a `std` veneer? | verdict |
|---|---|---|---|
| `wasm_safe_thread`, `wasm_safe_mutex` | yes (`std::thread`, `std::sync`) | yes | **absorbed** |
| [`async_file`](https://crates.io/crates/async_file) | yes (`std::fs`) | yes — wraps `std::fs` | **qualifies** |
| [`send_cells`](https://crates.io/crates/send_cells) | **no** — `std` has no `SendCell` | n/a | **no** |

`send_cells` is the instructive rejection. It is tempting, because
`wasm_lite_std` wants a `Send` wrapper for `!Send` futures (`JsValue` is `!Send`
by construction) and `send_cells` has one. But its thread-affinity problem is not
a wasm problem: `app_window` uses `SendCell<HWND>` on Win32. Absorbing it would
make Windows code depend on a wasm crate.

Two corollaries worth stating, because both came up while deciding this:

* **Reimplementing a two-line primitive beats depending upward.** Taking a
  dependency to avoid duplicating `unsafe impl<T> Send for Cell<T> {}` costs a
  feature split, publish-order coupling and a dev-cycle, to save something with no
  behaviour to drift.
* **Watch the arrow.** `send_cells` already depends on `wasm_lite_std`. A crate
  that depends on you cannot become your dependency; if you need what it has,
  own the primitive and let it consume yours later.

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
* ~~**Promise interop (`await` a JS `Promise` from Rust)**~~ — **done**
  ([`JsFuture`](../crates/wasm_lite/src/future.rs)), and now used in anger:
  `wasm_lite::fetch` is built on it, streaming response bodies included.

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
  then DOM/host APIs), gated on the `js_class!` work above. Note `wasm_lite::fetch`
  did **not** need it: `import!`'s `#[constructor]`/`#[getter]`/`#[setter]` cover
  a real binding surface today, with a hand-written newtype per class. What
  `js_class!` would remove is the boilerplate, not a capability.
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
  the bulk of the native unit suite (52 tests across spin/block/sync/async +
  timeouts), and `scripts/wasm32/tests` runs it in **both** Firefox and Chrome,
  because the two disagree on nested worker spawn. Remaining: multi-reader
  `RwLock` and `park`/`unpark`.
* **Deployment niceties** — a `wasm-lite bundle` command, and session
  pooling/idle reaper for the persistent browser. (Test filtering already works:
  `cargo test NAME`, `--exact` and `--list` all follow libtest.)
* **Smaller items** — deeply nested generics on imports (`Option<Result<…>>` does
  not work yet, though single-level `Option<Vec<u8>>`/`Result<…>` do), and a
  `panic = "unwind"` mode (catch-unwind per poll, drop just the failed task, poison
  its locks — vs `abort`'s per-thread trap).
