# wasm_lite

A dependency-light rewrite of wasm-bindgen: bind JavaScript and Rust to each
other on `wasm32-unknown-unknown`, with no *runtime* dependencies and a small
host-side codegen tool. The core (`import!`, `JsValue`, runtime) and the codegen
are dependency-free; the proc-macros (`wasm_lite_macro`) use `syn`/`quote`, which
are build-time-only (zero bytes in the `.wasm`).

**Coming from wasm-bindgen?** See [MIGRATION.md](./MIGRATION.md) for a detailed
pros/cons comparison, a side-by-side "rosetta stone" of how to do X in each, and
the design trade-offs and gotchas to expect.

## Design goals

* Runner for major web browsers — **done** (WebDriver: Firefox/Chrome/Safari).
* Support with and without +atomics — **done**: shared-memory `+atomics` builds
  run, threads spawn onto Web Workers (`wasm_lite::thread::spawn`), and the
  `std::thread`-like layer `wasm_lite_std` (sync **and** async) sits on top.
* Unit test support — **done** (`#[wasm_lite_test]`, `cargo test` via a custom runner).
* Bind JS APIs to Rust and vice versa — **done** (`import!` / `#[export]`).
* Doctest support — **done** (runs rustdoc doctests in a browser).
* Simple, clean architecture — ongoing.
* Avoid dependencies — **mostly held**: zero *runtime* deps; the core crate and
  codegen pull nothing. The proc-macro crate uses `syn`/`quote` (build-time only)
  — a deliberate trade for typed parsing + hygienic codegen over hand-rolled
  token wrangling.

Nice to have:
* Interop with wasm-bindgen crates — **done** behind the `wasm-bindgen` feature.

## How it works

There is no `#[wasm_bindgen]`-style all-in-one macro. Instead the flow is:

1. **Rust side.** `import!`, `#[export]`, and `js_class!` emit the usual wasm
   imports/exports *plus* a descriptor line into a custom wasm section
   (`__wasm_lite_imports`, `__wl_exports`) describing each binding's ABI.
2. **Codegen.** The `wasm-lite` CLI reads those sections from the compiled
   `.wasm` and generates a matching JavaScript glue module — the import object
   the wasm expects, plus one wrapper per `#[export]`. No JS is hand-written
   per binding.
3. **Run.** The `runner` launches the module in a real browser over WebDriver,
   and doubles as a `cargo` test/run runner.

```
cargo build --target wasm32-unknown-unknown      # produces app.wasm
wasm-lite app.wasm -o glue.js                     # generates the JS glue
# import { instantiate, <your exports> } from "./glue.js"
```

A first taste of the binding macros:

```rust
wasm_lite::import! {
    "Math" { fn random() -> f64; }
}

#[wasm_lite::export]
pub fn greet(name: &str) -> String { format!("hello, {name}!") }
```

See the [binding model](./docs/binding-model.md) docs for the full story.

## Quickstart

**Prerequisites**

* A Rust toolchain and the wasm target: `rustup target add wasm32-unknown-unknown`.
* A WebDriver-capable browser on `PATH`: Firefox + `geckodriver`, or Chrome +
  `chromedriver`. The runner drives a *real* browser, so one must be installed.

**Run an example end to end**

The `runner` is a `cargo` runner: it reads the descriptor sections from your
compiled `.wasm`, generates the JS glue, serves it, and opens it in a browser
(for `cargo run`) or drives it headless and exits (for `cargo test`).

```bash
# 1. Build the runner once (from the workspace root).
cargo build -p runner

# 2. Point the wasm target at it. The examples already ship a .cargo/config.toml
#    that defaults `--target` to wasm32; you just supply the runner path.
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/runner"

# 3. Run an example in the browser (no `--target` needed; the example sets it).
cd examples/hello-rust
cargo run            # opens the module in your browser
cargo test           # runs any #[wasm_lite_test]s headless and exits
```

