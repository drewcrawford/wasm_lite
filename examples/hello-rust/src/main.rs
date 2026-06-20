// Exercises generalized imports: two `&str` imports (console.log / console.error)
// and a numeric-returning import (performance.now), all from the wasm_lite lib.
//
// The `js` module additionally shows that *any* crate can declare its own
// bindings, and that two `import!` calls now coexist in one module (the
// anonymous-const trick) — previously this required separate modules.
mod js {
    wasm_lite::import! {
        "Math" {
            fn random() -> f64;
            // Two Rust fns bound to the *same* JS function `Math.max` — an
            // overload, enabled by `as "max"` decoupling the JS name from the
            // (unique) Rust/import name.
            fn max2(a: f64, b: f64) -> f64 as "max";
            fn max3(a: f64, b: f64, c: f64) -> f64 as "max";
        }
    }
    wasm_lite::import! { "Date" { fn now() -> f64; } }
}

// Independently bind `console.log` with a *different* signature (a number) than
// `wasm_lite::console::log` (which takes `&str`). Both are import `(console, log)`
// by JS name — before per-binding-unique symbols this was a hard link conflict;
// now each gets a distinct wasm symbol, so they coexist.
mod my_console {
    wasm_lite::import! { "console" { fn log(n: f64); } }
}

// JS object handles: pass/return live JS values via the value table.
mod jsapi {
    use wasm_lite::JsValue;
    wasm_lite::import! {
        "JSON" {
            fn parse(text: &str) -> JsValue;   // returns an object handle
        }
        "Array" {
            fn push(this: &JsValue, value: f64) -> f64;  // method on a handle
        }
        "console" {
            fn log_value(value: &JsValue) as "log";      // handle as an argument
        }
    }
}

fn main() {
    wasm_lite::console::log("hello, world from rust");
    wasm_lite::console::error("this is console.error from rust");

    let t = wasm_lite::performance::now();
    wasm_lite::console::log(&format!("performance.now() returned {t}"));

    wasm_lite::console::log(&format!("Math.random() returned {}", js::random()));
    wasm_lite::console::log(&format!("Date.now() returned {}", js::now()));

    // Both call JS `Math.max`, with different arities.
    wasm_lite::console::log(&format!("Math.max(3, 7) = {}", js::max2(3.0, 7.0)));
    wasm_lite::console::log(&format!("Math.max(3, 7, 5) = {}", js::max3(3.0, 7.0, 5.0)));

    // A second, independent binding of console.log (number signature).
    my_console::log(42.0);

    // Value-table handles: parse a JS array, call a method on it, log the object.
    let arr = jsapi::parse("[1, 2, 3]"); // -> JsValue handle to a JS array
    let new_len = jsapi::push(&arr, 4.0); // arr.push(4) -> new length
    wasm_lite::console::log(&format!("after push, array length = {new_len}"));
    jsapi::log_value(&arr); // console.log(arr) -> shows the live JS array
    // `arr` drops here, freeing its value-table slot via __wl_drop.
}
