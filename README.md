# wasm_lite

A "smol"-style rewrite of wasm-bindgen: bind JavaScript and Rust to each other
on `wasm32-unknown-unknown`, with no proc-macro dependencies (no `syn`/`quote`)
and no runtime crates — just hand-rolled `macro_rules!`/`proc_macro` and a small
host-side codegen tool.

A checkout of wasm-bindgen is available in the `wasm-bindgen/` folder for reference.

## Design goals

* Runner for major web browsers — **done** (WebDriver: Firefox/Chrome/Safari).
* Support with and without +atomics — **done**: shared-memory `+atomics` builds
  run, and threads spawn onto Web Workers (`wasm_lite::thread::spawn`). A
  `std::thread`-like layer (`wasm_lite_std`) is the next step.
* Unit test support — **done** (`#[wasm_lite_test]`, `cargo test` via a custom runner).
* Bind JS APIs to Rust and vice versa — **done** (`import!` / `#[export]`).
* Doctest support — **done** (runs rustdoc doctests in a browser).
* Simple, clean architecture — ongoing.
* Avoid dependencies — **held** (zero third-party deps in the core path).

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

## Workspace layout

| crate | role |
|---|---|
| `crates/wasm_lite` | core: `import!`, `#[export]`, `js_class!`, `JsValue`, runtime (`__wl_malloc`/`__wl_free`, panic hook), `thread::spawn`, `console`/`performance` bindings |
| `crates/wasm_lite_macro` | zero-dep proc-macros: `#[export]`, `#[wasm_lite_test]`, `js_class!` |
| `crates/wasm_lite_codegen` | host-side: read descriptor sections, generate JS glue |
| `crates/wasm_lite_cli` | the `wasm-lite` binary wrapping codegen |
| `runner` | WebDriver runner; serves a bin interactively, or drives tests/doctests headless and exits |

Examples (each standalone, builds to `wasm32-unknown-unknown`):
`examples/hello-rust` (imports, handles, strings, bytes, `js_class!`),
`exports-demo` (Rust→JS exports), `tests-demo` (`#[wasm_lite_test]`),
`doctest-demo` (doctests), `interop` (wasm-bindgen bridge),
`atomics-demo` (shared memory + atomics; nightly),
`threads-demo` (`thread::spawn` over Web Workers; nightly).

## Binding model

**Import JS into Rust** — `import!`, grouped by JS namespace:

```rust
wasm_lite::import! {
    "Math" {
        fn random() -> f64;
        fn max2(a: f64, b: f64) -> f64 as "max";   // `as` decouples JS name -> overloads
    }
    "JSON" { fn parse(text: &str) -> JsValue; }     // returns an object handle
    "Array" { fn push(this: &JsValue, value: f64) -> f64; }  // method on a handle
}
```

Each binding gets a unique wasm import symbol (via `module_path!()`), so the
same JS function can be bound from many crates/modules without link conflicts.

**Export Rust to JS** — `#[export]`:

```rust
#[wasm_lite::export]
pub fn greet(name: &str) -> String { format!("hello, {name}!") }
// JS: import { greet } from "./glue.js"; greet("world")
```

**Typed object wrappers** — `js_class!` (a newtype over `JsValue`; methods lower
to `receiver[name](args)`, delegating all ABI work to `import!`):

```rust
wasm_lite::js_class! {
    type JsArray;
    impl JsArray {
        fn push(&self, value: f64) -> f64;
        fn join(&self, sep: &str) -> String;
        fn concat(&self, other: &JsArray) -> JsArray;  // typed arg + typed return
    }
}
```

**`JsValue`** is an opaque handle into a host-side value table; it is `!Send`/
`!Sync` (a handle is only meaningful in the realm that created it) and frees its
table slot on `Drop`.

### Type marshalling

Symmetric across imports and exports:

| type | import arg | import return | export arg | export return |
|---|---|---|---|---|
| numbers / `bool` | ✓ | ✓ | ✓ | ✓ |
| strings | `&str` | `String` | `&str` | `String` |
| bytes | `&[u8]` | `Vec<u8>` | `&[u8]` | `Vec<u8>` |
| JS objects | `&JsValue` | `JsValue` | `JsValue` | `JsValue` |

Strings/bytes are passed by allocating in wasm memory (`__wl_malloc`, align 1)
and handing over a packed `(ptr<<32 | len)` `i64`; ownership transfers to
whichever side allocated last. Objects cross as `u32` value-table indices.
The import/export asymmetry for objects is deliberate: an import *lends* Rust's
handle (`&JsValue`), an export *takes* ownership from JS (`JsValue` by value).

`Option<T>` and `Result<T, E>` are supported as **return** types (imports and
exports), where the scalar return ABI can't carry a discriminant. They use a
return pointer (sret): a 16-byte buffer holds a discriminant word plus the
payload at offset 8. `None` ↔ JS `null`; `Err(e)` ↔ a **thrown** JS exception
(`Ok`/`Some` carry the value). Inner types may be any scalar/string/bytes/handle.

`Option<T>` is also supported as an **argument** (a nullable parameter): it
flattens to a discriminant `i32` plus T's normal parameters. On exports JS
`null`/`undefined` → `None`; on imports `None` → JS `undefined` (so a JS default
parameter applies). `Result` arguments are *not* supported — JS has no `Result`
type, so there is no natural value to pass (this matches wasm-bindgen).

```rust
#[wasm_lite::export]
pub fn divide(a: f64, b: f64) -> Result<f64, String> {       // Err -> JS throw
    if b == 0.0 { Err("division by zero".into()) } else { Ok(a / b) }
}

wasm_lite::import! {
    "JSON" { fn try_parse(text: &str) -> Result<f64, JsValue> as "parse"; }  // JS throw -> Err
}
```

## Testing

```toml
# .cargo/config.toml
[target.wasm32-unknown-unknown]
runner = "path/to/runner"
```

* `#[wasm_lite_test]` marks a test; it is recorded in `__wasm_lite_tests` and
  the runner discovers and drives each one in a browser (pass / fail / panic).
* Plain `cargo run --example foo` serves the bin interactively in the browser;
  `cargo test` runs headless and exits — the runner distinguishes them by path.
