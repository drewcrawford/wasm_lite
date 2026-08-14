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
pub use interop::{
    InteropBundle, build_interop, interop_harness_loader, interop_loader, patch_wasm_bindgen_glue,
};
pub use wasm::{MemoryImport, exported_names, imported_memory};

/// Name of the custom section the `import!` macro writes descriptors into.
pub const SECTION_NAME: &str = "__wasm_lite_imports";

/// Thread symbols the **generated glue** reads, needed by every threaded build.
///
/// `wasm-ld` exports none of these on its own — not even in a `--shared-memory`
/// build — so each needs an explicit `-C link-arg=--export=…`. The worker sets
/// `__stack_pointer` and calls `__wasm_init_tls`; the spawning side reads
/// `__tls_size` to size the TLS block.
const GLUE_THREAD_EXPORTS: [&str; 3] = ["__stack_pointer", "__tls_size", "__wasm_init_tls"];

/// Thread symbols only the **wasm-bindgen backend** needs, on top of
/// [`GLUE_THREAD_EXPORTS`].
///
/// Nothing wasm_lite generates ever reads these. They are required by the
/// `wasm-bindgen` CLI's own threading transform, which runs over any module
/// carrying wasm-bindgen's schema section, and which fails with `failed to find
/// __tls_align` and then `failed to find tls base` — in that order, so supplying
/// only the first just moves the error.
const BINDGEN_BACKEND_THREAD_EXPORTS: [&str; 2] = ["__tls_base", "__tls_align"];

/// The Rust-side entry point a spawned thread lands on.
///
/// Necessary evidence that a module could spawn, but not sufficient: the core
/// crate keeps this alive unconditionally, so even a single-threaded module
/// exports it. It is paired with a shared-memory check below.
const SPAWN_MARKER: &str = "__wl_thread_entry";

/// What a thread-spawning module is missing, and which backend it is on.
///
/// The two travel together so they cannot disagree: the required list depends on
/// the backend, and a message that named a different list from the one checked
/// is exactly how an earlier version told users to apply a fix that could not
/// work for their module.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MissingThreadExports {
    /// Required symbols this module does not export, in canonical order.
    pub missing: Vec<&'static str>,
    /// Whether the `wasm-bindgen` CLI will run over this module — that is,
    /// whether it is on the wasm-bindgen backend, which additionally requires
    /// `__tls_base` and `__tls_align` for the CLI's own threading transform.
    pub wasm_bindgen_backend: bool,
}

impl MissingThreadExports {
    /// Nothing missing — the module is correctly linked for its backend.
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Which linker thread symbols a thread-spawning module fails to export.
///
/// The required set depends on the backend the module ends up on. Every
/// threaded build needs the three the glue reads; a module carrying
/// wasm-bindgen's schema section is additionally processed by the `wasm-bindgen`
/// CLI, whose threading transform needs two more. That is checked rather than
/// assumed, so a module genuinely clear of wasm-bindgen is not asked for flags it
/// has no use for — and one that later acquires a wasm-bindgen dependency (they
/// arrive transitively, and a dev-dependency counts) is told at its next build.
///
/// Empty for a module that cannot spawn threads at all, so a shared-memory
/// module that only uses atomics is never faulted.
///
/// A module that spawns but is missing these links still builds, and then dies
/// inside the generated worker bootstrap on
/// `instance.exports.__stack_pointer.value = …` with nothing but a `TypeError`
/// about `undefined` to go on. Checking here turns that into a build error
/// naming the flag.
pub fn missing_thread_exports(wasm: &[u8]) -> Result<MissingThreadExports, String> {
    let wasm_bindgen_backend = uses_wasm_bindgen(wasm);
    let none = || MissingThreadExports {
        missing: Vec::new(),
        wasm_bindgen_backend,
    };

    // A thread needs a shared memory to land in. Without one the glue never
    // generates `__wl_spawn` at all and `spawn` reports `Unsupported`, so these
    // symbols are genuinely unnecessary — and every ordinary single-threaded
    // module would otherwise be faulted, since the core crate keeps
    // `__wl_thread_entry` alive whether or not anything can spawn.
    if !wasm::imported_memory(wasm)?.is_some_and(|memory| memory.shared) {
        return Ok(none());
    }
    let names = wasm::exported_names(wasm)?;
    if !names.iter().any(|n| n == SPAWN_MARKER) {
        return Ok(none());
    }
    Ok(MissingThreadExports {
        missing: required_thread_exports(wasm_bindgen_backend)
            .into_iter()
            .filter(|want| !names.iter().any(|have| have == want))
            .collect(),
        wasm_bindgen_backend,
    })
}

/// Every thread symbol a module on this backend must export, canonical order.
fn required_thread_exports(wasm_bindgen_backend: bool) -> Vec<&'static str> {
    let mut all = vec![
        GLUE_THREAD_EXPORTS[0],
        BINDGEN_BACKEND_THREAD_EXPORTS[0],
        GLUE_THREAD_EXPORTS[1],
        BINDGEN_BACKEND_THREAD_EXPORTS[1],
        GLUE_THREAD_EXPORTS[2],
    ];
    if !wasm_bindgen_backend {
        all.retain(|name| !BINDGEN_BACKEND_THREAD_EXPORTS.contains(name));
    }
    all
}

