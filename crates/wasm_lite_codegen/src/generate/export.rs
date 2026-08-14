// SPDX-License-Identifier: MIT OR Apache-2.0
//! Emit one JS wrapper per `#[wasm_lite::export]` function.

use super::js_string;
use super::payload::{read_payload, sret_call};
use crate::exports::{Export, ExportArg, ExportRet, Payload};
use std::fmt::Write;

pub(super) fn emit_export(js: &mut String, export: &Export, index: usize) {
    let params: Vec<String> = (0..export.args.len()).map(|i| format!("p{i}")).collect();

    let mut lines = Vec::new();
    // Refuse re-entry after an escaped import exception: the shadow stack
    // pointer was not restored, so running more wasm would be silent UB.
    lines.push("__wl_check_live();".to_string());
    let mut wasm_args = Vec::new();
    let mut frees = Vec::new();
    for (i, arg) in export.args.iter().enumerate() {
        match arg {
            ExportArg::Num => wasm_args.push(format!("p{i}")),
            ExportArg::Str => {
                lines.push(format!("const [__a{i}p, __a{i}l] = __wl_pass_str(p{i});"));
                wasm_args.push(format!("__a{i}p"));
                wasm_args.push(format!("__a{i}l"));
                frees.push(format!(
                    "__wl_instance.exports.__wl_free(__a{i}p, __a{i}l);"
                ));
            }
            ExportArg::Bytes => {
                lines.push(format!("const [__a{i}p, __a{i}l] = __wl_pass_bytes(p{i});"));
                wasm_args.push(format!("__a{i}p"));
                wasm_args.push(format!("__a{i}l"));
                frees.push(format!(
                    "__wl_instance.exports.__wl_free(__a{i}p, __a{i}l);"
                ));
            }
            ExportArg::Handle => {
                // Register the object; Rust owns the handle and frees the slot
                // itself when its JsValue drops, so JS does not free it here.
                lines.push(format!("const __a{i}h = __wl_add(p{i});"));
                wasm_args.push(format!("__a{i}h"));
            }
            ExportArg::Opt(p) => {
                // A nullable arg: discriminant (null/undefined → 0) + payload.
                lines.push(format!(
                    "const __s{i} = (p{i} === null || p{i} === undefined) ? 0 : 1;"
                ));
                wasm_args.push(format!("__s{i}"));
                match p {
                    // `Option<()>` carries nothing beyond its discriminant.
                    Payload::Unit => {}
                    Payload::I32
                    | Payload::U32
                    | Payload::F64
                    | Payload::I8
                    | Payload::I16
                    | Payload::U8
                    | Payload::U16
                    | Payload::F32 => {
                        wasm_args.push(format!("(__s{i} ? p{i} : 0)"));
                    }
                    // A 64-bit wasm param is a BigInt, so its absent value has
                    // to be one too.
                    Payload::I64 | Payload::U64 => {
                        wasm_args.push(format!("(__s{i} ? BigInt(p{i}) : 0n)"));
                    }
                    Payload::Bool => wasm_args.push(format!("(__s{i} ? (p{i} ? 1 : 0) : 0)")),
                    Payload::Handle => {
                        lines.push(format!("const __a{i}h = __s{i} ? __wl_add(p{i}) : 0;"));
                        wasm_args.push(format!("__a{i}h"));
                    }
                    Payload::Str | Payload::Bytes => {
                        let pass = if matches!(p, Payload::Str) {
                            "__wl_pass_str"
                        } else {
                            "__wl_pass_bytes"
                        };
                        // ptr/len default to 0 (a null pointer + zero length → None
                        // on the Rust side); __wl_free(0, 0) is a no-op.
                        lines.push(format!("let __a{i}p = 0, __a{i}l = 0; if (__s{i}) {{ [__a{i}p, __a{i}l] = {pass}(p{i}); }}"));
                        wasm_args.push(format!("__a{i}p"));
                        wasm_args.push(format!("__a{i}l"));
                        frees.push(format!(
                            "__wl_instance.exports.__wl_free(__a{i}p, __a{i}l);"
                        ));
                    }
                }
            }
        }
    }

    let wasm_name = js_string(&format!("__wl_export_{}", export.name));
    let call = format!(
        "__wl_instance.exports[{wasm_name}]({})",
        wasm_args.join(", ")
    );

    match export.ret {
        ExportRet::Void => {
            lines.push(format!("{call};"));
            lines.extend(frees);
        }
        ExportRet::Value => {
            lines.push(format!("const __ret = {call};"));
            lines.extend(frees);
            lines.push("return __ret;".into());
        }
        ExportRet::U32 => {
            // The wasm i32 result surfaces as a signed Number; reinterpret.
            lines.push(format!("const __ret = {call};"));
            lines.extend(frees);
            lines.push("return __ret >>> 0;".into());
        }
        ExportRet::Bool => {
            lines.push(format!("const __ret = {call};"));
            lines.extend(frees);
            lines.push("return Boolean(__ret);".into());
        }
        ExportRet::Str => {
            lines.push(format!("const __ret = {call};"));
            lines.extend(frees);
            // The shim returns a packed i64 (BigInt in JS): (ptr << 32) | len.
            lines.push("const __packed = BigInt.asUintN(64, __ret);".into());
            lines.push(
                "const __p = Number(__packed >> 32n), __l = Number(__packed & 0xffffffffn);".into(),
            );
            lines.push("const __s = __wl_str(__p, __l);".into());
            lines.push("__wl_instance.exports.__wl_free(__p, __l);".into());
            lines.push("return __s;".into());
        }
        ExportRet::Bytes => {
            lines.push(format!("const __ret = {call};"));
            lines.extend(frees);
            lines.push("const __packed = BigInt.asUintN(64, __ret);".into());
            lines.push(
                "const __p = Number(__packed >> 32n), __l = Number(__packed & 0xffffffffn);".into(),
            );
            // Copy out of wasm memory before freeing (a view would dangle after free).
            lines.push("const __b = new Uint8Array(__wl_memory.buffer, __p, __l).slice();".into());
            lines.push("__wl_instance.exports.__wl_free(__p, __l);".into());
            lines.push("return __b;".into());
        }
        ExportRet::Handle => {
            lines.push(format!("const __idx = {call};"));
            lines.extend(frees);
            // Rust transferred ownership (it `forget`s the JsValue); read the
            // object out, free the now-orphaned table slot, and hand it to JS.
            lines.push("const __o = __wl_obj(__idx);".into());
            lines.push("__wl_drop(__idx);".into());
            lines.push("return __o;".into());
        }
        // sret returns: Rust wrote a discriminant at __ret and a payload at
        // __ret+8. The buffer is the export's leading argument.
        ExportRet::Opt(p) => {
            let sret_call = sret_call(&wasm_name, &wasm_args);
            lines.push("const __ret = __wl_instance.exports.__wl_malloc(16);".into());
            lines.push(format!("{sret_call};"));
            lines.extend(frees);
            lines.push("const __dv = new DataView(__wl_memory.buffer);".into());
            lines.push("let __result;".into());
            lines.push("if (__dv.getUint32(__ret, true) === 1) {".into());
            let (stmts, expr) = read_payload(p, "__ret + 8", "v");
            for s in stmts {
                lines.push(format!("    {s}"));
            }
            lines.push(format!("    __result = {expr};"));
            lines.push("} else { __result = null; }".into());
            lines.push("__wl_instance.exports.__wl_free(__ret, 16);".into());
            lines.push("return __result;".into());
        }
        ExportRet::Res(ok, err) => {
            let sret_call = sret_call(&wasm_name, &wasm_args);
            lines.push("const __ret = __wl_instance.exports.__wl_malloc(16);".into());
            lines.push(format!("{sret_call};"));
            lines.extend(frees);
            lines.push("const __dv = new DataView(__wl_memory.buffer);".into());
            lines.push("const __tag = __dv.getUint32(__ret, true);".into());
            lines.push("if (__tag === 0) {".into());
            let (ok_stmts, ok_expr) = read_payload(ok, "__ret + 8", "ok");
            for s in ok_stmts {
                lines.push(format!("    {s}"));
            }
            lines.push(format!("    const __okv = {ok_expr};"));
            lines.push("    __wl_instance.exports.__wl_free(__ret, 16);".into());
            lines.push("    return __okv;".into());
            lines.push("} else {".into());
            let (err_stmts, err_expr) = read_payload(err, "__ret + 8", "err");
            for s in err_stmts {
                lines.push(format!("    {s}"));
            }
            lines.push(format!("    const __errv = {err_expr};"));
            lines.push("    __wl_instance.exports.__wl_free(__ret, 16);".into());
            lines.push("    throw __errv;".into());
            lines.push("}".into());
        }
    }

    // The descriptor comes from the wasm binary. Keep its name data, never JS
    // source: a stable local identifier plus a quoted export alias prevents a
    // crafted custom section from injecting statements into the glue module.
    let wrapper = format!("__wl_export_wrapper_{index}");
    let public_name = js_string(&export.name);
    let _ = writeln!(
        js,
        "\nfunction {wrapper}({}) {{ {} }}\nexport {{ {wrapper} as {public_name} }};",
        params.join(", "),
        lines.join(" ")
    );
}
