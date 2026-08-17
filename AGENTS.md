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
3. **Run (`wasm_lite run`).** A `cargo` runner that reads the descriptor sections, generates glue,
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
| `crates/wasm_lite_cli` | the `wasm_lite` binary: `build` writes glue, bundle-specific worker modules, and wasm-bindgen interop artifact sets; `run` serves a bin interactively, or drives tests/doctests/benchmarks headless and exits |
| `crates/wasm_lite_std` | std-like veneer (`std::thread`/`std::sync`/`std::time`, sync **and** async) ported from `wasm_safe_thread`, plus opt-in async read-only `std::fs`; atomics builds use workers + `Atomics.waitAsync`, while stable non-atomic wasm uses a local event-loop executor and host timers |

Two separate shim workspaces deliberately reuse package names and therefore cannot be root
workspace members:

| workspace | role |
|---|---|
| `backend-wasm-bindgen/` | **the wasm-bindgen backend.** Substitutes named `wasm_lite` / `wasm_lite_std` but implemented on real wasm-bindgen, so a wasm_lite-authored leaf can live under a wasm-bindgen host |
| `backend-wasm-lite/` | **the wasm_lite backend.** Substitutes named `wasm-bindgen` / `wasm-bindgen-test` implemented on wasm_lite, lowering a substantial upstream API subset onto it; includes consumer demos and browser tests |

Say **backend**, not "shim", whenever it matters which runtime is underneath — the directory
names point the wrong way on their own. A shim is named for the API it *provides*; the
backend is what *implements* it, and that is what decides whether the `wasm-bindgen` CLI runs
over the module and therefore which linker exports are required. `backend-wasm-bindgen/` puts real
wasm-bindgen underneath your wasm_lite code (wasm-bindgen backend); `backend-wasm-lite/`
puts wasm_lite underneath your wasm-bindgen code (wasm_lite backend). "We use the shim"
settles nothing; "we are on the wasm-bindgen backend" settles it.

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
bench smoke, `wasm_lite_std`'s doctests under atomics, and `backend-wasm-lite` consumers.
`scripts/wasm32/tests` opens with `scripts/wasm32/negative`, which runs
`examples/must-fail-demo` and requires each fixture to **fail**, for a stated reason. Nothing
else in the gate can catch a runner that swallows a failure, because a swallowed failure is
indistinguishable from a green run — which is how two of them shipped. A negative fixture
that starts passing is a runner regression; fix the runner, not the fixture.
Run it before considering a change done. Note that `scripts/wasm32/docs` only *builds* docs —
doctests are *executed* by `scripts/wasm32/tests`, which is where a new doctest suite belongs.
The reverse `backend-wasm-bindgen/` workspace is not currently part of that gate and needs an explicit
`cargo test --manifest-path backend-wasm-bindgen/Cargo.toml --workspace` when touched. Each stage is also
runnable alone (`scripts/fmt`, `scripts/check`, `scripts/clippy`, `scripts/tests`,
`scripts/docs`), and each splits into `scripts/native/*` and `scripts/wasm32/*` halves.
Everything runs with `-D warnings`; pass `--relaxed` to allow warnings. Note
`cargo fmt` at the root does **not** cover the examples — `scripts/fmt` iterates their
manifests explicitly.

Two distinct worlds:

**Host-side crates** (`wasm_lite_codegen`, `wasm_lite_cli`, `wasm_lite_macro`) build and test natively:

```bash
cargo build -p wasm_lite_cli
cargo test -p wasm_lite_codegen           # parser/codegen unit tests (in #[cfg(test)] mods)
cargo test -p wasm_lite_codegen wasm::    # a single module's tests
```

Note: `wasm_lite` is an rlib that also builds on the host (kept as a workspace member for
IDE/CI coverage), but its binding behavior only means anything on wasm32.

**Wasm-side code** must run in a browser via the runner. Build the CLI once, then point the
wasm target's runner at it:

```bash
cargo build -p wasm_lite_cli
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/wasm_lite run"
cd examples/hello-rust
cargo run     # opens the module in a browser (bin)
cargo test    # drives #[wasm_lite_test]s headless and exits
```

