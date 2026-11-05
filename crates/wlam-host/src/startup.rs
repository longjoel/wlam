//! Process startup information: `GetStartupInfoA`, `GetCommandLineA`,
//! `GetModuleFileNameA`, `SetHandleCount`.
//!
//! ## Why these are on the critical path
//!
//! Uxn32's startup calls `GetStartupInfoA` at 0x40cd51 with a local
//! `STARTUPINFOA`, then branches on it:
//!
//! ```text
//!   lea  eax, [ebp-0x5c]      ; &si
//!   push eax
//!   call [GetStartupInfoA]
//!   test byte [ebp-0x30], 1   ; si.dwFlags & STARTF_USESHOWWINDOW
//!   jz   default
//!   movzx eax, word [ebp-0x2c]; si.wShowWindow
//!   ...
//! ```
//!
//! So `dwFlags` and `wShowWindow` are read directly. Filling them with zeros
//! would be a *truthful* answer -- "no startup flags" is a real state, and the
//! guest handles it by taking its default. That is different from returning a
//! made-up value: the difference is that zeros here mean what they say.
//!
//! ## What this host honestly reports
//!
//! There is no parent process, no console, no standard handles and no command
//! line beyond the one the host was given. Each of those is reported as
//! "absent" in the way Windows itself does it:
//!
//! - `dwFlags = 0`: no field of the struct is meaningful.
//! - handles `= 0` and `cbReserved2 = 0`: no standard handles, no console
//!   alias, no extra data. Windows sets `hStdOutput`/`hStdError` to `NULL` for
//!   a windowed (GUI-subsystem) process, which is exactly what a PE with
//!   `Subsystem = WINDOWS` gets.
//! - `dwX`/`dwY`/`dwXSize`/`dwYSize = CW_USEDEFAULT` is *not* invented here:
//!   those fields are only meaningful with `STARTF_USEPOSITION` /
//!   `STARTF_USESIZE`, so they are left zero.

/// Size of `STARTUPINFOA` in bytes.
pub const STARTUPINFOA_SIZE: u32 = 68;

/// Field offsets in `STARTUPINFOA`.
pub mod offset {
    pub const CB: u32 = 0x00;
    pub const LP_RESERVED: u32 = 0x04;
    pub const LP_DESKTOP: u32 = 0x08;
    pub const LP_TITLE: u32 = 0x0c;
    pub const DW_X: u32 = 0x10;
    pub const DW_Y: u32 = 0x14;
    pub const DW_X_SIZE: u32 = 0x18;
    pub const DW_Y_SIZE: u32 = 0x1c;
    pub const DW_X_COUNT_CHARS: u32 = 0x20;
    pub const DW_Y_COUNT_CHARS: u32 = 0x24;
    pub const DW_FILL_ATTRIBUTE: u32 = 0x28;
    pub const DW_FLAGS: u32 = 0x2c;
    pub const W_SHOW_WINDOW: u32 = 0x30;
    pub const CB_RESERVED2: u32 = 0x32;
    pub const LP_RESERVED2: u32 = 0x34;
    pub const H_STD_INPUT: u32 = 0x38;
    pub const H_STD_OUTPUT: u32 = 0x3c;
    pub const H_STD_ERROR: u32 = 0x40;
}

/// `STARTF_USESHOWWINDOW`, the only flag Uxn32 tests.
pub const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;

/// What `GetStartupInfoA` writes, as decided by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupInfo {
    pub flags: u32,
    pub show_window: u16,
}

impl Default for StartupInfo {
    /// A GUI process started with no parent: no flags, nothing meaningful.
    fn default() -> Self {
        StartupInfo {
            flags: 0,
            show_window: 0,
        }
    }
}

impl StartupInfo {
    /// Write the struct at `at`.
    ///
    /// GetStartupInfo's buffer is an output parameter, including `cb`:
    /// https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-getstartupinfoa
    pub fn write(
        &self,
        mem: &mut dyn crate::memory::MemoryAccess,
        at: u32,
    ) -> Result<(), StartupInfoError> {
        // All-or-nothing: check the whole struct is addressable before writing,
        // so a bad pointer cannot leave the guest with a half-filled struct.
        let mut region = [0u8; STARTUPINFOA_SIZE as usize];

        // Everything except the fields below is "absent" and left zero.
        region.fill(0);
        region[offset::CB as usize..offset::CB as usize + 4]
            .copy_from_slice(&STARTUPINFOA_SIZE.to_le_bytes());
        region[offset::DW_FLAGS as usize..offset::DW_FLAGS as usize + 4]
            .copy_from_slice(&self.flags.to_le_bytes());
        region[offset::W_SHOW_WINDOW as usize..offset::W_SHOW_WINDOW as usize + 2]
            .copy_from_slice(&self.show_window.to_le_bytes());
        mem.write(at, &region)
            .map_err(|_| StartupInfoError::OutOfBounds { addr: at })
    }
}

/// Why `GetStartupInfoA` could not be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupInfoError {
    OutOfBounds { addr: u32 },
}

