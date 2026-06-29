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
  non-blocking async on the main thread. Only runtime dep: `continue` (a
  wasm-bindgen-free continuation primitive). Browser-validated (see
  [testing](./testing.md)). *Like `std` (the `std::thread`/`std::sync` slice).*
* `wasm_lite_js` *(future)* — ECMAScript built-ins (`Object`, `Array`, `Map`,
  `JSON`, `Date`, …) bound with `js_class!`. *Like `js-sys`.*
* `wasm_lite_web` *(future)* — Web/host APIs (DOM, `fetch`, …). *Like `web-sys`.*

Bindings stay out of core so it remains small; `js_class!` is the primitive all
upper layers build on (so its constructors + property get/set are the gate for
`wasm_lite_js`/`wasm_lite_web`).

## Known gaps / roadmap

Roughly in priority order — the threading/async/testing layer is complete and
browser-validated; the next frontier is the binding crates.

* **Cooperative cancellation** for graceful worker shutdown. Drain-before-teardown
  is done, but a worker whose task *never completes* lingers forever. The plan: a
  `CancelToken` (shared atomic + `continue` waiters; `cancel`/`is_cancelled`/
  `cancelled()`) plus a `run_until_cancelled(token, fut) -> Option<T>` combinator,
  so a cancelled task exits cleanly and the worker drains + closes. It's
  library-only — the cross-thread wake reuses the executor path (no runtime/glue
  changes). *Designed, not built.* (`*_timeout` APIs already give a crude
  poll-based version today.)
* **Async timer / `sleep_async`** — a `setTimeout`-backed timer (a
  `__wl_set_timeout`-style runtime import resolving a `continue` waiter on the
  event loop). Gives an `async sleep` usable on the main thread (`thread::sleep`
  uses `Atomics.wait`, which traps there) and, more importantly, replaces the
  current `*_timeout` implementation — which spawns a **whole Web Worker per
  timeout** just to sleep to the deadline — with a cheap event-loop timer.
  *Highest-leverage `std`-coverage item: it also improves existing internals.*
