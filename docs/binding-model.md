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

**Operations that aren't calls** — a JS binding surface needs property access,
`new`, and computed indexing as well. None of these can be inferred from a Rust
signature (`fn tag_name(this: &JsValue) -> String` reads identically to a
zero-argument method), so each is requested with an attribute:

```rust
wasm_lite::import! {
    "URL" {
        #[constructor] fn new_url(spec: &str) -> JsValue as "URL";  // new URL(spec)
        #[getter]      fn pathname(this: &JsValue) -> String;       // url.pathname
        #[setter]      fn set_hash(this: &JsValue, v: &str) as "hash";  // url.hash = v
    }
    "Array" {
        #[indexing_getter] fn at(this: &JsValue, i: u32) -> f64;       // arr[i]
        #[indexing_setter] fn put(this: &JsValue, i: u32, v: f64);     // arr[i] = v
        #[indexing_deleter] fn remove_at(this: &JsValue, i: u32) -> bool; // delete arr[i]
        #[instanceof]      fn is_array(this: &JsValue) -> bool as "Array";
    }
    "Math" {
        #[static_getter]   fn pi() -> f64 as "PI";                     // Math.PI
    }
}
```

`#[static_getter]` is the receiver-less property read — `Math.PI` is a constant,
not a function, so `Kind::Function` would call it and throw.

`#[instanceof]` is the type test a checked downcast needs: given an opaque
handle, decide whether it really is the class you are about to treat it as. It
is guarded, so a class this engine does not define answers `false` rather than
throwing a `TypeError` (bare `x instanceof undefined` does).

Getting this wrong is not a subtle mismatch: emitting `el.tagName()` for a
property read throws, so the shapes are checked in the macro *and* in the
descriptor parser — a getter takes only the receiver and must return; a setter
takes receiver plus value and must not; a constructor must return a handle.

There is deliberately no static-method kind: `Klass.method(args)` is already a
namespaced function with the class as the namespace.

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

**Rust closures into JS** — `Closure` is the dynamic counterpart of `#[export]`:
it wraps a closure, captured state and all, as a real JS function value, which
is what an event listener or a callback argument needs.

```rust
let mut count = 0;
let cb = wasm_lite::Closure::new(move || { count += 1; });
some_import(cb.as_js_value());
cb.forget();   // hand it to JS for the life of the realm
```

JS receives an **id** into a thread-local registry, not a pointer. Dropping the
`Closure` removes the entry, so a JS reference that outlives it — a listener
nobody unregistered — calls a no-op instead of reading freed memory. Calling a
closure takes it out of the registry for the duration, so re-entrant calls
no-op rather than aliasing `&mut` to the captured state.

Four shapes: `Closure::new` (no arguments), `Closure::new_with_arg` (one),
`Closure::new_variadic` (takes `&[JsValue]`, may return one), and
`Closure::new_variadic_fallible`, whose `Err` becomes a **thrown** JS exception
at the call site — which is how a JS API reports failure, and something a Rust
closure cannot do by itself.

The variadic forms are what a general binding layer needs —
`Array.prototype.sort` passes two arguments and `find` passes three — and they
avoid a trampoline per (arity × return type) combination.

**`JsValue` is `Clone`.** The value table holds references, so cloning
allocates a table slot rather than copying the object: both handles denote the
same JS value and each frees only its own slot.

**The JS singletons are constants** — `JsValue::UNDEFINED`/`NULL`/`TRUE`/
`FALSE` occupy reserved table slots, so naming one costs nothing and they are
never freed or reallocated.

**`JsValue` supports JavaScript's operators** (`+`, `-`, `*`, `/`, `%`, `&`,
`|`, `^`, `<<`, `>>`, unary `-`/`!`, plus `unsigned_shr` and `bit_not` for the
JS-only `>>>` and `~`). These are *JavaScript's* semantics, not Rust's: `+`
concatenates strings, `/` yields `Infinity`, and the bitwise operators coerce
to 32-bit integers.

**`JsValue` also answers the questions JS code asks about a value** —
`PartialOrd` (`None` for `NaN`, which is why it is not `Ord`), `loose_eq` for
`==` against `PartialEq`'s `===`, `pow`, and the `is_object`/`is_function`/
`is_string`/`is_null`/`is_undefined`/`is_truthy`/`is_bigint` predicates.

