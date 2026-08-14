# Design notes: [wasm-bindgen](https://wasm-bindgen.github.io/wasm-bindgen/) coexistence

*(Part of the [wasm_lite](../README.md) docs. See also: [roadmap](./roadmap.md),
[interop](./interop.md), [thread-ownership census](./wasm-thread-ownership-census.md),
[migration guide](../MIGRATION.md).)*

These notes record the options for letting wasm_lite and wasm-bindgen coexist in
one binary. Both `[patch]` directions now have implementations, with very
different coverage, so read the status line on each:

| option | status |
|---|---|
| [Fake-wasm-bindgen shim](#fake-wasm-bindgen-shim-host-on-wasm_lite) (host on wasm_lite) | **SHIPPED** — [`shims_wasm_bindgen/`](../shims_wasm_bindgen/README.md), exercised by `scripts/wasm32/tests` |
| [Fake-wasm_lite shim](#fake-wasm_lite-shim-host-on-wasm-bindgen) (host on wasm-bindgen) | **SHIPPED, PARTIAL** — [`shims/`](../shims/); covers the original core subset but trails newer wasm_lite APIs |
| [Reverse interop](#reverse-interop-a-wasm_lite-leaf-under-a-wasm-bindgen-app) | design only |
| [Porting wgpu by hand](#porting-wgpu-off-wasm-bindgen-the-capstone) | design only; not required for the upstream surface the shim already covers |

Today's [interop](./interop.md) is one-directional: `wasm_lite` is always the
*outer* tool (it runs the wasm-bindgen CLI internally and merges both glues).
The direct CLI interop remains one-directional; the reverse-direction shim is a
package substitution, not a glue-merging post-pass.

## The forcing case: mixed wasm_lite + wgpu binaries

**Largely answered by the shim** — see the status table above. `wgpu`'s web
backend is irreducibly wasm-bindgen/web-sys, but it does not follow that the app
"stays a wasm-bindgen-driven build": the shim replaces wasm-bindgen underneath
unmodified wgpu, so the binary is all-wasm_lite while wgpu's source is untouched.
Measured on a real wgpu application with
`[patch.crates-io] wasm-bindgen = { path = ".../shims_wasm_bindgen/wasm_bindgen" }`:
`cargo check` and `cargo build` for `wasm32-unknown-unknown` both succeed, the lock
holds one `wasm-bindgen` at 0.2.108 whose macro is `wasm-bindgen-macro-wl`, and no
real wasm-bindgen remains in the graph. The shim's own browser coverage now runs
unmodified js-sys and DOM bindings, constructs a wgpu 28 `Instance`, and reaches
`navigator.gpu` from `request_adapter`. It still rejects block-level
`inline_js`/`module`/`raw_module`, which prevents some complete applications
(including the then-current Metropolis graph) from running unchanged; see the
shim's [current coverage table](../shims_wasm_bindgen/README.md#what-works).

The subordination design below is the *alternative* route, kept for the case where
an app must host on wasm-bindgen instead. Original framing follows.

`wgpu`'s web backend is irreducibly wasm-bindgen/web-sys (WebGPU/WebGL/canvas), so
an app that renders with wgpu and does *not* use the shim stays a
wasm-bindgen-driven build. The goal is to let
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

* a `wasm_lite patch` **post-pass** the app runs *after* wasm-bindgen (the inverse
  of `build_interop` — inject wasm_lite's import object into wasm-bindgen's
  loader), so the consumer adds one build command rather than swapping tools; or
* a codegen mode that re-expresses wasm_lite descriptors as wasm-bindgen **schema**
  so the downstream CLI resolves them with no extra step.

Until then the options are: have the app make `wasm_lite` its final codegen step
(its `#[wasm_bindgen]` code keeps working), patch the compatible portion of the
leaf through [`shims/`](../shims/), or ship the leaf **dual-backend**
(feature-gate a wasm-bindgen binding surface alongside the wasm_lite one). The
*threaded* variant of a true post-pass is exactly what subordinating wgpu (above)
would need.

## Porting wgpu off wasm-bindgen (the capstone)

The most ambitious option: re-express wgpu's web backend
(`wgpu/src/backend/webgpu.rs` + the web-sys WebGPU/WebGL/canvas surface) as
wasm_lite `import!`/`js_class!` bindings, eliminating wasm-bindgen from the binary
entirely. **Not** required for *mixed* binaries (subordination handles those) —
this is for an all-wasm_lite world. The low-level prerequisites now exist in
`import!` (`#[constructor]`, property/indexing kinds, `#[instanceof]`, variadics),
`Closure`, and `JsFuture`; `js_class!` still lacks constructor/property sugar and
checked downcasts. The remaining work is therefore mostly the enormous typed
WebGPU surface and those typed-wrapper conveniences, rather than inventing
callbacks or Promise interop. The one simplifying fact: wgpu-web doesn't thread,
so there's no worker bootstrap or atomics interplay to reimplement.

## The two `[patch]` shims

A symmetric pair: the final app picks the *host* ecosystem and `[patch]`es in the
other's shim. wgpu-style apps host on wasm-bindgen; all-wasm_lite apps host on
wasm_lite.

### Fake-wasm-bindgen shim (host on wasm_lite)

> **SHIPPED.** Lives in [`shims_wasm_bindgen/`](../shims_wasm_bindgen/README.md),
> which documents the crate graph in detail. Two consumer demos plus a
> `wasm_bindgen_test` harness run under `scripts/wasm32/tests`. The rest of this
> section is the original rationale, which still holds.

Instead of reconciling two binding worlds or rewriting wgpu by hand, **replace
wasm-bindgen itself**: ship a crate that implements the upstream subset in scope
but whose `#[wasm_bindgen]` macro lowers to wasm_lite's ABI + descriptor sections, and
have the app drop it in graph-wide with
`[patch.crates-io] wasm-bindgen = { … }`. Because js-sys, web-sys,
wasm-bindgen-futures, and wgpu are all written *against* wasm-bindgen, the
*unmodified* upstream crates then compile on our foundation and emit wasm_lite
descriptors — so a single `wasm_lite` codegen pass produces glue for the whole
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

> **SHIPPED AS A BOUNDED COMPATIBILITY SHIM.** It lives in [`shims/`](../shims/),
> a separate workspace because it reuses the `wasm_lite` and `wasm_lite_std`
> package names. Its compile-time grammar test is
> [`shims/wasm_lite/tests/grammar.rs`](../shims/wasm_lite/tests/grammar.rs).

The mirror image, and probably the best answer to the original leaf-migration
problem. A leaf is authored against `wasm_lite`; a downstream app that stays on
wasm-bindgen (e.g. anything using wgpu) `[patch]`es `wasm_lite` / `wasm_lite_std`
to shims **backed by wasm-bindgen**, so the leaf's
`import!`/`#[export]`/`js_class!`/`wasm_lite_std::*` lower onto wasm-bindgen and
the whole binary is an ordinary wasm-bindgen build — the app's existing
`wasm-pack` pipeline is unchanged, no `wasm_lite` codegen step, no reverse-interop
loader surgery.

This direction is structurally easier than faking wasm-bindgen, for two reasons:

* **wasm_lite's binding surface is a strict subset of wasm-bindgen's**, so the
  macro shim (`import!`→`#[wasm_bindgen] extern`, `#[export]`→`#[wasm_bindgen] pub
  fn`, `js_class!`→extern type, `JsValue`→`wasm_bindgen::JsValue`,
  `console`/`performance`/`date`→web-sys/js-sys) translates *down* with no missing
  features in the subset the shim translates.
* **The threading half already exists.** A wasm-bindgen-backed `wasm_lite_std` is
  essentially [`wasm_safe_thread`](https://crates.io/crates/wasm_safe_thread)
  (deps: `wasm-bindgen` + `wasm-bindgen-futures` + `js-sys` + `web-time`) — the
  crate `wasm_lite_std` was *ported from*, with a near-identical API. The shim is
  mostly a thin re-export plus `wasm_bindgen_futures::spawn_local` for
  `spawn_local` and `web-time` for `time`.

For the subset it implements, this supersedes reverse interop:
`images_and_words` stays exactly as it is (wasm-bindgen + wgpu + atomics threads)
and just patches `wasm_lite`→shim; the migrated leaves compile down to
wasm-bindgen with no per-binary glue merge. The payoff for the leaf author is
**one source tree, dual deployment** — native wasm_lite *and* (via the shim) the
wasm-bindgen world — without the leaf maintaining two binding surfaces itself.

The current boundary matters. `shims/wasm_lite_macro_wb` predates the real
macro's property/indexing/`instanceof`/variadic forms, and the runtime shim does
not expose the newer `Closure`, `JsFuture`, browser modules, benchmark API, or
`AsJsValue`. `shims/wasm_lite_std` likewise trails `sleep_async` and newer test/
runtime details. Threaded leaves still require the wasm-bindgen host's atomics
setup, and `#[wasm_lite_test(worker)]` is accepted but maps to a normal
`wasm_bindgen_test` because that harness has no per-test worker form. Expand and
test this shim deliberately when a leaf uses surface beyond its grammar test.
