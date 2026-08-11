# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

(The real file is `AGENTS.md`; `CLAUDE.md` is a symlink to it. Edit either — they are one file.)

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
   binding's ABI into a custom wasm section: `__wasm_lite_imports` for imports and
   `__wl_exports` for exports. `#[wasm_lite_test]` and `#[wasm_lite_bench]` similarly
   record names in `__wasm_lite_tests` and `__wasm_lite_benches`. Each entry goes into
   a `#[link_section]` static (see `descriptor_bytes` in
   `crates/wasm_lite/src/lib.rs`).
2. **Host side (`crates/wasm_lite_codegen`).** Reads those sections back out of the compiled
   `.wasm` (`wasm.rs` = minimal dependency-free wasm binary reader; `descriptor.rs` /
   `exports.rs` = text parsers) and generates matching JS glue (`generate.rs` plus
   `generate/` helpers): one shim per import that unmarshals wasm-level args (e.g. `&str`
   arrives as a `(ptr, len)` pair decoded from linear memory), plus one wrapper per export.
3. **Run (`runner`).** A `cargo` runner that reads the descriptor sections, generates glue,
   serves it, and drives it in a **real browser over WebDriver**.

Section name constants and the public codegen API live in `crates/wasm_lite_codegen/src/lib.rs`.
The macro ABI (what text gets emitted) and the codegen parser (what reads it) must stay in
lockstep — a change to one almost always requires a matching change to the other.

## ABI marshalling conventions

- String/byte arguments flatten to `(ptr, len)`. Direct string/byte returns use a packed
  `(ptr << 32 | len)` `i64`; `Option`/`Result` payloads store pointer and length separately
  in the sret buffer. The allocating side uses exported `__wl_malloc`; ownership transfers
  across the boundary and the receiving side eventually calls `__wl_free`.
- `Option`/`Result` returns use an sret buffer: discriminant at `base`, payload at `base + 8`.
  The Rust read side is the `FromSretPayload` trait in `crates/wasm_lite/src/lib.rs` — one
  impl per inner type, so the `import!` macro needs only one terminal rule per `Option`/`Result`.
