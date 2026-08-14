# `shims_wasm_bindgen` — wasm-bindgen's API, on wasm_lite

The **fake-wasm-bindgen shim** from [`docs/design-notes.md`](../docs/design-notes.md):
rather than reconciling two binding systems, or rewriting `wgpu` by hand,
replace wasm-bindgen itself. An application substitutes this crate graph-wide
and the *unmodified* upstream crates — `js-sys`, `web-sys`, `wgpu` — compile on
wasm_lite and emit wasm_lite descriptors, so one `wasm_lite` codegen pass covers
the whole module.

The mirror of [`shims/`](../shims), which goes the other way: wasm_lite's API
lowered onto wasm-bindgen, for an app that stays on wasm-bindgen.

## How it sits in the graph

The shim does **not** sit between js-sys and wasm-bindgen. It *is* wasm-bindgen,
as far as the graph is concerned: same package name, substituted by
`[patch.crates-io]`. js-sys, web-sys and wgpu are the real, unmodified crates
from crates.io and never learn otherwise.

```
   crates.io, UNMODIFIED
   ┌──────────────────────────────────────────────────────┐
   │  wgpu 28 ──┬── its own vendored webgpu_sys           │
   │            │   (128 files, 16,635 lines)             │
   │            └── web-sys 0.3.85 ── js-sys 0.3.85       │
   │  glow · plotters · uuid · wasm-bindgen-futures       │
   └───────────────────────┬──────────────────────────────┘
                           │  every one of them does
                           │  `use wasm_bindgen::…`
                           ▼
   ┌──────────────────────────────────────────────────────┐
   │  package `wasm-bindgen` v0.2.108   ←── THE SHIM      │
   │  shims_wasm_bindgen/wasm_bindgen                     │
   │  JsValue · JsCast · #[wasm_bindgen] · Closure        │
   └───────────────────────┬──────────────────────────────┘
                           ▼
   ┌──────────────────────────────────────────────────────┐
   │  wasm_lite                                           │
   │  JsValue · import! · js_class! · Closure             │
   └───────────────────────┬──────────────────────────────┘
                           ▼
              descriptor custom sections
                           ▼
              wasm_lite codegen ──> ES module glue
```

So there is exactly **one** binding system at runtime. `#[wasm_bindgen]` in
wgpu's vendored bindings lowers to a wasm_lite descriptor, the same as
`import!` in your own code, and one codegen pass emits glue for both.

Code *you* write should use `wasm_lite` directly — `import!`/`js_class!` are the
js-sys equivalent, and going through the shim buys nothing. The shim is for code
you are not going to rewrite.

## Using it

```toml
[patch.crates-io]
wasm-bindgen = { path = ".../shims_wasm_bindgen/wasm_bindgen" }
# Only if your suites use `#[wasm_bindgen_test]`:
wasm-bindgen-test = { path = ".../shims_wasm_bindgen/wasm_bindgen_test" }
```

Two things routinely go wrong here:

* **`[patch]` only replaces the exact version it declares.** The shim declares
  `0.2.108` (and `0.3.56` for the test crate). One dependency resolving above
  that leaves the *real* wasm-bindgen in the graph alongside it, and the symptom
  — the runner refusing with "test mode does not yet support wasm-bindgen
  interop modules" — points nowhere near the cause. Check with
  `grep -c 'name = "wasm-bindgen"' Cargo.lock`; the answer should be 1.
* **Pin the family together.** `js-sys`, `web-sys` and `wasm-bindgen-futures`
  pin wasm-bindgen exactly, so they all have to land on the matching versions:
  `js-sys 0.3.85`, `web-sys 0.3.85`, `wasm-bindgen-futures 0.4.58`.

Test suites additionally need `harness = false` per test target. That is the one
thing no macro can do for you — wasm_lite's runner discovers tests from a custom
wasm section and owns `main`.

## What works

| crate | compiles | runs in a browser |
|---|---|---|
| `js-sys` 0.3.85 | ✓ | ✓ |
| `web-sys` 0.3.85, DOM features | ✓ | ✓ |
| `web-sys` 0.3.85, all 135 WebGPU features | ✓ | — |
| `wgpu` 28, stable and `+atomics` | ✓ | `Instance::new`; `request_adapter` reaches `navigator.gpu` |

The attribute grammar: `extends`, `method`, `getter`, `setter`, `constructor`,
`static_method_of`, `indexing_getter`/`indexing_setter`/`indexing_deleter`,
`js_name`, `js_class`, `js_namespace`, `catch`, `variadic`, `is_type_of`,
`thread_local`/`thread_local_v2`, string enums, `pub static`, generic extern
types, and `#[wasm_bindgen]` on a Rust fn.

The runtime: `JsValue` (constructors, accessors, operators, ordering,
predicates, `BigInt` conversions, clonable handles, singleton constants),
`JsCast`, `JsError`, `Clamped`, `UnwrapThrowExt`, `Closure` (including variadic,
throwing and one-shot forms), `memory()`, `module()`, `link_to!`, `wbg_cast`.

## What does not

* **Block-level `#[wasm_bindgen(..)]` arguments** — `inline_js`, `module`,
  `raw_module`, `js_namespace` on an `extern` block. These say the bindings live
  somewhere other than `globalThis`, and honouring them needs a **codegen**
  feature: resolving imports against a JS module. They are refused rather than
  ignored, because ignoring one generates glue that throws on first call.

  This is what stops `wasm_safe_thread` — and therefore `images_and_words` and
  Metropolis — from building. See `NEEDS_REVIEW.md` in Metropolis.
* **`async fn` tests** without the test shim's `async` feature, which pulls in
  `wasm_lite_std` and so requires nightly and an `+atomics` build.
* **`Closure` taking `&T`.** It would overlap the by-value impl, and rustc
  tolerates that only under a future-compatibility lint; ruling it out needs
  negative reasoning stable Rust does not have.
* **`final`.** Accepted and ignored. It binds a method from the prototype where
  wasm_lite looks it up on the receiver, so an object shadowing the method would
  diverge. Nothing in js-sys, web-sys or wgpu depends on it.

## The two things that shaped the design

**`JsArg`/`FromJs` rather than `JsObject` on the argument and return paths.**
Most binding types *are* a handle and can lend theirs; a string enum — how the
entire WebGPU vocabulary is declared — has to make one. `JsArgRef` is
borrowed-or-owned so the common case still costs nothing.

**`AsJsValue` in wasm_lite.** The orphan rule stops the shim implementing `From`
for a `JsValue` it only re-exports, so the trait lives in wasm_lite and the
macro emits the impl per generated type. If that hook has to stretch much
further, moving `JsValue` into the shim is the cleaner answer.