**Primitives cross both ways** — `JsValue::from_f64`/`from_bool`/`from_str`/
`null()`/`undefined()` (plus `From` impls for the numeric types, `bool` and
`&str`) make one; `as_f64()`/`as_bool()`/`as_string()` read one back, returning
`None` when the handle holds a different type. Presence is tracked separately
from the value, so a genuine `NaN` is `Some(NaN)` and `""` is `Some("")`.

**Awaiting JS promises** — `JsFuture` wraps a promise as a Rust `Future`
resolving to `Result<JsValue, JsValue>`: `Ok` for fulfilled, `Err` for
rejected, the same split as binding a throwing import as `Result<_, JsValue>`.

```rust
let value = wasm_lite::JsFuture::new(&promise).await;
```

It is built on `Closure` — the two `then` callbacks are closures the future
owns — which is also what makes cancellation safe: dropping a pending
`JsFuture` drops those callbacks, so the promise settles with nowhere to
report instead of writing into freed state. Driving one needs an executor;
on wasm that is `wasm_lite_std::spawn_local`.

## Type marshalling

Symmetric across imports and exports:

| type | import arg | import return | export arg | export return |
|---|---|---|---|---|
| numbers / `bool` | ✓ | ✓ | ✓ | ✓ |
| strings | `&str` | `String` | `&str` | `String` |
| bytes | `&[u8]` | `Vec<u8>` | `&[u8]` | `Vec<u8>` |
| numeric slices | `&[f32]`, `&[u32]`, … | — | — | — |
| JS objects | `&JsValue` | `JsValue` | `JsValue` | `JsValue` |

Numeric slices (`i8`/`i16`/`u16`/`i32`/`u32`/`i64`/`u64`/`f32`/`f64`; `&[u8]`
keeps its own `bytes` spelling) become a typed-array view over wasm memory — `&[f32]` arrives
as a `Float32Array`. The length crosses in *elements*, so the typed array's
constructor does the scaling. Like `&[u8]`, the view is only valid for the
duration of the call.

They are **arguments only**. That direction is a borrowed view of memory Rust
already aligned; the return direction would have the host allocate through
`__wl_malloc`, which is align-1, and a `Float32Array` cannot view an unaligned
offset. Returning one needs an aligned allocator first.

Strings/bytes are passed by allocating in wasm memory (`__wl_malloc`, align 1)
and handing over a packed `(ptr<<32 | len)` `i64`; ownership transfers to
whichever side allocated last. Objects cross as `u32` value-table indices.
The import/export asymmetry for objects is deliberate: an import *lends* Rust's
handle (`&JsValue`), an export *takes* ownership from JS (`JsValue` by value).

Integers up to 32 bits cross as wasm `i32`/`f32`/`f64` and reach JS as Numbers.
`i64`/`u64` cross as wasm `i64`, which the WebAssembly JS API surfaces as a
**`BigInt`** — the only faithful mapping, since a Number cannot hold the range.
`u32`/`usize`/`u64` need reinterpreting on the JS side, because the wasm
parameter is signed.

`Option<T>` and `Result<T, E>` are supported as **return** types (imports and
exports), where the scalar return ABI can't carry a discriminant. They use a
return pointer (sret): a 16-byte buffer holds a discriminant word plus the
payload at offset 8. `None` ↔ JS `null`; `Err(e)` ↔ a **thrown** JS exception
(`Ok`/`Some` carry the value). Inner types may be any scalar (every integer
width, including the 64-bit pair through `BigInt`), string, bytes or handle,
or `()` — `Result<(), E>` is a fallible operation with nothing to hand back, and
its buffer carries only the discriminant.

`Option<T>` is also supported as an **argument** (a nullable parameter): it
flattens to a discriminant `i32` plus T's normal parameters. On exports JS
`null`/`undefined` → `None`; on imports `None` → JS `undefined` (so a JS default
parameter applies). `Result` arguments are *not* supported — JS has no `Result`
type, so there is no natural value to pass (this matches
[wasm-bindgen](https://wasm-bindgen.github.io/wasm-bindgen/)).

```rust
#[wasm_lite::export]
pub fn divide(a: f64, b: f64) -> Result<f64, String> {       // Err -> JS throw
    if b == 0.0 { Err("division by zero".into()) } else { Ok(a / b) }
}

wasm_lite::import! {
    "JSON" { fn try_parse(text: &str) -> Result<f64, JsValue> as "parse"; }  // JS throw -> Err
}
```
