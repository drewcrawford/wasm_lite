//! The descriptor format written by the `import!` macro.
//!
//! Each import is one line: `namespace|import_name|js_name|argtags|rettag\n`,
//! where `argtags` is a comma-separated list (possibly empty) and `rettag` is
//! empty for a function that returns nothing.
//!
//! `import_name` is the wasm import symbol (unique per binding — it's the Rust
//! fn name); `js_name` is the JavaScript function the shim actually calls. They
//! differ when several Rust functions bind the same (possibly overloaded) JS
//! function.

/// A single imported JavaScript function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// JS namespace, e.g. `console` (the object the function hangs off).
    pub namespace: String,
    /// The wasm import name (the Rust fn name); keys the import object slot.
    pub import_name: String,
    /// The JavaScript function name the shim calls (may differ from `import_name`).
    pub js_name: String,
    /// Argument types, in declaration order.
    pub args: Vec<AbiArg>,
    /// Return type tag (e.g. `f64`), or `None` for no return value.
    pub ret: Option<String>,
}

/// How an argument crosses the wasm boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiArg {
    /// A `&str`: arrives as two wasm params `(ptr, len)`; decode from memory.
    Str,
    /// A `bool`: arrives as one `i32`; present to JS as a real boolean.
    Bool,
    /// A numeric type (`i32`, `u32`, `f64`, ...): one param, passed through.
    Num,
}

impl AbiArg {
    /// Number of wasm-level parameters this argument occupies.
    pub fn param_count(self) -> usize {
        match self {
            AbiArg::Str => 2,
            AbiArg::Bool | AbiArg::Num => 1,
        }
    }

    fn from_tag(tag: &str) -> Self {
        match tag {
            "str" => AbiArg::Str,
            "bool" => AbiArg::Bool,
            _ => AbiArg::Num,
        }
    }
}

/// Parse the descriptor section's bytes into a list of descriptors.
pub fn parse(bytes: &[u8]) -> Result<Vec<Descriptor>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("descriptor section is not UTF-8: {e}"))?;

    let mut descriptors = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }

        let mut fields = line.split('|');
        let namespace = fields.next().unwrap_or_default();
        let import_name = fields.next().unwrap_or_default();
        let js_name = fields.next().unwrap_or_default();
        let arg_tags = fields.next().unwrap_or_default();
        let ret_tag = fields.next().unwrap_or_default();

        if namespace.is_empty() || import_name.is_empty() || js_name.is_empty() {
            return Err(format!("malformed descriptor line: {line:?}"));
        }

        let args = arg_tags
            .split(',')
            .filter(|t| !t.is_empty())
            .map(AbiArg::from_tag)
            .collect();
        let ret = if ret_tag.is_empty() {
            None
        } else {
            Some(ret_tag.to_string())
        };

        descriptors.push(Descriptor {
            namespace: namespace.to_string(),
            import_name: import_name.to_string(),
            js_name: js_name.to_string(),
            args,
            ret,
        });
    }

    Ok(descriptors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_descriptors() {
        // Includes an overload: two import names mapping to the JS `max`.
        let section = b"console|log|log|str|\nperformance|now|now||f64\nMath|max2|max|f64,f64|f64\n";
        let got = parse(section).unwrap();
        assert_eq!(
            got,
            vec![
                Descriptor {
                    namespace: "console".into(),
                    import_name: "log".into(),
                    js_name: "log".into(),
                    args: vec![AbiArg::Str],
                    ret: None,
                },
                Descriptor {
                    namespace: "performance".into(),
                    import_name: "now".into(),
                    js_name: "now".into(),
                    args: vec![],
                    ret: Some("f64".into()),
                },
                Descriptor {
                    namespace: "Math".into(),
                    import_name: "max2".into(),
                    js_name: "max".into(),
                    args: vec![AbiArg::Num, AbiArg::Num],
                    ret: Some("f64".into()),
                },
            ]
        );
    }
}
