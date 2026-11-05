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
            || u16::from_le_bytes(bytes[12..14].try_into().unwrap()) != 1
            || !matches!(bit_count, 8 | 32)
            || word(16) != 0
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
        let palette = if bit_count == 8 {
            let mut colors = word(32) as usize;
            if colors == 0 {
                colors = 256;
            }
            if colors > 256 {
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
        })
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
        if self.bit_count == 32 {
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
