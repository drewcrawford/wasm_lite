# wasm_lite

A "smol"-style rewrite of wasm-bindgen: bind JavaScript and Rust to each other
on `wasm32-unknown-unknown`, with no proc-macro dependencies (no `syn`/`quote`)
and no runtime crates — just hand-rolled `macro_rules!`/`proc_macro` and a small
host-side codegen tool.

A checkout of wasm-bindgen is available in the `wasm-bindgen/` folder for reference.

## Design goals

* Runner for major web browsers — **done** (WebDriver: Firefox/Chrome/Safari).
* Support with and without +atomics — *not started.*
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
| `crates/wasm_lite` | core: `import!`, `#[export]`, `js_class!`, `JsValue`, runtime (`__wl_malloc`/`__wl_free`, panic hook), `console`/`performance` bindings |
| `crates/wasm_lite_macro` | zero-dep proc-macros: `#[export]`, `#[wasm_lite_test]`, `js_class!` |
| `crates/wasm_lite_codegen` | host-side: read descriptor sections, generate JS glue |
| `crates/wasm_lite_cli` | the `wasm-lite` binary wrapping codegen |
| `runner` | WebDriver runner; serves a bin interactively, or drives tests/doctests headless and exits |

Examples (each standalone, builds to `wasm32-unknown-unknown`):
`examples/hello-rust` (imports, handles, strings, bytes, `js_class!`),
`exports-demo` (Rust→JS exports), `tests-demo` (`#[wasm_lite_test]`),
`doctest-demo` (doctests), `interop` (wasm-bindgen bridge).

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

`Option<T>` and `Result<T, E>` are also supported as **return** types (imports
and exports), where the scalar return ABI can't carry a discriminant. They use a
return pointer (sret): a 16-byte buffer holds a discriminant word plus the
payload at offset 8. `None` ↔ JS `null`; `Err(e)` ↔ a **thrown** JS exception
(`Ok`/`Some` carry the value). Inner types may be any scalar/string/bytes/handle.

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
* `+atomics` / threads (and the `wasm_lite_std` layer over Workers).
* `Option`/`Result` *arguments* (returns are done), and nested generics like
  `Option<Vec<u8>>` on the import side (the macro grammar takes single-ident
  inner types today; the proc-macro export side already allows nesting).
* Deployment niceties: a `wasm-lite bundle` command, session pooling/idle reaper
  for the persistent browser, test filtering (`cargo test NAME`).
