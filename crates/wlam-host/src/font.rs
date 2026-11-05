//! Browser Sans: a 12-pixel bitmap fallback derived from Liberation Sans.
//! See assets/default-font-LICENSE.txt. Glyphs use Windows-1252 byte indices.
const DATA: &[u8] = include_bytes!("../assets/default-font.bin");
pub fn extent(text: &[u8]) -> (u32, u32) {
    (
        text.iter()
            .map(|byte| DATA[4 + *byte as usize] as u32)
            .sum(),
        DATA[2] as u32,
    )
}
pub fn metrics() -> [u8; 56] {
    let mut bytes = [0; 56];
    let values = [
        DATA[2] as u32,
        DATA[0] as u32,
        DATA[1] as u32,
        2,
        0,
        6,
        DATA[3] as u32,
        400,
        0,
        96,
        96,
    ];
    for (i, value) in values.into_iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[44] = 0;
    bytes[45] = 255;
    bytes[46] = b'?';
    bytes[47] = b' ';
    bytes[51] = 0x21;
    bytes
}
pub fn glyph(byte: u8) -> &'static [u8] {
    let offset = 260 + byte as usize * 256;
    &DATA[offset..offset + 256]
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measured_advances_match_the_shipped_bitmap_font() {
        assert_eq!(extent(b"M"), (10, 14));
        assert_eq!(extent(b"MM"), (20, 14));
        assert!(glyph(b'M').iter().any(|alpha| *alpha != 0));
        assert_eq!(u32::from_le_bytes(metrics()[..4].try_into().unwrap()), 14);
    }
}