`examples/hello-rust` covers imports, handles, strings, bytes, and `js_class!`.
The other examples (see [Workspace layout](#workspace-layout)) build the same
way; the threaded/async ones additionally need **nightly + `-Z build-std`** and
the atomics link flags — see
[Threads, async & shared memory](./docs/threads-and-async.md) and the
`run-browser-tests.sh` script under `crates/wasm_lite_std/`.

**Wire it into your own crate**

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
runner = "/abs/path/to/wasm_lite/target/debug/runner"   # or set the env var above
```

Then `cargo run` / `cargo test` as in step 3. To generate glue *without* the
runner (e.g. to ship it yourself), use the `wasm-lite` CLI directly — this is the
manual form of what the runner automates:

```bash
cargo install --path crates/wasm_lite_cli      # provides the `wasm-lite` binary
cargo build --target wasm32-unknown-unknown    # produces target/.../app.wasm
wasm-lite app.wasm -o glue.js                  # generates the JS glue
# import { instantiate, <your exports> } from "./glue.js"
```

## Documentation

| doc | covers |
|---|---|
| [Binding model](./docs/binding-model.md) | `import!`, `#[export]`, `js_class!`, `JsValue`, type marshalling (`Option`/`Result`, strings, bytes, handles) |
| [Testing](./docs/testing.md) | `#[wasm_lite_test]` (and `(worker)`), `cargo test`/`cargo run` in-browser, doctests, the `wasm_lite_std` browser suite |
| [Threads, async & shared memory](./docs/threads-and-async.md) | `+atomics` builds, `thread::spawn`, `wasm_lite_std` (`Mutex`/`RwLock`/`Condvar`/`mpsc`, sync + async), the `spawn_local` executor, panic surfacing, the `std::time` veneer |
| [wasm-bindgen interop](./docs/interop.md) | the `wasm-bindgen` feature and `.to_wasm_bindgen()` / `.to_wasm_lite()` conversions |
| [Crate layering & roadmap](./docs/roadmap.md) | planned `wasm_lite_js`/`wasm_lite_web` split and known gaps |
| [Design notes](./docs/design-notes.md) | forward-looking strategy for running wasm_lite and wasm-bindgen (incl. wgpu) in one binary |
| [wasm-bindgen thread-ownership census](./docs/wasm-thread-ownership-census.md) | db-dump data: ~1% of the wasm-bindgen ecosystem owns wasm threads (backs the interop strategy) |
| [Migration guide](./MIGRATION.md) | moving from wasm-bindgen: pros/cons, rosetta stone, gotchas |

## Workspace layout

| crate | role |
|---|---|
| `crates/wasm_lite` | core: `import!`, `#[export]`, `js_class!`, `JsValue`, runtime (`__wl_malloc`/`__wl_free`, panic hook), `thread::spawn`, `console`/`performance`/`date` bindings |
| `crates/wasm_lite_macro` | proc-macros (`syn`/`quote`): `import!`, `#[export]`, `#[wasm_lite_test]` (`(worker)` runs the body on a Web Worker), `js_class!` (shared type→ABI dispatch in `ty`) |
| `crates/wasm_lite_codegen` | host-side: read descriptor sections, generate JS glue |
| `crates/wasm_lite_cli` | the `wasm-lite` binary wrapping codegen |
| `crates/wasm_lite_std` | std-like veneer (`std::thread`/`std::sync`/`std::time`, sync + async); ported from `wasm_safe_thread`, retargeted off wasm-bindgen onto `wasm_lite` + a `spawn_local` event-loop executor |
| `runner` | WebDriver runner; serves a bin interactively, or drives tests/doctests headless and exits |

Examples (each standalone, builds to `wasm32-unknown-unknown`):
`examples/hello-rust` (imports, handles, strings, bytes, `js_class!`),
`exports-demo` (Rust→JS exports), `tests-demo` (`#[wasm_lite_test]`),
`doctest-demo` (doctests), `interop` (wasm-bindgen bridge),
`atomics-demo` (shared memory + atomics; nightly),
`threads-demo` (`thread::spawn` over Web Workers; nightly),
`std-threads-demo` (`wasm_lite_std::spawn`, the std-like API; nightly),
`async-demo` (non-blocking `join_async` on the main thread; nightly),
`async-mutex-demo` (main `lock_async` woken cross-thread by a worker; nightly),
`async-doctest-demo` (a fail-closed async **doctest**; nightly),
`async-fail-demo` / `async-pass-demo` (fail-closed async-test verdict; nightly),
`panic-demo` (worker panic surfaced on the CLI; nightly),
`worker-spawn-local-demo` (a worker that itself spawn_locals async work; nightly).
