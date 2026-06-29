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

// Imports that *return* strings: the host allocates the JS string into wasm
// memory and Rust receives an owned `String`.
mod strings {
    wasm_lite::import! {
        "String" {
            fn from_char_code(code: i32) -> String as "fromCharCode";
        }
        "JSON" {
            fn stringify(value: &str) -> String;  // &str in, String out
        }
        "console" {
            fn log_bytes(data: &[u8]) as "log";   // &[u8] in -> console.log(Uint8Array)
        }
        "Uint8Array" {
            fn of3(a: i32, b: i32, c: i32) -> Vec<u8> as "of";  // JS bytes -> Vec<u8>
        }
    }
}

// An import that hands us a fresh JS array to wrap.
mod arr {
    wasm_lite::import! {
        "Array" { fn of3(a: f64, b: f64, c: f64) -> JsValue as "of"; }
    }
}

// Imports with optional (`Option`) arguments — None is passed to JS as
// `undefined`, so the JS default applies (radix 10; default "," separator).
mod opt_in {
    use wasm_lite::JsValue;
    wasm_lite::import! {
        "Number" { fn parse_int(s: &str, radix: Option<f64>) -> f64 as "parseInt"; }
        "Array" { fn join_opt(this: &JsValue, sep: Option<&str>) -> String as "join"; }
    }
}

// Fallible imports: JSON.parse yields `null` for "null" and *throws* on bad
// input, so the same JS function models both `Option` and `Result` returns.
mod fallible {
    use wasm_lite::JsValue;
    wasm_lite::import! {
        "JSON" {
            fn parse_num(text: &str) -> Option<f64> as "parse";          // null -> None
            fn try_parse(text: &str) -> Result<f64, JsValue> as "parse"; // throw -> Err
        }
    }
}

// A typed handle wrapper over a JS `Array` — methods lower to `arr[name](args)`,
// reusing the import! ABI. Object args/returns (`&JsArray`/`JsArray`) cross as
// handles and are wrapped automatically.
wasm_lite::js_class! {
    type JsArray;
    impl JsArray {
        fn push(&self, value: f64) -> f64;
        fn join(&self, sep: &str) -> String;
        fn concat(&self, other: &JsArray) -> JsArray;
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

    // Imports returning owned Strings (host allocates into wasm memory).
    let a: String = strings::from_char_code(65);
    wasm_lite::console::log(&format!("String.fromCharCode(65) = {a}"));
    let s: String = strings::stringify("hi 🌍");
    wasm_lite::console::log(&format!("JSON.stringify(\"hi 🌍\") = {s}"));

    // A borrowed byte slice passed to JS (no UTF-8 — raw bytes as a Uint8Array).
    strings::log_bytes(&[72, 73, 74]); // console.log(Uint8Array[72, 73, 74])

    // An owned Vec<u8> received from JS (Uint8Array.of -> Rust takes ownership).
    let v: Vec<u8> = strings::of3(1, 2, 3);
    wasm_lite::console::log(&format!("Uint8Array.of(1,2,3) -> Vec<u8> = {v:?} (len {})", v.len()));

    // Typed JS object via js_class!: wrap an array handle, call typed methods.
    let a = JsArray::from_js(arr::of3(1.0, 2.0, 3.0));
    let len = a.push(4.0); // arr.push(4) -> new length
    wasm_lite::console::log(&format!("JsArray::push(4) -> len {len}, join = {}", a.join("-")));
    let b = JsArray::from_js(arr::of3(5.0, 6.0, 7.0));
    let c = a.concat(&b); // typed arg + typed return -> JsArray
    wasm_lite::console::log(&format!("a.concat(b).join(\",\") = {}", c.join(",")));

    // Option *arguments* on imports (None -> JS undefined -> JS default applies).
    wasm_lite::console::log(&format!("parse_int(\"ff\", Some(16)) = {}", opt_in::parse_int("ff", Some(16.0))));
    wasm_lite::console::log(&format!("parse_int(\"10\", None) = {}", opt_in::parse_int("10", None)));
    wasm_lite::console::log(&format!("join_opt(Some(\"-\")) = {}", opt_in::join_opt(c.as_js(), Some("-"))));
    wasm_lite::console::log(&format!("join_opt(None) = {}", opt_in::join_opt(c.as_js(), None)));

    // Option/Result imports (JS null -> None; JS throw -> Err).
    wasm_lite::console::log(&format!("parse_num(\"3.14\") = {:?}", fallible::parse_num("3.14")));
    wasm_lite::console::log(&format!("parse_num(\"null\") = {:?}", fallible::parse_num("null")));
    match fallible::try_parse("42") {
        Ok(n) => wasm_lite::console::log(&format!("try_parse(\"42\") = Ok({n})")),
        Err(_) => wasm_lite::console::log("try_parse(\"42\") = Err"),
    }
    match fallible::try_parse("{bad json") {
        Ok(n) => wasm_lite::console::log(&format!("try_parse = Ok({n})")),
        Err(e) => {
            wasm_lite::console::log("try_parse(\"{bad json\") = Err, error object:");
            jsapi::log_value(&e); // log the caught JS exception
        }
    }
}