* **No-atomics `queueMicrotask` executor** — today `spawn_local` is built only on
  the shared-memory path: it sleeps on `Atomics.waitAsync` (`__wl_wait_async`) and
  is woken by `memory.atomic.notify`
  (`crates/wasm_lite_std/src/wasm/executor.rs`). That requires a `+atomics`
  shared-memory build (nightly + `-Z build-std` + COOP/COEP) **even for purely
  single-threaded async**. By contrast `wasm-bindgen-futures::spawn_local` is a
  plain `queueMicrotask`/Promise loop that runs on **stable, non-atomic** wasm.
  Any crate that today uses `wasm-bindgen-futures` for main-thread async (e.g.
  `some_executor`, `test_executors`) therefore can't migrate onto `wasm_lite_std`
  without forcing atomics on every downstream consumer — a regression for any
  consumer that ships a **non-atomic, single-threaded** build (the default for most
  wgpu-on-web apps, which take wgpu's `fragile-send-sync-non-atomic-wasm` path).
  (An *atomics* consumer like `images_and_words` is unaffected — it can use today's
  shared-memory `spawn_local` as-is.) The plan: a `queueMicrotask`-backed `spawn_local`
  (a `__wl_schedule`-style runtime import with a non-atomic fallback) selected when
  the module is **not** a shared-memory build, so single-threaded async needs no
  `SharedArrayBuffer`. The atomics path's reason to exist is *cross-realm* wakes
  (a worker waking the main realm) — a main-realm-only executor doesn't need it, so
  the two can coexist behind one `spawn_local` surface. *Unlocks the executor/logging
  subtree migration and is a prerequisite for non-atomic mixed binaries.*
* **Promise interop (`await` a JS `Promise` from Rust)** — the analogue of
  `wasm-bindgen-futures`' `JsFuture`. Today wasm_lite's async executor only
  coordinates *threads* (`join_async`, `lock_async`); there is no ABI path that
  turns a resolved JS `Promise` into a Rust `Future`, so host async APIs like
  `fetch(url).await` are not expressible. The plan: a runtime import that attaches
  `.then`/`.catch` to a `Promise` and resolves a `continue` waiter on the owning
  realm's event loop (reusing the `spawn_local` wake path), exposed as a
  `JsFuture`-style `impl Future<Output = Result<JsValue, JsValue>>`. This is the
  **prerequisite for any `fetch`/streaming/`async` host API** in `wasm_lite_web`,
  and the single biggest behavioral gap vs wasm-bindgen called out in the
  [migration guide](../MIGRATION.md). Pairs naturally with the async-timer import
  above (same event-loop resolution machinery). *Designed at the sketch level,
  not built.*
* **Closures into JS (`Closure<dyn FnMut(...)>` analogue)** — let JS call *back*
  into Rust (event listeners, `setTimeout`, Promise `.then`). Today `import!`
  accepts only scalar/string/bytes/handle arguments, **not** function pointers, so
  callbacks have to be structured around `#[export]`ed functions or polled shared
  state. The plan: a codegen shim kind that boxes a Rust closure, hands JS a
  callable handle backed by a generated `__wl_trampoline` export, and frees the box
  on drop (`Closure::forget`-style leak opt-out for `'static` listeners). Together
  with **Promise interop** above this is what unblocks idiomatic event-driven
  `wasm_lite_web` APIs (`addEventListener`, `requestAnimationFrame`, …). *Needs a
  new shim kind; not built.*
* **Entropy (`crypto.getRandomValues`)** — a wasm-bindgen-free randomness source
  bound via `import!`, exposed as a `getrandom` backend. On
  `wasm32-unknown-unknown` `HashMap` falls back to a weak fixed seed, and the
  `getrandom`/`rand`/`uuid` ecosystem otherwise needs `getrandom`'s `js` feature
  (which pulls in wasm-bindgen). Unblocks a large slice of crates while staying
  on-mission.
* **More `std::sync` / `std::thread` parity** — `Once`/`OnceLock`/`LazyLock`
  (worker-safe, async-aware init; std's blocking init can trap on the main
  thread), `Barrier`, and `thread::scope` (scoped threads borrowing non-`'static`
  data — flagged missing in the `wasm_thread` comparison). Library-only additions
  to the `wasm_lite_std` veneer. (Browser-shaped `std` APIs — `std::net`,
  `std::fs`, `std::env` — belong to `wasm_lite_web` below, not a `std` drop-in.)
* **`js_class!` constructors (`new Foo()`) + property get/set** (`el.textContent`),
  plus owned-object args and `instanceof`-checked downcasting — each needs a new
  codegen shim kind. This is the **prerequisite for `wasm_lite_js`/`wasm_lite_web`**.
* **`wasm_lite_js` / `wasm_lite_web`** — the binding crates (ECMAScript built-ins,
  then DOM/host APIs), gated on the `js_class!` work above.
* **Reverse interop (a wasm_lite *leaf* under a wasm-bindgen-driven app)** —
  today's interop is one-directional: `wasm-lite` is always the *outer* tool (it
  runs the wasm-bindgen CLI internally and merges both glues). The opposite case —
  you migrate a leaf crate to `import!`/`#[export]`, but your downstream consumers
  keep a `wasm-bindgen`/`wasm-pack` pipeline — does **not** work, because their
  toolchain never runs the wasm_lite codegen pass that satisfies the leaf's
  imports, so the module fails to instantiate. Two candidate fixes: (a) a
  `wasm-lite patch` **post-pass** the app runs *after* wasm-bindgen (the inverse of
  `build_interop` — inject wasm_lite's import object into wasm-bindgen's loader),
  so the consumer adds one build command rather than swapping tools; or (b) a
  codegen mode that re-expresses wasm_lite descriptors as wasm-bindgen **schema**
  so the downstream CLI resolves them with no extra step. Until then the options
  are: have the app make `wasm-lite` its final codegen step (its `#[wasm_bindgen]`
  code keeps working), or ship the leaf **dual-backend** (feature-gate a
  wasm-bindgen binding surface alongside the wasm_lite one). *Designed at the
  sketch level, not built.*
