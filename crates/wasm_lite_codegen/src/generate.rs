// SPDX-License-Identifier: MIT OR Apache-2.0
//! Generate the JavaScript glue module from import descriptors.

use crate::descriptor::Descriptor;
use crate::exports::Export;
use crate::wasm::MemoryImport;
use std::fmt::Write;

mod export;
mod payload;
mod runtime;
mod shim;

use export::emit_export;
use runtime::{INSTANTIATE, PREAMBLE, instantiate_shared};
use shim::emit_shim;

pub use runtime::generate_worker;

/// Generate the JS glue module for the given imports and exports.
///
/// The module exports `makeImports()` (the import object the wasm expects),
/// `instantiate(wasmUrl)` (fetch + instantiate), and one wrapper per
/// `#[wasm_lite::export]` function.
///
/// `memory` describes an imported (shared) linear memory when the module was
/// linked with `--import-memory` (an `+atomics` build); pass `None` for the
/// usual module-defined memory.
pub fn generate_glue(
    imports: &[Descriptor],
    exports: &[Export],
    memory: Option<&MemoryImport>,
) -> String {
    generate_glue_with_worker(imports, exports, memory, "./wl_worker.js")
}

/// Generate glue whose shared-memory thread spawner loads `worker_specifier`.
///
/// This is the same output as [`generate_glue`], except callers that emit files
/// to disk can give every bundle a distinct worker module rather than sharing
/// the default `./wl_worker.js` name.
pub fn generate_glue_with_worker(
    imports: &[Descriptor],
    exports: &[Export],
    memory: Option<&MemoryImport>,
    worker_specifier: &str,
) -> String {
    let mut js = String::from(PREAMBLE);
    // Null-prototype objects make wasm import names literal keys. On ordinary
    // objects, names such as `__proto__` and `constructor` mutate or resolve
    // through Object.prototype instead of defining the requested import.
    js.push_str("export function makeImports() {\n    const imports = Object.create(null);\n");
    // Runtime support: Rust calls __wl_drop when a JsValue drops, __wl_schedule
    // to drive the async executor; shared-memory builds also get __wl_spawn for
    // thread spawning. (Unused entries are harmless — the wasm only imports what
    // it references.)
    let test_rt = "__wl_test_pending: __wl_test_pending, __wl_test_pass: __wl_test_pass, __wl_closure_new: __wl_closure_new, __wl_clone: __wl_clone, __wl_num: __wl_num, __wl_bigint: __wl_bigint, __wl_ubigint: __wl_ubigint, __wl_bigint_str: __wl_bigint_str, __wl_str_val: __wl_str_val, __wl_as_f64: __wl_as_f64, __wl_as_bool: __wl_as_bool, __wl_as_str: __wl_as_str, __wl_eq: __wl_eq, __wl_binop: __wl_binop, __wl_unop: __wl_unop, __wl_cmp: __wl_cmp, __wl_is: __wl_is, __wl_checked_div: __wl_checked_div, __wl_num_str: __wl_num_str, __wl_memory_obj: __wl_memory_obj, __wl_module_obj: __wl_module_obj";
    if memory.is_some_and(|memory| memory.shared) {
        let _ = writeln!(
            js,
            "    imports[\"__wasm_lite\"] = Object.assign(Object.create(null), {{ __wl_drop: __wl_drop, __wl_spawn: __wl_spawn, __wl_schedule: __wl_schedule, __wl_wait_async: __wl_wait_async, __wl_timer_arm: __wl_timer_arm, __wl_timer_cancel: __wl_timer_cancel, {test_rt} }});"
        );
    } else {
        let _ = writeln!(
            js,
            "    imports[\"__wasm_lite\"] = Object.assign(Object.create(null), {{ __wl_drop: __wl_drop, __wl_spawn: __wl_spawn_unavailable, __wl_schedule: __wl_schedule, __wl_wait_async: __wl_wait_async, __wl_timer_arm: __wl_timer_arm, __wl_timer_cancel: __wl_timer_cancel, {test_rt} }});"
        );
    }
    // Reserved, versioned structured observability transport. Unlike ordinary
    // descriptors this ABI is supplied whenever a module asks for it.
    js.push_str(
        "    imports[\"logwise_v1\"] = Object.assign(Object.create(null), { emit: __wl_logwise_emit });\n",
    );

    for d in imports {
        emit_shim(&mut js, d);
    }

    // Fail closed: wrap every import shim so an escaping JS exception is routed
    // through __wl_fatal instead of silently unwinding the wasm frames.
    // Result-returning shims already catch and never reach the wrapper's catch;
    // runtime entries (__wl_spawn in particular — a throwing `new Worker()`
    // would otherwise leak the closure and its stack/TLS blocks) are covered too.
    js.push_str(
        "    for (const __ns in imports) {\n\
         \x20       for (const __key in imports[__ns]) {\n\
         \x20           const __f = imports[__ns][__key];\n\
         \x20           if (typeof __f !== \"function\") continue;\n\
         \x20           imports[__ns][__key] = (...__a) => {\n\
         \x20               try { return __f(...__a); } catch (__e) { __wl_fatal(__e, __ns + \".\" + __key); }\n\
         \x20           };\n\
         \x20       }\n\
         \x20   }\n",
    );
    js.push_str("    return imports;\n}\n");
    match memory {
        Some(mem) => js.push_str(&instantiate_shared(mem, worker_specifier)),
        None => js.push_str(INSTANTIATE),
    }

    for (index, export) in exports.iter().enumerate() {
        emit_export(&mut js, export, index);
    }
    js
}

