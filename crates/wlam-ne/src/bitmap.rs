//! Windows 3.x RT_BITMAP payloads normalized to a 32-bit BI_RGB DIB for
//! the shared renderer. RLE escape/padding rules follow Microsoft's bitmap
//! compression specification; malformed streams fail before creating an object.
pub fn decode(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let word = |at: usize| -> Result<u16, String> {
        let b = bytes.get(at..at + 2).ok_or("truncated bitmap header")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    };
    let dword = |at: usize| -> Result<u32, String> {
        let b = bytes.get(at..at + 4).ok_or("truncated bitmap header")?;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    };
    let header = dword(0)? as usize;
    let (width, height, bits, compression, colors, entry) = match header {
        12 => {
            if word(8)? != 1 {
                return Err("invalid bitmap planes".into());
            }
            (i32::from(word(4)?), i32::from(word(6)?), word(10)?, 0, 0, 3)
        }
        40 => {
            if word(12)? != 1 {
                return Err("invalid bitmap planes".into());
            }
            (
                dword(4)? as i32,
                dword(8)? as i32,
                word(14)?,
                dword(16)?,
                dword(32)?,
                4,
            )
        }
        _ => return Err(format!("unsupported bitmap header {header}")),
    };
    if width <= 0 || height == 0 || height == i32::MIN || !matches!(bits, 1 | 4 | 8 | 24 | 32) {
        return Err("invalid bitmap dimensions/depth".into());
    }
    if compression != 0 && !matches!((bits, compression), (4, 2) | (8, 1)) {
        return Err("unsupported bitmap compression".into());
    }
    if compression != 0 && height < 0 {
        return Err("top-down compressed bitmap".into());
    }
    let width = width as usize;
    let rows = height.unsigned_abs() as usize;
    let count = width
        .checked_mul(rows)
        .filter(|v| *v <= 16 * 1024 * 1024)
        .ok_or("bitmap too large")?;
    let colors = if bits <= 8 {
        if colors == 0 {
            1usize << bits
        } else {
            colors as usize
        }
    } else {
        0
    };
    if colors > 256 || (bits <= 8 && colors > (1usize << bits)) {
        return Err("invalid bitmap palette size".into());
    }
    let palette = bytes
        .get(header..header + colors * entry)
        .ok_or("truncated bitmap palette")?;
    let payload = bytes
        .get(header + colors * entry..)
        .ok_or("truncated bitmap data")?;
    let mut pixels = vec![0u8; count * 4];
    let color = |index: usize| -> Result<[u8; 4], String> {
        let p = palette
            .get(index * entry..index * entry + entry)
            .ok_or("bitmap color index exceeds palette")?;
        Ok([p[0], p[1], p[2], 0])
    };
    if bits <= 8 {
        let zero = color(0)?;
        for p in pixels.chunks_exact_mut(4) {
            p.copy_from_slice(&zero);
        }
    }
    if compression == 0 {
        let stride = (width * usize::from(bits) + 31) / 32 * 4;
        let data = payload
            .get(..stride.checked_mul(rows).ok_or("bitmap size overflow")?)
            .ok_or("truncated bitmap rows")?;
        for y in 0..rows {
            for x in 0..width {
                let row = &data[y * stride..(y + 1) * stride];
                let value = match bits {
                    1 => color(usize::from((row[x / 8] >> (7 - x % 8)) & 1))?,
                    4 => color(usize::from(
                        (row[x / 2] >> (if x % 2 == 0 { 4 } else { 0 })) & 15,
                    ))?,
                    8 => color(usize::from(row[x]))?,
                    24 => [row[x * 3], row[x * 3 + 1], row[x * 3 + 2], 0],
                    _ => [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], 0],
                };
                pixels[(y * width + x) * 4..(y * width + x + 1) * 4].copy_from_slice(&value);
            }
        }
    } else {
        let mut position = 0;
        let mut x = 0usize;
        let mut y = 0usize;
        let mut done = false;
        while position + 2 <= payload.len() {
            let run = usize::from(payload[position]);
            let value = payload[position + 1];
            position += 2;
            if run == 0 {
                match value {
                    0 => {
                        x = 0;
                        y += 1;
                        continue;
                    }
                    1 => {
                        done = true;
                        break;
                    }
                    2 => {
                        let delta = payload
                            .get(position..position + 2)
                            .ok_or("truncated bitmap delta")?;
                        position += 2;
                        x += usize::from(delta[0]);
                        y += usize::from(delta[1]);
                        if x > width || y >= rows {
                            return Err("bitmap delta out of bounds".into());
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            let length = if run == 0 { usize::from(value) } else { run };
            if y >= rows || x + length > width {
                return Err("bitmap run out of bounds".into());
            }
            let absolute = if bits == 4 { (length + 1) / 2 } else { length };
            if run == 0 && position + absolute > payload.len() {
                return Err("truncated absolute bitmap run".into());
            }
            for i in 0..length {
                let b = if run == 0 {
                    payload[position + if bits == 4 { i / 2 } else { i }]
                } else {
                    value
                };
                let index = if bits == 4 {
                    usize::from((b >> (if i % 2 == 0 { 4 } else { 0 })) & 15)
                } else {
                    usize::from(b)
                };
                let c = color(index)?;
                let pixel = (y * width + x + i) * 4;
                pixels[pixel..pixel + 4].copy_from_slice(&c);
            }
            x += length;
            if run == 0 {
                position += (absolute + 1) & !1;
                if position > payload.len() {
                    return Err("missing bitmap word padding".into());
                }
            }
        }
        if !done {
            return Err("missing bitmap end marker".into());
        }
    }
    let mut output = vec![0; 40];
    output[0..4].copy_from_slice(&40u32.to_le_bytes());
    output[4..8].copy_from_slice(&(width as u32).to_le_bytes());
    output[8..12].copy_from_slice(&height.to_le_bytes());
    output[12..14].copy_from_slice(&1u16.to_le_bytes());
    output[14..16].copy_from_slice(&32u16.to_le_bytes());
    output[20..24].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
    output.extend_from_slice(&pixels);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bitmap(width: u32, height: u32, bits: u16, compression: u32, payload: &[u8]) -> Vec<u8> {
        let mut b = vec![0; 40];
        for (at, v) in [(0, 40), (4, width), (8, height), (16, compression)] {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[12..14].copy_from_slice(&1u16.to_le_bytes());
        b[14..16].copy_from_slice(&bits.to_le_bytes());
        for i in 0..(1u32 << bits) {
            b.extend_from_slice(&[i as u8, 0, 0, 0]);
        }
        b.extend_from_slice(payload);
        b
    }
    #[test]
    fn monochrome_bits_and_row_padding_are_decoded() {
        let decoded = decode(&bitmap(3, 2, 1, 0, &[0xa0, 0, 0, 0, 0x40, 0, 0, 0])).unwrap();
        assert_eq!(
            decoded[40..]
                .chunks_exact(4)
                .map(|p| p[0])
                .collect::<Vec<_>>(),
            [1, 0, 1, 0, 1, 0]
        );
    }
    #[test]
    fn rle4_encoded_absolute_and_delta_commands_preserve_palette_indices() {
        let b = bitmap(
            8,
            2,
            4,
            2,
            &[3, 0x12, 0, 2, 1, 0, 0, 3, 0x34, 0x50, 0, 0, 8, 0x67, 0, 1],
        );
        let decoded = decode(&b).unwrap();
        assert_eq!(
            decoded[40..]
                .chunks_exact(4)
                .map(|p| p[0])
                .collect::<Vec<_>>(),
            [1, 2, 1, 0, 3, 4, 5, 0, 6, 7, 6, 7, 6, 7, 6, 7]
        );
        assert!(decode(&bitmap(2, 1, 4, 2, &[3, 0x12, 0, 1])).is_err());
        assert!(decode(&bitmap(4, 1, 4, 2, &[0, 3, 0x12])).is_err());
    }
}
