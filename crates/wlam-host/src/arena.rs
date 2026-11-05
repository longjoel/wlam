//! Process-owned address ranges for host-created guest objects.
use std::collections::BTreeMap;
#[derive(Debug)]
pub struct Arena {
    next: u64,
    live: BTreeMap<u32, u64>,
    free: BTreeMap<u32, u64>,
}
impl Default for Arena {
    fn default() -> Self {
        Self {
            next: 0x10000000,
            live: BTreeMap::new(),
            free: BTreeMap::new(),
        }
    }
}
impl Arena {
    pub fn allocated_bytes(&self) -> u64 {
        self.live.values().sum()
    }
    pub fn allocate(&mut self, size: u32, memory_length: u64) -> Result<u32, String> {
        let length = (u64::from(size).max(1) + 7) & !7;
        if let Some((&address, &available)) = self
            .free
            .iter()
            .find(|(_, available)| **available >= length)
        {
            self.free.remove(&address);
            if available > length {
                self.free
                    .insert((address as u64 + length) as u32, available - length);
            }
            self.live.insert(address, length);
            return Ok(address);
        }
        let end = self.next.checked_add(length).ok_or("allocation overflow")?;
        if end > memory_length.min(1u64 << 32) {
            return Err("guest allocation exceeds memory".into());
        }
        let address = u32::try_from(self.next).map_err(|_| "allocation address overflow")?;
        self.next = end;
        self.live.insert(address, length);
        Ok(address)
    }
    pub fn release(&mut self, address: u32) -> Result<(), String> {
        let mut length = self
            .live
            .remove(&address)
            .ok_or("unknown allocation or double release")?;
        let mut start = address;
        if let Some((&previous, &size)) = self.free.range(..address).next_back() {
            if previous as u64 + size == address as u64 {
                self.free.remove(&previous);
                start = previous;
                length += size;
            }
        }
        if let Some((&next, &size)) = self.free.range(start..).next() {
            if start as u64 + length == next as u64 {
                self.free.remove(&next);
                length += size;
            }
        }
        self.free.insert(start, length);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocations_reuse_and_coalesce_without_overlapping_live_objects() {
        let mut a = Arena::default();
        let x = a.allocate(8, 1u64 << 32).unwrap();
        let y = a.allocate(16, 1u64 << 32).unwrap();
        let z = a.allocate(8, 1u64 << 32).unwrap();
        a.release(x).unwrap();
        a.release(y).unwrap();
        assert_eq!(a.allocate(24, 1u64 << 32).unwrap(), x);
        assert!(a.release(y).is_err());
        assert_eq!(z, x + 24);
        assert!(a.allocate(u32::MAX, 1u64 << 32).is_err());
    }
}
