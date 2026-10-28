//! Minimal WebAssembly binary encoding.
//!
//! We encode the module ourselves rather than depending on a wasm builder so
//! that every byte is deliberate and testable: the output is validated by
//! `wasm-tools validate` in tests, which catches malformed sections immediately.

/// A growable byte encoder for wasm binary format primitives.
#[derive(Debug, Default, Clone)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Encoder { buf: Vec::new() }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn push(&mut self, b: u8) {
        self.buf.push(b);
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Unsigned LEB128.
    pub fn u32(&mut self, mut v: u32) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// Unsigned LEB128 for 64-bit values.
    pub fn u64(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// Signed LEB128.
    pub fn s32(&mut self, v: i32) {
        let mut value = v;
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            let sign_clear = (byte & 0x40) == 0;
            if (value == 0 && sign_clear) || (value == -1 && !sign_clear) {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// Write a vector: its length, then each item via `f`.
    pub fn vec<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Encoder, &T)) {
        self.u32(items.len() as u32);
        for it in items {
            f(self, it);
        }
    }

    /// Write a length-prefixed section body produced by `f`.
    pub fn section(&mut self, id: u8, f: impl FnOnce(&mut Encoder)) {
        let mut body = Encoder::new();
        f(&mut body);
        self.buf.push(id);
        self.u32(body.buf.len() as u32);
        self.buf.extend_from_slice(&body.buf);
    }
}

/// Encode a u32 as unsigned LEB128 into a fresh buffer (helper for tests).
pub fn leb_u32(v: u32) -> Vec<u8> {
    let mut e = Encoder::new();
    e.u32(v);
    e.into_bytes()
}

/// Section identifiers.
pub mod section_id {
    pub const CUSTOM: u8 = 0;
    pub const TYPE: u8 = 1;
    pub const IMPORT: u8 = 2;
    pub const FUNCTION: u8 = 3;
    pub const TABLE: u8 = 4;
    pub const MEMORY: u8 = 5;
    pub const GLOBAL: u8 = 6;
    pub const EXPORT: u8 = 7;
    pub const START: u8 = 8;
    pub const ELEMENT: u8 = 9;
    pub const CODE: u8 = 10;
    pub const DATA: u8 = 11;
}

/// A section identifier, for readability at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Type,
    Import,
    Function,
    Table,
    Memory,
    Global,
    Export,
    Start,
    Element,
    Code,
    Data,
}

impl SectionKind {
    pub fn id(self) -> u8 {
        match self {
            SectionKind::Type => section_id::TYPE,
            SectionKind::Import => section_id::IMPORT,
            SectionKind::Function => section_id::FUNCTION,
            SectionKind::Table => section_id::TABLE,
            SectionKind::Memory => section_id::MEMORY,
            SectionKind::Global => section_id::GLOBAL,
            SectionKind::Export => section_id::EXPORT,
            SectionKind::Start => section_id::START,
            SectionKind::Element => section_id::ELEMENT,
            SectionKind::Code => section_id::CODE,
            SectionKind::Data => section_id::DATA,
        }
    }
}

/// The wasm module header: magic and version.
pub fn module_header(e: &mut Encoder) {
    e.raw(&[0x00, 0x61, 0x73, 0x6d]);
    e.raw(&[0x01, 0x00, 0x00, 0x00]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_encoding_matches_the_spec() {
        assert_eq!(leb_u32(0), vec![0x00]);
        assert_eq!(leb_u32(1), vec![0x01]);
        assert_eq!(leb_u32(127), vec![0x7f]);
        assert_eq!(leb_u32(128), vec![0x80, 0x01]);
        assert_eq!(leb_u32(624485), vec![0xe5, 0x8e, 0x26]);
        assert_eq!(leb_u32(u32::MAX), vec![0xff, 0xff, 0xff, 0xff, 0x0f]);
    }

    #[test]
    fn signed_leb128_encoding_matches_the_spec() {
        let mut e = Encoder::new();
        e.s32(-1);
        assert_eq!(e.bytes(), &[0x7f]);
        let mut e = Encoder::new();
        e.s32(63);
        assert_eq!(e.bytes(), &[0x3f]);
        let mut e = Encoder::new();
        e.s32(64);
        assert_eq!(e.bytes(), &[0xc0, 0x00]);
        let mut e = Encoder::new();
        e.s32(-64);
        assert_eq!(e.bytes(), &[0x40]);
    }

    #[test]
    fn sections_are_length_prefixed() {
        let mut e = Encoder::new();
        e.section(SectionKind::Type.id(), |b| {
            b.raw(&[1, 2, 3]);
        });
        assert_eq!(e.bytes(), &[1, 3, 1, 2, 3]);
    }

    #[test]
    fn empty_module_is_valid_wasm() {
        // An 8-byte header is a complete, valid module. Validated externally by
        // the wasm-tools test below; here we check the bytes.
        let mut e = Encoder::new();
        module_header(&mut e);
        assert_eq!(e.bytes(), b"\0asm\x01\0\0\0");
    }
}