/// Emit one export wrapper: a JS function that marshals `&str` args into wasm
/// memory, forwards to the export shim, frees, and unwraps the return.
fn next_param(params: &mut Vec<String>) -> String {
    let p = format!("p{}", params.len());
    params.push(p.clone());
    p
}

/// Render a string as a double-quoted JS string literal.
fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests name this type; the emitter infers it from the match.
    use crate::descriptor::{AbiArg, Kind, Ret, SliceElem};
    use crate::exports::{ExportArg, ExportRet, Payload};

    fn func(ns: &str, import: &str, js: &str, args: Vec<AbiArg>, ret: Ret) -> Descriptor {
        Descriptor {
            kind: Kind::Function,
            namespace: ns.into(),
            import_name: import.into(),
            js_name: js.into(),
            args,
            ret,
            variadic: false,
        }
    }

    #[test]
    fn generates_str_and_numeric_shims() {
        let descriptors = vec![
            func("console", "log", "log", vec![AbiArg::Str], Ret::Void),
            func(
                "performance",
                "now",
                "now",
                vec![],
                Ret::Value("f64".into()),
            ),
        ];
        let js = generate_glue(&descriptors, &[], None);
        assert!(js.contains("imports[\"console\"][\"log\"] = (p0, p1) => globalThis[\"console\"][\"log\"](__wl_str(p0, p1));"));
        assert!(js.contains(
            "imports[\"performance\"][\"now\"] = () => globalThis[\"performance\"][\"now\"]();"
        ));
        assert!(js.contains("export async function instantiate"));
        // The value-table runtime import is always wired.
        assert!(js.contains("imports[\"__wasm_lite\"] = Object.assign(Object.create(null), { __wl_drop: __wl_drop, __wl_spawn: __wl_spawn_unavailable, __wl_schedule: __wl_schedule, __wl_wait_async: __wl_wait_async, __wl_timer_arm: __wl_timer_arm, __wl_timer_cancel: __wl_timer_cancel, __wl_test_pending: __wl_test_pending, __wl_test_pass: __wl_test_pass, __wl_closure_new: __wl_closure_new, __wl_clone: __wl_clone, __wl_num: __wl_num, __wl_bigint: __wl_bigint, __wl_ubigint: __wl_ubigint, __wl_bigint_str: __wl_bigint_str, __wl_str_val: __wl_str_val, __wl_as_f64: __wl_as_f64, __wl_as_bool: __wl_as_bool, __wl_as_str: __wl_as_str, __wl_eq: __wl_eq, __wl_binop: __wl_binop, __wl_unop: __wl_unop, __wl_cmp: __wl_cmp, __wl_is: __wl_is, __wl_checked_div: __wl_checked_div, __wl_num_str: __wl_num_str, __wl_memory_obj: __wl_memory_obj, __wl_module_obj: __wl_module_obj });"));
        assert!(js.contains("const __wl_timers = new Map();"));
        assert!(js.contains(
            "imports[\"logwise_v1\"] = Object.assign(Object.create(null), { emit: __wl_logwise_emit });"
        ));
        assert!(js.contains("function __wl_logwise_emit(ptr, len)"));
        assert!(js.contains("const __WL_LOGWISE_MAX_RECORD = 64 * 1024;"));
        assert!(js.contains("const __WL_LOGWISE_MAX_BYTES = 4 * 1024 * 1024;"));
        assert!(js.contains("globalThis.__wl_logwise_dropped"));
        assert!(js.contains("new CustomEvent(\"__wl_logwise\""));
    }

    #[test]
    fn u32_args_and_returns_are_reinterpreted_unsigned() {
        // Wasm i32 params/results surface in JS as signed Numbers; u32 values
        // above 2^31 must be normalized with `>>> 0` on their way to JS.
        let descriptors = vec![
            func("ns", "set_id", "set_id", vec![AbiArg::U32], Ret::Void),
            func(
                "ns",
                "pick",
                "pick",
                vec![AbiArg::Opt(Payload::U32)],
                Ret::Void,
            ),
        ];
        let exports = vec![Export {
            name: "next_id".into(),
            args: vec![],
            ret: ExportRet::U32,
        }];
        let js = generate_glue(&descriptors, &exports, None);
        assert!(js.contains(
            "imports[\"ns\"][\"set_id\"] = (p0) => globalThis[\"ns\"][\"set_id\"]((p0 >>> 0));"
        ));
        assert!(js.contains("imports[\"ns\"][\"pick\"] = (p0, p1) => globalThis[\"ns\"][\"pick\"]((p0 ? (p1 >>> 0) : undefined));"));
        assert!(js.contains("function __wl_export_wrapper_0() { __wl_check_live(); const __ret = __wl_instance.exports[\"__wl_export_next_id\"](); return __ret >>> 0; }"));
        assert!(js.contains("export { __wl_export_wrapper_0 as \"next_id\" };"));
    }

    #[test]
    fn overload_keys_slot_on_import_name_but_calls_js_name() {
        let descriptors = vec![func(
            "Math",
            "max2",
            "max",
            vec![AbiArg::Num, AbiArg::Num],
            Ret::Value("f64".into()),
        )];
        let js = generate_glue(&descriptors, &[], None);
        assert!(js.contains(
            "imports[\"Math\"][\"max2\"] = (p0, p1) => globalThis[\"Math\"][\"max\"](p0, p1);"
        ));
    }

    #[test]
    fn long_member_paths_generate_without_quadratic_copying() {
        let js_name = vec!["member"; 50_000].join(".");
        let descriptor = func("ns", "binding", &js_name, vec![], Ret::Void);

        let started = std::time::Instant::now();
        let js = generate_glue(&[descriptor], &[], None);
        let elapsed = started.elapsed();

        assert!(js.ends_with("\n"));
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "a 50,000-segment descriptor took {elapsed:?}; member-path generation should be linear"
        );
    }

    #[test]
    fn handle_return_is_added_to_the_table() {
        let descriptors = vec![func(
            "JSON",
            "parse",
            "parse",
            vec![AbiArg::Str],
            Ret::Handle,
        )];
        let js = generate_glue(&descriptors, &[], None);
        assert!(js.contains(
            "imports[\"JSON\"][\"parse\"] = (p0, p1) => __wl_add(globalThis[\"JSON\"][\"parse\"](__wl_str(p0, p1)));"
        ));
    }

    #[test]
    fn method_calls_through_the_receiver_handle() {
        let descriptors = vec![Descriptor {
            kind: Kind::Method,
            namespace: "Array".into(),
            import_name: "push".into(),
            js_name: "push".into(),
            args: vec![AbiArg::Handle, AbiArg::Num],
            ret: Ret::Value("f64".into()),
            variadic: false,
        }];
        let js = generate_glue(&descriptors, &[], None);
        assert!(
            js.contains("imports[\"Array\"][\"push\"] = (p0, p1) => __wl_obj(p0)[\"push\"](p1);")
        );
    }

    fn shaped(kind: Kind, js: &str, args: Vec<AbiArg>, ret: Ret) -> Descriptor {
        Descriptor {
            kind,
            namespace: "Element".into(),
            import_name: "b".into(),
            js_name: js.into(),
            args,
            ret,
            variadic: false,
        }
    }

    /// A property read must not be emitted as a call: `el.tagName` and
    /// `el.tagName()` are different programs and only the first one works.
    #[test]
    fn getter_reads_the_property_without_calling_it() {
        let js = generate_glue(
            &[shaped(
                Kind::Getter,
                "tagName",
                vec![AbiArg::Handle],
                Ret::Str,
            )],
            &[],
            None,
        );
        assert!(
            js.contains("const __r = __wl_obj(p0)[\"tagName\"];"),
            "{js}"
        );
        assert!(!js.contains("[\"tagName\"]()"), "{js}");
    }

    #[test]
    fn setter_assigns_the_property() {
        let js = generate_glue(
            &[shaped(
                Kind::Setter,
                "scrollTop",
                vec![AbiArg::Handle, AbiArg::Num],
                Ret::Void,
            )],
            &[],
            None,
        );
        assert!(
            js.contains(
                "imports[\"Element\"][\"b\"] = (p0, p1) => __wl_obj(p0)[\"scrollTop\"] = p1;"
            ),
            "{js}"
        );
    }

    #[test]
    fn constructor_uses_new_on_the_global_class() {
        let js = generate_glue(
            &[shaped(
                Kind::Constructor,
                "URL",
                vec![AbiArg::Str],
                Ret::Handle,
            )],
            &[],
            None,
        );
        assert!(
            js.contains(
                "imports[\"Element\"][\"b\"] = (p0, p1) => __wl_add(new globalThis[\"URL\"](__wl_str(p0, p1)));"
            ),
            "{js}"
        );
    }

    #[test]
    fn indexing_uses_computed_access_on_the_receiver() {
        let get = generate_glue(
            &[shaped(
                Kind::IndexGet,
                "unused",
                vec![AbiArg::Handle, AbiArg::U32],
                Ret::Handle,
            )],
            &[],
            None,
        );
        assert!(
            get.contains(
                "imports[\"Element\"][\"b\"] = (p0, p1) => __wl_add(__wl_obj(p0)[(p1 >>> 0)]);"
            ),
            "{get}"
        );

        let set = generate_glue(
            &[shaped(
                Kind::IndexSet,
                "unused",
                vec![AbiArg::Handle, AbiArg::U32, AbiArg::Handle],
                Ret::Void,
            )],
            &[],
            None,
        );
        assert!(
            set.contains(
                "imports[\"Element\"][\"b\"] = (p0, p1, p2) => __wl_obj(p0)[(p1 >>> 0)] = __wl_obj(p2);"
            ),
            "{set}"
        );
    }

    /// A missing class must answer "not an instance", not throw: bare
    /// `x instanceof undefined` is a TypeError, and a downcast test that kills
    /// the instance is worse than one that returns false.
    #[test]
    fn instanceof_is_guarded_against_a_missing_class() {
        let js = generate_glue(
            &[shaped(
                Kind::InstanceOf,
                "Element",
                vec![AbiArg::Handle],
                Ret::Value("bool".into()),
            )],
            &[],
            None,
        );
        assert!(
            js.contains(
                "typeof globalThis[\"Element\"] === \"function\" && __wl_obj(p0) instanceof globalThis[\"Element\"]"
            ),
            "{js}"
        );
    }

    /// `len` crosses in elements, so the typed array's constructor scales it.
    /// Passing a byte length here would read 4x too far.
    #[test]
    fn numeric_slices_become_typed_array_views() {
        let js = generate_glue(
            &[
                func(
                    "Array",
                    "from_f32",
                    "from",
                    vec![AbiArg::Slice(SliceElem::F32)],
                    Ret::Handle,
                ),
                func(
                    "Array",
                    "from_u32",
                    "from",
                    vec![AbiArg::Slice(SliceElem::U32)],
                    Ret::Handle,
                ),
            ],
            &[],
            None,
        );
        assert!(
            js.contains("new Float32Array(__wl_memory.buffer, p0, p1)"),
            "{js}"
        );
        assert!(
            js.contains("new Uint32Array(__wl_memory.buffer, p0, p1)"),
            "{js}"
        );
    }

    #[test]
    fn generates_export_wrappers() {
        let exports = vec![
            Export {
                name: "add".into(),
                args: vec![ExportArg::Num, ExportArg::Num],
                ret: ExportRet::Value,
            },
            Export {
                name: "is_even".into(),
                args: vec![ExportArg::Num],
                ret: ExportRet::Bool,
            },
            Export {
                name: "tick".into(),
                args: vec![],
                ret: ExportRet::Void,
            },
        ];
        let js = generate_glue(&[], &exports, None);
        assert!(js.contains("function __wl_export_wrapper_0(p0, p1) { __wl_check_live(); const __ret = __wl_instance.exports[\"__wl_export_add\"](p0, p1); return __ret; }"));
        assert!(js.contains("export { __wl_export_wrapper_0 as \"add\" };"));
        assert!(js.contains("function __wl_export_wrapper_1(p0) { __wl_check_live(); const __ret = __wl_instance.exports[\"__wl_export_is_even\"](p0); return Boolean(__ret); }"));
        assert!(
            js.contains("function __wl_export_wrapper_2() { __wl_check_live(); __wl_instance.exports[\"__wl_export_tick\"](); }")
        );
    }

    #[test]
    fn generates_string_export_wrapper() {
        let exports = vec![Export {
            name: "greet".into(),
            args: vec![ExportArg::Str],
            ret: ExportRet::Str,
        }];
        let js = generate_glue(&[], &exports, None);
        assert!(js.contains("const [__a0p, __a0l] = __wl_pass_str(p0);"));
        assert!(js.contains("__wl_instance.exports[\"__wl_export_greet\"](__a0p, __a0l)"));
        assert!(js.contains("__wl_instance.exports.__wl_free(__a0p, __a0l);"));
        assert!(js.contains("const __s = __wl_str(__p, __l);"));
    }

    #[test]
    fn parent_frees_worker_stack_and_tls_after_completion() {
        let memory = MemoryImport {
            module: "env".into(),
            name: "memory".into(),
            initial: 1,
            maximum: Some(2),
            shared: true,
        };
        let glue = generate_glue(&[], &[], Some(&memory));
        assert!(glue.contains("__wl_instance.exports.__wl_thread_free(stackPtr, __WL_STACK);"));
        assert!(
            glue.contains("if (tlsSize) __wl_instance.exports.__wl_thread_free(tlsPtr, tlsSize);")
        );

        let worker = generate_worker("./program.js");
        assert!(
            !worker.contains("__wl_thread_free"),
            "worker must not deallocate the stack it is executing on: {worker}"
        );
        assert!(glue.contains("m && m.__wl_logwise"));
        assert!(glue.contains("__wl_sink_logwise(m.__wl_logwise)"));
        assert!(worker.contains("[\"log\", \"error\", \"warn\", \"info\", \"debug\"]"));
    }

    #[test]
    fn import_keys_cannot_reach_object_prototypes() {
        let descriptor = func("__wasm_lite", "__proto__", "call", vec![], Ret::Void);
        let js = generate_glue(&[descriptor], &[], None);
        assert!(js.contains("const imports = Object.create(null);"));
        assert!(js.contains(
            "imports[\"__wasm_lite\"] = Object.assign(Object.create(null), { __wl_drop:"
        ));
        assert!(js.contains("imports[\"__wasm_lite\"][\"__proto__\"] = () =>"));
    }

    #[test]
    fn export_descriptor_names_are_always_quoted_data() {
        let export = Export {
            name: "x\"); globalThis.pwned = true; //".into(),
            args: vec![],
            ret: ExportRet::Void,
        };
        let js = generate_glue(&[], &[export], None);
        assert!(!js.contains("export function x"));
        assert!(js.contains(
            "__wl_instance.exports[\"__wl_export_x\\\"); globalThis.pwned = true; //\"]()"
        ));
        assert!(js.contains(
            "export { __wl_export_wrapper_0 as \"x\\\"); globalThis.pwned = true; //\" };"
        ));
    }

    #[test]
    fn the_async_test_verdict_counts_outstanding_bodies() {
        // Regression: both halves used to be single-slot, so a page holding more
        // than one async body — which is exactly an edition-2024 merged doctest
        // bundle — had the first body to finish publish the verdict for all of
        // them. The runner exited on it and a sibling that panicked afterwards
        // was never seen.
        let js = generate_glue(&[], &[], None);

        // Pending counts, so N bodies are distinguishable from one.
        assert!(js.contains(
            "function __wl_test_pending() { globalThis.__wl_async_pending = (globalThis.__wl_async_pending || 0) + 1; }"
        ));

        // Pass retires one obligation; only the last one publishes a verdict.
        let body = js
            .split_once("function __wl_test_pass() {")
            .expect("the runtime defines __wl_test_pass")
            .1
            .split_once("\n}")
            .expect("__wl_test_pass has a body")
            .0;
        assert!(
            body.contains("- 1) === 0"),
            "__wl_test_pass must decrement the count: {body}"
        );
        assert!(
            body.trim_start().starts_with("if ("),
            "__wl_done must be published only on the last completion, not unconditionally: {body}"
        );
    }
}