* **Mixed wasm_lite + wgpu binaries (hard requirement)** — the concrete forcing
  case for reverse interop. [`wgpu`](https://crates.io/crates/wgpu)'s web backend
  is irreducibly wasm-bindgen/web-sys (WebGPU/WebGL/canvas) and **cannot** be
  migrated off it in the near term, so any app that renders with wgpu (e.g.
  `images_and_words`) stays a wasm-bindgen-driven build *permanently*. The goal:
  let such an app move its **non-graphics** crates (`continue`, `some_executor`,
  `test_executors`, `logwise`, its own glue) onto wasm_lite while wgpu stays on
  wasm-bindgen, in **one** binary. Key facts that make this tractable: wgpu does
  **no threading of its own** on wasm (the `Worker`/`OffscreenCanvas` references are
  about *running inside* a worker, not spawning one) — all threads in a wgpu app
  are the app's own. So the integration is: wasm_lite owns thread spawning +
  instantiation (it already does, via `__wl_spawn` + `wl_worker.js`), and wgpu's
  wasm-bindgen glue is **subordinated** — merged into the import object on the main
  thread and in the worker bootstrap, wired without re-running start — i.e. exactly
  the **threaded reverse-interop** design above, with wgpu as the subordinated
  glue. Because the flagship consumer is already an `+atomics`, shared-memory,
  threaded build, this — not the easy non-atomic post-pass — is the path that
  matters for wgpu. App-level invariant (unchanged from today): wgpu handles are
  `!Send` on atomics builds, so all wgpu calls stay on one thread (`send_cells`-style
  management). *This is the make-or-break integration; reverse interop's threaded
  variant exists to serve it.*
* **Port wgpu off wasm-bindgen (`wasm_lite_web`-on-WebGPU capstone)** — the
  "insane but ultimately required" option: re-express wgpu's web backend
  (`wgpu/src/backend/webgpu.rs` + the web-sys WebGPU/WebGL/canvas surface) as
  wasm_lite `import!`/`js_class!` bindings, eliminating wasm-bindgen from the binary
  entirely. **Not** required for *mixed* binaries (subordination above handles
  those) — this is for an all-wasm_lite world. It is gated on essentially every
  other binding feature at once: `js_class!` constructors + property get/set,
  closures-into-JS (wgpu uses `Closure` for device-lost / uncaptured-error /
  promise callbacks), and **Promise interop** (WebGPU is pervasively async —
  `requestAdapter`/`requestDevice`/`mapAsync`/… return promises). The one thing
  that makes it merely enormous rather than impossible: wgpu-web doesn't thread, so
  there's no worker bootstrap or atomics interplay to reimplement — it's a (very
  large) *binding* surface, the natural capstone proving out `wasm_lite_web`.
  *Long-horizon; sequence behind the binding-feature foundations above.*
* **Fake-wasm-bindgen shim via `[patch]` (the high-leverage endgame)** — instead of
  reconciling two binding worlds (reverse interop) or rewriting wgpu by hand (the
  capstone above), **replace wasm-bindgen itself**. Ship a crate that is
  API-compatible with `wasm-bindgen` but whose `#[wasm_bindgen]` macro lowers to
  wasm_lite's ABI + descriptor sections, and have the app drop it in graph-wide
  with `[patch.crates-io] wasm-bindgen = { … }`. Because **js-sys, web-sys,
  wasm-bindgen-futures, and wgpu are all written *against* wasm-bindgen**, the
  *unmodified* upstream crates then compile on our foundation and emit wasm_lite
  descriptors — so a single `wasm-lite` codegen pass produces glue for the whole
  module, WebGPU included. This **inverts** the problem: there is no second binding
  system to subordinate (no reverse-interop loader surgery, no dual worker
  bootstrap), threading is uniformly `wasm_lite_std` (patch `wasm_safe_thread` →
  `wasm_lite_std` too). The one piece this shim deliberately does **not** build is
  impersonation of wasm-bindgen's *own* threading (`wasm-bindgen-rayon`/`wasm_thread`/
  `web_sys::Worker` spawning): a [crates.io db-dump census](./wasm-thread-ownership-census.md)
  (2026-06-29) found only **~1% of the 5,063-crate wasm-bindgen ecosystem directly
  owns wasm threads** (49 crates, almost all `wasm-bindgen-rayon` ZK/compute; ~3.8%
  upper bound touching `Worker*` at all). So thread-owning wasm-bindgen crates are
  declared **out of scope** for this shim (they host on wasm-bindgen via the dual
  shim instead) rather than reimplemented — `wasm-bindgen-rayon` is the single lever
  if that tail is ever worth covering. It **subsumes** *port wgpu* and most of
  `wasm_lite_js`/`wasm_lite_web` — you impersonate js-sys/web-sys so the real ones
  run, fixing the entire ecosystem at once rather than crate-by-crate. Consumer
  cost: a `[patch]` block + swapping the final codegen tool to `wasm-lite`, with
  **zero source changes** anywhere (and it's opt-in + reversible, applied at the app
  workspace — a leaf can't impose it). Honest cost: the shim is wasm-bindgen's
  *codegen contract*, not just an API — it must reproduce the `#[wasm_bindgen]`
  attribute grammar web-sys/wgpu actually use (`getter`/`setter`, `method`/
  `structural`, `constructor`, `catch`→`Result`, `extends`, `variadic`, typed
  arrays/`Clamped`, optional args), the runtime/trait surface hand-written
  js-sys/web-sys touch (`JsValue`, `JsCast` with `instanceof` downcasting,
  `Closure`, `Function`, typed arrays, the `convert`/`describe` trait names
  generated code references), and `wasm-bindgen-futures::JsFuture`. So it does **not**
  dodge the hard primitives — closures-into-JS, Promise interop, `instanceof`
  downcasting, typed arrays, the full attribute grammar — it *amortizes* them across
  the whole wasm-bindgen ecosystem instead of one crate. Trade-off: you inherit
  web-sys's design rather than getting to design a cleaner one. *Complementary to
  reverse interop (which ships sooner and keeps real wasm-bindgen); this is the
  strategic endgame for an all-wasm_lite world that still uses wgpu.*
* **Fake-wasm_lite shim via `[patch]` (the *dual*, and the easy direction)** — the
  mirror image of the shim above, and probably the **best answer to the original
  leaf-migration problem**. A leaf is authored against `wasm_lite`; a downstream app
  that stays on wasm-bindgen (e.g. anything using wgpu — `images_and_words`)
  `[patch]`es `wasm_lite` / `wasm_lite_std` to shims **backed by wasm-bindgen**, so
  the leaf's `import!`/`#[export]`/`js_class!`/`wasm_lite_std::*` lower onto
  wasm-bindgen and the whole binary is an ordinary wasm-bindgen build — **the app's
  existing `wasm-pack` pipeline is unchanged, no `wasm-lite` codegen step, no
  reverse-interop loader surgery.** This is *much* easier than faking wasm-bindgen,
  for two reasons:
  * **wasm_lite's binding surface is a strict subset of wasm-bindgen's**, so the
    macro shim (`import!`→`#[wasm_bindgen] extern`, `#[export]`→`#[wasm_bindgen] pub
    fn`, `js_class!`→extern type, `JsValue`→`wasm_bindgen::JsValue`, `console`/
    `performance`/`date`→web-sys/js-sys) translates *down* with **no missing
    features** — unlike impersonating web-sys's huge internal trait surface.
  * **The threading half already exists.** A wasm-bindgen-backed `wasm_lite_std`
    is essentially [`wasm_safe_thread`](https://crates.io/crates/wasm_safe_thread)
    (deps: `wasm-bindgen` + `wasm-bindgen-futures` + `js-sys` + `web-time`) — the
    crate `wasm_lite_std` was *ported from*, with a near-identical API
    (`spawn`/`JoinHandle`/`join_async`/`lock_async`/`Mutex`/`RwLock`/`mpsc`/…). The
    shim is mostly a thin re-export plus `wasm_bindgen_futures::spawn_local` for
    `spawn_local` and `web-time` for `time`.

  So for the realistic flagship case this **supersedes reverse interop**:
  `images_and_words` stays exactly as it is (wasm-bindgen + wgpu + atomics threads)
  and just patches `wasm_lite`→shim; the migrated leaves (`continue`,
  `some_executor`, `test_executors`, `logwise`) compile down to wasm-bindgen with no
  per-binary glue merge. The payoff for the leaf author is **one source tree, dual
  deployment** — native wasm_lite *and* (via the shim) the wasm-bindgen world —
  without the leaf maintaining two binding surfaces itself. Honest caveats: the
  binding-macro shim is real (if bounded) new code; minor API drift between
  `wasm_lite_std` and `wasm_safe_thread` needs thin wrappers; threaded leaves still
  require an atomics build (inherent — `images_and_words` already is one); and
  test-only surface (`#[wasm_lite_test(worker)]`, `async_doctest!`) is dev-side, so
  it maps to `wasm_bindgen_test` or is simply irrelevant downstream. Together with
  the fake-wasm-bindgen shim this forms a **symmetric pair**: the final app picks the
  *host* ecosystem and `[patch]`es in the other's shim — wgpu-style apps host on
  wasm-bindgen (this item), all-wasm_lite apps host on wasm_lite (item above).
  *The most viable path of everything here for leaf-under-wasm-bindgen-app; the new
  code is just the binding-macro translation.*
* **Richer type marshalling (a `serde-wasm-bindgen` analogue)** — the ABI today
  carries numbers, `bool`, strings, bytes, and opaque `JsValue` handles, plus
  `Option`/`Result` as *returns*; anything richer (a `Vec<T>` of structs, tuples,
  enums with data) must be encoded by hand (e.g. JSON through a `&str`). The plan:
  a `serde` `Serializer`/`Deserializer` pair that lowers Rust values to/from JS
  values over the `js_class!` object primitives (or a fast bytes path), so
  `#[derive(Serialize, Deserialize)]` types cross the boundary directly. Gated on
  the `js_class!` constructor + property get/set work. *Library-only once those
  land; not built.*
* **Worker pool** — one Web Worker is created per `spawn` today; a persistent pool
  would cut spawn cost and enable a synchronous `block_on` against pre-warmed
  workers. Pairs with cooperative cancellation for pool teardown.
* **Broaden the wasm test suite** — `crates/wasm_lite_std/tests/browser.rs` now
  ports the bulk of the native unit suite (~46 tests: `Mutex`/`Spinlock`/`Condvar`/
  `mpsc`/`time` across spin/block/sync/async + timeouts), using `(worker)` tests
  for blocking variants. Remaining: multi-reader `RwLock`, `park`/`unpark`, and the
  Node-only paths (the runner is browser-only).
* **Deployment niceties** — a `wasm-lite bundle` command, session pooling/idle
  reaper for the persistent browser, and test filtering (`cargo test NAME`).
* **Smaller items** — deeply nested generics on imports (single-level
  `Option<Vec<u8>>`/`Option<&[u8]>`/`Result<…>` already work since the `import!`
  proc-macro rewrite; `Option<Result<…>>` does not); a `panic = "unwind"` mode
  (catch-unwind per poll, drop just the failed task, poison its locks — vs
  `abort`'s per-thread trap); and refreshing the `wasm_lite_std` crate-level doc
  comment, whose `wasm_thread` comparison table still lists wasm-bindgen/js-sys
  deps (the code no longer uses them — the test harness was moved off
  `wasm_bindgen_test` onto `#[wasm_lite_test]`).