`cargo run` vs `cargo test` is distinguished by the runner *by path*. A WebDriver-capable
browser must be installed (Firefox + `geckodriver`, or Chrome + `chromedriver`).

Runner knobs (env vars): `WASM_LITE_BROWSER=chrome|chromium|safari` picks a non-default
browser (default Firefox); `WASM_LITE_REUSE_BROWSER=1` keeps one browser session alive across
test invocations (stop it with `wasm_lite run --stop-browser`); `WASM_LITE_NO_OPEN=1` serves a bin
without launching a browser (attach a debugger/browser manually). `WASM_LITE_TIMEOUT_SECS`,
`WASM_LITE_RUN_SECONDS`, `WASM_LITE_SERVE_DIR`, `WASM_LITE_BROWSER_ARGS`, and
`WASM_LITE_GPU` cover deadlines, long-running bins, assets, browser flags, and Chrome WebGPU.

Doctests invoke the runner once per test, in parallel across every core, so the runner caps
concurrent browsers (free memory / ~1 GiB, clamped to the core count) and makes the rest wait.
`WASM_LITE_MAX_BROWSERS` overrides the cap. Without it, a memory-tight machine fails arbitrary
tests with `os error 11` or a discarded browsing context — see `docs/testing.md`.

**Atomics and thread spawning need nightly + `-Z build-std`** because enabling the `atomics`
target feature forces recompiling `std`. The canonical `.cargo/config.toml` for a threaded
build — including the `rustdocflags` half, which must be keyed by the exact triple — is in
`docs/threads-and-async.md`; copy it whole, since a partial copy links fine and fails later.
Those examples ship a `.cargo/config.toml` with the
atomics rustflags (`+atomics,+bulk-memory,+mutable-globals`, `--shared-memory`,
`--import-memory`, etc.) and run with `cargo +nightly run`. Async work that only needs
`spawn_local`, `JsFuture`, or `sleep_async` also works in an ordinary stable, non-shared wasm
build; spawning a thread there reports `Unsupported`. The canonical full threaded invocation
for the std test suite is `crates/wasm_lite_std/run-browser-tests.sh` — copy its `RUSTFLAGS`
when running any atomics/threaded wasm test by hand.

Manual `wasm_lite app.wasm -o app.js` output is bundle-specific. Shared-memory modules add
`app.js.worker.js`; wasm-bindgen interop adds `app.js.wasm`, `app.js.wl.js`, and
`app.js.wb.js`. The CLI writes multi-file outputs through sibling temporary files and refuses
to replace an implicit worker artifact unless it bears the generated-file marker.

## Examples = integration tests

`examples/` are standalone crates (excluded from or isolated from the root workspace when
they are wasm-only), each exercising a feature: `hello-rust` (imports/handles/strings/bytes/
`js_class!`), `exports-demo`, `tests-demo`, `doctest-demo`, `reexport-demo`
(compile-only: the macros must reach the runtime through the paths they are *given*),
`must-fail-demo` (fixtures that must fail — see the negative stage above), `bench-demo`, `interop`
(wasm-bindgen bridge), and the nightly atomics/thread-spawning family (`atomics-demo`,
`threads-demo`, `std-threads-demo`, the worker-using `async-*` demos, `panic-demo`,
`worker-spawn-local-demo`). When changing macro output or codegen, the relevant example is
the end-to-end check.

## Testing-on-wasm specifics

- `#[wasm_lite_test]` bodies run on the **main thread** by default, where blocking APIs
  (`lock_block`, `recv_block`, `park`, sync `join`) trap. Use `#[wasm_lite_test(worker)]` to
  run the body on a Web Worker for blocking/threaded code.
- Doctests run in-browser too; call `wasm_lite::set_panic_hook()` at the top of a doctest so
  failures report the panic message instead of a bare "unreachable" trap. That hook does not
  reach a *deferred* body: libtest takes the current hook before each doctest in an
  edition-2024 merged bundle and restores it afterwards, so anything installed while `main`
  ran is gone once the event loop polls. `async_doctest!`/`worker_doctest!` install it inside
  the body for that reason — keep it there, not at the call site, or the message goes back to
  being a bare stack trace.
