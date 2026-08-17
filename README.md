# wasm_lite

![logo](art/wasm_lite.png)

Browser-first Rust/JavaScript bindings for `wasm32-unknown-unknown`, with
real-browser tests/doctests, first-class threads, and zero runtime dependencies.

`wasm_lite` is intentionally narrower than
[wasm-bindgen](https://wasm-bindgen.github.io/wasm-bindgen/). It focuses on the
path where one tool owns the browser loop end to end: codegen, local serving,
WebDriver launch, `cargo run`, `cargo test`, rustdoc doctests, worker bootstrap,
logs, and panics. Atomics, Web Workers, and std-like browser concurrency through
`wasm_lite_std` are part of that target, not a separate afterthought.

Put differently: this is not an IE6-era compatibility project. WebAssembly
already implies a modern-enough runtime; `wasm_lite` leans into that instead of
carrying legacy script-tag, no-module, CommonJS, and bundler-specific branches
through every layer.

The proc-macros (`wasm_lite_macro`) use `syn`/`quote`, which are build-time-only
and add zero bytes to the final `.wasm`.

**Coming from wasm-bindgen?** See the [migration guide](./MIGRATION.md) for a
detailed pros/cons comparison, a side-by-side "rosetta stone" of how to do X in each, and
the design trade-offs and gotchas to expect.

## Why wasm_lite?

Use `wasm_lite` when your main wasm target is a modern browser and you want the
Rust side of that browser app to stay small, explicit, and testable:
zero runtime dependencies in the core crate and codegen, binding descriptors in
custom wasm sections, generated ES-module glue, and one runner that owns serving,
WebDriver launch, tests, doctests, logs, panics, and worker bootstrap.

That narrower target is the point. `wasm_lite` is optimized for projects that:

* ship to modern browsers rather than Node CommonJS, IE-era script loading,
  legacy no-module scripts, or a matrix of bundler-specific outputs;
* need shared-memory `+atomics` builds, Web Workers, or std-like browser
  concurrency through `wasm_lite_std`;
* want `cargo test` and rustdoc doctests to run in a real browser, with useful
  logs and panic messages in the CLI;
* prefer a small, auditable binding model over a broad generated Web API
  surface.

This is the short version of the [migration guide](./MIGRATION.md),
[roadmap](./docs/roadmap.md), and [interop notes](./docs/interop.md).

The main alternatives are still good tools:

| tool | best fit |
|---|---|
| [`wasm-bindgen`](https://wasm-bindgen.github.io/wasm-bindgen/) | the mature Rust/JS binding ecosystem: rich JS types, closures, classes, `js-sys`/`web-sys`, and TypeScript output |
| [`wasm-pack`](https://wasm-bindgen.github.io/wasm-pack/) | packaging and publishing Rust-generated wasm into npm-oriented JavaScript workflows |
| [Component Model / WIT](https://component-model.bytecodealliance.org/) | language-neutral component interfaces, WASI, composition, and tooling such as `wit-bindgen` and `jco` |
| raw `WebAssembly.instantiate` | tiny ABIs that only need numeric imports/exports and handwritten JavaScript |

The trade-off is intentional. `wasm_lite` does **not** yet replace the broad
`js-sys`/`web-sys` ecosystem, TypeScript declaration generation, or rich
serde-style marshalling. Three things it *does* now have: Rust closures passed
into JS (`Closure`, zero- and one-argument signatures), awaiting JS promises
(`JsFuture`), and the `fetch` API built on both. The `wasm-bindgen` feature
supports incremental migration in the direction where `wasm_lite` is the final
codegen step. Bounded package-substitution shims cover both host directions too:
[`backend-wasm-bindgen/`](./backend-wasm-bindgen) runs a wasm_lite-authored leaf on the **wasm-bindgen backend**,
while [`backend-wasm-lite/`](./backend-wasm-lite) is the **wasm_lite backend** for
wasm-bindgen code, lowering the supported
wasm-bindgen ecosystem surface onto wasm_lite.

Prefer `wasm-bindgen` when you need its mature ecosystem surface today. Prefer
`wasm_lite` when the browser path itself is the product surface you want the
tooling to own: atomics, workers, testing, doctests, logging, panics, and small
bindings. Prefer Component Model tooling when your primary goal is
language-neutral component composition rather than a browser-first Rust/JS
binding layer.

## Project Goals

`wasm_lite` is opinionated about the target. It is not trying to generate every
JavaScript packaging shape; the main backend is **modern browsers**.

* **Modern browsers first.** The generated glue is an ES module, the runner
  serves it over HTTP, and shared-memory pages get COOP/COEP headers. We do not
  currently target Node CommonJS, IE-era script loading, legacy no-module
  scripts, or every bundler mode. The upside is that browser behavior, module
  workers, `SharedArrayBuffer`, cross-origin isolation, and WebDriver testing
  can be handled directly.
* **Atomics and threads first-class.** Shared-memory `+atomics` builds are not
  an edge case: codegen creates shared `WebAssembly.Memory`, emits a
  module-worker bootstrap, and the runner serves it with cross-origin isolation.
* **Std-like browser abstractions.** `wasm_lite_std` provides the
  `std::thread`/`std::sync`/`std::time` slice that browser wasm is missing:
  `spawn`, `JoinHandle`, `Mutex`, `RwLock`, `Condvar`, `mpsc`, `Instant`, and
  `SystemTime`, with sync and async paths where the browser main thread cannot
  block.
* **First-class testing.** The same runner drives `cargo run`, `cargo test`,
  and rustdoc doctests in a real browser. Harness tests run one page load per
  test; async tests are fail-closed so a dropped task, panic, or hang cannot
  accidentally pass.
* **First-class logging and panic surfacing.** Panic hooks and generated glue
  route logs through the browser console, bridge worker console output back to
  the main realm, and print useful panic output in the CLI instead of a bare
  `unreachable` trap.
* **One server/runner path.** Local serving, generated glue, worker bootstrap
  files, browser launch, test execution, console capture, and failure reporting
  live in one runner instead of separate JS harnesses per mode.

These goals explain several choices that are deliberately different from
wasm-bindgen. wasm-bindgen supports many output targets (`bundler`, `web`,
`nodejs`, `no-modules`, Deno, and module variants), but that breadth creates
target-specific caveats: JS snippets only work for some targets, threaded wasm
needs particular target modes and hand-shaped worker shims, and `wasm-bindgen`
tests default to Node unless the suite asks for a browser. `wasm_lite` narrows
the target so the browser runner, atomics, worker startup, doctests, and logging
can be designed as one path. Giving up legacy/no-module/CJS targets means the
glue can stay one ES-module loader, worker startup can use module workers, the
runner can always serve the headers shared memory needs, and tests/log capture do
not need a separate implementation for every JavaScript packaging format.

## Example

Declare JavaScript imports with `import!`, and export Rust functions with
`#[wasm_lite::export]`:

```rust
wasm_lite::import! {
    "Math" { fn random() -> f64; }
}

#[wasm_lite::export]
pub fn greet(name: &str) -> String { format!("hello, {name}!") }
```

No per-binding JavaScript is hand-written. The compiled `.wasm` carries binding
descriptors in custom sections, and the host-side codegen emits matching JS
glue.

## Quickstart

### Prerequisites

* A Rust toolchain and the wasm target:
  `rustup target add wasm32-unknown-unknown`.
* A WebDriver-capable browser on `PATH`: Firefox + `geckodriver`, or Chrome +
  `chromedriver`. The runner drives a *real* browser, and defaults to **Firefox**
  — `WASM_LITE_BROWSER=chrome` switches, which anything using **WebGPU** needs
  (Firefox has none headless), along with `WASM_LITE_GPU=1`. See
  [configure the runner](docs/testing.md#configure-the-runner) for that and the
  rest; several of the defaults suit a small DOM test rather than a real
  application.

### Run an example

`wasm_lite run` is a `cargo` runner: it reads descriptor sections from your
compiled `.wasm`, generates the JS glue, serves it, and opens it in a browser
for `cargo run` or drives it headless for `cargo test`.

```bash
# 1. Build the CLI once from the workspace root.
cargo build -p wasm_lite_cli

# 2. Point the wasm target at it. The examples already ship a .cargo/config.toml
#    that defaults `--target` to wasm32; you just supply the command.
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/wasm_lite run"

# 3. Run an example in the browser.
cd examples/hello-rust
cargo run
cargo test
```

Working from a checkout, that is; anywhere else, `cargo install wasm_lite_cli`
and use `wasm_lite run`, which needs no path at all.

`examples/hello-rust` covers imports, handles, strings, bytes, and `js_class!`.
The other examples build the same way. Examples that spawn workers need
**nightly + `-Z build-std`** and atomics link flags; local async work through
`spawn_local`, `JsFuture`, and `sleep_async` also works in an ordinary stable,
non-atomic build. See
[Threads, async & shared memory](./docs/threads-and-async.md) and
`crates/wasm_lite_std/run-browser-tests.sh`.

### Add it to a crate

```toml
# Cargo.toml
[dependencies]
wasm_lite = "0.1"
```

```bash
cargo install wasm_lite_cli    # provides the `wasm_lite` command
```

```toml
# .cargo/config.toml
[build]
target = "wasm32-unknown-unknown"

[target.wasm32-unknown-unknown]
runner = ["wasm_lite", "run"]
```

You can also set `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="wasm_lite run"`
instead of putting it in `.cargo/config.toml`.

### Generate glue manually

The runner automates this, but the `wasm_lite` CLI can generate the JS glue
directly:

```bash
cargo install wasm_lite_cli
cargo build --target wasm32-unknown-unknown
wasm_lite build app.wasm -o glue.js
# import { instantiate, <your exports> } from "./glue.js"
```

## How It Works

There is no `#[wasm_bindgen]`-style all-in-one macro. Instead:

1. **Rust side.** `import!`, `#[export]`, and `js_class!` emit normal wasm
   imports/exports *plus* a descriptor line into a custom wasm section
   (`__wasm_lite_imports`, `__wl_exports`) describing each binding's ABI.
   `#[wasm_lite_test]` and `#[wasm_lite_bench]` record harness entries in their
   own discovery sections.
2. **Codegen.** `wasm_lite build` reads those sections from the compiled
   `.wasm` and generates a matching JavaScript glue module: the import object
   the wasm expects, plus one wrapper per `#[export]`.
3. **Run.** `wasm_lite run` launches the module in a real browser over
   WebDriver, and doubles as a `cargo` test/run runner.

```bash
cargo build --target wasm32-unknown-unknown
wasm_lite build app.wasm -o glue.js
```

See the [binding model](./docs/binding-model.md) docs for the full ABI story.

## Crate API

The `wasm_lite` crate provides the core binding surface:

| item | role |
|---|---|
| `import!` | declare imported JavaScript functions grouped by namespace |
| `#[export]` | export Rust functions to JavaScript callers |
| `js_class!` | define typed `JsValue` wrappers |
| `#[wasm_lite_test]` | register browser-driven wasm tests; `(worker)` runs the body on a Web Worker |
| `#[wasm_lite_bench]` / `Bencher` | register and measure browser-driven benchmarks |
| `JsValue` | opaque handle to a JavaScript value owned by the host value table |
| `Closure` | pass Rust closures to JavaScript callbacks |
| `JsFuture` | await JavaScript promises from Rust |
| `set_panic_hook` | report wasm panic messages through `console.error` |

The core crate also exposes these modules:

| module | role |
|---|---|
| `console` | `console.log` / `console.error` bindings |
| `date` | `Date.now()` binding |
| `performance` | `performance.now()` binding |
| `timer` | host timers used by non-blocking sleeps and scheduling |
| `event` | browser event-listener bindings |
| `fetch` | Fetch + Streams bindings |
| `websocket` | WebSocket bindings |
| `dom` | focused DOM bindings |
| `thread` | raw cross-thread primitives; prefer `wasm_lite_std` for the full `std::thread` + `std::sync` surface |
| `interop` | optional `wasm-bindgen` feature: conversions to/from `wasm_bindgen::JsValue` |

## Documentation

| doc | covers |
|---|---|
| [Binding model](./docs/binding-model.md) | `import!`, `#[export]`, `js_class!`, `JsValue`, type marshalling (`Option`/`Result`, strings, bytes, handles) |
| [Testing and benchmarking](./docs/testing.md) | `#[wasm_lite_test]`, `#[wasm_lite_bench]`, `(worker)`, `cargo test`/`cargo bench` in-browser, doctests, the `wasm_lite_std` browser suite |
| [Threads, async & shared memory](./docs/threads-and-async.md) | `+atomics` builds, `thread::spawn`, `wasm_lite_std` (`Mutex`/`RwLock`/`Condvar`/`mpsc`, sync + async), the `spawn_local` executor, panic surfacing, the `std::time` veneer |
| [wasm-bindgen interop](./docs/interop.md) | the `wasm-bindgen` feature and `.to_wasm_bindgen()` / `.to_wasm_lite()` conversions |
| [Crate layering & roadmap](./docs/roadmap.md) | planned `wasm_lite_js`/`wasm_lite_web` split and known gaps |
| [**Running wgpu / unmodified wasm-bindgen crates**](./backend-wasm-lite/README.md) | the **fake-wasm-bindgen shim**: substitute it graph-wide and unmodified `js-sys`/`web-sys`/`wgpu` compile on wasm_lite |
| [Design notes](./docs/design-notes.md) | the coexistence options and which have shipped; strategy for wasm_lite and wasm-bindgen in one binary |
| [wasm-bindgen thread-ownership census](./docs/wasm-thread-ownership-census.md) | db-dump data: about 1% of the wasm-bindgen ecosystem owns wasm threads; backs the interop strategy |
| [Migration guide](./MIGRATION.md) | moving from wasm-bindgen: pros/cons, rosetta stone, gotchas |

## Workspace

| crate | role |
|---|---|
| `crates/wasm_lite` | core: `import!`, `#[export]`, `js_class!`, `JsValue`, runtime (`__wl_malloc`/`__wl_free`, panic hook), `thread::spawn`, `console`/`performance`/`date`/`fetch`/`websocket`/`dom` bindings |
| `crates/wasm_lite_macro` | proc-macros (`syn`/`quote`): `import!`, `#[export]`, `#[wasm_lite_test]`, `#[wasm_lite_bench]`, `js_class!`; shared type-to-ABI dispatch lives in `ty` |
| `crates/wasm_lite_codegen` | host-side: read binding/test/benchmark sections; generate export wrappers, worker glue, and interop bundles |
| `crates/wasm_lite_cli` | the `wasm_lite` binary: `build` writes glue plus bundle-specific worker and wasm-bindgen interop artifacts; `run` serves a bin interactively, or drives tests/doctests/benchmarks headless and exits |
| `crates/wasm_lite_std` | std-like veneer (`std::thread`/`std::sync`/`std::time`, sync + async); atomics builds use workers while stable non-atomic wasm uses a local event-loop executor |
| `backend-wasm-bindgen/` | separate workspace: the **wasm-bindgen backend** — substitutes named `wasm_lite` / `wasm_lite_std` but implemented on real wasm-bindgen, so a wasm_lite-authored leaf can live under a wasm-bindgen host |
| `backend-wasm-lite/` | separate workspace: wasm-bindgen's API lowered onto wasm_lite, so unmodified `js-sys`/`web-sys`/`wgpu` compile here; substituted via `[patch.crates-io]` and never published |

## Examples

Each example is a standalone crate that builds to `wasm32-unknown-unknown`:

| example | covers |
|---|---|
| `examples/hello-rust` | imports, handles, strings, bytes, `js_class!` |
| `examples/exports-demo` | Rust-to-JS exports |
| `examples/tests-demo` | `#[wasm_lite_test]` |
| `examples/doctest-demo` | browser-driven doctests |
| `examples/reexport-demo` | forwarding the macros through a wrapper crate's re-exports |
| `examples/must-fail-demo` | fixtures that must *fail*; driven by `scripts/wasm32/negative` |
| `examples/bench-demo` | `#[wasm_lite_bench]` and sync/async browser benchmarks |
| `examples/interop` | wasm-bindgen bridge |
| `examples/atomics-demo` | shared memory + atomics; nightly |
| `examples/threads-demo` | `thread::spawn` over Web Workers; nightly |
| `examples/std-threads-demo` | `wasm_lite_std::spawn`, the std-like API; nightly |
| `examples/async-demo` | non-blocking `join_async` on the main thread; nightly |
| `examples/async-mutex-demo` | main-thread `lock_async` woken cross-thread by a worker; nightly |
| `examples/async-doctest-demo` | fail-closed async doctest; nightly |
| `examples/async-fail-demo` / `examples/async-pass-demo` | fail-closed async-test verdict; nightly |
| `examples/panic-demo` | worker panic surfaced on the CLI; nightly |
| `examples/worker-spawn-local-demo` | a worker that itself `spawn_local`s async work; nightly |

## Status

* Modern-browser runner: **done** (WebDriver: Firefox/Chrome/Safari).
* `+atomics` / shared-memory builds: **done**; threads spawn onto Web Workers.
* Std-like thread/sync/time veneer: **done** in `wasm_lite_std` (sync and async).
* Unit tests, doctests, and benchmarks in-browser: **done**.
* Rust/JS imports and exports: **done** (`import!` / `#[export]`).
* Logging and panic surfacing to the CLI: **done** for main-thread failures,
  joined workers, detached-worker warnings, and doctests with `set_panic_hook`.
* Simple, clean architecture: ongoing.
* Avoid dependencies: **mostly held**. The core crate and codegen have zero
  runtime dependencies. The proc-macro crate uses `syn`/`quote` at build time
  for typed parsing and hygienic codegen.
* Interop with wasm-bindgen crates: **done** behind the `wasm-bindgen` feature
  when `wasm_lite` owns final codegen; bounded `[patch]` shims exist for both
  host directions, while a general reverse glue post-pass remains roadmap work.