/// Runtime import a module only carries if its code can reach `thread::spawn`.
const SPAWN_IMPORT: (&str, &str) = ("__wasm_lite", "__wl_spawn");

/// Whether a module can spawn threads but its memory cannot hold one.
///
/// This is the `+atomics`-without-shared-memory build. It is a genuine
/// contradiction rather than a lesser configuration: `Builder::spawn` picks its
/// implementation on `#[cfg(target_feature = "atomics")]`, so enabling the
/// feature compiles *out* the graceful `Unsupported` path and compiles *in* the
/// one that calls `__wl_spawn`. Meanwhile the glue only wires a real spawner for
/// a shared memory, and otherwise supplies a stub that throws.
///
/// Left alone it surfaces as a JS exception from an import, which traps the
/// instance and takes the whole run down mid-test — and the generic advice that
/// accompanies an import throw ("bind it as `Result<_, JsValue>`") is useless
/// here, because the import is wasm_lite's own and the user never bound it.
/// Everything needed to say so is known at generation time.
///
/// A module built *without* `+atomics` never imports `__wl_spawn` at all — the
/// call is dead-code-eliminated — so this cannot fault the single-threaded
/// build that degrades gracefully by design.
pub fn spawns_without_shared_memory(wasm: &[u8]) -> Result<bool, String> {
    if wasm::imported_memory(wasm)?.is_some_and(|memory| memory.shared) {
        return Ok(false);
    }
    Ok(wasm::imported_functions(wasm)?
        .iter()
        .any(|(module, name)| (module.as_str(), name.as_str()) == SPAWN_IMPORT))
}

/// The build error for a module [`spawns_without_shared_memory`] faults.
pub fn spawns_without_shared_memory_message(wasm: &[u8]) -> String {
    let mut msg = String::from(
        "this module was built with `+atomics` and can spawn threads, but its \
         memory is not shared, so no thread can actually start.\n\n\
         `+atomics` compiles out the fallback that would report \
         `io::ErrorKind::Unsupported`, so this cannot degrade gracefully — it \
         would fail at runtime as a JS exception that traps the instance.\n\n\
         Add the shared-memory link args:\n\n\
         [build]\n\
         rustflags = [\n\
        \x20   \"-C\", \"link-arg=--shared-memory\",\n\
        \x20   \"-C\", \"link-arg=--import-memory\",\n\
        \x20   \"-C\", \"link-arg=--max-memory=1073741824\",\n",
    );
    // Same list the export check would demand of this module, so following one
    // diagnostic never contradicts the other.
    for name in required_thread_exports(uses_wasm_bindgen(wasm)) {
        msg.push_str(&format!("    \"-C\", \"link-arg=--export={name}\",\n"));
    }
    msg.push_str(
        "]\n\n\
         Repeat the list under `rustdocflags`, keyed by the exact triple \
         `[target.wasm32-unknown-unknown]`. Or drop `+atomics`, if this program \
         does not need threads: `spawn` then reports `Unsupported` instead of \
         failing.\n",
    );
    msg
}