- **Registered tests and `main` are not alternatives.** A harness run drives the
  `__wasm_lite_tests` entries and then runs `main` as one more case named `main`, because
  outside a `harness = false` target `main` is libtest's and owns every plain `#[test]` and
  every doctest in a merged bundle. Skipping it made one `#[wasm_lite_test]` silently
  disable all of them while the suite reported `ok`. The single exception is positive
  evidence: `test_main!`/`bench_main!` stamp a `__wl_noop_main` section saying their `main`
  is `fn main() {}`. Absence of the marker must keep meaning "run it" — the module cannot
  otherwise be told apart from a libtest binary, and guessing the other way loses tests.
- The async-test verdict (`__wl_test_pending`/`__wl_test_pass`) is a **count** of outstanding
  bodies, not a flag. A merged doctest bundle puts several in one page; as a flag the first
  to finish published the page's verdict and every later failure went unseen. Any change here
  must keep the fail-closed property: an abandoned body leaves the count above zero, so the
  page never reports done and the runner times out.
- `#[wasm_lite_test]` registers a libtest `#[test]` off wasm32, so one function is a native
  test and a browser test at once (see `examples/dual-demo/tests/dual.rs`). The older paired
  `cfg_attr` idiom still works and is still in the tree —
  `crates/wasm_lite_std/src/sync_tests.rs` — because off wasm32 the `cfg_attr` holding the
  macro is false, so it never runs; do not "fix" those files on sight. A *literal* `#[test]`
  alongside is only detected when written below the attribute: above it, rustc consumes it
  before the macro runs and the case registers twice. That is undetectable from the macro,
  so it is documented rather than guarded.
  `#[should_panic]` and `#[ignore]` are *read* off the function and left on it, so the same
  attribute keeps meaning what it says under libtest — never consume them. The one exception
  is an `async fn`, whose body is not itself the libtest test: there they are moved onto the
  generated test, or `#[ignore]` would run the case anyway. They
  travel to the runner as tab-separated fields on the harness-section record, which the
  codegen parses strictly: an unrecognised field is a hard error, because it means the macro
  and the parser are out of step.
- `#[wasm_lite_bench]` accepts `#[ignore]` but rejects `#[should_panic]` — a benchmark that
  panicked recorded no measurement.
- `cargo test` works on wasm-bindgen interop modules: the runner builds the interop bundle
  and serves a loader exporting `instantiate` instead of running on import. The wasm-bindgen
  CLI preserves our custom sections and `__wl_test_*` exports, which is what makes discovery
  work. Threads are the gap — an interop bundle has no worker bootstrap.

## Failure modes the runner diagnoses at build time

Three misconfigurations used to surface as unreadable runtime failures and are now build
errors; if you touch this area, keep them that way rather than relaxing them into warnings.

- **Missing thread exports.** A threaded build must export all **five** of `__stack_pointer`,
  `__tls_base`, `__tls_size`, `__tls_align`, `__wasm_init_tls`. Only three are read by
  anything here (`__stack_pointer`, `__tls_size`, `__wasm_init_tls`); `__tls_base` and
  `__tls_align` are required by the **wasm-bindgen CLI's** threading transform, so an interop
  build without them dies with `failed to find __tls_align`. Do not prune the list to what
  the glue reads — that was tried, and it broke every interop build while every native
  threaded test kept passing. The check demands all five unconditionally because a
  wasm-bindgen dependency arrives transitively (a dev-dependency four levels down is enough),
  so a crate cannot tell from its own manifest which path it is on. `docs/threads-and-async.md`
  has the per-symbol breakdown and how to check a graph; that is the reference to point users
  at, rather than re-deriving it.
- **`+atomics` without shared memory.** `Builder::spawn` selects on
  `#[cfg(target_feature = "atomics")]`, so enabling atomics compiles *out* the graceful
  `io::ErrorKind::Unsupported` path. A module that imports `__wl_spawn` but has unshared
  memory can therefore only fail at runtime — it is a contradiction, not a lesser build.
- Both checks key on evidence that cannot false-positive: a **shared** memory import, and the
  `__wl_spawn` import (absent when dead-code-eliminated). `__wl_thread_entry` is *not* usable
  as a marker — the core crate keeps it alive unconditionally, so keying on it faults every
  single-threaded module.
