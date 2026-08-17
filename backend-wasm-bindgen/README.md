# wasm-bindgen backend for wasm_lite APIs

This workspace provides crates named `wasm_lite` and `wasm_lite_std`, but
implements them on the real wasm-bindgen runtime. It lets a wasm_lite-authored
leaf live inside a host application whose build remains an ordinary
wasm-bindgen build.

Patch whichever wasm_lite packages the leaf uses:

```toml
[patch.crates-io]
wasm_lite = { path = ".../backend-wasm-bindgen/wasm_lite" }
wasm_lite_std = { path = ".../backend-wasm-bindgen/wasm_lite_std" }
```

The substitute package versions must match the current released wasm_lite
line. Cargo considers a patch package only when its version satisfies the
consumer's dependency requirement; a `0.1.0` substitute cannot replace a
dependency declared as `wasm_lite = "0.1.2"`.

Unlike the wasm_lite backend in `backend-wasm-lite/`, this direction needs no
dependency-source rewrite. These substitutes already consume released
`wasm-bindgen`, `js-sys`, and `wasm_safe_thread` packages, so they naturally
unify with the host application's wasm-bindgen dependency graph.
