// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host-side code generation for wasm_lite.
//!
//! The wasm-side macros record imports, exports, tests, and benchmarks in custom
//! sections. This crate reads those sections from a compiled module and
//! generates the matching JavaScript: import shims that unmarshal wasm-level
//! arguments, wrappers for Rust exports, harness discovery, and (for
//! shared-memory modules) a Web Worker bootstrap.
//!
//! It also detects imported memory and wasm-bindgen schema sections. The latter
//! feeds the interop bundler, which runs the wasm-bindgen CLI and assembles one
//! loader around the finalized wasm, wasm_lite glue, and patched wasm-bindgen
//! glue. Callers can select bundle-specific sibling paths for both interop and
//! worker artifacts.
//!
//! It is dependency-free: a minimal wasm binary reader plus a small text parser.

mod descriptor;
mod exports;
mod generate;
mod interop;
mod wasm;

pub use descriptor::{AbiArg, Descriptor, Kind, Ret, SliceElem};
pub use exports::{Export, ExportArg, ExportRet, Payload, exports_from_wasm};
pub use generate::{generate_glue, generate_glue_with_worker, generate_worker};
pub use interop::{InteropBundle, build_interop, interop_loader, patch_wasm_bindgen_glue};
pub use wasm::{MemoryImport, exported_names, imported_memory};

/// Name of the custom section the `import!` macro writes descriptors into.
pub const SECTION_NAME: &str = "__wasm_lite_imports";

/// Linker-generated symbols the worker bootstrap needs to start a thread.
///
/// `wasm-ld` does not export any of these on its own — not even in a
/// `--shared-memory` build — so each one needs an explicit
/// `-C link-arg=--export=…`.
const THREAD_EXPORTS: [&str; 5] = [
    "__stack_pointer",
    "__tls_base",
    "__tls_size",
    "__tls_align",
    "__wasm_init_tls",
];

/// Rust-side symbol that only exists once `wasm_lite`'s thread spawner is
/// linked in. Its presence is what makes the missing linker exports an error
/// rather than a module that simply never spawns.
const SPAWN_MARKER: &str = "__wl_thread_entry";

/// Which of the linker's thread symbols a thread-spawning module fails to
/// export: `__stack_pointer`, `__tls_base`, `__tls_size`, `__tls_align`,
/// `__wasm_init_tls`. `wasm-ld` exports none of them on its own, so each needs
/// an explicit `-C link-arg=--export=…`.
///
/// Empty for a module that cannot spawn threads at all, so a shared-memory
/// module that only uses atomics is never faulted.
///
/// A module that spawns but is missing these links and runs, then dies inside
/// the generated worker bootstrap on `instance.exports.__stack_pointer.value =
/// …` with nothing but a `TypeError` about `undefined` to go on. Checking here
/// turns that into a build error naming the flag.
pub fn missing_thread_exports(wasm: &[u8]) -> Result<Vec<&'static str>, String> {
    let names = wasm::exported_names(wasm)?;
    if !names.iter().any(|n| n == SPAWN_MARKER) {
        return Ok(Vec::new());
    }
    Ok(THREAD_EXPORTS
        .into_iter()
        .filter(|want| !names.iter().any(|have| have == want))
        .collect())
}

/// The build error for a module [`missing_thread_exports`] faults.
///
/// Spells out the flags verbatim, and names `rustdocflags` separately: doctests
/// are linked by rustdoc, which ignores `rustflags`, so a crate whose tests
/// spawn threads happily can still have every doctest that spawns one fail.
pub fn missing_thread_exports_message(missing: &[&str]) -> String {
    let mut msg = String::from(
        "this module spawns threads but does not export the linker symbols the \
         worker bootstrap needs:\n",
    );
    for name in missing {
        msg.push_str(&format!("    {name}\n"));
    }
    msg.push_str(
        "\nAdd them to the wasm32 link flags:\n\n\
         [build]\n\
         rustflags = [\n",
    );
    for name in THREAD_EXPORTS {
        msg.push_str(&format!("    \"-C\", \"link-arg=--export={name}\",\n"));
    }
    msg.push_str(
        "]\n\n\
         Repeat the same list under `rustdocflags` — doctests are linked by \
         rustdoc, which does not read `rustflags`. Put that copy under the \
         exact triple, `[target.wasm32-unknown-unknown]`: rustdoc ignores \
         `rustdocflags` under a `cfg(...)` predicate, so a `cfg(target_arch = \
         \"wasm32\")` section silently does nothing.\n",
    );
    msg
}

/// Returns true if the module was produced with wasm-bindgen (carries its
/// schema section), meaning it needs the wasm-bindgen CLI before it can run.
pub fn uses_wasm_bindgen(wasm: &[u8]) -> bool {
    matches!(
        wasm::custom_section(wasm, "__wasm_bindgen_unstable"),
        Ok(Some(_))
    )
}

