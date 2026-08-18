# wasm_lite

![logo](art/wasm_lite.png)

Browser-first Rust/JavaScript bindings for `wasm32-unknown-unknown`, with
real-browser tests/doctests, first-class threads, and zero runtime dependencies.

One tool owns the whole browser loop: it generates the JS glue from your compiled
`.wasm`, serves it, launches a real browser over WebDriver, and drives
`cargo run`, `cargo test`, `cargo bench`, and rustdoc doctests through it —
including shared-memory `+atomics` builds, where Rust threads land on Web
Workers and `wasm_lite_std` fills in the `std::thread`/`std::sync`/`std::time`
slice the browser is missing.

**Coming from wasm-bindgen?** The [migration guide](./MIGRATION.md) has a
side-by-side rosetta stone, an honest pros/cons list, and the gotchas.

## A taste

```rust
use wasm_lite::{JsValue, export, import, js_class, wasm_lite_test};

// Import JavaScript into Rust. Strings, bytes, numbers, Option, and JS object
// handles all cross the boundary; `as "..."` decouples the Rust name from the
// JS one.
import! {
    "JSON" {
        fn parse(text: &str) -> JsValue;
        fn stringify(value: &JsValue) -> String;
    }
    "Number" {
        fn parse_int(s: &str, radix: Option<f64>) -> f64 as "parseInt";
    }
}

// Give a JS object a typed Rust wrapper.
js_class! {
    type JsArray;
    impl JsArray {
        fn push(&self, value: f64) -> f64;
        fn join(&self, sep: &str) -> String;
    }
}

// Export Rust to JavaScript. `Err` becomes a thrown exception on the JS side.
#[export]
pub fn divide(a: f64, b: f64) -> Result<f64, String> {
    if b == 0.0 { Err("division by zero".into()) } else { Ok(a / b) }
}

// Tests run in a real browser under `cargo test` (and as ordinary libtest tests
// off wasm32). An `async fn` body is driven fail-closed: it can't pass by
// being dropped.
#[wasm_lite_test]
fn round_trips_through_json() {
    let arr = JsArray::from_js(parse("[1, 2]"));
    assert_eq!(arr.push(3.0), 3.0);
    assert_eq!(arr.join(","), "1,2,3");
    assert_eq!(parse_int("ff", Some(16.0)), 255.0);
}
```

No JavaScript is hand-written for any of that. Each binding leaves a small
descriptor in a custom section of the `.wasm`; the `wasm_lite` CLI reads those
back out and emits the matching glue.

## Quickstart

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm_lite_cli          # provides the `wasm_lite` command
```

```toml
# Cargo.toml
[dependencies]
wasm_lite = "0.1"
```

```toml
# .cargo/config.toml — make wasm32 the default target and hand cargo the runner
[build]
target = "wasm32-unknown-unknown"

[target.wasm32-unknown-unknown]
runner = ["wasm_lite", "run"]
```

Then `cargo run` opens your bin in a browser, and `cargo test` drives your
`#[wasm_lite_test]`s and doctests headless and exits with the verdict.
(`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="wasm_lite run"` in the
environment works instead of the config file.)

