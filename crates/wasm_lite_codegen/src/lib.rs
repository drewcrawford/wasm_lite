// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host-side code generation for wasm_lite.
//!
//! The wasm-side `import!` macro records a descriptor for every imported JS
//! function into the `__wasm_lite_imports` custom section. This crate reads that
//! section out of a compiled module and generates the matching JavaScript glue:
//! one shim per import that unmarshals the wasm-level arguments (e.g. a `&str`
//! arrives as a `(ptr, len)` pair to decode from linear memory) and calls the
//! real JS function.
//!
//! It is dependency-free: a minimal wasm binary reader plus a small text parser.

mod descriptor;
mod exports;
mod generate;
mod interop;
mod wasm;

pub use descriptor::{AbiArg, Descriptor, Kind, Ret};
pub use exports::{Export, ExportArg, ExportRet, Payload, exports_from_wasm};
pub use generate::{generate_glue, generate_glue_with_worker, generate_worker};
pub use interop::{InteropBundle, build_interop, interop_loader, patch_wasm_bindgen_glue};
pub use wasm::{MemoryImport, imported_memory};

/// Name of the custom section the `import!` macro writes descriptors into.
pub const SECTION_NAME: &str = "__wasm_lite_imports";

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
