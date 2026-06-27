//! The descriptor format written by the `import!` macro.
//!
//! Each import is one line: `kind|namespace|import_name|js_name|argtags|rettag\n`,
//! where `kind` is `f` (namespaced function) or `m` (method on a handle
//! receiver), `argtags` is a comma-separated list (possibly empty) and `rettag`
//! is empty for a function that returns nothing.
//!
//! `import_name` is the wasm import symbol (unique per binding — it carries the
//! crate/module path); `js_name` is the JavaScript function the shim actually
//! calls. They differ for overloads, where several Rust functions bind the same
//! JS function.

/// Whether an import is a namespaced free function or a method on a receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `globalThis[namespace][js_name](args)`.
    Function,
    /// `receiver[js_name](args)`, where the first argument is the handle receiver.
    Method,
}

/// The return marshalling of an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ret {
    /// No return value.
    Void,
    /// A JS object: store it in the value table and return the handle.
    Handle,
    /// A JS string: allocate it in wasm memory and return a packed `(ptr, len)`.
    Str,
    /// A primitive returned directly (the tag is kept for documentation).
    Value(String),
}

/// A single imported JavaScript function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// Whether this is a namespaced function or a method call.
    pub kind: Kind,
    /// JS namespace, e.g. `console` (unused for methods, but keys the slot).
    pub namespace: String,
    /// The wasm import name; keys the import object slot.
    pub import_name: String,
    /// The JavaScript function name the shim calls (may differ from `import_name`).
    pub js_name: String,
    /// Argument types, in declaration order (for methods, the first is the receiver).
    pub args: Vec<AbiArg>,
    /// How the return value is marshalled.
    pub ret: Ret,
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
    /// A `&JsValue`: arrives as one `u32` index; look up in the value table.
    Handle,
}

impl AbiArg {
    /// Number of wasm-level parameters this argument occupies.
    pub fn param_count(self) -> usize {
        match self {
            AbiArg::Str => 2,
            AbiArg::Bool | AbiArg::Num | AbiArg::Handle => 1,
        }
    }

    fn from_tag(tag: &str) -> Self {
        match tag {
            "str" => AbiArg::Str,
            "bool" => AbiArg::Bool,
            "handle" => AbiArg::Handle,
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
        let kind_tag = fields.next().unwrap_or_default();
        let namespace = fields.next().unwrap_or_default();
        let import_name = fields.next().unwrap_or_default();
        let js_name = fields.next().unwrap_or_default();
        let arg_tags = fields.next().unwrap_or_default();
        let ret_tag = fields.next().unwrap_or_default();

        if namespace.is_empty() || import_name.is_empty() || js_name.is_empty() {
            return Err(format!("malformed descriptor line: {line:?}"));
        }

        let kind = match kind_tag {
            "f" => Kind::Function,
            "m" => Kind::Method,
            other => return Err(format!("unknown import kind {other:?} in {line:?}")),
        };

        let args: Vec<AbiArg> = arg_tags
            .split(',')
            .filter(|t| !t.is_empty())
            .map(AbiArg::from_tag)
            .collect();

        if kind == Kind::Method && args.first() != Some(&AbiArg::Handle) {
            return Err(format!("method {import_name:?} needs a handle receiver"));
        }

        let ret = match ret_tag {
            "" => Ret::Void,
            "handle" => Ret::Handle,
            "str" => Ret::Str,
            other => Ret::Value(other.to_string()),
        };

        descriptors.push(Descriptor {
            kind,
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
    fn parses_functions_methods_and_handles() {
        let section = b"f|console|c::log|log|str|\n\
                        f|JSON|c::parse|parse|str|handle\n\
                        m|Array|c::push|push|handle,f64|f64\n";
        let got = parse(section).unwrap();
        assert_eq!(
            got,
            vec![
                Descriptor {
                    kind: Kind::Function,
                    namespace: "console".into(),
                    import_name: "c::log".into(),
                    js_name: "log".into(),
                    args: vec![AbiArg::Str],
                    ret: Ret::Void,
                },
                Descriptor {
                    kind: Kind::Function,
                    namespace: "JSON".into(),
                    import_name: "c::parse".into(),
                    js_name: "parse".into(),
                    args: vec![AbiArg::Str],
                    ret: Ret::Handle,
                },
                Descriptor {
                    kind: Kind::Method,
                    namespace: "Array".into(),
                    import_name: "c::push".into(),
                    js_name: "push".into(),
                    args: vec![AbiArg::Handle, AbiArg::Num],
                    ret: Ret::Value("f64".into()),
                },
            ]
        );
    }

    #[test]
    fn rejects_method_without_receiver() {
        let section = b"m|Array|c::bad|bad|f64|\n";
        assert!(parse(section).is_err());
    }
}