You need a WebDriver-capable browser on `PATH`: Firefox + `geckodriver` (the
default) or Chrome + `chromedriver` (`WASM_LITE_BROWSER=chrome`; also what
WebGPU needs, with `WASM_LITE_GPU=1`, since headless Firefox has none). The
other knobs — timeouts, reusing one browser across runs, serving extra assets,
running a bin without opening a browser — are in
[configure the runner](./docs/testing.md#configure-the-runner).

**From a checkout**, build the CLI once and point the runner at it; the
examples already default `--target` to wasm32:

```bash
cargo build -p wasm_lite_cli
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/wasm_lite run"
cd examples/hello-rust && cargo run && cargo test
```

`examples/` holds one standalone crate per feature — imports and handles,
exports, tests, doctests, benchmarks, wasm-bindgen interop, and the nightly
atomics/threads family. Threads and shared memory need **nightly +
`-Z build-std`** and the link flags in
[threads & async](./docs/threads-and-async.md); async work that stays on one
thread (`spawn_local`, `JsFuture`, `sleep_async`) is fine on stable.

**Without the runner**, the CLI generates the glue directly:

```bash
cargo build --target wasm32-unknown-unknown
wasm_lite build target/wasm32-unknown-unknown/debug/app.wasm -o glue.js
# import { instantiate, divide } from "./glue.js"
```

## What you get

* **Bindings.** `import!` for JS functions grouped by namespace, `#[export]`
  for Rust functions, `js_class!` for typed `JsValue` wrappers, `Closure` to
  hand Rust callbacks to JS, `JsFuture` to await JS promises. Strings, bytes,
  numbers, `Option`, `Result`, and live JS handles marshal in both directions
  over a small, auditable ABI. Focused `console`, `dom`, `event`, `fetch`,
  `websocket`, `timer`, `date`, and `performance` modules cover the common
  browser calls. → [binding model](./docs/binding-model.md)
* **Testing in a real browser.** `#[wasm_lite_test]` runs under `cargo test`
  on wasm32 and as a plain libtest test elsewhere; `#[should_panic]` and
  `#[ignore]` keep their meaning. Rustdoc doctests run in the browser too.
  `async fn` tests are fail-closed, `(worker)` moves a body onto a Web Worker
  where it may block, and `#[wasm_lite_bench]` measures in-browser.
  → [testing and benchmarking](./docs/testing.md)
* **Threads and shared memory.** `+atomics` builds get a shared
  `WebAssembly.Memory`, a module-worker bootstrap, and COOP/COEP serving out
  of the box. `wasm_lite_std` provides `spawn`, `JoinHandle`, `Mutex`,
  `RwLock`, `Condvar`, `mpsc`, `Instant`, and `SystemTime`, with sync and
  async variants because the browser main thread cannot block; on stable,
  non-atomic wasm it degrades to a local event-loop executor.
  → [threads, async & shared memory](./docs/threads-and-async.md)
* **Panics and logs where you can see them.** Console output — from workers
  too — is bridged to the CLI, and a panic prints its message instead of a bare
  `unreachable` trap. Misconfigured threaded builds are diagnosed at build time
  rather than as runtime mysteries.
* **wasm-bindgen interop, both ways.** The `wasm-bindgen` feature converts
  between the two `JsValue`s when `wasm_lite` owns final codegen. Two package
  substitution shims cover the host directions:
  [`backend-wasm-lite/`](./backend-wasm-lite) lowers wasm-bindgen's API onto
  wasm_lite so unmodified `js-sys`/`web-sys`/`wgpu` compile here, and
  [`backend-wasm-bindgen/`](./backend-wasm-bindgen) runs wasm_lite-authored
  code on real wasm-bindgen. → [interop](./docs/interop.md),
  [running wgpu on wasm_lite](./backend-wasm-lite/README.md)
* **Zero runtime dependencies.** The core crate and codegen depend on nothing.
  The proc-macros use `syn`/`quote` at build time, which adds zero bytes to the
  `.wasm`; `wasm_lite_std` adds `atomic-waker`.

The scope is deliberate: the target is **modern browsers**, and the generated
glue is one ES module. There is no Node CommonJS, no-module, or bundler-specific
output, and no TypeScript declarations. Giving those up is what lets one loader,
one worker bootstrap, one server, and one test harness cover everything above.

## How it works

There is no `#[wasm_bindgen]`-style all-in-one macro.

1. **Rust side.** `import!`, `#[export]`, and `js_class!` emit ordinary wasm
   imports/exports *plus* a text descriptor of each binding's ABI into a custom
   wasm section. `#[wasm_lite_test]` and `#[wasm_lite_bench]` register
   themselves the same way.
2. **Codegen.** `wasm_lite build` reads those sections out of the compiled
   `.wasm` — a dependency-free wasm reader and text parser — and writes the JS
   glue: the import object the module expects, one wrapper per export, and the
   worker bootstrap when memory is shared.
3. **Run.** `wasm_lite run` does that on the fly, serves the result with the
   headers shared memory needs, opens it in a browser over WebDriver, and
   collects the console and test verdicts. `cargo run` and `cargo test` are told
   apart by the artifact path, so one runner serves both.

The full ABI — how strings, bytes, `Option`, `Result`, and handles cross the
boundary — is in [binding model](./docs/binding-model.md).

## When to use something else

`wasm_lite` does not try to replace the breadth of the wasm-bindgen ecosystem:
there is no generated `js-sys`/`web-sys`-scale surface, no TypeScript output,
and no serde-style marshalling.

| tool | best fit |
|---|---|
| [`wasm-bindgen`](https://wasm-bindgen.github.io/wasm-bindgen/) | the mature Rust/JS binding ecosystem: rich JS types, `js-sys`/`web-sys`, TypeScript output, many packaging targets |
| [`wasm-pack`](https://wasm-bindgen.github.io/wasm-pack/) | packaging and publishing Rust-generated wasm into npm-oriented JavaScript workflows |
| [Component Model / WIT](https://component-model.bytecodealliance.org/) | language-neutral component interfaces, WASI, composition, and tooling such as `wit-bindgen` and `jco` |
| raw `WebAssembly.instantiate` | tiny ABIs that only need numeric imports/exports and handwritten JavaScript |

Prefer `wasm-bindgen` when you need its ecosystem surface today. Prefer
`wasm_lite` when the browser path itself — atomics, workers, tests, doctests,
logs, panics, small explicit bindings — is what you want the tooling to own.
The [migration guide](./MIGRATION.md) goes through this in detail.

## Documentation

| doc | covers |
|---|---|
| [Binding model](./docs/binding-model.md) | `import!`, `#[export]`, `js_class!`, `JsValue`, type marshalling (`Option`/`Result`, strings, bytes, handles) |
| [Testing and benchmarking](./docs/testing.md) | `#[wasm_lite_test]`, `#[wasm_lite_bench]`, `(worker)`, `cargo test`/`cargo bench` in-browser, doctests, configuring the runner |
| [Threads, async & shared memory](./docs/threads-and-async.md) | `+atomics` builds, `thread::spawn`, `wasm_lite_std`, the `spawn_local` executor, panic surfacing, the `std::time` veneer |
| [wasm-bindgen interop](./docs/interop.md) | the `wasm-bindgen` feature and `.to_wasm_bindgen()` / `.to_wasm_lite()` conversions |
| [Running wgpu / unmodified wasm-bindgen crates](./backend-wasm-lite/README.md) | the wasm_lite backend for wasm-bindgen code: substitute it graph-wide and `js-sys`/`web-sys`/`wgpu` compile on wasm_lite |
| [Migration guide](./MIGRATION.md) | moving from wasm-bindgen: pros/cons, rosetta stone, gotchas |
| [Roadmap & status](./docs/roadmap.md) | what is done, what is planned, crate layering, known gaps |
| [Design notes](./docs/design-notes.md) | coexistence strategies for wasm_lite and wasm-bindgen in one binary, and which have shipped |

The workspace itself is described in [`AGENTS.md`](./AGENTS.md).

## License

MIT OR Apache-2.0, at your option.
