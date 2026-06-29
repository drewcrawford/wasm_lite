# Design notes: wasm-bindgen coexistence

*(Part of the [wasm_lite](../README.md) docs. See also: [roadmap](./roadmap.md),
[interop](./interop.md), [thread-ownership census](./wasm-thread-ownership-census.md),
[migration guide](../MIGRATION.md).)*

These are forward-looking design notes, not shipped features. They record the
options we've considered for letting wasm_lite and wasm-bindgen coexist in one
binary — the hardest open problem, because the realistic large app (one that
renders with [`wgpu`](https://crates.io/crates/wgpu)) cannot leave wasm-bindgen
behind in the near term. Today's [interop](./interop.md) is one-directional:
`wasm-lite` is always the *outer* tool (it runs the wasm-bindgen CLI internally
and merges both glues). Everything below is about the inverse and adjacent cases.

## The forcing case: mixed wasm_lite + wgpu binaries

`wgpu`'s web backend is irreducibly wasm-bindgen/web-sys (WebGPU/WebGL/canvas) and
cannot be migrated off it in the near term, so any app that renders with wgpu
(e.g. `images_and_words`) stays a wasm-bindgen-driven build. The goal is to let
such an app move its **non-graphics** crates (`continue`, `some_executor`,
`test_executors`, `logwise`, its own glue) onto wasm_lite while wgpu stays on
wasm-bindgen, in **one** binary.

What makes this tractable: wgpu does **no threading of its own** on wasm (its
`Worker`/`OffscreenCanvas` references are about *running inside* a worker, not
spawning one) — all threads in a wgpu app are the app's own. So the integration
is: wasm_lite owns thread spawning + instantiation (it already does, via
`__wl_spawn` + `wl_worker.js`), and wgpu's wasm-bindgen glue is **subordinated** —
merged into the import object on the main thread and in the worker bootstrap,
wired without re-running `start`. Because the target app is already an `+atomics`,
shared-memory, threaded build, this — not the easy non-atomic post-pass — is the
path that matters for wgpu. App-level invariant (unchanged from today): wgpu
handles are `!Send` on atomics builds, so all wgpu calls stay on one thread
(`send_cells`-style management).

## Reverse interop (a wasm_lite *leaf* under a wasm-bindgen app)

You migrate a leaf crate to `import!`/`#[export]`, but your downstream consumers
keep a `wasm-bindgen`/`wasm-pack` pipeline. This does **not** work today, because
their toolchain never runs the wasm_lite codegen pass that satisfies the leaf's
imports, so the module fails to instantiate. Two candidate fixes:

* a `wasm-lite patch` **post-pass** the app runs *after* wasm-bindgen (the inverse
  of `build_interop` — inject wasm_lite's import object into wasm-bindgen's
  loader), so the consumer adds one build command rather than swapping tools; or
* a codegen mode that re-expresses wasm_lite descriptors as wasm-bindgen **schema**
  so the downstream CLI resolves them with no extra step.

Until then the options are: have the app make `wasm-lite` its final codegen step
(its `#[wasm_bindgen]` code keeps working), or ship the leaf **dual-backend**
(feature-gate a wasm-bindgen binding surface alongside the wasm_lite one). The
*threaded* variant of this design is exactly what subordinating wgpu (above)
needs.

## Porting wgpu off wasm-bindgen (the capstone)

The most ambitious option: re-express wgpu's web backend
(`wgpu/src/backend/webgpu.rs` + the web-sys WebGPU/WebGL/canvas surface) as
wasm_lite `import!`/`js_class!` bindings, eliminating wasm-bindgen from the binary
entirely. **Not** required for *mixed* binaries (subordination handles those) —
this is for an all-wasm_lite world. It is gated on essentially every binding
feature at once: `js_class!` constructors + property get/set, closures-into-JS
(wgpu uses `Closure` for device-lost / uncaptured-error / promise callbacks), and
Promise interop (WebGPU is pervasively async). The one simplifying fact: wgpu-web
doesn't thread, so there's no worker bootstrap or atomics interplay to
reimplement — it's a (very large) *binding* surface, a natural capstone for
`wasm_lite_web`.

## The two `[patch]` shims

A symmetric pair: the final app picks the *host* ecosystem and `[patch]`es in the
other's shim. wgpu-style apps host on wasm-bindgen; all-wasm_lite apps host on
wasm_lite.

### Fake-wasm-bindgen shim (host on wasm_lite)

Instead of reconciling two binding worlds or rewriting wgpu by hand, **replace
wasm-bindgen itself**: ship a crate that is API-compatible with `wasm-bindgen` but
whose `#[wasm_bindgen]` macro lowers to wasm_lite's ABI + descriptor sections, and
have the app drop it in graph-wide with
`[patch.crates-io] wasm-bindgen = { … }`. Because js-sys, web-sys,
wasm-bindgen-futures, and wgpu are all written *against* wasm-bindgen, the
*unmodified* upstream crates then compile on our foundation and emit wasm_lite
descriptors — so a single `wasm-lite` codegen pass produces glue for the whole
module, WebGPU included. There is no second binding system to subordinate.

The piece this shim deliberately does **not** build is impersonation of
wasm-bindgen's *own* threading (`wasm-bindgen-rayon`/`wasm_thread`/`web_sys::Worker`
spawning). A [crates.io db-dump census](./wasm-thread-ownership-census.md)
(2026-06-29) found only **~1% of the 5,063-crate wasm-bindgen ecosystem directly
owns wasm threads** (49 crates, almost all `wasm-bindgen-rayon` ZK/compute; ~3.8%
upper bound touching `Worker*` at all). So thread-owning wasm-bindgen crates are
out of scope for this shim (they host on wasm-bindgen via the dual shim instead);
`wasm-bindgen-rayon` is the single lever if that tail is ever worth covering.

Honest cost: the shim is wasm-bindgen's *codegen contract*, not just an API — it
must reproduce the `#[wasm_bindgen]` attribute grammar web-sys/wgpu actually use
(`getter`/`setter`, `method`/`structural`, `constructor`, `catch`→`Result`,
`extends`, `variadic`, typed arrays/`Clamped`, optional args), the runtime/trait
surface hand-written js-sys/web-sys touch (`JsValue`, `JsCast` with `instanceof`
downcasting, `Closure`, `Function`, typed arrays, the `convert`/`describe` trait
names generated code references), and `wasm-bindgen-futures::JsFuture`. It does
not dodge the hard primitives — it *amortizes* them across the whole ecosystem
instead of one crate. Trade-off: you inherit web-sys's design rather than
designing a cleaner one.

### Fake-wasm_lite shim (host on wasm-bindgen)

The mirror image, and probably the best answer to the original leaf-migration
problem. A leaf is authored against `wasm_lite`; a downstream app that stays on
wasm-bindgen (e.g. anything using wgpu) `[patch]`es `wasm_lite` / `wasm_lite_std`
to shims **backed by wasm-bindgen**, so the leaf's
`import!`/`#[export]`/`js_class!`/`wasm_lite_std::*` lower onto wasm-bindgen and
the whole binary is an ordinary wasm-bindgen build — the app's existing
`wasm-pack` pipeline is unchanged, no `wasm-lite` codegen step, no reverse-interop
loader surgery.

This is much easier than faking wasm-bindgen, for two reasons:

* **wasm_lite's binding surface is a strict subset of wasm-bindgen's**, so the
  macro shim (`import!`→`#[wasm_bindgen] extern`, `#[export]`→`#[wasm_bindgen] pub
  fn`, `js_class!`→extern type, `JsValue`→`wasm_bindgen::JsValue`,
  `console`/`performance`/`date`→web-sys/js-sys) translates *down* with no missing
  features.
* **The threading half already exists.** A wasm-bindgen-backed `wasm_lite_std` is
  essentially [`wasm_safe_thread`](https://crates.io/crates/wasm_safe_thread)
  (deps: `wasm-bindgen` + `wasm-bindgen-futures` + `js-sys` + `web-time`) — the
  crate `wasm_lite_std` was *ported from*, with a near-identical API. The shim is
  mostly a thin re-export plus `wasm_bindgen_futures::spawn_local` for
  `spawn_local` and `web-time` for `time`.

So for the realistic large-app case this supersedes reverse interop:
`images_and_words` stays exactly as it is (wasm-bindgen + wgpu + atomics threads)
and just patches `wasm_lite`→shim; the migrated leaves compile down to
wasm-bindgen with no per-binary glue merge. The payoff for the leaf author is
**one source tree, dual deployment** — native wasm_lite *and* (via the shim) the
wasm-bindgen world — without the leaf maintaining two binding surfaces itself.
Honest caveats: the binding-macro shim is real (if bounded) new code; minor API
drift between `wasm_lite_std` and `wasm_safe_thread` needs thin wrappers; threaded
leaves still require an atomics build (inherent); and test-only surface
(`#[wasm_lite_test(worker)]`, `async_doctest!`) is dev-side, so it maps to
`wasm_bindgen_test` or is irrelevant downstream.