impl std::fmt::Display for StartupInfoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartupInfoError::OutOfBounds { addr } => {
                write!(f, "STARTUPINFOA at {addr:#x} is out of bounds")
            }
        }
    }
}

/// Write a NUL-terminated copy of `s` at `at`, never writing more than `cap`
/// bytes including the terminator.
///
/// Used by `GetModuleFileNameA`/`GetCommandLineA`. Windows truncates and returns
/// the buffer size when the path does not fit, rather than overflowing, so the
/// same clamping is applied here.
///
/// Returns `(bytes_written_excluding_nul, truncated)`.
pub fn write_cstr(
    mem: &mut dyn crate::memory::MemoryAccess,
    at: u32,
    cap: u32,
    s: &[u8],
) -> Result<(u32, bool), StartupInfoError> {
    if cap == 0 {
        return Ok((0, !s.is_empty()));
    }
    if at as u64 + cap as u64 > mem.byte_len() {
        return Err(StartupInfoError::OutOfBounds { addr: at });
    }
    let n = s.len().min(cap as usize - 1);
    let mut bytes = s[..n].to_vec();
    bytes.push(0);
    mem.write(at, &bytes)
        .map_err(|_| StartupInfoError::OutOfBounds { addr: at })?;
    Ok((n as u32, n < s.len()))
}

/// The module path this host presents to the guest.
///
/// A recompiled image has no Windows path: it is a bundle directory, not a file
/// the guest loader resolved. The path still has to be usable, because Windows
/// programs routinely derive their working directory by truncating it at the
/// last backslash (`GetModuleFileNameA` followed by `strrchr('\\')`). Returning
/// `""` makes that scan walk backwards out of the caller's buffer, so the host
/// presents a stable synthetic path under `C:\` instead.
pub const MODULE_FILE_NAME: &[u8] = b"C:\\PROGRAM.EXE";

/// Guest address where host-supplied process strings begin.
///
/// This is below the TIB (`0x60000`) and loader stack (`0x70000`), and below
/// every plausible PE image base, so it never overlaps a mapped section. The
/// region is part of the module's reserved address space; the host writes each
/// string once and caches its address for the life of the process.
pub const PROCESS_STRING_BASE: u32 = 0x0005_0000;

/// Strings owned by the process rather than by one call.
///
/// Windows guarantees that `GetCommandLineA` returns the same pointer until the
/// process ends, and callers are entitled to compare or cache it. A fresh copy
/// per call would be a subtle lie and would also leak guest bytes. This hands
/// out each string once, from a monotonic bump allocator in the reserved
/// region.
#[derive(Debug)]
pub struct Process {
    /// The command line the host was given. Empty means "no arguments": the
    /// recompiler does not invent a program name for an image that has none.
    command_line: &'static [u8],
    command_line_address: Option<u32>,
    /// The environment block. This host inherits no variables.
    environment: &'static [u8],
    environment_address: Option<u32>,
    next: u32,
}

impl Default for Process {
    fn default() -> Self {
        Process {
            command_line: b"",
            command_line_address: None,
            environment: b"",
            environment_address: None,
            next: PROCESS_STRING_BASE,
        }
    }
}

impl Process {
    /// The address of the NUL-terminated command line, or `None` if the guest
    /// cannot address the reserved region.
    pub fn command_line(&mut self, mem: &mut dyn crate::memory::MemoryAccess) -> Option<u32> {
        if let Some(address) = self.command_line_address {
            return Some(address);
        }
        let address = self.store(mem, self.command_line)?;
        self.command_line_address = Some(address);
        Some(address)
    }

    /// The address of the environment block: each entry is `NAME=value\0`, and
    /// the block is terminated by an extra NUL.
    ///
    /// With no inherited variables the block is a single empty entry. That is
    /// a real environment, not a missing one, and an enumeration loop sees the
    /// leading NUL and stops.
    pub fn environment_strings(
        &mut self,
        mem: &mut dyn crate::memory::MemoryAccess,
    ) -> Option<u32> {
        if let Some(address) = self.environment_address {
            return Some(address);
        }
        // `store` appends the terminator, so passing one NUL produces the two
        // NUL bytes that end an environment block.
        let address = self.store(mem, b"\0")?;
        self.environment_address = Some(address);
        Some(address)
    }

