//! Bounded memory operations usable across separate guest and host Wasm modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryError {
    pub address: u32,
    pub length: usize,
}

pub trait MemoryAccess {
    fn byte_len(&self) -> u64;
    fn read(&self, address: u32, output: &mut [u8]) -> Result<(), MemoryError>;
    fn write(&mut self, address: u32, input: &[u8]) -> Result<(), MemoryError>;
    fn read_word(&self, address: u32) -> Result<u32, MemoryError> {
        let mut bytes = [0; 4];
        self.read(address, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn write_word(&mut self, address: u32, value: u32) -> Result<(), MemoryError> {
        self.write(address, &value.to_le_bytes())
    }
}
impl MemoryAccess for crate::GuestMemory<'_> {
    fn byte_len(&self) -> u64 {
        self.len() as u64
    }
    fn read(&self, address: u32, output: &mut [u8]) -> Result<(), MemoryError> {
        let input = self.get(address, output.len()).ok_or(MemoryError {
            address,
            length: output.len(),
        })?;
        output.copy_from_slice(input);
        Ok(())
    }
    fn write(&mut self, address: u32, input: &[u8]) -> Result<(), MemoryError> {
        let output = self.get_mut(address, input.len()).ok_or(MemoryError {
            address,
            length: input.len(),
        })?;
        output.copy_from_slice(input);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_access_does_not_partially_modify_either_buffer() {
        let mut bytes = [7; 8];
        let mut memory = crate::GuestMemory::new(&mut bytes);
        let mut output = [9; 4];
        assert!(memory.read(6, &mut output).is_err());
        assert_eq!(output, [9; 4]);
        assert!(memory.write(6, &[1; 4]).is_err());
        assert!(memory.write(u32::MAX, &[1; 4]).is_err());
        let mut observed = [0; 8];
        memory.read(0, &mut observed).unwrap();
        assert_eq!(observed, [7; 8]);
    }
}
