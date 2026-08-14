// SPDX-License-Identifier: MIT OR Apache-2.0
//! Emit one JS shim per `import!` binding.

use super::payload::write_payload;
use super::{js_string, next_param};
use crate::descriptor::{AbiArg, Descriptor, Kind, Ret};
use crate::exports::Payload;
use std::fmt::Write;

/// Emit one import shim: a JS function that unmarshals wasm params and calls the
/// real JS function — `globalThis[ns][js_name](args)` for a function, or
/// `receiver[js_name](args)` for a method (whose first argument is the receiver).
pub(super) fn emit_shim(js: &mut String, d: &Descriptor) {
    let ns = js_string(&d.namespace);
    let import_name = js_string(&d.import_name);

    // Build the wasm-level parameter list and the marshalled JS arguments. For a
    // method, the first (handle) argument is peeled off as the receiver.
    let mut params = Vec::new();
    let mut js_args = Vec::new();
    let mut receiver = None;

    for (i, arg) in d.args.iter().enumerate() {
        let is_receiver = i == 0
            && matches!(
                d.kind,
                Kind::Method
                    | Kind::Getter
                    | Kind::Setter
                    | Kind::IndexGet
                    | Kind::IndexSet
                    | Kind::IndexDelete
                    | Kind::InstanceOf
            );
        let marshalled = match arg {
            AbiArg::Str => {
                let ptr = next_param(&mut params);
                let len = next_param(&mut params);
                format!("__wl_str({ptr}, {len})")
            }
            AbiArg::Bytes => {
                // A transient view into wasm memory — valid for the call's duration.
                let ptr = next_param(&mut params);
                let len = next_param(&mut params);
                format!("new Uint8Array(__wl_memory.buffer, {ptr}, {len})")
            }
            // Like Bytes, a transient view into wasm memory. `len` is in
            // elements, so the typed array's own constructor does the scaling
            // and neither side needs to know the element size.
            AbiArg::Slice(elem) => {
                let ptr = next_param(&mut params);
                let len = next_param(&mut params);
                format!("new {}(__wl_memory.buffer, {ptr}, {len})", elem.js_array())
            }
            // A wasm i64 param arrives as a signed BigInt, which is already the
            // value Rust had.
            AbiArg::I64 => next_param(&mut params),
            // ...and for u64 the same bits need reading unsigned.
            AbiArg::U64 => format!("BigInt.asUintN(64, {})", next_param(&mut params)),
            // A run of table indices in wasm memory, read out as the objects
            // they denote. The view is transient, like the other slices.
            AbiArg::Handles => {
                let ptr = next_param(&mut params);
                let len = next_param(&mut params);
                format!("Array.from(new Uint32Array(__wl_memory.buffer, {ptr}, {len}), __wl_obj)")
            }
            AbiArg::Bool => format!("Boolean({})", next_param(&mut params)),
            AbiArg::Num => next_param(&mut params),
            // The wasm i32 param surfaces as a signed Number; reinterpret.
            AbiArg::U32 => format!("({} >>> 0)", next_param(&mut params)),
            AbiArg::Handle => format!("__wl_obj({})", next_param(&mut params)),
            AbiArg::Opt(p) => {
                // Leading discriminant param, then the inner payload's params.
                let pres = next_param(&mut params);
                let payload = match p {
                    // `Option<()>` occupies no parameter beyond the
                    // discriminant, so there is nothing to unmarshal.
                    Payload::Unit => "undefined".to_string(),
                    Payload::I32
                    | Payload::F64
                    | Payload::I8
                    | Payload::I16
                    | Payload::U8
                    | Payload::U16
                    | Payload::F32
                    | Payload::I64
                    | Payload::U64 => next_param(&mut params),
                    // The wasm i32 param surfaces as a signed Number; reinterpret.
                    Payload::U32 => format!("({} >>> 0)", next_param(&mut params)),
                    Payload::Bool => format!("Boolean({})", next_param(&mut params)),
                    Payload::Handle => format!("__wl_obj({})", next_param(&mut params)),
                    Payload::Str => {
                        let ptr = next_param(&mut params);
                        let len = next_param(&mut params);
                        format!("__wl_str({ptr}, {len})")
                    }
                    Payload::Bytes => {
                        let ptr = next_param(&mut params);
                        let len = next_param(&mut params);
                        format!("new Uint8Array(__wl_memory.buffer, {ptr}, {len})")
                    }
                };
                // `undefined` (not null) for None, so JS default-parameter
                // handling treats the argument as absent.
                format!("({pres} ? {payload} : undefined)")
            }
        };
        if is_receiver {
            receiver = Some(marshalled);
        } else {
            js_args.push(marshalled);
        }
    }

    // `variadic` spreads the last marshalled argument into the call.
    if d.variadic
        && let Some(last) = js_args.last_mut()
    {
        *last = format!("...{last}");
    }

    let params = params.join(", ");

    // `check_shape` has already fixed the arity of every kind that indexes
    // positionally here, so these expects are unreachable for any descriptor
    // that parsed.
    let recv = || receiver.clone().expect("kind has a receiver");
    let arg = |n: usize| {
        js_args
            .get(n)
            .cloned()
            .expect("check_shape fixed this kind's arity")
    };

    // The expression whose *value* is what the import yields. For calls that is
    // a call; for a property write it is an assignment, which in JS evaluates
    // to the assigned value — harmless, since setters are Ret::Void.
    // `js_name` may be a *path* — js-sys binds `Uint8Array.prototype.set.call`
    // — so each dotted segment becomes another lookup.
    let member = |base: &str| {
        let mut out = String::with_capacity(base.len() + d.js_name.len() + 4);
        out.push_str(base);
        for segment in d.js_name.split('.') {
            out.push('[');
            out.push_str(&js_string(segment));
            out.push(']');
        }
        out
    };

    let call = match d.kind {
        Kind::Function => format!(
            "{}({})",
            member(&format!("globalThis[{ns}]")),
            js_args.join(", ")
        ),
        Kind::Method => format!("{}({})", member(&recv()), js_args.join(", ")),
        Kind::Getter => member(&recv()),
        Kind::Setter => format!("{} = {}", member(&recv()), arg(0)),
        Kind::Constructor => format!("new {}({})", member("globalThis"), js_args.join(", ")),
        Kind::StaticGetter => member(&format!("globalThis[{ns}]")),
        Kind::IndexGet => format!("{}[{}]", recv(), arg(0)),
        Kind::IndexDelete => format!("delete {}[{}]", recv(), arg(0)),
        Kind::IndexSet => format!("{}[{}] = {}", recv(), arg(0), arg(1)),
        // Guarded: `instanceof` throws a TypeError if the right-hand side is
        // not a constructor, which is exactly what happens when the class is
        // missing from this engine. A downcast test should answer "no", not
        // kill the instance.
        Kind::InstanceOf => {
            let class = member("globalThis");
            format!(
                "(typeof {class} === \"function\" && {} instanceof {class})",
                recv()
            )
        }
    };

    // Ensure the namespace exists, then assign the shim by wasm import name.
    let _ = writeln!(
        js,
        "    imports[{ns}] = imports[{ns}] || Object.create(null);"
    );
    match &d.ret {
        // A handle return stores the result in the value table and yields its index.
        Ret::Handle => {
            let _ = writeln!(
                js,
                "    imports[{ns}][{import_name}] = ({params}) => __wl_add({call});"
            );
        }
        // A string return is copied into wasm memory; Rust takes ownership and frees.
        Ret::Str => {
            let _ = writeln!(
                js,
                "    imports[{ns}][{import_name}] = ({params}) => {{ \
                     const __r = {call}; \
                     const __b = new TextEncoder().encode(__r); \
                     const __p = __wl_instance.exports.__wl_malloc(__b.length); \
                     new Uint8Array(__wl_memory.buffer, __p, __b.length).set(__b); \
                     return (BigInt(__p) << 32n) | BigInt(__b.length); \
                 }};"
            );
        }
        // A bytes return: coerce to a Uint8Array, copy into wasm memory, hand off.
        Ret::Bytes => {
            let _ = writeln!(
                js,
                "    imports[{ns}][{import_name}] = ({params}) => {{ \
                     const __r = {call}; \
                     const __b = __r instanceof Uint8Array ? __r : new Uint8Array(__r); \
                     const __p = __wl_instance.exports.__wl_malloc(__b.length); \
                     new Uint8Array(__wl_memory.buffer, __p, __b.length).set(__b); \
                     return (BigInt(__p) << 32n) | BigInt(__b.length); \
                 }};"
            );
        }
        // sret returns: the shim writes a discriminant + payload into a leading
        // retptr buffer rather than returning a value.
        Ret::Opt(p) => {
            let shim_params = if params.is_empty() {
                "__retp".to_string()
            } else {
                format!("__retp, {params}")
            };
            let (prep, set) = write_payload(*p, "__retp + 8", "__r");
            let body = format!(
                "const __r = {call}; \
                 if (__r === null || __r === undefined) {{ new DataView(__wl_memory.buffer).setUint32(__retp, 0, true); }} \
                 else {{ {prep} const __dv = new DataView(__wl_memory.buffer); __dv.setUint32(__retp, 1, true); {set} }}",
                prep = prep.join(" "),
                set = set.join(" "),
            );
            let _ = writeln!(
                js,
                "    imports[{ns}][{import_name}] = ({shim_params}) => {{ {body} }};"
            );
        }
        Ret::Res(ok, err) => {
            let shim_params = if params.is_empty() {
                "__retp".to_string()
            } else {
                format!("__retp, {params}")
            };
            let (ok_prep, ok_set) = write_payload(*ok, "__retp + 8", "__r");
            let (err_prep, err_set) = write_payload(*err, "__retp + 8", "__e");
            let body = format!(
                "try {{ const __r = {call}; {ok_prep} const __dv = new DataView(__wl_memory.buffer); \
                 __dv.setUint32(__retp, 0, true); {ok_set} }} \
                 catch (__e) {{ {err_prep} const __dv = new DataView(__wl_memory.buffer); \
                 __dv.setUint32(__retp, 1, true); {err_set} }}",
                ok_prep = ok_prep.join(" "),
                ok_set = ok_set.join(" "),
                err_prep = err_prep.join(" "),
                err_set = err_set.join(" "),
            );
            let _ = writeln!(
                js,
                "    imports[{ns}][{import_name}] = ({shim_params}) => {{ {body} }};"
            );
        }
        // As `Ret::Res`, plus a third outcome: `null`/`undefined` from a
        // successful call means `Ok(None)`.
        Ret::ResOpt(ok, err) => {
            let shim_params = if params.is_empty() {
                "__retp".to_string()
            } else {
                format!("__retp, {params}")
            };
            let (ok_prep, ok_set) = write_payload(*ok, "__retp + 8", "__r");
            let (err_prep, err_set) = write_payload(*err, "__retp + 8", "__e");
            let body = format!(
                "try {{ const __r = {call}; \
                 if (__r === null || __r === undefined) {{ \
                     new DataView(__wl_memory.buffer).setUint32(__retp, 2, true); \
                 }} else {{ \
                     {ok_prep} const __dv = new DataView(__wl_memory.buffer); \
                     __dv.setUint32(__retp, 0, true); {ok_set} \
                 }} }} \
                 catch (__e) {{ {err_prep} const __dv = new DataView(__wl_memory.buffer); \
                 __dv.setUint32(__retp, 1, true); {err_set} }}",
                ok_prep = ok_prep.join(" "),
                ok_set = ok_set.join(" "),
                err_prep = err_prep.join(" "),
                err_set = err_set.join(" "),
            );
            let _ = writeln!(
                js,
                "    imports[{ns}][{import_name}] = ({shim_params}) => {{ {body} }};"
            );
        }
        // A wasm i64 result must be a BigInt in range. `BigInt(..)` is a no-op
        // on a value that already is one and rescues a JS API that hands back a
        // Number, which would otherwise be a TypeError at the boundary;
        // `asIntN` wraps a u64 above 2^63 into the signed representation Rust
        // reinterprets.
        Ret::Value(tag) if tag == "i64" || tag == "u64" => {
            let _ = writeln!(
                js,
                "    imports[{ns}][{import_name}] = ({params}) => BigInt.asIntN(64, BigInt({call}));"
            );
        }
        Ret::Void | Ret::Value(_) => {
            let _ = writeln!(
                js,
                "    imports[{ns}][{import_name}] = ({params}) => {call};"
            );
        }
    }
}
