//! Classic RT_ICON DIB: an XOR color bitmap followed by a one-bit AND mask.
#[derive(Debug)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    /// RGB contains XOR bits; the fourth byte contains the expanded AND bit.
    pub pixels: Vec<[u8; 4]>,
}
pub fn decode(bytes: &[u8]) -> Result<Icon, String> {
    let header = bytes.get(..40).ok_or("truncated icon header")?;
    let long = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let short = |at: usize| u16::from_le_bytes(header[at..at + 2].try_into().unwrap());
    let (width, total_height, bits) = (long(4) as i32, long(8) as i32, short(14));
    if long(0) != 40 || short(12) != 1 || long(16) != 0 || !matches!(bits, 1 | 4 | 8 | 24 | 32) {
        return Err("unsupported icon DIB format".into());
    }
    if !(1..=256).contains(&width) || !(2..=512).contains(&total_height) || total_height % 2 != 0 {
        return Err("invalid icon dimensions".into());
    }
    let (width, height) = (width as usize, total_height as usize / 2);
    let colors = if bits <= 8 {
        if long(32) == 0 {
            1usize << bits
        } else {
            long(32) as usize
        }
    } else {
        0
    };
    if colors > 256 {
        return Err("invalid icon palette".into());
    }
    let xor_start = 40 + colors * 4;
    let xor_stride = (width * bits as usize).div_ceil(32) * 4;
    let mask_stride = width.div_ceil(32) * 4;
    let mask_start = xor_start + xor_stride * height;
    if bytes.len() < mask_start + mask_stride * height {
        return Err("truncated icon pixels or mask".into());
    }
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        let row = xor_start + (height - 1 - y) * xor_stride;
        let mask = mask_start + (height - 1 - y) * mask_stride;
        for x in 0..width {
            let rgb = if bits <= 8 {
                let index = match bits {
                    1 => (bytes[row + x / 8] >> (7 - x % 8)) & 1,
                    4 => (bytes[row + x / 2] >> (if x % 2 == 0 { 4 } else { 0 })) & 15,
                    _ => bytes[row + x],
                } as usize;
                if index >= colors {
                    return Err("icon palette index out of range".into());
                }
                let at = 40 + index * 4;
                [bytes[at + 2], bytes[at + 1], bytes[at]]
            } else {
                let at = row + x * (bits as usize / 8);
                [bytes[at + 2], bytes[at + 1], bytes[at]]
            };
            let and = if bytes[mask + x / 8] & (0x80 >> (x % 8)) != 0 {
                255
            } else {
                0
            };
            pixels.push([rgb[0], rgb[1], rgb[2], and]);
        }
    }
    Ok(Icon {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packed_color_and_mask_keep_bottom_up_orientation_and_transparency() {
        let mut bytes = vec![0; 40 + 16 * 4 + 16];
        for (at, value) in [(0, 40u32), (4, 2), (8, 4)] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&4u16.to_le_bytes());
        bytes[44..48].copy_from_slice(&[0, 0, 255, 0]);
        bytes[104] = 0x10; // bottom row red, black
        bytes[108] = 0x01; // top row black, red
        bytes[116] = 0x80; // top-left transparent black XOR
        let icon = decode(&bytes).unwrap();
        assert_eq!(
            icon.pixels,
            [[0, 0, 0, 255], [255, 0, 0, 0], [255, 0, 0, 0], [0, 0, 0, 0]]
        );
        assert!(decode(&bytes[..119]).is_err());
    }
}
