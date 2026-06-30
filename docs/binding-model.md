# Binding model

*(Part of the [wasm_lite](../README.md) docs. See also: [testing](./testing.md),
[threads & async](./threads-and-async.md), [interop](./interop.md),
[roadmap](./roadmap.md), [migration guide](../MIGRATION.md).)*

The binding model follows the same browser-first goals as the rest of the
project: keep the runtime dependency graph empty, make the wasm ABI small enough
to audit, and let the host-side codegen/runner own the browser-specific glue.
Instead of an all-in-one `#[wasm_bindgen]` macro, Rust emits normal wasm
imports/exports plus descriptors in custom sections; `wasm-lite` reads those
descriptors after compilation and generates the ES-module loader, wrappers, and
worker-aware runtime shims.

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

## Type marshalling

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
