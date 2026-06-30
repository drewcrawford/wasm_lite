// SPDX-License-Identifier: MIT OR Apache-2.0
//! Minimal reader for the wasm binary format — just enough to find a custom
//! section by name. See <https://webassembly.github.io/spec/core/binary/>.

/// Return the payload of the custom section named `name`, if present.
pub fn custom_section<'a>(wasm: &'a [u8], name: &str) -> Result<Option<&'a [u8]>, String> {
    let mut r = Reader::new(wasm);
    if r.take(4)? != b"\0asm" {
        return Err("not a wasm module (bad magic)".to_string());
    }
    let _version = r.take(4)?;

    while !r.eof() {
        let id = r.byte()?;
        let size = r.leb_u32()? as usize;
        let body = r.take(size)?;

        // Section id 0 is a custom section: a name followed by raw contents.
        if id == 0 {
            let mut br = Reader::new(body);
            let name_len = br.leb_u32()? as usize;
            let section_name = br.take(name_len)?;
            if section_name == name.as_bytes() {
                return Ok(Some(&body[br.pos..]));
            }
        }
    }
    Ok(None)
}

/// An imported memory, as produced by linking with `--import-memory`
/// (e.g. shared-memory `+atomics` builds, where JS creates the `WebAssembly.Memory`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct MemoryImport {
    /// Import module (LLD uses `env`).
    pub module: String,
    /// Import field name (LLD uses `memory`).
    pub name: String,
    /// Initial size in 64 KiB pages.
    pub initial: u32,
    /// Maximum size in pages (required for shared memory).
    pub maximum: Option<u32>,
    /// Whether the memory is `shared` (backed by a `SharedArrayBuffer`).
    pub shared: bool,
}

/// Find the module's imported memory, if any.
///
/// Modules that define+export their own memory (the default) return `None`; a
/// `--import-memory` build (shared `+atomics`) returns the memory's limits so
/// the glue can create a matching `WebAssembly.Memory` and supply it.
pub fn imported_memory(wasm: &[u8]) -> Result<Option<MemoryImport>, String> {
    let mut r = Reader::new(wasm);
    if r.take(4)? != b"\0asm" {
        return Err("not a wasm module (bad magic)".to_string());
    }
    let _version = r.take(4)?;

    while !r.eof() {
        let id = r.byte()?;
        let size = r.leb_u32()? as usize;
        let body = r.take(size)?;
        // Section id 2 is the import section.
        if id == 2 {
            return parse_imports_for_memory(body);
        }
    }
    Ok(None)
}

/// Scan an import section's body for a memory import.
fn parse_imports_for_memory(body: &[u8]) -> Result<Option<MemoryImport>, String> {
    let mut r = Reader::new(body);
    let count = r.leb_u32()?;
    for _ in 0..count {
        let module = r.name()?;
        let name = r.name()?;
        // Import kind: 0=func, 1=table, 2=memory, 3=global.
        match r.byte()? {
            0x00 => {
                r.leb_u32()?; // type index
            }
            0x01 => {
                r.byte()?; // reftype
                r.skip_limits()?;
            }
            0x02 => {
                let (initial, maximum, shared) = r.read_limits()?;
                return Ok(Some(MemoryImport {
                    module,
                    name,
                    initial,
                    maximum,
                    shared,
                }));
            }
            0x03 => {
                r.byte()?; // valtype
                r.byte()?; // mutability
            }
            other => return Err(format!("unknown import kind {other} in import section")),
        }
    }
    Ok(None)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.buf.get(self.pos).ok_or("unexpected end of wasm")?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or("unexpected end of wasm")?;
        self.pos = end;
        Ok(slice)
    }

    /// Read a name: a LEB128 length followed by that many UTF-8 bytes.
    fn name(&mut self) -> Result<String, String> {
        let len = self.leb_u32()? as usize;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|e| format!("import name is not UTF-8: {e}"))
    }

    /// Read a limits descriptor: `(min, max, shared)`.
    ///
    /// Flags bit 0 = has-max, bit 1 = shared (threads proposal), bit 2 = 64-bit.
    fn read_limits(&mut self) -> Result<(u32, Option<u32>, bool), String> {
        let flags = self.byte()?;
        let has_max = flags & 0x01 != 0;
        let shared = flags & 0x02 != 0;
        let min = self.leb_u32()?;
        let max = if has_max { Some(self.leb_u32()?) } else { None };
        Ok((min, max, shared))
    }

    /// Skip a limits descriptor (for table imports we don't care about).
    fn skip_limits(&mut self) -> Result<(), String> {
        let flags = self.byte()?;
        self.leb_u32()?;
        if flags & 0x01 != 0 {
            self.leb_u32()?;
        }
        Ok(())
    }

    /// Read an unsigned LEB128 value (used for section sizes and name lengths).
    fn leb_u32(&mut self) -> Result<u32, String> {
        let mut result = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            result |= u32::from(byte & 0x7f)
                .checked_shl(shift)
                .ok_or("LEB128 overflow")?;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift >= 32 {
                return Err("LEB128 too long".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_shared_imported_memory() {
        // A module whose only section is an import of `env.memory`, a shared
        // memory with initial 17 pages and max 16384 (LEB `80 80 01`).
        let wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic + version
            0x02, 0x12, // import section, body length 18
            0x01, // one import
            0x03, b'e', b'n', b'v', // module "env"
            0x06, b'm', b'e', b'm', b'o', b'r', b'y', // name "memory"
            0x02, // kind: memory
            0x03, 0x11, 0x80, 0x80, 0x01, // limits: has_max|shared, min 17, max 16384
        ];
        assert_eq!(
            imported_memory(wasm).unwrap(),
            Some(MemoryImport {
                module: "env".into(),
                name: "memory".into(),
                initial: 17,
                maximum: Some(16384),
                shared: true,
            })
        );
    }

    #[test]
    fn no_imported_memory_when_absent() {
        let wasm: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        assert_eq!(imported_memory(wasm).unwrap(), None);
    }
}