/// The build error for a module [`missing_thread_exports`] faults.
///
/// Takes the whole finding rather than just the list, so the flags it prints are
/// always the ones that were actually checked. Spells them out verbatim, and
/// names `rustdocflags` separately: doctests are linked by rustdoc, which
/// ignores `rustflags`, so a crate whose tests spawn threads happily can still
/// have every doctest that spawns one fail.
pub fn missing_thread_exports_message(found: &MissingThreadExports) -> String {
    let mut msg = String::from(
        "this module spawns threads but does not export the linker symbols the \
         worker bootstrap needs:\n",
    );
    for name in &found.missing {
        msg.push_str(&format!("    {name}\n"));
    }
    msg.push_str(
        "\nAdd them to the wasm32 link flags:\n\n\
         [build]\n\
         rustflags = [\n",
    );
    for name in required_thread_exports(found.wasm_bindgen_backend) {
        msg.push_str(&format!("    \"-C\", \"link-arg=--export={name}\",\n"));
    }
    msg.push_str("]\n\n");
    if found.wasm_bindgen_backend {
        // Say why the list is longer than the symbols the glue reads, or the two
        // extra look like padding and get pruned again.
        msg.push_str(
            "Two of those — `__tls_base` and `__tls_align` — are not read by \
             wasm_lite. This module carries wasm-bindgen's schema section, so it \
             is built on the wasm-bindgen backend and the `wasm-bindgen` CLI runs \
             a threading transform of its own that requires them.\n\n",
        );
    } else {
        // And say why it is shorter than the recipe they may have copied.
        msg.push_str(
            "Nothing in this module's graph reaches wasm-bindgen, so the three \
             symbols wasm_lite reads are all it needs. Recipes commonly list \
             `__tls_base` and `__tls_align` as well; those are for the \
             wasm-bindgen backend, are harmless here, and become required if a \
             dependency ever pulls wasm-bindgen in.\n\n",
        );
    }
    msg.push_str(
        "Repeat the same list under `rustdocflags` — doctests are linked by \
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

/// What `#[should_panic]` on a test asks the runner to check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShouldPanic {
    /// `#[should_panic]` — any panic passes.
    Any,
    /// `#[should_panic(expected = "…")]` — the panic message must contain this.
    Expected(String),
}

/// One case from a harness section: its Rust path plus the libtest attributes
/// that change how its result is judged.
///
/// These ride along in the section because the runner, not the module, decides
/// the verdict — a `#[should_panic]` test traps, and only something outside the
/// instance can tell "trapped as intended" from "trapped".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct TestDecl {
    /// Full Rust path, e.g. `my_crate::tests::works`.
    pub path: String,
    /// `#[ignore]`: skipped unless the runner is asked to include ignored cases.
    pub ignored: bool,
    /// `#[should_panic]`, if present. Always `None` for benchmarks.
    pub should_panic: Option<ShouldPanic>,
}

/// Tests declared via `#[wasm_lite_test]`, in order.
///
/// Empty if the module has no test section (i.e. it isn't a test harness).
/// A malformed module or non-UTF-8 section is an error, not "no tests" —
/// silently reporting an empty suite would let a corrupted harness pass.
pub fn test_decls(wasm: &[u8]) -> Result<Vec<TestDecl>, String> {
    decls(wasm, "__wasm_lite_tests", "test")
}

/// Benchmarks declared via `#[wasm_lite_bench]`, in order.
///
/// As with [`test_decls`], a malformed module or non-UTF-8 section is an error
/// rather than an empty suite: a corrupted harness must fail, not silently
/// report nothing to run. `should_panic` is never set — a benchmark that
/// panicked measured nothing, so there is no result to invert.
pub fn bench_decls(wasm: &[u8]) -> Result<Vec<TestDecl>, String> {
    decls(wasm, "__wasm_lite_benches", "bench")
}

