# [wasm-bindgen](https://wasm-bindgen.github.io/wasm-bindgen/) interop

*(Part of the [wasm_lite](../README.md) docs. See also: [binding model](./binding-model.md),
[testing](./testing.md), [threads & async](./threads-and-async.md),
[roadmap](./roadmap.md), [migration guide](../MIGRATION.md).)*

Enable the `wasm-bindgen` feature to link a crate that itself uses wasm-bindgen.
`wasm_lite_codegen` runs the (version-matched) wasm-bindgen CLI, merges its
loader with our glue, and provides explicit `.to_wasm_bindgen()` /
`.to_wasm_lite()` conversions between the two `JsValue` types. This is the
recommended path for an *incremental* migration — see the
[migration guide](../MIGRATION.md).

With `-o app.js`, the CLI writes the complete interop set beside that file:
`app.js.wasm`, `app.js.wl.js`, and `app.js.wb.js`. The loader refers to those
bundle-specific names, so neighboring outputs do not retarget one another.
Multi-file writes are prepared in sibling temporary files before replacement;
a failure while preparing one artifact does not partially rewrite the requested
bundle.

## Direction matters: `wasm_lite` is the outer tool

The interop is **one-directional**. The supported case is a wasm_lite module
(the one you run `wasm_lite` over) depending on a wasm-bindgen crate: `wasm_lite`
runs the wasm-bindgen CLI *internally* and merges `__wbg_get_imports()` with
wasm_lite's `makeImports()` into a single loader (which also defines the
`globalThis.__wlbridge` handoff slot the conversions use).

The **reverse** — you migrate a leaf crate to `import!`/`#[export]`, but your
downstream consumers keep a `wasm-bindgen`/`wasm-pack` pipeline — does **not**
work today. Your leaf's bindings are resolved by the `wasm_lite` codegen pass over
the *final* `.wasm`; a wasm-bindgen-only toolchain never runs that pass, so the
imports your leaf declares (`Math.random`, `__wl_spawn`, the atomics runtime
imports, `__wlbridge`, …) are left unsatisfied and the module fails to
instantiate. wasm_lite can wrap wasm-bindgen; wasm-bindgen can't wrap wasm_lite.

There are three options for a wasm_lite-migrated leaf:

* **Have the app make `wasm_lite` its final codegen step** (with the
  `wasm-bindgen` feature). The app's own `#[wasm_bindgen]` code keeps working
  unchanged — only the build command changes, not the source. Caveat: wasm-pack
  specifics (`--target bundler|nodejs`, `.d.ts`, JS snippets) don't carry over yet.
* **Patch in the wasm-bindgen-backed wasm_lite shim** from [`shims/`](../shims/).
  It lowers a useful subset of `import!`, `#[export]`, `js_class!`,
  `#[wasm_lite_test]`, and `wasm_lite_std` onto wasm-bindgen, so the leaf source
  stays unchanged and the app keeps its existing wasm-bindgen pipeline. It is
  not yet a drop-in replacement for the entire current API: newer property/
  variadic import forms, `Closure`/`JsFuture`, browser binding modules, benches,
  and `sleep_async` still need parity work. Treat it as a bounded compatibility
  shim and test the leaf surface you use.
* **Ship the leaf dual-backend** — feature-gate a wasm-bindgen binding surface
  alongside the wasm_lite one, behind a thin internal abstraction. The only way to
  hand the leaf to a consumer who changes *nothing at all*, at the cost of
  maintaining two glue surfaces.

Note that "keep the leaf pure Rust" does **not** sidestep this: `wasm_lite_std`
threads still emit imports (`__wl_spawn`, atomics runtime) that need the codegen
pass in a real wasm_lite build. The `shims/wasm_lite_std` substitute routes that
public surface through `wasm_safe_thread` instead. A leaf with no
`import!`/`#[export]`/threads needs no wasm_lite at all.

## Tests run through the bundle too

`cargo test` on an interop module works the same way `cargo run` does. The
runner sees wasm-bindgen's schema section, finalizes the module with the
`wasm-bindgen` CLI, and serves a merged loader over both glues — the only
difference from a plain target is that the loader exports `instantiate` instead
of running on import, so the runner can load a page per test case.

This works because the wasm-bindgen CLI leaves our metadata alone: it strips its
own `__wasm_bindgen_unstable` section and the debug sections, but preserves
`__wasm_lite_tests`, `__wasm_lite_imports`, and the `__wl_test_*` exports.
Discovery and invocation are therefore unchanged, and
`examples/interop/tests/harness.rs` keeps it that way — it calls a
`#[wasm_bindgen]` function and exercises the `JsValue` bridge from inside a
`#[wasm_lite_test]`.

One thing to know: enabling wasm_lite's `wasm-bindgen` feature is what puts the
schema section in your module, so it also makes the `wasm-bindgen` CLI a build
requirement, version-matched to your `wasm-bindgen` dependency. Threads are the
current gap — an interop bundle has no worker bootstrap, so a `#[wasm_lite_test(worker)]`
in an interop module is not supported.