/// Rust paths of the tests declared via `#[wasm_lite_test]`, in order.
///
/// Empty if the module has no test section (i.e. it isn't a test harness).
/// A malformed module or non-UTF-8 section is an error, not "no tests" —
/// silently reporting an empty suite would let a corrupted harness pass.
pub fn test_names(wasm: &[u8]) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for bytes in wasm::custom_sections(wasm, "__wasm_lite_tests")? {
        names.extend(
            std::str::from_utf8(bytes)
                .map_err(|e| format!("test-name section is not UTF-8: {e}"))?
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string),
        );
    }
    Ok(names)
}

/// Rust paths of the benchmarks declared via `#[wasm_lite_bench]`, in order.
///
/// Empty if the module has no benchmark section. As with [`test_names`], a
/// malformed module or non-UTF-8 section is an error rather than an empty
/// suite: a corrupted harness must fail, not silently report nothing to run.
pub fn bench_names(wasm: &[u8]) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for bytes in wasm::custom_sections(wasm, "__wasm_lite_benches")? {
        names.extend(
            std::str::from_utf8(bytes)
                .map_err(|e| format!("bench-name section is not UTF-8: {e}"))?
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string),
        );
    }
    Ok(names)
}

/// Read import descriptors from a compiled wasm module.
///
/// Returns an empty vector if the module has no descriptor section (e.g. it was
/// built without any `import!` declarations).
pub fn descriptors_from_wasm(wasm: &[u8]) -> Result<Vec<Descriptor>, String> {
    let mut descriptors = Vec::new();
    for bytes in wasm::custom_sections(wasm, SECTION_NAME)? {
        descriptors.extend(descriptor::parse(bytes)?);
    }
    Ok(descriptors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_module(name: &str, payloads: &[&[u8]]) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        for payload in payloads {
            let body_len = 1 + name.len() + payload.len();
            assert!(name.len() < 128 && body_len < 128);
            wasm.extend([0, body_len as u8, name.len() as u8]);
            wasm.extend(name.as_bytes());
            wasm.extend(*payload);
        }
        wasm
    }

    /// A module whose only section exports `names` (all as function index 0).
    fn export_module(names: &[&str]) -> Vec<u8> {
        let mut body = vec![names.len() as u8];
        for name in names {
            assert!(name.len() < 128);
            body.push(name.len() as u8);
            body.extend(name.as_bytes());
            body.extend([0x00, 0x00]);
        }
        assert!(body.len() < 128);
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        wasm.extend([7, body.len() as u8]);
        wasm.extend(body);
        wasm
    }

    #[test]
    fn faults_a_spawning_module_missing_stack_pointer() {
        // wasm-ld exports none of these on its own, so a link line that forgot
        // `--export=__stack_pointer` still links and still spawns.
        let wasm = export_module(&[
            SPAWN_MARKER,
            "__tls_base",
            "__tls_size",
            "__tls_align",
            "__wasm_init_tls",
        ]);
        assert_eq!(missing_thread_exports(&wasm).unwrap(), ["__stack_pointer"]);
    }

    #[test]
    fn a_module_that_cannot_spawn_is_never_faulted() {
        // Atomics without thread spawning is a legitimate build: it never
        // reaches the worker bootstrap, so it needs none of these exports.
        let wasm = export_module(&["main"]);
        assert!(missing_thread_exports(&wasm).unwrap().is_empty());
    }

    #[test]
    fn a_correctly_linked_spawning_module_passes() {
        let mut names = vec![SPAWN_MARKER];
        names.extend(THREAD_EXPORTS);
        let wasm = export_module(&names);
        assert!(missing_thread_exports(&wasm).unwrap().is_empty());
    }

    #[test]
    fn the_message_names_the_flag_and_rustdoc() {
        let msg = missing_thread_exports_message(&["__stack_pointer"]);
        assert!(msg.contains("link-arg=--export=__stack_pointer"));
        assert!(msg.contains("rustdocflags"));
    }

    #[test]
    fn test_names_include_repeated_custom_sections() {
        let wasm = custom_module(
            "__wasm_lite_tests",
            &[b"crate::first\n", b"crate::second\n"],
        );
        assert_eq!(
            test_names(&wasm).unwrap(),
            ["crate::first", "crate::second"]
        );
    }

    #[test]
    fn descriptors_include_repeated_custom_sections() {
        let wasm = custom_module(
            SECTION_NAME,
            &[b"f|console|a|log|str||\n", b"f|console|b|warn|str||\n"],
        );
        let descriptors = descriptors_from_wasm(&wasm).unwrap();
        assert_eq!(
            descriptors
                .iter()
                .map(|descriptor| descriptor.import_name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
