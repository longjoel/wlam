//! TLS and last-error state for the currently supported single guest thread.
//! https://learn.microsoft.com/en-us/windows/win32/procthread/using-thread-local-storage
#[derive(Debug)]
pub struct ThreadState {
    allocated: [bool; 64],
    values: [u32; 64],
    pub last_error: u32,
}
impl Default for ThreadState {
    fn default() -> Self {
        Self {
            allocated: [false; 64],
            values: [0; 64],
            last_error: 0,
        }
    }
}
impl ThreadState {
    pub fn call(&mut self, name: &str, args: &[u32]) -> Result<u32, String> {
        match name {
            "GetCurrentThread" => Ok(0xffff_fffe),
            "GetCurrentProcess" => Ok(0xffff_ffff),
            "GetCurrentThreadId" => Ok(1),
            "GetLastError" => Ok(self.last_error),
            "SetLastError" => {
                self.last_error = args[0];
                Ok(0)
            }
            "TlsAlloc" => {
                if let Some(index) = self.allocated.iter().position(|used| !*used) {
                    self.allocated[index] = true;
                    self.values[index] = 0;
                    Ok(index as u32)
                } else {
                    self.last_error = 8;
                    Ok(u32::MAX)
                }
            }
            "TlsFree" | "TlsSetValue" | "TlsGetValue" => {
                let index = args[0] as usize;
                if index >= 64 {
                    self.last_error = 87;
                    return Ok(0);
                }
                if name == "TlsGetValue" {
                    self.last_error = 0;
                    return Ok(self.values[index]);
                }
                if !self.allocated[index] {
                    self.last_error = 87;
                    return Ok(0);
                }
                if name == "TlsFree" {
                    self.allocated[index] = false;
                    self.values[index] = 0;
                } else {
                    self.values[index] = args[1];
                }
                Ok(1)
            }
            _ => Err(format!("unsupported thread API {name}")),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tls_allocation_values_exhaustion_and_reuse() {
        let mut t = ThreadState::default();
        let index = t.call("TlsAlloc", &[]).unwrap();
        assert_eq!(index, 0);
        t.call("SetLastError", &[123]).unwrap();
        assert_eq!(t.call("TlsGetValue", &[index]).unwrap(), 0);
        assert_eq!(t.last_error, 0);
        assert_eq!(t.call("TlsSetValue", &[index, 0x12345678]).unwrap(), 1);
        assert_eq!(t.call("TlsGetValue", &[index]).unwrap(), 0x12345678);
        for expected in 1..64 {
            assert_eq!(t.call("TlsAlloc", &[]).unwrap(), expected);
        }
        assert_eq!(t.call("TlsAlloc", &[]).unwrap(), u32::MAX);
        assert_eq!(t.last_error, 8);
        assert_eq!(t.call("TlsFree", &[index]).unwrap(), 1);
        assert_eq!(t.call("TlsAlloc", &[]).unwrap(), index);
        assert_eq!(t.call("TlsGetValue", &[index]).unwrap(), 0);
        assert_eq!(t.call("TlsGetValue", &[u32::MAX]).unwrap(), 0);
        assert_eq!(t.last_error, 87);
    }
}
