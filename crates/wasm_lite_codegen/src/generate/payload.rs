// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::exports::Payload;

/// The export call for an sret return: the buffer `__ret` is the leading argument.
pub(super) fn sret_call(wasm_name: &str, wasm_args: &[String]) -> String {
    let mut args = vec!["__ret".to_string()];
    args.extend(wasm_args.iter().cloned());
    format!("__wl_instance.exports[{wasm_name}]({})", args.join(", "))
}

/// Read one sret payload from the buffer at JS offset `off`. Returns the
/// statements to run (using a `__dv` DataView already in scope) and the final
/// value expression. `sfx` disambiguates locals between Ok/Err payloads.
pub(super) fn read_payload(p: Payload, off: &str, sfx: &str) -> (Vec<String>, String) {
    match p {
        // Nothing was written, so there is nothing to read; the discriminant
        // already carried the whole answer.
        Payload::Unit => (vec![], "undefined".to_string()),
        Payload::I8 => (vec![], format!("__dv.getInt8({off})")),
        Payload::I16 => (vec![], format!("__dv.getInt16({off}, true)")),
        Payload::U8 => (vec![], format!("__dv.getUint8({off})")),
        Payload::U16 => (vec![], format!("__dv.getUint16({off}, true)")),
        // 64-bit payloads are BigInts on the JS side, as everywhere else.
        Payload::I64 => (vec![], format!("__dv.getBigInt64({off}, true)")),
        Payload::U64 => (vec![], format!("__dv.getBigUint64({off}, true)")),
        Payload::F32 => (vec![], format!("__dv.getFloat32({off}, true)")),
        Payload::I32 => (vec![], format!("__dv.getInt32({off}, true)")),
        Payload::U32 => (vec![], format!("__dv.getUint32({off}, true)")),
        Payload::F64 => (vec![], format!("__dv.getFloat64({off}, true)")),
        Payload::Bool => (vec![], format!("Boolean(__dv.getInt32({off}, true))")),
        Payload::Handle => (
            vec![format!(
                "const __h_{sfx} = __dv.getUint32({off}, true); const __o_{sfx} = __wl_obj(__h_{sfx}); __wl_drop(__h_{sfx});"
            )],
            format!("__o_{sfx}"),
        ),
        Payload::Str => (
            vec![format!(
                "const __p_{sfx} = __dv.getUint32({off}, true), __l_{sfx} = __dv.getUint32(({off}) + 4, true); \
                 const __s_{sfx} = __wl_str(__p_{sfx}, __l_{sfx}); __wl_instance.exports.__wl_free(__p_{sfx}, __l_{sfx});"
            )],
            format!("__s_{sfx}"),
        ),
        Payload::Bytes => (
            vec![format!(
                "const __p_{sfx} = __dv.getUint32({off}, true), __l_{sfx} = __dv.getUint32(({off}) + 4, true); \
                 const __b_{sfx} = new Uint8Array(__wl_memory.buffer, __p_{sfx}, __l_{sfx}).slice(); \
                 __wl_instance.exports.__wl_free(__p_{sfx}, __l_{sfx});"
            )],
            format!("__b_{sfx}"),
        ),
    }
}

/// Write `val` (of payload type `p`) into the sret buffer at JS offset `off`.
/// Returns `(prep, set)`: `prep` runs first (may `malloc`, which can grow memory
/// and detach views); `set` runs after a fresh `__dv` DataView is created.
pub(super) fn write_payload(p: Payload, off: &str, val: &str) -> (Vec<String>, Vec<String>) {
    match p {
        // Nothing to store, but the value is still evaluated: for a `Result` it
        // is the call itself, and skipping it would skip the side effect the
        // binding exists for.
        Payload::Unit => (vec![format!("void ({val});")], vec![]),
        Payload::I8 => (vec![], vec![format!("__dv.setInt8({off}, {val});")]),
        Payload::I16 => (vec![], vec![format!("__dv.setInt16({off}, {val}, true);")]),
        Payload::U8 => (vec![], vec![format!("__dv.setUint8({off}, {val});")]),
        Payload::U16 => (vec![], vec![format!("__dv.setUint16({off}, {val}, true);")]),
        Payload::I64 => (
            vec![],
            vec![format!("__dv.setBigInt64({off}, BigInt({val}), true);")],
        ),
        Payload::U64 => (
            vec![],
            vec![format!("__dv.setBigUint64({off}, BigInt({val}), true);")],
        ),
        Payload::F32 => (
            vec![],
            vec![format!("__dv.setFloat32({off}, {val}, true);")],
        ),
        Payload::I32 => (vec![], vec![format!("__dv.setInt32({off}, {val}, true);")]),
        Payload::U32 => (vec![], vec![format!("__dv.setUint32({off}, {val}, true);")]),
        Payload::F64 => (
            vec![],
            vec![format!("__dv.setFloat64({off}, {val}, true);")],
        ),
        Payload::Bool => (
            vec![],
            vec![format!("__dv.setInt32({off}, ({val}) ? 1 : 0, true);")],
        ),
        Payload::Handle => (
            vec![format!("const __wh = __wl_add({val});")],
            vec![format!("__dv.setUint32({off}, __wh, true);")],
        ),
        Payload::Str => (
            vec![format!(
                "const __wb = new TextEncoder().encode({val}); \
                 const __wp = __wl_instance.exports.__wl_malloc(__wb.length); \
                 new Uint8Array(__wl_memory.buffer, __wp, __wb.length).set(__wb);"
            )],
            vec![format!(
                "__dv.setUint32({off}, __wp, true); __dv.setUint32(({off}) + 4, __wb.length, true);"
            )],
        ),
        Payload::Bytes => (
            vec![format!(
                "const __wb = ({val} instanceof Uint8Array ? {val} : new Uint8Array({val})); \
                 const __wp = __wl_instance.exports.__wl_malloc(__wb.length); \
                 new Uint8Array(__wl_memory.buffer, __wp, __wb.length).set(__wb);"
            )],
            vec![format!(
                "__dv.setUint32({off}, __wp, true); __dv.setUint32(({off}) + 4, __wb.length, true);"
            )],
        ),
    }
}
