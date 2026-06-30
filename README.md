# wasm_lite

![logo](art/logo.png)

A dependency-light rewrite of wasm-bindgen: bind JavaScript and Rust to each
other on `wasm32-unknown-unknown`, with no *runtime* dependencies and a small
host-side codegen tool.

The core crate (`import!`, `JsValue`, runtime helpers) and the host codegen are
dependency-free. The proc-macros (`wasm_lite_macro`) use `syn`/`quote`, which
are build-time-only and add zero bytes to the final `.wasm`.

**Coming from wasm-bindgen?** See [MIGRATION.md](./MIGRATION.md) for a detailed
pros/cons comparison, a side-by-side "rosetta stone" of how to do X in each, and
the design trade-offs and gotchas to expect.

## Why wasm_lite?

Use `wasm_lite` when you want Rust/JavaScript bindings on
`wasm32-unknown-unknown`, but want the runtime side to stay small and explicit:
zero runtime dependencies in the core crate and codegen, descriptor-driven JS
glue, and a browser runner that treats `cargo run`, `cargo test`, and doctests as
real browser executions. This is the short version of the
[migration guide](./MIGRATION.md), [roadmap](./docs/roadmap.md), and
[interop notes](./docs/interop.md).

The main alternatives are still good tools:

| tool | best fit |
|---|---|
| [`wasm-bindgen`](https://wasm-bindgen.github.io/wasm-bindgen/) | the mature Rust/JS binding ecosystem: rich JS types, closures, classes, `js-sys`/`web-sys`, and TypeScript output |
| [`wasm-pack`](https://wasm-bindgen.github.io/wasm-pack/) | packaging and publishing Rust-generated wasm into npm-oriented JavaScript workflows |
| [Component Model / WIT](https://component-model.bytecodealliance.org/) | language-neutral component interfaces, WASI, composition, and tooling such as `wit-bindgen` and `jco` |
| raw `WebAssembly.instantiate` | tiny ABIs that only need numeric imports/exports and handwritten JavaScript |

`wasm_lite` is intentionally narrower than `wasm-bindgen`: it favors a small,
auditable binding model and zero runtime deps over maximal Web API coverage. It
is a good fit for libraries or applications that only need a focused binding
surface, want browser tests without a JS test harness, or need the
`wasm_lite_std` thread/async path over shared memory.

The trade-off, called out in the migration guide and roadmap, is that `wasm_lite`
does **not** yet replace the broad `js-sys`/`web-sys` ecosystem, Promise interop
(`JsFuture` / `wasm-bindgen-futures`), Rust closures passed into JS, TypeScript
declaration generation, or rich serde-style marshalling. The `wasm-bindgen`
feature supports incremental migration in the direction where `wasm-lite` is the
final codegen step; the reverse direction, where a wasm-bindgen/wasm-pack app
consumes a wasm_lite leaf without running `wasm-lite`, is still roadmap work.

Prefer `wasm-bindgen` when you need its mature ecosystem surface today. Prefer
Component Model tooling when your primary goal is language-neutral component
composition rather than a browser-first Rust/JS binding layer.

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
  `chromedriver`. The runner drives a *real* browser.

### Run an example

The `runner` is a `cargo` runner: it reads descriptor sections from your
compiled `.wasm`, generates the JS glue, serves it, and opens it in a browser
for `cargo run` or drives it headless for `cargo test`.

```bash
# 1. Build the runner once from the workspace root.
cargo build -p runner

# 2. Point the wasm target at it. The examples already ship a .cargo/config.toml
#    that defaults `--target` to wasm32; you just supply the runner path.
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/runner"

# 3. Run an example in the browser.
cd examples/hello-rust
cargo run
cargo test
```

`examples/hello-rust` covers imports, handles, strings, bytes, and `js_class!`.
The other examples build the same way, except the threaded/async examples need
**nightly + `-Z build-std`** and atomics link flags. See
[Threads, async & shared memory](./docs/threads-and-async.md) and
`crates/wasm_lite_std/run-browser-tests.sh`.

### Add it to a crate

```toml
# Cargo.toml
[dependencies]
wasm_lite = "0.1"
```

```toml
# .cargo/config.toml
[build]
target = "wasm32-unknown-unknown"

[target.wasm32-unknown-unknown]
runner = "/abs/path/to/wasm_lite/target/debug/runner"
```

You can also set `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER` instead of putting
the runner path in `.cargo/config.toml`.

### Generate glue manually

The runner automates this, but the `wasm-lite` CLI can generate the JS glue
directly:

```bash
cargo install --path crates/wasm_lite_cli
cargo build --target wasm32-unknown-unknown
wasm-lite app.wasm -o glue.js
# import { instantiate, <your exports> } from "./glue.js"
```

## How It Works

There is no `#[wasm_bindgen]`-style all-in-one macro. Instead:

1. **Rust side.** `import!`, `#[export]`, and `js_class!` emit normal wasm
   imports/exports *plus* a descriptor line into a custom wasm section
   (`__wasm_lite_imports`, `__wl_exports`) describing each binding's ABI.
2. **Codegen.** The `wasm-lite` CLI reads those sections from the compiled
   `.wasm` and generates a matching JavaScript glue module: the import object
   the wasm expects, plus one wrapper per `#[export]`.
3. **Run.** The `runner` launches the module in a real browser over WebDriver,
   and doubles as a `cargo` test/run runner.

```bash
cargo build --target wasm32-unknown-unknown
wasm-lite app.wasm -o glue.js
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
| `JsValue` | opaque handle to a JavaScript value owned by the host value table |
| `set_panic_hook` | report wasm panic messages through `console.error` |

The core crate also exposes these modules:

| module | role |
|---|---|
| `console` | `console.log` / `console.error` bindings |
| `date` | `Date.now()` binding |
| `performance` | `performance.now()` binding |
| `thread` | raw cross-thread primitives; prefer `wasm_lite_std` for the full `std::thread` + `std::sync` surface |
| `interop` | optional `wasm-bindgen` feature: conversions to/from `wasm_bindgen::JsValue` |

## Documentation

| doc | covers |
|---|---|
| [Binding model](./docs/binding-model.md) | `import!`, `#[export]`, `js_class!`, `JsValue`, type marshalling (`Option`/`Result`, strings, bytes, handles) |
| [Testing](./docs/testing.md) | `#[wasm_lite_test]` (and `(worker)`), `cargo test`/`cargo run` in-browser, doctests, the `wasm_lite_std` browser suite |
| [Threads, async & shared memory](./docs/threads-and-async.md) | `+atomics` builds, `thread::spawn`, `wasm_lite_std` (`Mutex`/`RwLock`/`Condvar`/`mpsc`, sync + async), the `spawn_local` executor, panic surfacing, the `std::time` veneer |
| [wasm-bindgen interop](./docs/interop.md) | the `wasm-bindgen` feature and `.to_wasm_bindgen()` / `.to_wasm_lite()` conversions |
| [Crate layering & roadmap](./docs/roadmap.md) | planned `wasm_lite_js`/`wasm_lite_web` split and known gaps |
| [Design notes](./docs/design-notes.md) | forward-looking strategy for running wasm_lite and wasm-bindgen, including wgpu, in one binary |
| [wasm-bindgen thread-ownership census](./docs/wasm-thread-ownership-census.md) | db-dump data: about 1% of the wasm-bindgen ecosystem owns wasm threads; backs the interop strategy |
| [Migration guide](./MIGRATION.md) | moving from wasm-bindgen: pros/cons, rosetta stone, gotchas |

## Workspace

| crate | role |
|---|---|
| `crates/wasm_lite` | core: `import!`, `#[export]`, `js_class!`, `JsValue`, runtime (`__wl_malloc`/`__wl_free`, panic hook), `thread::spawn`, `console`/`performance`/`date` bindings |
| `crates/wasm_lite_macro` | proc-macros (`syn`/`quote`): `import!`, `#[export]`, `#[wasm_lite_test]`, `js_class!`; shared type-to-ABI dispatch lives in `ty` |
| `crates/wasm_lite_codegen` | host-side: read descriptor sections, generate JS glue |
| `crates/wasm_lite_cli` | the `wasm-lite` binary wrapping codegen |
| `crates/wasm_lite_std` | std-like veneer (`std::thread`/`std::sync`/`std::time`, sync + async); ported from `wasm_safe_thread`, retargeted off wasm-bindgen onto `wasm_lite` + a `spawn_local` event-loop executor |
| `runner` | WebDriver runner; serves a bin interactively, or drives tests/doctests headless and exits |

## Examples

Each example is a standalone crate that builds to `wasm32-unknown-unknown`:

| example | covers |
|---|---|
| `examples/hello-rust` | imports, handles, strings, bytes, `js_class!` |
| `examples/exports-demo` | Rust-to-JS exports |
| `examples/tests-demo` | `#[wasm_lite_test]` |
| `examples/doctest-demo` | browser-driven doctests |
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

## Design Goals

* Runner for major web browsers: **done** (WebDriver: Firefox/Chrome/Safari).
* Support with and without `+atomics`: **done**. Shared-memory `+atomics` builds
  run, threads spawn onto Web Workers (`wasm_lite::thread::spawn`), and the
  `std::thread`-like layer `wasm_lite_std` (sync **and** async) sits on top.
* Unit test support: **done** (`#[wasm_lite_test]`, `cargo test` via a custom runner).
* Bind JS APIs to Rust and vice versa: **done** (`import!` / `#[export]`).
* Doctest support: **done** (runs rustdoc doctests in a browser).
* Simple, clean architecture: ongoing.
* Avoid dependencies: **mostly held**. The core crate and codegen have zero
  runtime dependencies. The proc-macro crate uses `syn`/`quote` at build time
  for typed parsing and hygienic codegen.
* Interop with wasm-bindgen crates: **done** behind the `wasm-bindgen` feature.
