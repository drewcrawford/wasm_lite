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

## Direction matters: `wasm-lite` is the outer tool

The interop is **one-directional**. The supported case is a wasm_lite module
(the one you run `wasm-lite` over) depending on a wasm-bindgen crate: `wasm-lite`
runs the wasm-bindgen CLI *internally* and merges `__wbg_get_imports()` with
wasm_lite's `makeImports()` into a single loader (which also defines the
`globalThis.__wlbridge` handoff slot the conversions use).

The **reverse** — you migrate a leaf crate to `import!`/`#[export]`, but your
downstream consumers keep a `wasm-bindgen`/`wasm-pack` pipeline — does **not**
work today. Your leaf's bindings are resolved by the `wasm-lite` codegen pass over
the *final* `.wasm`; a wasm-bindgen-only toolchain never runs that pass, so the
imports your leaf declares (`Math.random`, `__wl_spawn`, the atomics runtime
imports, `__wlbridge`, …) are left unsatisfied and the module fails to
instantiate. wasm_lite can wrap wasm-bindgen; wasm-bindgen can't wrap wasm_lite.

Until "reverse interop" lands (see the [roadmap](./roadmap.md)) the options for a
wasm_lite-migrated leaf are:

* **Have the app make `wasm-lite` its final codegen step** (with the
  `wasm-bindgen` feature). The app's own `#[wasm_bindgen]` code keeps working
  unchanged — only the build command changes, not the source. Caveat: wasm-pack
  specifics (`--target bundler|nodejs`, `.d.ts`, JS snippets) don't carry over yet.
* **Ship the leaf dual-backend** — feature-gate a wasm-bindgen binding surface
  alongside the wasm_lite one, behind a thin internal abstraction. The only way to
  hand the leaf to a consumer who changes *nothing*, at the cost of maintaining two
  glue surfaces.

Note that "keep the leaf pure Rust" does **not** sidestep this: `wasm_lite_std`
threads still emit imports (`__wl_spawn`, atomics runtime) that need the codegen
pass. A leaf with no `import!`/`#[export]`/threads needs no wasm_lite at all.
