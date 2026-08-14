// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wasm_lite_cli_test_{}_{}_{}",
            std::process::id(),
            id,
            label
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn push_leb_u32(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn push_name(out: &mut Vec<u8>, name: &str) {
    push_leb_u32(out, name.len().try_into().unwrap());
    out.extend_from_slice(name.as_bytes());
}

fn push_section(wasm: &mut Vec<u8>, id: u8, body: &[u8]) {
    wasm.push(id);
    push_leb_u32(wasm, body.len().try_into().unwrap());
    wasm.extend_from_slice(body);
}

fn empty_wasm() -> Vec<u8> {
    b"\0asm\x01\0\0\0".to_vec()
}

fn wasm_with_custom_section(name: &str, payload: &[u8]) -> Vec<u8> {
    let mut wasm = empty_wasm();
    let mut body = Vec::new();
    push_name(&mut body, name);
    body.extend_from_slice(payload);
    push_section(&mut wasm, 0, &body);
    wasm
}

fn wasm_with_imported_memory(shared: bool) -> Vec<u8> {
    let mut wasm = empty_wasm();
    let mut imports = Vec::new();
    push_leb_u32(&mut imports, 1);
    push_name(&mut imports, "env");
    push_name(&mut imports, "memory");
    imports.push(0x02); // memory import
    if shared {
        imports.push(0x03); // has maximum + shared
        push_leb_u32(&mut imports, 1);
        push_leb_u32(&mut imports, 2);
    } else {
        imports.push(0x00); // minimum only, not shared
        push_leb_u32(&mut imports, 1);
    }
    push_section(&mut wasm, 2, &imports);
    wasm
}

fn write_input(dir: &TempDir, wasm: &[u8]) -> PathBuf {
    let input = dir.path().join("input.wasm");
    fs::write(&input, wasm).unwrap();
    input
}

fn run_cli(input: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wasm_lite"))
        .arg("build")
        .arg(input)
        .arg("-o")
        .arg(output)
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn generated_worker_does_not_follow_a_dangling_symlink() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("worker_symlink");
    let input = write_input(&dir, &wasm_with_imported_memory(true));
    let victim = dir.path().join("victim.txt");
    symlink(&victim, dir.path().join("glue.js.worker.js")).unwrap();

    let result = run_cli(&input, &dir.path().join("glue.js"));

    assert!(
        !victim.exists(),
        "the implicit worker output followed a dangling symlink and created {}",
        victim.display()
    );
    assert!(
        !result.status.success(),
        "a worker symlink should be rejected"
    );
}

#[test]
fn wasm_bindgen_marker_is_not_silently_ignored() {
    let dir = TempDir::new("wasm_bindgen");
    let input = write_input(
        &dir,
        &wasm_with_custom_section("__wasm_bindgen_unstable", b"not a real schema"),
    );

    let result = run_cli(&input, &dir.path().join("glue.js"));

    assert!(
        !result.status.success(),
        "a wasm-bindgen module must be bundled through the interop path or rejected, not emitted as plain wasm_lite glue"
    );
}

#[test]
fn worker_collision_does_not_modify_the_primary_output() {
    let dir = TempDir::new("worker_collision");
    let input = write_input(&dir, &wasm_with_imported_memory(true));
    let output = dir.path().join("glue.js");
    let original = b"existing primary output\n";
    fs::write(&output, original).unwrap();
    fs::write(
        dir.path().join("glue.js.worker.js"),
        b"handwritten worker\n",
    )
    .unwrap();

    let result = run_cli(&input, &output);

    assert!(!result.status.success(), "the worker collision must fail");
    assert!(
        fs::read(output).unwrap() == original,
        "a failed multi-file generation must not partially overwrite the primary output"
    );
}

#[test]
fn worker_output_name_never_replaces_the_requested_glue() {
    let dir = TempDir::new("worker_as_output");
    let input = write_input(&dir, &wasm_with_imported_memory(true));
    let output = dir.path().join("wl_worker.js");

    let result = run_cli(&input, &output);
    let generated = fs::read_to_string(&output).unwrap_or_default();

    if result.status.success() {
        assert!(
            generated.contains("export function makeImports()"),
            "successful -o output must contain the requested glue, not the worker bootstrap"
        );
    } else {
        assert!(
            !output.exists(),
            "rejecting a reserved output name must happen before creating it"
        );
    }
}

#[test]
fn generating_a_neighboring_bundle_does_not_retarget_the_first_worker() {
    let dir = TempDir::new("neighboring_bundles");
    let input = write_input(&dir, &wasm_with_imported_memory(true));
    let first = run_cli(&input, &dir.path().join("first.js"));
    assert!(first.status.success());
    let first_worker_path = dir.path().join("first.js.worker.js");
    let first_worker = fs::read(&first_worker_path).unwrap();

    let second = run_cli(&input, &dir.path().join("second.js"));

    assert!(second.status.success());
    assert_eq!(fs::read(first_worker_path).unwrap(), first_worker);
    assert!(dir.path().join("second.js.worker.js").exists());
}

#[test]
fn non_shared_imported_memory_does_not_emit_a_worker() {
    let dir = TempDir::new("unshared_memory");
    let input = write_input(&dir, &wasm_with_imported_memory(false));

    let result = run_cli(&input, &dir.path().join("glue.js"));

    assert!(result.status.success());
    assert!(
        !dir.path().join("glue.js.worker.js").exists() && !dir.path().join("wl_worker.js").exists(),
        "ordinary imported memory cannot be shared with a Web Worker"
    );
}

#[test]
fn worker_import_percent_encodes_the_glue_filename() {
    let dir = TempDir::new("url_filename");
    let input = write_input(&dir, &wasm_with_imported_memory(true));

    let result = run_cli(&input, &dir.path().join("app#v1.js"));
    assert!(result.status.success());
    let worker = fs::read_to_string(dir.path().join("app#v1.js.worker.js")).unwrap();

    assert!(
        worker.contains("from \"./app%23v1.js\""),
        "the module specifier must encode `#` as part of the filename"
    );
}

#[test]
fn a_closed_stdout_pipe_is_not_a_panic() {
    let dir = TempDir::new("broken_pipe");
    let mut descriptors = String::new();
    for i in 0..20_000 {
        writeln!(descriptors, "f|console|binding_{i}|log|||").unwrap();
    }
    let input = write_input(
        &dir,
        &wasm_with_custom_section("__wasm_lite_imports", descriptors.as_bytes()),
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_wasm_lite"))
        .arg("build")
        .arg(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let result = child.wait_with_output().unwrap();

    assert!(
        result.status.success(),
        "a downstream reader closing normally should not produce a panic: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
