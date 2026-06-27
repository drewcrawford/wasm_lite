//! Rust functions exported to JS via `#[wasm_lite::export]`.
//!
//! Each export is one line in the `__wl_exports` section: `name|argtags|rettag`.

/// A Rust function exported to JavaScript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The JS-callable name (the Rust fn name).
    pub name: String,
    /// Argument types, in order.
    pub args: Vec<ExportArg>,
    /// How the return value is presented to JS.
    pub ret: ExportRet,
}

/// How an exported function's argument crosses from JS into wasm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportArg {
    /// A number/bool, passed through.
    Num,
    /// A `&str`: allocate in wasm memory, write, pass `(ptr, len)`, then free.
    Str,
}

/// How an export's return value is presented to JS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportRet {
    /// No return value.
    Void,
    /// An `i32` to coerce to a JS boolean.
    Bool,
    /// A number, returned directly.
    Value,
    /// A `String`: returned as a packed `(ptr, len)` to decode and free.
    Str,
}

/// Read exported-function descriptors from a compiled wasm module.
pub fn exports_from_wasm(wasm: &[u8]) -> Result<Vec<Export>, String> {
    match crate::wasm::custom_section(wasm, "__wl_exports")? {
        Some(bytes) => parse(bytes),
        None => Ok(Vec::new()),
    }
}

fn parse(bytes: &[u8]) -> Result<Vec<Export>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("exports section is not UTF-8: {e}"))?;

    let mut exports = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('|');
        let name = fields.next().unwrap_or_default();
        let arg_tags = fields.next().unwrap_or_default();
        let ret_tag = fields.next().unwrap_or_default();

        if name.is_empty() {
            return Err(format!("malformed export line: {line:?}"));
        }

        let args = arg_tags
            .split(',')
            .filter(|t| !t.is_empty())
            .map(|t| if t == "str" { ExportArg::Str } else { ExportArg::Num })
            .collect();
        let ret = match ret_tag {
            "" => ExportRet::Void,
            "bool" => ExportRet::Bool,
            "str" => ExportRet::Str,
            _ => ExportRet::Value,
        };

        exports.push(Export {
            name: name.to_string(),
            args,
            ret,
        });
    }
    Ok(exports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exports() {
        let section = b"add|i32,i32|i32\nis_even|i32|bool\ngreet|str|str\ntick||\n";
        assert_eq!(
            parse(section).unwrap(),
            vec![
                Export { name: "add".into(), args: vec![ExportArg::Num, ExportArg::Num], ret: ExportRet::Value },
                Export { name: "is_even".into(), args: vec![ExportArg::Num], ret: ExportRet::Bool },
                Export { name: "greet".into(), args: vec![ExportArg::Str], ret: ExportRet::Str },
                Export { name: "tick".into(), args: vec![], ret: ExportRet::Void },
            ]
        );
    }
}