/// Parse a harness section: one case per line, `path` followed by tab-separated
/// attribute fields.
///
/// Unknown fields are rejected rather than skipped. The macro that writes this
/// and the parser that reads it ship together, so a field this build does not
/// recognise means the two are out of step — which is worth a hard error, not a
/// silently weaker verdict.
fn decls(wasm: &[u8], section: &str, kind: &str) -> Result<Vec<TestDecl>, String> {
    let mut out = Vec::new();
    for bytes in wasm::custom_sections(wasm, section)? {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| format!("{kind}-name section is not UTF-8: {e}"))?;
        for line in text.lines().filter(|line| !line.is_empty()) {
            let mut fields = line.split('\t');
            let path = fields.next().unwrap_or_default();
            if path.is_empty() {
                return Err(format!("{kind} section has a record with no name"));
            }
            let mut decl = TestDecl {
                path: path.to_string(),
                ignored: false,
                should_panic: None,
            };
            for field in fields {
                if field == "ignore" {
                    decl.ignored = true;
                } else if field == "should_panic" {
                    decl.should_panic = Some(ShouldPanic::Any);
                } else if let Some(expected) = field.strip_prefix("should_panic=") {
                    decl.should_panic = Some(ShouldPanic::Expected(expected.to_string()));
                } else {
                    return Err(format!(
                        "unknown attribute {field:?} on {kind} {path:?} — the macro and \
                         this codegen are out of step"
                    ));
                }
            }
            out.push(decl);
        }
    }
    Ok(out)
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

    /// A module exporting `names` (all as function index 0), optionally
    /// importing a shared memory the way an `+atomics` build does.
    fn export_module_with(names: &[&str], shared_memory: bool) -> Vec<u8> {
        let mut body = vec![names.len() as u8];
        for name in names {
            assert!(name.len() < 128);
            body.push(name.len() as u8);
            body.extend(name.as_bytes());
            body.extend([0x00, 0x00]);
        }
        assert!(body.len() < 128);
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        if shared_memory {
            wasm.extend([
                0x02, 0x12, // import section, body length 18
                0x01, // one import
                0x03, b'e', b'n', b'v', // module "env"
                0x06, b'm', b'e', b'm', b'o', b'r', b'y', // name "memory"
                0x02, // kind: memory
                0x03, 0x11, 0x80, 0x80, 0x01, // has_max|shared, min 17, max 16384
            ]);
        }
        wasm.extend([7, body.len() as u8]);
        wasm.extend(body);
        wasm
    }

    /// A module with no imported memory — an ordinary single-threaded build.
    fn export_module(names: &[&str]) -> Vec<u8> {
        export_module_with(names, false)
    }

    /// A shared-memory (`+atomics`) module, which is the only kind that can spawn.
    fn spawning_module(names: &[&str]) -> Vec<u8> {
        export_module_with(names, true)
    }

    /// A spawning module on the wasm_lite backend, exporting `names`.
    fn spawning_module_exporting(names: &[&str]) -> Vec<u8> {
        let mut all = vec![SPAWN_MARKER];
        all.extend_from_slice(names);
        spawning_module(&all)
    }

    /// The same, but carrying wasm-bindgen's schema section, which is what puts
    /// a module on the wasm-bindgen backend and makes the CLI run over it.
    fn bindgen_backend_module_exporting(names: &[&str]) -> Vec<u8> {
        let mut wasm = spawning_module_exporting(names);
        // A custom section named `__wasm_bindgen_unstable`; contents unread.
        let name = b"__wasm_bindgen_unstable";
        let mut body = vec![name.len() as u8];
        body.extend(name);
        wasm.extend([0, body.len() as u8]);
        wasm.extend(body);
        wasm
    }

    #[test]
    fn the_wasm_lite_backend_needs_only_the_three_the_glue_reads() {
        let wasm = spawning_module_exporting(&GLUE_THREAD_EXPORTS);
        let found = missing_thread_exports(&wasm).unwrap();
        assert!(!found.wasm_bindgen_backend);
        assert!(
            found.is_empty(),
            "wasm_lite backend should not need {:?}",
            found.missing
        );
    }

    #[test]
    fn the_wasm_bindgen_backend_needs_two_more() {
        // Same exports, but the CLI will run over this one and its threading
        // transform reads symbols nothing in wasm_lite ever touches.
        let wasm = bindgen_backend_module_exporting(&GLUE_THREAD_EXPORTS);
        let found = missing_thread_exports(&wasm).unwrap();
        assert!(found.wasm_bindgen_backend);
        assert_eq!(found.missing, ["__tls_base", "__tls_align"]);
    }

    #[test]
    fn faults_a_spawning_module_missing_stack_pointer() {
        // wasm-ld exports none of these on its own, so a link line that forgot
        // `--export=__stack_pointer` still links and still spawns.
        let wasm = spawning_module_exporting(&["__tls_size", "__wasm_init_tls"]);
        assert_eq!(
            missing_thread_exports(&wasm).unwrap().missing,
            ["__stack_pointer"]
        );
    }

    #[test]
    fn a_module_without_shared_memory_is_never_faulted() {
        // The core crate keeps `__wl_thread_entry` alive whether or not
        // anything can spawn, so its presence alone must not fault a module.
        // Without a shared memory the glue emits no `__wl_spawn` and `spawn`
        // reports Unsupported — every ordinary single-threaded test binary
        // looks like this, and faulting them broke all of them.
        let wasm = export_module(&[SPAWN_MARKER, "main"]);
        assert!(missing_thread_exports(&wasm).unwrap().is_empty());
    }

    #[test]
    fn a_shared_memory_module_that_cannot_spawn_is_never_faulted() {
        // Atomics without thread spawning is a legitimate build: it never
        // reaches the worker bootstrap, so it needs none of these exports.
        let wasm = spawning_module(&["main"]);
        assert!(missing_thread_exports(&wasm).unwrap().is_empty());
    }

    #[test]
    fn each_message_lists_the_flags_its_own_check_demanded() {
        // The two must never disagree: a message naming a different list from
        // the one checked is how an earlier version told users to apply a fix
        // that could not work for their module.
        let wl = missing_thread_exports(&spawning_module_exporting(&[])).unwrap();
        let wl_msg = missing_thread_exports_message(&wl);
        // The prose mentions the other two to explain why they are absent, so
        // check the emitted flag lines rather than any mention of the name.
        assert!(
            !wl_msg.contains("link-arg=--export=__tls_align"),
            "{wl_msg}"
        );
        assert!(wl_msg.contains("Nothing in this module's graph reaches wasm-bindgen"));

        let wb = missing_thread_exports(&bindgen_backend_module_exporting(&[])).unwrap();
        let wb_msg = missing_thread_exports_message(&wb);
        assert!(wb_msg.contains("link-arg=--export=__tls_align"));
        assert!(wb_msg.contains("wasm-bindgen backend"));
    }

    #[test]
    fn the_message_names_the_flag_and_rustdoc() {
        let found = missing_thread_exports(&spawning_module_exporting(&[])).unwrap();
        let msg = missing_thread_exports_message(&found);
        assert!(msg.contains("link-arg=--export=__stack_pointer"));
        assert!(msg.contains("rustdocflags"));
    }

    #[test]
    fn reads_ignore_and_should_panic_fields() {
        let wasm = custom_module(
            "__wasm_lite_tests",
            &[b"c::plain\nc::skipped\tignore\nc::boom\tshould_panic\nc::msg\tshould_panic=it broke\n"],
        );
        let decls = test_decls(&wasm).unwrap();
        assert_eq!(decls[0].path, "c::plain");
        assert!(!decls[0].ignored && decls[0].should_panic.is_none());
        assert!(decls[1].ignored);
        assert_eq!(decls[2].should_panic, Some(ShouldPanic::Any));
        assert_eq!(
            decls[3].should_panic,
            Some(ShouldPanic::Expected("it broke".into()))
        );
    }

    #[test]
    fn an_expected_message_may_contain_an_equals_sign() {
        // `strip_prefix` on the first `=` only; the rest is the message.
        let wasm = custom_module("__wasm_lite_tests", &[b"c::t\tshould_panic=a = b\n"]);
        assert_eq!(
            test_decls(&wasm).unwrap()[0].should_panic,
            Some(ShouldPanic::Expected("a = b".into()))
        );
    }

    #[test]
    fn an_unknown_attribute_field_is_an_error() {
        // The macro and this parser ship together, so an unrecognised field
        // means they are out of step — reporting a weaker verdict would let a
        // should_panic test silently become an ordinary one.
        let wasm = custom_module("__wasm_lite_tests", &[b"c::t\tunknown\n"]);
        assert!(test_decls(&wasm).is_err());
    }

    #[test]
    fn a_record_with_no_name_is_an_error() {
        let wasm = custom_module("__wasm_lite_tests", &[b"\tignore\n"]);
        assert!(test_decls(&wasm).is_err());
    }

    #[test]
    fn test_names_include_repeated_custom_sections() {
        let wasm = custom_module(
            "__wasm_lite_tests",
            &[b"crate::first\n", b"crate::second\n"],
        );
        let paths: Vec<String> = test_decls(&wasm)
            .unwrap()
            .into_iter()
            .map(|t| t.path)
            .collect();
        assert_eq!(paths, ["crate::first", "crate::second"]);
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