* Doctests run too (rustdoc's doctest binaries are detected and driven headless).
  Call `wasm_lite::set_panic_hook()` at the top of a doctest so failures report
  the panic message.

## Shared memory & atomics

wasm_lite runs modules built with the threads-related wasm features
(`+atomics,+bulk-memory,+mutable-globals`) on a **shared** linear memory (a
`SharedArrayBuffer`). This is the foundation for threads; actually spawning work
onto Web Workers is not implemented yet, but everything below is in place:

* **Toolchain.** `+atomics` means `std` must be recompiled with it, so these
  builds need **nightly** and `-Z build-std`. See
  `examples/atomics-demo/.cargo/config.toml`: it sets the target features, links
  with `--shared-memory --max-memory=… --import-memory`, and adds
  `build-std = ["std", "panic_abort"]`. Build with `cargo +nightly run`.
* **Imported memory.** `--import-memory` makes the module import its memory
  rather than define it, so JS owns the one `WebAssembly.Memory` object (the same
  object every future worker will share). The codegen reads the module's imported
  memory limits and emits `makeMemory()` plus an `instantiate(url, memory?)` that
  creates the shared memory (or accepts one) and supplies it as an import.
* **Cross-origin isolation.** Browsers only hand out `SharedArrayBuffer` to
  cross-origin-isolated pages, so the runner serves
  `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Embedder-Policy: require-corp` on every response.
* **Init.** LLD emits a `start` function that sets up the main thread's TLS and
  initializes passive data segments on first instantiation — so single-threaded
  atomic code and `thread_local!` work with no manual setup.

`JsValue` is already `!Send`/`!Sync`: a handle indexes a per-realm value table,
so it is only valid on the worker that created it — the type system forbids
sending one across threads.

### Spawning threads

`wasm_lite::thread::spawn(move || { … })` runs a closure on a new Web Worker
sharing this module's compiled `WebAssembly.Module` and shared memory — **no
wasm-bindgen, js-sys, or web-sys**. The `Worker` lives entirely in the generated
glue behind a single `__wl_spawn` import:

* `spawn` boxes the closure (double-boxed to a thin `u32` pointer) and calls
  `__wl_spawn`.
* The glue allocates a fresh stack + TLS block (`__wl_thread_alloc`) and starts a
  worker, postMessaging `{ module, memory, work, stackTop, tlsPtr }`.
* The worker (a codegen-emitted bootstrap, `wl_worker.js`) instantiates the same
  module on the same memory, points `__stack_pointer` at the new stack, calls
  `__wasm_init_tls`, then `__wl_thread_entry` — which reconstitutes the closure
  and runs it. Threads coordinate via `core::sync::atomic`.

This requires the worker bootstrap to import the glue *without* re-running
`main`, so the runner serves the glue (`program.js`) separately from a small
bootstrap module. A threaded build must export the linker's thread symbols;
`examples/threads-demo/.cargo/config.toml` shows the flags
(`--export=__stack_pointer`, `__tls_size`, `__wasm_init_tls`, …).

Spawning is **detached** today (no `JoinHandle`); a `std::thread`-like layer with
`JoinHandle`/`park`/`Mutex`/`Condvar`/`mpsc` is planned as `wasm_lite_std`
(modelled on `wasm_safe_thread`).

## wasm-bindgen interop

Enable the `wasm-bindgen` feature to link a crate that itself uses wasm-bindgen.
`wasm_lite_codegen` runs the (version-matched) wasm-bindgen CLI, merges its
loader with our glue, and provides explicit `.to_wasm_bindgen()` /
`.to_wasm_lite()` conversions between the two `JsValue` types.

## Planned crate layering

Following the wasm-bindgen ecosystem split (language vs browser):

* `wasm_lite` — core (above). *Like `wasm-bindgen`.*
* `wasm_lite_js` *(future)* — ECMAScript built-ins (`Object`, `Array`, `Map`,
  `JSON`, `Date`, …) bound with `js_class!`. *Like `js-sys`.*
* `wasm_lite_web` *(future)* — Web/host APIs (DOM, `fetch`, …). *Like `web-sys`.*
* `wasm_lite_std` *(future)* — std-like veneer (e.g. `std::thread` on Workers).

Bindings stay out of core so it remains small; `js_class!` is the primitive all
upper layers build on.

## Known gaps / roadmap

* `js_class!`: constructors (`new Foo()`), property get/set (`el.textContent`),
  owned-object args, and `instanceof`-checked downcasting — each needs a new
  codegen shim kind. Constructors + properties are the prerequisite for starting
  `wasm_lite_js` / `wasm_lite_web`.
* `wasm_lite_std`: a `std::thread` + `std::sync` veneer (`spawn -> JoinHandle`,
  `park`/`unpark`, `Mutex`/`RwLock`/`Condvar`/`mpsc`) built on the detached
  `thread::spawn` primitive — modelled on (and likely a port of)
  `wasm_safe_thread`, with its wasm backend retargeted off wasm-bindgen onto
  `__wl_spawn`. (Shared memory, atomics, and detached spawn: done, above.)
* Nested generics like `Option<Vec<u8>>` on the import side (the macro grammar
  takes single-ident inner types today; the proc-macro export side already
  allows nesting). `Option<&[u8]>` arguments on the import side.
* `js_class!` constructors (`new Foo()`) + property get/set — the prerequisite
  for standing up `wasm_lite_js` / `wasm_lite_web`.
* Deployment niceties: a `wasm-lite bundle` command, session pooling/idle reaper
  for the persistent browser, test filtering (`cargo test NAME`).
