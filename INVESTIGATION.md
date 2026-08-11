# Investigation

## Original problem

Audit `wasm_lite_codegen` for correctness and security bugs, then fix any issues that can be demonstrated with regression tests.

## Changes made

* Changed generated import maps, including the reserved `__wasm_lite` runtime
  namespace, to null-prototype objects so `__proto__` and inherited property
  names are always ordinary wasm import keys.
* Stopped interpolating export descriptor names as JavaScript identifiers or
  dotted properties. Generated wrappers now use stable internal identifiers,
  quoted export aliases, and bracketed wasm export lookup.
* Made custom-section scanning collect every matching section and continue
  validating the module framing after a match, rather than returning the first
  match and ignoring the rest of the file.
* Tightened descriptor/export grammar checks to reject truncated records,
  extra fields, empty interior argument tags, and invalid variadic flags.
* Validated wasm version, imported-memory limits, duplicate import sections,
  multiple imported memories, and trailing bytes in the import section.
* Added regression tests for each issue above.

## Verified

* The target crate is dependency-free and parses descriptors and wasm bytes before generating JavaScript, based on `AGENTS.md` and the crate source layout.
* No tracked code files were modified at the start of the investigation; this
  investigation log was the only untracked path.
* Ordinary JavaScript objects treat `__proto__` specially; the old generated
  `imports[ns][name] = shim` path could mutate an object's prototype instead of
  defining the requested import. The null-prototype regression test covers the
  reserved runtime namespace too.
* A crafted export name such as `x"); ... //` was inserted at two JavaScript
  source positions by the old generator. The new output quotes both positions,
  and its string-literal export alias syntax was accepted by Node v25.2.1.
* Wasm custom-section names are not unique. The former early return caused an
  empty first test/descriptor section to hide later entries; public reader tests
  now demonstrate that both sections are returned.
* `cargo test -p wasm_lite_codegen` passes all 36 tests and `cargo clippy
  -p wasm_lite_codegen --all-targets -- -D warnings` passes.
* Repository formatting/check stages and documentation passed. Native tests,
  Firefox browser tests, and the browser binding suites passed. The full test
  stage stopped only when ChromeDriver v145 refused installed Chrome v151.
* The full clippy stage is independently blocked by an unrelated concurrent
  macro test at `crates/wasm_lite_macro/tests/expansions.rs:40` (`unused_unit`).

## Ideas

* A future audit could make `build_interop` use an atomically-created unique
  temporary directory; its current PID-only path can collide between concurrent
  calls in one process.
* A future pass could examine exception cleanup in exported wrappers: trapped
  wasm calls currently skip frees for temporary argument/sret allocations.

## Steps taken

1. Loaded the repository instructions and the `investigate` skill workflow.
2. Located the crate at `crates/wasm_lite_codegen`; its source is split across descriptor parsing, export parsing, generation, interop, and wasm parsing modules.
3. Confirmed the tracked code worktree was initially clean.
4. Ran the baseline crate suite: all 27 original tests passed.
5. Audited descriptor and export parsing, wasm section/limit parsing, generated
   JavaScript object construction, string escaping, and export wrapper emission.
6. Reproduced and fixed prototype-key handling, export-name source injection,
   repeated-section truncation, ambiguous descriptor parsing, and invalid
   imported-memory acceptance.
7. Added 9 regression tests (36 total) and ran crate tests/clippy successfully.
8. Ran `scripts/check_all`: formatting and native/wasm checks passed; an
   unrelated concurrent macro-test lint stopped the clippy stage.
9. Ran the remaining tests directly. Native and Firefox/browser binding suites
   passed until the Chrome repeat encountered the installed driver/browser
   version mismatch. Ran `scripts/docs` successfully.
10. Re-ran the Firefox passing-suite smoke test after the final import-object
    hardening; both wasm tests passed.

## Notes

Unrelated edits appeared concurrently in `wasm_lite_macro` and its tests during
the investigation. They were left out of this change and were not reviewed.
