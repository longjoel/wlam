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
    /// Read one bounded row, converting Windows BGRX to opaque RGBA.
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
