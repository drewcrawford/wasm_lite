// The first wasm_lite Rust example: an idiomatic `bin` whose `main` is called
// by the runner's loader after instantiation. No `#[no_mangle]` and no codegen
// — a wasm `bin` exports its `main` automatically.

fn main() {
    wasm_lite::console::log("hello, world from rust");
}
