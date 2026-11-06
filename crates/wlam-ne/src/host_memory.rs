//! Memory view for shared rendering services. Host-owned DIBs occupy a separate
//! flat range; guest pointers retain their checked selector:offset encoding.
use crate::{NeAccess, NeLoadedImage};
use wlam_host::memory::{MemoryAccess, MemoryError};
pub const HOST_BASE: u32 = 0x8000_0000;
pub const ARENA_BASE: u32 = 0xc000_0000;
pub struct TaskMemory<'a> {
    pub loaded: &'a mut NeLoadedImage,
    pub graphics: &'a mut Vec<u8>,
    pub arena: &'a mut Vec<u8>,
}
impl MemoryAccess for TaskMemory<'_> {
    fn byte_len(&self) -> u64 {
        1u64 << 32
    }
    fn read(&self, address: u32, output: &mut [u8]) -> Result<(), MemoryError> {
        let error = || MemoryError {
            address,
            length: output.len(),
        };
        if address >= ARENA_BASE {
            let start = (address - ARENA_BASE) as usize;
            let end = start
                .checked_add(output.len())
                .filter(|end| *end <= 0x4000_0000)
                .ok_or_else(error)?;
            output.fill(0);
            if start < self.arena.len() {
                let available = end.min(self.arena.len()) - start;
                output[..available].copy_from_slice(&self.arena[start..start + available]);
            }
        } else if address >= HOST_BASE {
            let start = (address - HOST_BASE) as usize;
            let end = start.checked_add(output.len()).ok_or_else(error)?;
            output.copy_from_slice(self.graphics.get(start..end).ok_or_else(error)?);
        } else {
            let length = u32::try_from(output.len()).map_err(|_| error())?;
            let bytes = self
                .loaded
                .read(
                    (address >> 16) as u16,
                    address & 0xffff,
                    length,
                    NeAccess::Read,
                )
                .map_err(|_| error())?;
            output.copy_from_slice(&bytes);
        }
        Ok(())
    }
    fn write(&mut self, address: u32, input: &[u8]) -> Result<(), MemoryError> {
        let error = || MemoryError {
            address,
            length: input.len(),
        };
        if address >= ARENA_BASE {
            let start = (address - ARENA_BASE) as usize;
            let end = start
                .checked_add(input.len())
                .filter(|end| *end <= 0x4000_0000)
                .ok_or_else(error)?;
            if end > self.arena.len() {
                self.arena
                    .try_reserve(end - self.arena.len())
                    .map_err(|_| error())?;
                self.arena.resize(end, 0);
            }
            self.arena[start..end].copy_from_slice(input);
        } else if address >= HOST_BASE {
            let start = (address - HOST_BASE) as usize;
            let end = start.checked_add(input.len()).ok_or_else(error)?;
            self.graphics
                .get_mut(start..end)
                .ok_or_else(error)?
                .copy_from_slice(input);
        } else {
            self.loaded
                .write((address >> 16) as u16, address & 0xffff, input)
                .map_err(|_| error())?;
        }
        Ok(())
    }
}
