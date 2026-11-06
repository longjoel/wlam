//! Software DIB decoding. No browser or Wasm engine is required.
use crate::memory::{MemoryAccess, MemoryError};
#[derive(Debug, PartialEq, Eq)]
pub enum RasterError {
    UnsupportedFormat,
    InvalidDimensions,
    Memory(MemoryError),
}
impl From<MemoryError> for RasterError {
    fn from(e: MemoryError) -> Self {
        Self::Memory(e)
    }
}
#[derive(Debug, Clone)]
pub struct Dib {
    pub bits: u32,
    pub width: u32,
    pub height: u32,
    pub top_down: bool,
    bit_count: u16,
    stride: u32,
    palette: Vec<[u8; 4]>,
    section: bool,
    compression: u32,
    clr_used: u32,
    planes: u16,
}
impl Dib {
    pub fn from_header(
        memory: &dyn MemoryAccess,
        header: u32,
        bits: u32,
    ) -> Result<Self, RasterError> {
        let mut bytes = [0; 40];
        memory.read(header, &mut bytes)?;
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let bit_count = u16::from_le_bytes(bytes[14..16].try_into().unwrap());
        if word(0) != 40
            || u16::from_le_bytes(bytes[12..14].try_into().unwrap()) == 0
            || !matches!(bit_count, 1 | 2 | 4 | 8 | 16 | 24 | 32)
            || !matches!(word(16), 0 | 3)
            || (word(16) == 3 && !matches!(bit_count, 16 | 32))
        {
            return Err(RasterError::UnsupportedFormat);
        }
        let width = word(4) as i32;
        let height = word(8) as i32;
        if width <= 0 || height == 0 || height == i32::MIN {
            return Err(RasterError::InvalidDimensions);
        }
        let stride = (((width as u64 * bit_count as u64 + 31) / 32) * 4) as u32;
        let size = stride as u64 * height.unsigned_abs() as u64;
        if u64::from(bits) + size > memory.byte_len() || u64::from(bits) + size > 1u64 << 32 {
            return Err(RasterError::InvalidDimensions);
        }
        let palette = if bit_count <= 8 {
            let mut colors = word(32) as usize;
            if colors == 0 {
                colors = 1usize << bit_count;
            }
            if colors > 256 || colors > (1usize << bit_count) {
                return Err(RasterError::UnsupportedFormat);
            }
            let mut raw = vec![0; colors * 4];
            memory.read(
                header
                    .checked_add(40)
                    .ok_or(RasterError::InvalidDimensions)?,
                &mut raw,
            )?;
            raw.chunks_exact(4)
                .map(|v| [v[2], v[1], v[0], 255])
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            bits,
            width: width as u32,
            height: height.unsigned_abs(),
            top_down: height < 0,
            bit_count,
            stride,
            palette,
            section: true,
            compression: word(16),
            clr_used: if bit_count <= 8 {
                if word(32) == 0 {
                    1 << bit_count
                } else {
                    word(32)
                }
            } else {
                0
            },
            planes: 1,
        })
    }
    /// Wrap already-allocated guest pixels as a top-down DIB.
    ///
    /// `CreateCompatibleBitmap` uses this for a device-independent bitmap it
    /// owns; passing the guest memory keeps the range check in one place.
    pub fn new(
        memory: &dyn MemoryAccess,
        width: u32,
        height: u32,
        bits: u32,
        bit_count: u16,
    ) -> Result<Self, RasterError> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32
            || !matches!(bit_count, 1 | 2 | 4 | 8 | 16 | 24 | 32)
        {
            return Err(RasterError::UnsupportedFormat);
        }
        let stride = (((width as u64 * bit_count as u64 + 31) / 32) * 4) as u32;
        let size = stride as u64 * height as u64;
        if u64::from(bits) + size > memory.byte_len() || u64::from(bits) + size > 1u64 << 32 {
            return Err(RasterError::InvalidDimensions);
        }
        let palette = match bit_count {
            1 => vec![[0, 0, 0, 255], [255, 255, 255, 255]],
            2 => (0..4).map(|value| [value as u8 * 85, value as u8 * 85, value as u8 * 85, 255]).collect(),
            4 => (0..16).map(|value| [value as u8 * 17, value as u8 * 17, value as u8 * 17, 255]).collect(),
            8 => (0..256).map(|value| [value as u8, value as u8, value as u8, 255]).collect(),
            _ => Vec::new(),
        };
        Ok(Self {
            bits,
            width,
            height,
            top_down: true,
            bit_count,
            stride,
            palette,
            section: false,
            compression: 0,
            clr_used: if bit_count <= 8 { 1 << bit_count } else { 0 },
            planes: 1,
        })
    }
    /// Build a DIB section object from already-parsed header fields.
    pub fn section(
        memory: &dyn MemoryAccess,
        width: u32,
        height: u32,
        bits: u32,
        bit_count: u16,
        compression: u32,
        top_down: bool,
        palette: Vec<[u8; 4]>,
    ) -> Result<Self, RasterError> {
        if width == 0
            || height == 0
            || width > i32::MAX as u32
            || height > i32::MAX as u32
            || !matches!(bit_count, 1 | 2 | 4 | 8 | 16 | 24 | 32)
        {
            return Err(RasterError::UnsupportedFormat);
        }
        let stride = (((width as u64 * bit_count as u64 + 31) / 32) * 4) as u32;
        let size = stride as u64 * height as u64;
        if u64::from(bits) + size > memory.byte_len() || u64::from(bits) + size > 1u64 << 32 {
            return Err(RasterError::InvalidDimensions);
        }
        let clr_used = if bit_count <= 8 {
            if palette.is_empty() {
                1 << bit_count
            } else {
                palette.len() as u32
            }
        } else {
            0
        };
        Ok(Self {
            bits,
            width,
            height,
            top_down,
            bit_count,
            stride,
            palette,
            section: true,
            compression,
            clr_used,
            planes: 1,
        })
    }
    /// Override the storage orientation (used for bottom-up GetDIBits output).
    pub fn with_top_down(mut self, top_down: bool) -> Self {
        self.top_down = top_down;
        self
    }
    pub fn is_section(&self) -> bool {
        self.section
    }
    pub fn bit_count(&self) -> u16 {
        self.bit_count
    }
    pub fn stride(&self) -> u32 {
        self.stride
    }
    pub fn palette(&self) -> &[[u8; 4]] {
        &self.palette
    }
    pub fn set_palette_entries(&mut self, start: usize, colors: &[[u8; 4]]) -> usize {
        let mut written = 0;
        for (index, color) in colors.iter().enumerate() {
            if let Some(entry) = self.palette.get_mut(start + index) {
                *entry = *color;
                written += 1;
            } else {
                break;
            }
        }
        written
    }
    pub fn compression(&self) -> u32 {
        self.compression
    }
    pub fn clr_used(&self) -> u32 {
        self.clr_used
    }
    pub fn planes(&self) -> u16 {
        self.planes
    }
    /// Write one palette index into a 1/2/4/8-bpp DIB.
    pub fn write_index(
        &self,
        memory: &mut dyn MemoryAccess,
        x: i32,
        y: i32,
        index: u8,
    ) -> Result<(), RasterError> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return Err(RasterError::InvalidDimensions);
        }
        if self.bit_count > 8 {
            return Err(RasterError::UnsupportedFormat);
        }
        let row = if self.top_down {
            y as u32
        } else {
            self.height - 1 - y as u32
        };
        let address = u32::try_from(self.bits as u64 + row as u64 * self.stride as u64)
            .map_err(|_| RasterError::InvalidDimensions)?;
        let bits = self.bit_count as u32;
        let mut bytes = vec![0u8; self.stride as usize];
        memory.read(address, &mut bytes)?;
        let mask = ((1u16 << bits) - 1) as u8;
        let shift = 8 - bits - (x as u32 * bits % 8);
        let byte = &mut bytes[(x as u32 * bits / 8) as usize];
        *byte = (*byte & !(mask << shift)) | ((index & mask) << shift);
        memory.write(address, &bytes)?;
        Ok(())
    }
    /// Store opaque RGBA pixels in a DIB, preserving row padding and orientation.
    pub fn write_rgba_row(
        &self,
        memory: &mut dyn MemoryAccess,
        x: u32,
        y: u32,
        pixels: &[u8],
    ) -> Result<(), RasterError> {
        if pixels.len() % 4 != 0
            || y >= self.height
            || x as u64 + (pixels.len() / 4) as u64 > self.width as u64
        {
            return Err(RasterError::InvalidDimensions);
        }
        let row = if self.top_down {
            y
        } else {
            self.height - 1 - y
        };
        let address = self.bits as u64
            + row as u64 * self.stride as u64
            + x as u64 * (self.bit_count / 8) as u64;
        let address = u32::try_from(address).map_err(|_| RasterError::InvalidDimensions)?;
        if self.bit_count == 1 {
            let mut row = vec![0u8; self.stride as usize];
            memory.read(address, &mut row)?;
            for (i, pixel) in pixels.chunks_exact(4).enumerate() {
                let bit_x = x as usize + i;
                let mask = 0x80u8 >> (bit_x % 8);
                let index = self
                    .palette
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, color)| {
                        (0..3)
                            .map(|c| (color[c] as i32 - pixel[c] as i32).pow(2))
                            .sum::<i32>()
                    })
                    .map(|(index, _)| index as u8)
                    .unwrap_or(0);
                if index != 0 {
                    row[bit_x / 8] |= mask;
                } else {
                    row[bit_x / 8] &= !mask;
                }
            }
            memory.write(address, &row)?;
            return Ok(());
        }
        if self.bit_count == 24 {
            let output: Vec<u8> = pixels
                .chunks_exact(4)
                .flat_map(|p| [p[2], p[1], p[0]])
                .collect();
            memory.write(address, &output)?;
            return Ok(());
        }
        if self.bit_count == 16 {
            let output: Vec<u8> = pixels
                .chunks_exact(4)
                .flat_map(|p| {
                    let v = ((p[0] as u16 / 8) << 10) | ((p[1] as u16 / 8) << 5) | (p[2] as u16 / 8);
                    v.to_le_bytes()
                })
                .collect();
            memory.write(address, &output)?;
            return Ok(());
        }
        if self.bit_count == 2 || self.bit_count == 4 {
            let bits = self.bit_count as usize;
            let mut row = vec![0u8; self.stride as usize];
            memory.read(address, &mut row)?;
            let mask = (1u8 << bits) - 1;
            for (i, pixel) in pixels.chunks_exact(4).enumerate() {
                let index = self
                    .palette
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, color)| {
                        (0..3)
                            .map(|c| (color[c] as i32 - pixel[c] as i32).pow(2))
                            .sum::<i32>()
                    })
                    .map(|(index, _)| index as u8)
                    .unwrap_or(0);
                let bit_x = x as usize + i;
                let shift = 8 - bits - (bit_x * bits % 8);
                let byte = &mut row[bit_x * bits / 8];
                *byte = (*byte & !(mask << shift)) | ((index & mask) << shift);
            }
            memory.write(address, &row)?;
            return Ok(());
        }
        let output: Vec<u8> = if self.bit_count == 32 {
            pixels
                .chunks_exact(4)
                .flat_map(|p| [p[2], p[1], p[0], 0])
                .collect()
        } else {
            let mut indices = std::collections::BTreeMap::new();
            pixels
                .chunks_exact(4)
                .map(|pixel| {
                    let key = [pixel[0], pixel[1], pixel[2]];
                    *indices.entry(key).or_insert_with(|| {
                        self.palette
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, color)| {
                                (0..3)
                                    .map(|i| (color[i] as i32 - pixel[i] as i32).pow(2))
                                    .sum::<i32>()
                            })
                            .map(|(index, _)| index as u8)
                            .unwrap_or(0)
                    })
                })
                .collect()
        };
        memory.write(address, &output)?;
        Ok(())
    }
    /// Fill a clipped DIB rectangle, mapping black/white to indexed colors.
    pub fn fill_rect(
        &self,
        memory: &mut dyn MemoryAccess,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        white: bool,
    ) -> Result<(), RasterError> {
        if width < 0 || height < 0 {
            return Err(RasterError::InvalidDimensions);
        }
        let left = (x as i64).max(0).min(self.width as i64);
        let right = (x as i64 + width as i64).max(0).min(self.width as i64);
        let top = (y as i64).max(0).min(self.height as i64);
        let bottom = (y as i64 + height as i64).max(0).min(self.height as i64);
        if left >= right || top >= bottom {
            return Ok(());
        }
        let color = if white { 255u8 } else { 0u8 };
        if self.bit_count == 1 {
            let index = self
                .palette
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| {
                    let target = if white { 255i32 } else { 0 };
                    (0..3)
                        .map(|c| (entry[c] as i32 - target).pow(2))
                        .sum::<i32>()
                })
                .map(|(index, _)| index as u8)
                .unwrap_or(0);
            for py in top..bottom {
                let physical = if self.top_down { py as u32 } else { self.height - 1 - py as u32 };
                let address = u32::try_from(self.bits as u64 + physical as u64 * self.stride as u64)
                    .map_err(|_| RasterError::InvalidDimensions)?;
                let mut row = vec![0u8; self.stride as usize];
                memory.read(address, &mut row)?;
                for px in left..right {
                    let mask = 0x80u8 >> (px % 8);
                    if index != 0 {
                        row[(px / 8) as usize] |= mask;
                    } else {
                        row[(px / 8) as usize] &= !mask;
                    }
                }
                memory.write(address, &row)?;
            }
            return Ok(());
        }
        let bytes_per_pixel = (self.bit_count / 8) as usize;
        let mut row = vec![color; (right - left) as usize * bytes_per_pixel];
        if self.bit_count == 8 {
            let index = self
                .palette
                .iter()
                .enumerate()
                .min_by_key(|(_, rgb)| {
                    rgb[..3]
                        .iter()
                        .map(|v| (*v as i32 - color as i32).pow(2))
                        .sum::<i32>()
                })
                .ok_or(RasterError::UnsupportedFormat)?
                .0 as u8;
            row.fill(index);
        }
        for py in top..bottom {
            let physical = if self.top_down {
                py as u32
            } else {
                self.height - 1 - py as u32
            };
            let address = self.bits as u64
                + physical as u64 * self.stride as u64
                + left as u64 * bytes_per_pixel as u64;
            memory.write(
                u32::try_from(address).map_err(|_| RasterError::InvalidDimensions)?,
                &row,
            )?;
        }
        Ok(())
    }
    /// Read one bounded row, converting Windows BGRX or indexed RGB to RGBA.
    pub fn rgba_row(
        &self,
        memory: &dyn MemoryAccess,
        x: u32,
        y: u32,
        output: &mut [u8],
    ) -> Result<(), RasterError> {
        if output.len() % 4 != 0
            || y >= self.height
            || x as u64 + (output.len() / 4) as u64 > self.width as u64
        {
            return Err(RasterError::InvalidDimensions);
        }
        let row = if self.top_down {
            y
        } else {
            self.height - 1 - y
        };
        let address = self.bits as u64 + row as u64 * self.stride as u64;
        let address = u32::try_from(address).map_err(|_| RasterError::InvalidDimensions)?;
        if self.bit_count == 1 || self.bit_count == 2 || self.bit_count == 4 {
            let mut row_bytes = vec![0u8; self.stride as usize];
            memory.read(address, &mut row_bytes)?;
            let bits = self.bit_count as usize;
            for (i, pixel) in output.chunks_exact_mut(4).enumerate() {
                let bit_x = x as usize + i;
                let byte = row_bytes[bit_x * bits / 8] as usize;
                let shift = 8 - bits - (bit_x * bits % 8);
                let index = (byte >> shift) & ((1 << bits) - 1);
                pixel.copy_from_slice(
                    self.palette
                        .get(index)
                        .ok_or(RasterError::InvalidDimensions)?,
                );
            }
        } else if self.bit_count == 16 {
            let address = address
                .checked_add(x.checked_mul(2).ok_or(RasterError::InvalidDimensions)?)
                .ok_or(RasterError::InvalidDimensions)?;
            let mut words = vec![0u8; output.len() / 2];
            memory.read(address, &mut words)?;
            for (word, pixel) in words.chunks_exact(2).zip(output.chunks_exact_mut(4)) {
                let value = u16::from_le_bytes([word[0], word[1]]);
                let expand = |v: u16| ((v as u32 * 255 + 15) / 31) as u8;
                pixel[0] = expand((value >> 10) & 0x1f);
                pixel[1] = expand((value >> 5) & 0x1f);
                pixel[2] = expand(value & 0x1f);
                pixel[3] = 255;
            }
        } else if self.bit_count == 24 {
            let address = address
                .checked_add(x.checked_mul(3).ok_or(RasterError::InvalidDimensions)?)
                .ok_or(RasterError::InvalidDimensions)?;
            let mut bytes = vec![0u8; output.len() / 4 * 3];
            memory.read(address, &mut bytes)?;
            for (bgr, pixel) in bytes.chunks_exact(3).zip(output.chunks_exact_mut(4)) {
                pixel[0] = bgr[2];
                pixel[1] = bgr[1];
                pixel[2] = bgr[0];
                pixel[3] = 255;
            }
        } else if self.bit_count == 32 {
            let address = address
                .checked_add(x.checked_mul(4).ok_or(RasterError::InvalidDimensions)?)
                .ok_or(RasterError::InvalidDimensions)?;
            memory.read(address, output)?;
            for pixel in output.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
        } else {
            let mut row_bytes = vec![0; self.stride as usize];
            memory.read(address, &mut row_bytes)?;
            for (i, pixel) in output.chunks_exact_mut(4).enumerate() {
                let index = row_bytes[x as usize + i] as usize;
                pixel.copy_from_slice(
                    self.palette
                        .get(index)
                        .ok_or(RasterError::InvalidDimensions)?,
                );
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_pixels_use_rgb_table_and_fills_preserve_padding_and_orientation() {
        for height in [2i32, -2] {
            let mut bytes = vec![0xaa; 1200];
            bytes[..40].fill(0);
            bytes[..4].copy_from_slice(&40u32.to_le_bytes());
            bytes[4..8].copy_from_slice(&3i32.to_le_bytes());
            bytes[8..12].copy_from_slice(&height.to_le_bytes());
            bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
            bytes[14..16].copy_from_slice(&8u16.to_le_bytes());
            // Full 256-entry RGBQUAD table. Black is deliberately at index 9.
            bytes[40..1064].fill(255);
            bytes[40 + 9 * 4..44 + 9 * 4].fill(0);
            bytes[40 + 200 * 4..44 + 200 * 4].copy_from_slice(&[30, 20, 10, 0]);
            bytes[1100..1108].copy_from_slice(&[200, 200, 200, 0xee, 200, 200, 200, 0xdd]);
            let mut memory = crate::GuestMemory::new(&mut bytes);
            let dib = Dib::from_header(&memory, 0, 1100).unwrap();
            let mut pixels = [0; 12];
            dib.rgba_row(&memory, 0, 0, &mut pixels).unwrap();
            assert_eq!(pixels, [10, 20, 30, 255].repeat(3).as_slice());
            dib.fill_rect(&mut memory, -1, 0, 2, 1, false).unwrap();
            dib.rgba_row(&memory, 0, 0, &mut pixels).unwrap();
            assert_eq!(pixels, [0, 0, 0, 255, 10, 20, 30, 255, 10, 20, 30, 255]);
            let mut padding = [0];
            memory.read(1103, &mut padding).unwrap();
            assert_eq!(padding, [0xee]);
            memory.read(1107, &mut padding).unwrap();
            assert_eq!(padding, [0xdd]);
        }
    }
    #[test]
    fn dib_orientation_color_and_bounds() {
        for height in [2i32, -2] {
            let mut bytes = vec![0; 128];
            bytes[0..4].copy_from_slice(&40u32.to_le_bytes());
            bytes[4..8].copy_from_slice(&1i32.to_le_bytes());
            bytes[8..12].copy_from_slice(&height.to_le_bytes());
            bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
            bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
            bytes[64..72].copy_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
            let memory = crate::GuestMemory::new(&mut bytes);
            let dib = Dib::from_header(&memory, 0, 64).unwrap();
            let mut rgba = [0; 4];
            dib.rgba_row(&memory, 0, 0, &mut rgba).unwrap();
            assert_eq!(
                rgba,
                if height < 0 {
                    [3, 2, 1, 255]
                } else {
                    [6, 5, 4, 255]
                }
            );
            assert_eq!(
                dib.rgba_row(&memory, 1, 0, &mut rgba),
                Err(RasterError::InvalidDimensions)
            );
        }
    }
}
