// Exercises generalized imports: two `&str` imports (console.log / console.error)
// and a numeric-returning import (performance.now), all from the wasm_lite lib.
//
// The `js` module additionally shows that *any* crate can declare its own
// bindings, and that two `import!` calls now coexist in one module (the
// anonymous-const trick) — previously this required separate modules.
mod js {
    wasm_lite::import! { "Math" { fn random() -> f64; } }
    wasm_lite::import! { "Date" { fn now() -> f64; } }
}

fn main() {
    wasm_lite::console::log("hello, world from rust");
    wasm_lite::console::error("this is console.error from rust");

    let t = wasm_lite::performance::now();
    wasm_lite::console::log(&format!("performance.now() returned {t}"));

    wasm_lite::console::log(&format!("Math.random() returned {}", js::random()));
    wasm_lite::console::log(&format!("Date.now() returned {}", js::now()));
}