- Proc-macros emit absolute `::wasm_lite::…` paths (a proc-macro can't use `$crate`); the core
  crate does `extern crate self as wasm_lite;` so those paths resolve when macros are used
  inside the crate itself.

## Workspace crates

| crate | role |
|---|---|
| `crates/wasm_lite` | core runtime: `JsValue`, `Closure`, `JsFuture`, browser benchmarks, `__wl_malloc`/`__wl_free`, panic hook, `thread::spawn`, and `console`/`date`/`performance`/`timer`/`event`/`fetch`/`websocket`/`dom` bindings. Re-exports the macros. |
| `crates/wasm_lite_macro` | proc-macros (`syn`/`quote`, build-time only): `import!`, `#[export]`, `#[wasm_lite_test]`, `#[wasm_lite_bench]`, `js_class!`. `ty.rs` holds the shared type→ABI dispatch. |
| `crates/wasm_lite_codegen` | host-side: parse descriptor sections, generate JS glue. Dependency-free. |
| `crates/wasm_lite_cli` | the `wasm-lite` binary wrapping codegen; writes glue, bundle-specific worker modules, and wasm-bindgen interop artifact sets |
| `crates/wasm_lite_std` | std-like veneer (`std::thread`/`std::sync`/`std::time`, sync **and** async) ported from `wasm_safe_thread`; atomics builds use workers + `Atomics.waitAsync`, while stable non-atomic wasm uses a local event-loop executor and host timers |
| `runner` | WebDriver runner: serves a bin interactively, or drives tests/doctests/benchmarks headless and exits |

Two separate shim workspaces deliberately reuse package names and therefore cannot be root
workspace members:

| workspace | role |
|---|---|
| `shims/` | partial wasm-bindgen-backed substitutes for `wasm_lite` / `wasm_lite_std`, so a wasm_lite-authored leaf can live under a wasm-bindgen host |
| `shims_wasm_bindgen/` | substitutes for `wasm-bindgen` / `wasm-bindgen-test` that lower a substantial upstream API subset onto wasm_lite; includes consumer demos and browser tests |

## Dependency policy (important)

Zero default core-runtime deps is a hard design goal. `wasm_lite_codegen` is dependency-free;
`wasm_lite` has no default runtime dependency beyond its proc-macro crate, but its optional
`wasm-bindgen` interop feature deliberately adds wasm-bindgen. `wasm_lite_std` uses
`atomic-waker`. `syn`/`quote` are allowed in proc-macro crates because they are build-time
(zero bytes in the `.wasm`). Do not add dependencies; before adding any new one, ask the user,
and prefer crates by `drewcrawford`.

## Building and testing

**The full gate is `scripts/check_all`** — fmt, check, clippy, tests, and docs across *both*
worlds (native and wasm32), including the script-selected examples, browser binding suites,
bench smoke, and `shims_wasm_bindgen` consumers. Run it before considering a change done.
The reverse `shims/` workspace is not currently part of that gate and needs an explicit
`cargo test --manifest-path shims/Cargo.toml --workspace` when touched. Each stage is also
runnable alone (`scripts/fmt`, `scripts/check`, `scripts/clippy`, `scripts/tests`,
`scripts/docs`), and each splits into `scripts/native/*` and `scripts/wasm32/*` halves.
Everything runs with `-D warnings`; pass `--relaxed` to allow warnings. Note
`cargo fmt` at the root does **not** cover the examples — `scripts/fmt` iterates their
manifests explicitly.

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

Runner knobs (env vars): `WASM_LITE_BROWSER=chrome|chromium|safari` picks a non-default
browser (default Firefox); `WASM_LITE_REUSE_BROWSER=1` keeps one browser session alive across
test invocations (stop it with `runner --stop-browser`); `WASM_LITE_NO_OPEN=1` serves a bin
without launching a browser (attach a debugger/browser manually). `WASM_LITE_TIMEOUT_SECS`,
`WASM_LITE_RUN_SECONDS`, `WASM_LITE_SERVE_DIR`, `WASM_LITE_BROWSER_ARGS`, and
`WASM_LITE_GPU` cover deadlines, long-running bins, assets, browser flags, and Chrome WebGPU.

**Atomics and thread spawning need nightly + `-Z build-std`** because enabling the `atomics`
target feature forces recompiling `std`. Those examples ship a `.cargo/config.toml` with the
atomics rustflags (`+atomics,+bulk-memory,+mutable-globals`, `--shared-memory`,
`--import-memory`, etc.) and run with `cargo +nightly run`. Async work that only needs
`spawn_local`, `JsFuture`, or `sleep_async` also works in an ordinary stable, non-shared wasm
build; spawning a thread there reports `Unsupported`. The canonical full threaded invocation
for the std test suite is `crates/wasm_lite_std/run-browser-tests.sh` — copy its `RUSTFLAGS`
when running any atomics/threaded wasm test by hand.

Manual `wasm-lite app.wasm -o app.js` output is bundle-specific. Shared-memory modules add
`app.js.worker.js`; wasm-bindgen interop adds `app.js.wasm`, `app.js.wl.js`, and
`app.js.wb.js`. The CLI writes multi-file outputs through sibling temporary files and refuses
to replace an implicit worker artifact unless it bears the generated-file marker.

## Examples = integration tests

`examples/` are standalone crates (excluded from or isolated from the root workspace when
they are wasm-only), each exercising a feature: `hello-rust` (imports/handles/strings/bytes/
`js_class!`), `exports-demo`, `tests-demo`, `doctest-demo`, `bench-demo`, `interop`
(wasm-bindgen bridge), and the nightly atomics/thread-spawning family (`atomics-demo`,
`threads-demo`, `std-threads-demo`, the worker-using `async-*` demos, `panic-demo`,
`worker-spawn-local-demo`). When changing macro output or codegen, the relevant example is
the end-to-end check.

## Testing-on-wasm specifics

- `#[wasm_lite_test]` bodies run on the **main thread** by default, where blocking APIs
  (`lock_block`, `recv_block`, `park`, sync `join`) trap. Use `#[wasm_lite_test(worker)]` to
  run the body on a Web Worker for blocking/threaded code.
- Doctests run in-browser too; call `wasm_lite::set_panic_hook()` at the top of a doctest so
  failures report the panic message instead of a bare "unreachable" trap.