    /// Copy `bytes` plus a terminator into the reserved region, returning the
    /// address. The whole range is checked before writing. Each allocation is
    /// 4-byte aligned, matching the alignment callers expect for a pointer.
    fn store(&mut self, mem: &mut dyn crate::memory::MemoryAccess, bytes: &[u8]) -> Option<u32> {
        let length = bytes.len() + 1;
        let address = (self.next + 3) & !3;
        let mut stored = bytes.to_vec();
        stored.push(0);
        mem.write(address, &stored).ok()?;
        self.next = address.checked_add(length as u32)?;
        Some(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GuestMemory;

    fn seeded(size: u32) -> Vec<u8> {
        let mut data = vec![0xAAu8; 0x1000];
        data[0x100..0x104].copy_from_slice(&size.to_le_bytes());
        data
    }

    fn u32_at(d: &[u8], at: u32) -> u32 {
        let o = at as usize;
        u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
    }

    #[test]
    fn the_struct_is_written_in_full_with_cb_and_flags() {
        let mut data = seeded(STARTUPINFOA_SIZE);
        let mut mem = GuestMemory::new(&mut data);
        StartupInfo::default().write(&mut mem, 0x100).unwrap();
        let d = mem.get(0x100, STARTUPINFOA_SIZE as usize).unwrap();
        assert_eq!(u32_at(d, offset::CB), STARTUPINFOA_SIZE);
        assert_eq!(u32_at(d, offset::DW_FLAGS), 0, "no startup flags");
        // The pre-fill was 0xAA, so zeroed fields prove the whole struct was
        // written rather than just the interesting four bytes.
        assert!(
            d.iter()
                .enumerate()
                .all(|(i, &b)| b == 0 || (i >= 0x2c && i < 0x30) || (i < 4)),
            "every field must be written, not left holding stale bytes"
        );
    }

    #[test]
    fn uninitialized_output_is_overwritten_including_cb() {
        let mut data = seeded(0xcccccccc);
        let mut mem = GuestMemory::new(&mut data);
        StartupInfo::default().write(&mut mem, 0x100).unwrap();
        assert_eq!(u32_at(&data, 0x100), STARTUPINFOA_SIZE);
        assert_eq!(u32_at(&data, 0x104), 0);
    }

    #[test]
    fn an_out_of_bounds_struct_is_reported() {
        let mut data = vec![0u8; 0x200];
        let mut mem = GuestMemory::new(&mut data);
        let err = StartupInfo::default()
            .write(&mut mem, 0xFFFF_FF00)
            .unwrap_err();
        assert!(matches!(err, StartupInfoError::OutOfBounds { .. }));
    }

    #[test]
    fn flags_and_show_window_are_written_when_set() {
        let mut data = seeded(STARTUPINFOA_SIZE);
        let mut mem = GuestMemory::new(&mut data);
        let si = StartupInfo {
            flags: STARTF_USESHOWWINDOW,
            show_window: 10, // SW_SHOWDEFAULT
        };
        si.write(&mut mem, 0x100).unwrap();
        let d = mem.get(0x100, STARTUPINFOA_SIZE as usize).unwrap();
        assert_eq!(u32_at(d, offset::DW_FLAGS), STARTF_USESHOWWINDOW);
        let o = offset::W_SHOW_WINDOW as usize;
        assert_eq!(u16::from_le_bytes([d[o], d[o + 1]]), 10);
    }

    #[test]
    fn a_string_is_nul_terminated_and_truncated_to_fit() {
        let mut data = vec![0u8; 0x100];
        let mut mem = GuestMemory::new(&mut data);
        // The string fits: 6 characters + terminator in an 8-byte buffer.
        let (n, trunc) = write_cstr(&mut mem, 0x10, 8, b"game.x").unwrap();
        assert_eq!(n, 6, "the terminator is not counted in the return value");
        assert!(!trunc);
        assert_eq!(mem.get(0x10, 7), Some(&b"game.x\0"[..]));
    }

    #[test]
    fn a_long_string_is_clamped_rather_than_overflowing_the_buffer() {
        let mut data = vec![0u8; 0x100];
        let mut mem = GuestMemory::new(&mut data);
        let (n, trunc) = write_cstr(&mut mem, 0x10, 4, b"abcdef").unwrap();
        assert_eq!(n, 3);
        assert!(trunc, "the caller must be told it did not fit");
        assert_eq!(mem.get(0x10, 4), Some(&b"abc\0"[..]),);
        // Nothing past the buffer may be touched.
        assert_eq!(mem.get(0x14, 4), Some(&[0u8; 4][..]));
    }

    #[test]
    fn the_command_line_is_stable_and_nul_terminated() {
        let mut data = vec![0xAAu8; 0x6_0000];
        let mut mem = GuestMemory::new(&mut data);
        let mut process = Process::default();
        let first = process
            .command_line(&mut mem)
            .expect("reserved region is addressable");
        let second = process.command_line(&mut mem).expect("cached address");
        assert_eq!(first, second, "the pointer must not change between calls");
        assert_eq!(first, PROCESS_STRING_BASE);
        assert_eq!(mem.get(first, 1), Some(&b"\0"[..]));
    }

    #[test]
    fn the_command_line_write_is_all_or_nothing() {
        // A memory that stops before the reserved region cannot serve it, and
        // must not report a pointer into nowhere.
        let mut data = vec![0u8; 0x100];
        let mut mem = GuestMemory::new(&mut data);
        let mut process = Process::default();
        assert_eq!(process.command_line(&mut mem), None);
    }

    #[test]
    fn the_module_path_has_a_backslash_to_truncate_at() {
        // Windows programs find their directory with `strrchr(path, '\\')`.
        let slash = MODULE_FILE_NAME
            .iter()
            .rposition(|&b| b == b'\\')
            .expect("a module path must contain a directory separator");
        assert!(slash > 0, "the directory must not be empty");
        assert!(MODULE_FILE_NAME.ends_with(b".EXE"));
    }
}
