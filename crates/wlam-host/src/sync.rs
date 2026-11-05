//! Recursive critical sections for the supported single guest thread.
//! Multi-thread contention is not implemented and must not be inferred here.
use crate::memory::MemoryAccess;
use std::collections::BTreeMap;
#[derive(Debug, Default)]
pub struct CriticalSections {
    depths: BTreeMap<u32, u32>,
}
impl CriticalSections {
    fn write(memory: &mut dyn MemoryAccess, address: u32, depth: u32) -> Result<(), String> {
        let mut fields = [0u8; 24];
        fields[4..8].copy_from_slice(&depth.wrapping_sub(1).to_le_bytes());
        fields[8..12].copy_from_slice(&depth.to_le_bytes());
        fields[12..16].copy_from_slice(&(if depth == 0 { 0u32 } else { 1 }).to_le_bytes());
        memory
            .write(address, &fields)
            .map_err(|e| format!("CRITICAL_SECTION write: {e:?}"))
    }
    pub fn call(
        &mut self,
        name: &str,
        address: u32,
        memory: &mut dyn MemoryAccess,
    ) -> Result<u32, String> {
        if address == 0 || address as u64 + 24 > memory.byte_len() {
            return Err("invalid critical-section pointer".into());
        }
        match name {
            "InitializeCriticalSection" => {
                if self.depths.contains_key(&address) {
                    return Err("critical section already initialized".into());
                }
                Self::write(memory, address, 0)?;
                self.depths.insert(address, 0);
                Ok(0)
            }
            "EnterCriticalSection" | "TryEnterCriticalSection" => {
                let depth = self
                    .depths
                    .get(&address)
                    .ok_or("uninitialized critical section")?
                    .checked_add(1)
                    .ok_or("critical-section recursion overflow")?;
                Self::write(memory, address, depth)?;
                self.depths.insert(address, depth);
                Ok(if name == "TryEnterCriticalSection" {
                    1
                } else {
                    0
                })
            }
            "LeaveCriticalSection" => {
                let depth = self
                    .depths
                    .get(&address)
                    .ok_or("uninitialized critical section")?
                    .checked_sub(1)
                    .ok_or("critical section not owned")?;
                Self::write(memory, address, depth)?;
                self.depths.insert(address, depth);
                Ok(0)
            }
            "DeleteCriticalSection" => {
                if *self
                    .depths
                    .get(&address)
                    .ok_or("uninitialized critical section")?
                    != 0
                {
                    return Err("deleting owned critical section".into());
                }
                self.depths.remove(&address);
                Ok(0)
            }
            _ => Err(format!("unsupported synchronization operation {name}")),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursive_ownership_and_lifetime_are_real() {
        let mut bytes = [0; 128];
        let mut memory = crate::GuestMemory::new(&mut bytes);
        let mut state = CriticalSections::default();
        state
            .call("InitializeCriticalSection", 32, &mut memory)
            .unwrap();
        assert_eq!(memory.read_word(36).unwrap(), u32::MAX);
        state.call("EnterCriticalSection", 32, &mut memory).unwrap();
        assert_eq!(
            state
                .call("TryEnterCriticalSection", 32, &mut memory)
                .unwrap(),
            1
        );
        assert_eq!(memory.read_word(40).unwrap(), 2);
        assert_eq!(memory.read_word(44).unwrap(), 1);
        assert!(state
            .call("DeleteCriticalSection", 32, &mut memory)
            .is_err());
        state.call("LeaveCriticalSection", 32, &mut memory).unwrap();
        state.call("LeaveCriticalSection", 32, &mut memory).unwrap();
        assert!(state.call("LeaveCriticalSection", 32, &mut memory).is_err());
        state
            .call("DeleteCriticalSection", 32, &mut memory)
            .unwrap();
        assert!(state.call("EnterCriticalSection", 32, &mut memory).is_err());
        assert!(state
            .call("InitializeCriticalSection", 120, &mut memory)
            .is_err());
    }
}
