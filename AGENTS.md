# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`wasm_lite` is a dependency-light rewrite of wasm-bindgen for `wasm32-unknown-unknown`:
it binds JavaScript and Rust to each other with **zero runtime dependencies** and a
host-side codegen tool. There is no all-in-one `#[wasm_bindgen]` macro. See `README.md`
for the user-facing story and `docs/` for deep dives (`binding-model.md`, `testing.md`,
`threads-and-async.md`, `interop.md`, `roadmap.md`).

## The central idea: descriptors in custom wasm sections

The whole architecture hinges on this flow — understand it before touching codegen or macros:

1. **Wasm side (`crates/wasm_lite` + `crates/wasm_lite_macro`).** `import!`, `#[export]`,
   and `js_class!` emit the normal wasm imports/exports *plus* a text descriptor of each
   binding's ABI into a custom wasm section: `__wasm_lite_imports` for imports,
   `__wl_exports` for exports, `__wasm_lite_tests` for `#[wasm_lite_test]` names. The
   descriptor goes into a `#[link_section]` static (see `descriptor_bytes` in
   `crates/wasm_lite/src/lib.rs`).
2. **Host side (`crates/wasm_lite_codegen`).** Reads those sections back out of the compiled
   `.wasm` (`wasm.rs` = minimal dependency-free wasm binary reader; `descriptor.rs` /
   `exports.rs` = text parsers) and generates matching JS glue (`generate.rs`): one shim per
   import that unmarshals wasm-level args (e.g. `&str` arrives as a `(ptr, len)` pair decoded
   from linear memory via `__wl_malloc`/`__wl_free`), plus one wrapper per export.
3. **Run (`runner`).** A `cargo` runner that reads the descriptor sections, generates glue,
   serves it, and drives it in a **real browser over WebDriver**.

Section name constants and the public codegen API live in `crates/wasm_lite_codegen/src/lib.rs`.
The macro ABI (what text gets emitted) and the codegen parser (what reads it) must stay in
lockstep — a change to one almost always requires a matching change to the other.

## ABI marshalling conventions

- Strings/bytes cross as `(ptr, len)` into linear memory; the host allocates with the
  exported `__wl_malloc` and frees with `__wl_free`.
- `Option`/`Result` returns use an sret buffer: discriminant at `base`, payload at `base + 8`.
  The Rust read side is the `FromSretPayload` trait in `crates/wasm_lite/src/lib.rs` — one
  impl per inner type, so the `import!` macro needs only one terminal rule per `Option`/`Result`.
- Proc-macros emit absolute `::wasm_lite::…` paths (a proc-macro can't use `$crate`); the core
  crate does `extern crate self as wasm_lite;` so those paths resolve when macros are used
  inside the crate itself.

## Workspace crates

| crate | role |
|---|---|
| `crates/wasm_lite` | core runtime: `JsValue`, `__wl_malloc`/`__wl_free`, panic hook, `thread::spawn`, `console`/`date`/`performance` bindings. Re-exports the macros. |
| `crates/wasm_lite_macro` | proc-macros (`syn`/`quote`, build-time only): `import!`, `#[export]`, `#[wasm_lite_test]`, `js_class!`. `ty.rs` holds the shared type→ABI dispatch. |
| `crates/wasm_lite_codegen` | host-side: parse descriptor sections, generate JS glue. Dependency-free. |
| `crates/wasm_lite_cli` | the `wasm-lite` binary wrapping codegen |
| `crates/wasm_lite_std` | std-like veneer (`std::thread`/`std::sync`/`std::time`, sync **and** async) ported from `wasm_safe_thread`, retargeted onto `wasm_lite` + a `spawn_local` executor |
| `runner` | WebDriver runner: serves a bin interactively, or drives tests/doctests headless and exits |

## Dependency policy (important)

Zero *runtime* deps is a hard design goal. `wasm_lite` and `wasm_lite_codegen` pull nothing.
`syn`/`quote` are allowed **only** in `wasm_lite_macro` because they are build-time
(zero bytes in the `.wasm`). Do not add runtime dependencies; before adding any new dependency,
ask the user, and prefer crates by `drewcrawford`.

## Building and testing

Two distinct worlds:

**Host-side crates** (`wasm_lite_codegen`, `runner`, `wasm_lite_macro`) build and test natively:

```bash
cargo build -p runner
cargo test -p wasm_lite_codegen           # parser/codegen unit tests (in #[cfg(test)] mods)
cargo test -p wasm_lite_codegen wasm::    # a single module's tests
```

Note: `wasm_lite` is an rlib that also builds on the host (kept as a workspace member for
IDE/CI coverage), but its binding behavior only means anything on wasm32.

**Wasm-side code** must run in a browser via the runner. Build the runner once, then point the
wasm target's runner at it:

```bash
cargo build -p runner
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/runner"
cd examples/hello-rust
cargo run     # opens the module in a browser (bin)
cargo test    # drives #[wasm_lite_test]s headless and exits
```

`cargo run` vs `cargo test` is distinguished by the runner *by path*. A WebDriver-capable
browser must be installed (Firefox + `geckodriver`, or Chrome + `chromedriver`).

**Atomics / threads / async examples need nightly + `-Z build-std`** because enabling the
`atomics` target feature forces recompiling `std`. These examples ship a `.cargo/config.toml`
with the atomics rustflags (`+atomics,+bulk-memory,+mutable-globals`, `--shared-memory`,
`--import-memory`, etc.). Run them with `cargo +nightly run`. The canonical full invocation
for the std test suite is `crates/wasm_lite_std/run-browser-tests.sh` — copy its `RUSTFLAGS`
when running any atomics/threaded wasm test by hand.

## Examples = integration tests

`examples/` are standalone crates (excluded from the workspace in the root `Cargo.toml` when
they are wasm-only bins), each exercising a feature: `hello-rust` (imports/handles/strings/
bytes/`js_class!`), `exports-demo`, `tests-demo`, `doctest-demo`, `interop` (wasm-bindgen
bridge), and the nightly atomics/threads/async family (`atomics-demo`, `threads-demo`,
`std-threads-demo`, `async-*`, `panic-demo`, `worker-spawn-local-demo`). When changing macro
output or codegen, the relevant example is the end-to-end check.

## Testing-on-wasm specifics

- `#[wasm_lite_test]` bodies run on the **main thread** by default, where blocking APIs
  (`lock_block`, `recv_block`, `park`, sync `join`) trap. Use `#[wasm_lite_test(worker)]` to
  run the body on a Web Worker for blocking/threaded code.
- Doctests run in-browser too; call `wasm_lite::set_panic_hook()` at the top of a doctest so
  failures report the panic message instead of a bare "unreachable" trap.
