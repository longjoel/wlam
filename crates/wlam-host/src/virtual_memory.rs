//! VirtualAlloc/VirtualFree reservation and commitment state.
//! Guest Wasm currently maps its full address space: decommitted accesses are
//! not hardware-trapped. This module guarantees allocation/zeroing semantics.
use crate::memory::MemoryAccess;
use std::collections::{BTreeMap, BTreeSet};
const PAGE: u64 = 4096;
const GRANULARITY: u64 = 65536;
const BASE: u64 = 0x08000000;
const LIMIT: u64 = 0x10000000; // separate from image, stack and heaps
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidParameter,
    InvalidAddress,
    NoMemory,
    Unsupported,
    Memory,
}
impl Error {
    pub fn code(&self) -> u32 {
        match self {
            Self::InvalidParameter | Self::Unsupported => 87,
            Self::InvalidAddress => 487,
            Self::NoMemory => 8,
            Self::Memory => 998,
        }
    }
}
#[derive(Debug)]
struct Reservation {
    end: u64,
    committed: BTreeSet<u64>,
}
#[derive(Debug, Default)]
pub struct VirtualMemory {
    regions: BTreeMap<u64, Reservation>,
}
fn up(value: u64, alignment: u64) -> u64 {
    (value + alignment - 1) & !(alignment - 1)
}
impl VirtualMemory {
    pub fn committed_bytes(&self) -> u64 {
        self.regions
            .values()
            .map(|region| region.committed.len() as u64 * PAGE)
            .sum()
    }
    pub fn allocate(
        &mut self,
        address: u32,
        size: u32,
        flags: u32,
        protection: u32,
        memory: &mut dyn MemoryAccess,
    ) -> Result<u32, Error> {
        if size == 0 || flags & !0x3000 != 0 || flags & 0x3000 == 0 {
            return Err(Error::InvalidParameter);
        }
        // Other protection modes require access instrumentation, not a guessed success.
        if protection != 4 {
            return Err(Error::Unsupported);
        }
        let reserve = flags & 0x2000 != 0 || address == 0;
        let commit = flags & 0x1000 != 0;
        let limit = LIMIT.min(memory.byte_len());
        let mut created = None;
        let (start, end, base) = if reserve {
            let start = if address != 0 {
                (address as u64) & !(GRANULARITY - 1)
            } else {
                let length = up(size as u64, PAGE);
                let mut candidate = BASE;
                for (base, region) in &self.regions {
                    if candidate + length <= *base {
                        break;
                    }
                    candidate = up(region.end, GRANULARITY);
                }
                candidate
            };
            let end = up(
                if address == 0 {
                    start + size as u64
                } else {
                    address as u64 + size as u64
                },
                PAGE,
            );
            if start < BASE || end > limit {
                return Err(Error::NoMemory);
            }
            if self
                .regions
                .iter()
                .any(|(base, r)| start < r.end && *base < end)
            {
                return Err(Error::InvalidAddress);
            }
            self.regions.insert(
                start,
                Reservation {
                    end,
                    committed: BTreeSet::new(),
                },
            );
            created = Some(start);
            (
                if address == 0 {
                    start
                } else {
                    (address as u64) & !(PAGE - 1)
                },
                end,
                start,
            )
        } else {
            let start = (address as u64) & !(PAGE - 1);
            let end = up(address as u64 + size as u64, PAGE);
            let (&base, region) = self
                .regions
                .range(..=start)
                .next_back()
                .ok_or(Error::InvalidAddress)?;
            if end > region.end {
                return Err(Error::InvalidAddress);
            }
            (start, end, base)
        };
        if commit {
            let region = self.regions.get_mut(&base).unwrap();
            for page in (start..end).step_by(PAGE as usize) {
                if !region.committed.contains(&page) {
                    if memory.write(page as u32, &[0; 4096]).is_err() {
                        if let Some(base) = created {
                            self.regions.remove(&base);
                        }
                        return Err(Error::Memory);
                    }
                    region.committed.insert(page);
                }
            }
        }
        Ok(if reserve { base as u32 } else { start as u32 })
    }
    pub fn free(&mut self, address: u32, size: u32, flags: u32) -> Result<u32, Error> {
        let address = address as u64;
        if flags == 0x8000 {
            if size != 0 {
                return Err(Error::InvalidParameter);
            }
            self.regions.remove(&address).ok_or(Error::InvalidAddress)?;
            return Ok(1);
        }
        if flags != 0x4000 {
            return Err(Error::Unsupported);
        }
        let start = address & !(PAGE - 1);
        let (&base, region) = self
            .regions
            .range_mut(..=start)
            .next_back()
            .ok_or(Error::InvalidAddress)?;
        let end = if size == 0 && address == base {
            region.end
        } else if size == 0 {
            return Err(Error::InvalidParameter);
        } else {
            up(address + size as u64, PAGE)
        };
        if end > region.end {
            return Err(Error::InvalidAddress);
        }
        region
            .committed
            .retain(|page| *page < start || *page >= end);
        Ok(1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryError;
    #[derive(Default)]
    struct Memory(BTreeMap<u32, u8>);
    impl MemoryAccess for Memory {
        fn byte_len(&self) -> u64 {
            1u64 << 32
        }
        fn read(&self, address: u32, out: &mut [u8]) -> Result<(), MemoryError> {
            for (i, b) in out.iter_mut().enumerate() {
                *b = *self.0.get(&(address + i as u32)).unwrap_or(&0);
            }
            Ok(())
        }
        fn write(&mut self, address: u32, input: &[u8]) -> Result<(), MemoryError> {
            for (i, b) in input.iter().enumerate() {
                self.0.insert(address + i as u32, *b);
            }
            Ok(())
        }
    }
    #[test]
    fn reserve_commit_recommit_decommit_and_release() {
        let mut vm = VirtualMemory::default();
        let mut mem = Memory::default();
        let base = vm.allocate(0, 0x100000, 0x2000, 4, &mut mem).unwrap();
        assert_eq!(base % 65536, 0);
        assert_eq!(vm.allocate(base + 7, 9, 0x1000, 4, &mut mem).unwrap(), base);
        mem.write_word(base, 42).unwrap();
        vm.allocate(base, 4096, 0x1000, 4, &mut mem).unwrap();
        assert_eq!(mem.read_word(base).unwrap(), 42);
        vm.free(base, 4096, 0x4000).unwrap();
        vm.allocate(base, 4096, 0x1000, 4, &mut mem).unwrap();
        assert_eq!(mem.read_word(base).unwrap(), 0);
        assert_eq!(
            vm.allocate(base, 4096, 0x2000, 4, &mut mem),
            Err(Error::InvalidAddress)
        );
        assert!(vm.free(base, 1, 0x8000).is_err());
        vm.free(base, 0, 0x8000).unwrap();
        assert_eq!(vm.allocate(0, 0x100000, 0x2000, 4, &mut mem).unwrap(), base);
        assert!(vm.allocate(0, 4096, 0x3000, 0x40, &mut mem).is_err());
    }
}
