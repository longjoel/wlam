//! TLS and last-error state for the currently supported single guest thread.
//! https://learn.microsoft.com/en-us/windows/win32/procthread/using-thread-local-storage
#[derive(Debug)]
pub struct ThreadState {
    allocated: [bool; 64],
    values: [u32; 64],
    pub last_error: u32,
    priority: i32,
}
impl Default for ThreadState {
    fn default() -> Self {
        Self {
            allocated: [false; 64],
            values: [0; 64],
            last_error: 0,
            priority: 0,
        }
    }
}
impl ThreadState {
    pub fn call(&mut self, name: &str, args: &[u32]) -> Result<u32, String> {
        match name {
            "GetCurrentThread" => Ok(0xffff_fffe),
            "GetCurrentProcess" => Ok(0xffff_ffff),
            "GetCurrentThreadId" => Ok(1),
            "SetThreadPriority" => {
                if args[0] != 0xffff_fffe {
                    self.last_error = 6;
                    return Ok(0);
                }
                let priority = args[1] as i32;
                if !matches!(priority, -15 | -2..=2 | 15) {
                    self.last_error = 87;
                    return Ok(0);
                }
                // Scheduling is single-threaded; retain the guest's priority
                // without changing the browser worker's OS scheduling policy.
                self.priority = priority;
                Ok(1)
            }
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
    fn priority_changes_only_the_current_guest_thread() {
        let mut thread = ThreadState::default();
        assert_eq!(
            thread.call("SetThreadPriority", &[0xffff_fffe, 2]).unwrap(),
            1
        );
        assert_eq!(thread.priority, 2);
        assert_eq!(thread.call("SetThreadPriority", &[1, 0]).unwrap(), 0);
        assert_eq!(thread.last_error, 6);
        assert_eq!(
            thread.call("SetThreadPriority", &[0xffff_fffe, 3]).unwrap(),
            0
        );
        assert_eq!(thread.last_error, 87);
        assert_eq!(thread.priority, 2);
    }
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
