//! Version-reporting APIs: `GetVersion`, `GetVersionExA`, `GetVersionExW`.
//!
//! ## Why this is the first real API
//!
//! Uxn32 calls `GetVersion` four instructions into `WinMain`, before it builds
//! any window:
//!
//! ```text
//! 40ccd8  ff 15 6c 10 41 00   call dword ptr [0x41106c]   ; GetVersion
//! 40ccde  33 d2               xor  edx, edx
//! 40cce0  8a d4               mov  dl, ah                  ; major version
//! 40cce2  89 15 80 5b 41 00   mov  [0x415b80], edx
//! 40cce8  8b c8               mov  ecx, eax
//! 40ccea  81 e1 ff 00 00 00   and  ecx, 0xff               ; minor version
//! 40ccf0  89 0d 7c 5b 41 00   mov  [0x415b7c], ecx
//! 40ccf6  c1 e1 08            shl  ecx, 8
//! 40ccf9  03 ca               add  ecx, edx                ; 'NT' if major > 4
//! 40ccfb  89 0d 78 5b 41 00   mov  [0x415b78], ecx
//! ```
//!
//! So the guest splits the dword into `major = (v >> 8) & 0xff` and
//! `minor = v & 0xff`, and derives a platform tag from `major > 4`. That makes
//! the value meaningful, not decorative: reporting Windows 2000 (major 5) and
//! reporting Windows 95 (major 4) send the program down different paths.
//!
//! ## What "the host" is
//!
//! There is no Windows underneath. The host *presents as* a particular
//! Windows version, the same choice Wine makes, and that choice is part of the
//! host's contract: whichever one is reported must also be the version whose
//! behaviour the rest of the host layer implements. [`WindowsVersion`] is a
//! named, enumerable thing for exactly that reason -- so the choice can be
//! stated once and be visible.

/// A Windows version, as the host chooses to present itself.
///
/// The numbers are the `dwMajorVersion`/`dwMinorVersion` values the real API
/// reports, i.e. the fields packed into `GetVersion`'s dword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsVersion {
    pub major: u8,
    pub minor: u8,
    /// `OSVERSIONINFOA.dwPlatformId`
    pub platform: Platform,
    /// Build number, as `OSVERSIONINFOA.dwBuildNumber`. `GetVersion` packs only
    /// the low 16 bits of this into the high word of its result.
    pub build: u16,
}

/// `OSVERSIONINFO.dwPlatformId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// `VER_PLATFORM_WIN32s` (0). Not offered: the host does not pretend to be
    /// a 16-bit Windows on top of Windows 3.1.
    Win32s = 0,
    /// `VER_PLATFORM_WIN32_WINDOWS` (1): Windows 95/98/ME.
    Win32Windows = 1,
    /// `VER_PLATFORM_WIN32_NT` (2): Windows NT and everything after it.
    Win32Nt = 2,
}

impl WindowsVersion {
    /// Windows 2000, the oldest NT-flagged version the guest distinguishes.
    ///
    /// This is the default for one reason: it is the version whose semantics
    /// the rest of this host is being written against, and it is old enough
    /// that programs asking "is this NT?" get a yes without also being told
    /// about features the host does not have (XP theming, `GetVersionEx`
    /// lying to unmanifested binaries).
    pub const WIN2000: WindowsVersion = WindowsVersion {
        major: 5,
        minor: 0,
        platform: Platform::Win32Nt,
        build: 2195,
    };

    /// `GetVersion`'s return dword:
    /// `(build & 0xffff) << 16 | (major & 0xff) << 8 | (minor & 0xff)`.
    ///
    /// For NT-platform versions the high bit of the high word is *not* set; a
    /// platform id of `VER_PLATFORM_WIN32_WINDOWS` sets it. That bit is how
    /// callers that do not use `GetVersionEx` tell NT from 9x.
    pub fn version_dword(self) -> u32 {
        let platform_bit = match self.platform {
            Platform::Win32Windows => 0x8000_0000,
            _ => 0,
        };
        (self.build as u32) << 16
            | platform_bit
            | ((self.major as u32) << 8)
            | self.minor as u32
    }

    /// The `OSVERSIONINFOA` fields the guest can read back.
    ///
    /// Returns `(major, minor, build, platform)`.
    pub fn os_version_info(self) -> (u32, u32, u32, u32) {
        (
            self.major as u32,
            self.minor as u32,
            self.build as u32,
            self.platform as u32,
        )
    }
}

/// Size of `OSVERSIONINFOA` in bytes.
pub const OSVERSIONINFOA_SIZE: u32 = 148;

/// Size of `OSVERSIONINFOEXA`, which is what a caller passes when it wants the
/// extended fields (`wServicePackMajor` and friends).
pub const OSVERSIONINFOEXA_SIZE: u32 = 284;

/// Field offsets in `OSVERSIONINFOA`.
pub mod infoa_offset {
    pub const SIZE: u32 = 0x00;
    pub const MAJOR: u32 = 0x04;
    pub const MINOR: u32 = 0x08;
    pub const BUILD: u32 = 0x0c;
    pub const PLATFORM: u32 = 0x10;
    pub const CSD_VERSION: u32 = 0x14;
    pub const CSD_VERSION_LEN: u32 = 128;
}

/// Why a `GetVersionEx*` call could not be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionExError {
    /// `dwOSVersionInfoSize` was neither `OSVERSIONINFOA_SIZE` nor
    /// `OSVERSIONINFOEXA_SIZE`. The real API fails with
    /// `ERROR_INSUFFICIENT_BUFFER`; reporting the mismatch is more useful than
    /// silently filling in whatever the guest asked for.
    BadSize { got: u32, at: u32 },
    /// The structure is not entirely inside guest memory.
    OutOfBounds { addr: u32 },
}

impl std::fmt::Display for VersionExError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionExError::BadSize { got, at } => write!(
                f,
                "dwOSVersionInfoSize is {got} at {at:#x} (expected {OSVERSIONINFOA_SIZE} or \
                 {OSVERSIONINFOEXA_SIZE})"
            ),
            VersionExError::OutOfBounds { addr } => {
                write!(f, "OSVERSIONINFO struct at {addr:#x} is out of bounds")
            }
        }
    }
}

impl WindowsVersion {
    /// Fill an `OSVERSIONINFOA` at `at` in guest memory.
    ///
    /// ## Why the size field is checked rather than trusted
    ///
    /// The guest passes `dwOSVersionInfoSize` in, and the API is required to
    /// reject a value it does not recognise. Writing a fixed 148 bytes anyway
    /// would corrupt a caller that passed a smaller struct, so the size decides
    /// both success and how much is written.
    ///
    /// The extended `OSVERSIONINFOEXA` shares this prefix; its extra fields are
    /// zeroed, which is truthful here: this host offers no service pack and no
    /// product type, so claiming otherwise would be an invented value.
    pub fn write_os_version_info(
        self,
        mem: &mut crate::GuestMemory<'_>,
        at: u32,
    ) -> Result<(), VersionExError> {
        let size = mem
            .read_u32(at + infoa_offset::SIZE)
            .ok_or(VersionExError::OutOfBounds { addr: at })?;
        let total = match size {
            OSVERSIONINFOA_SIZE => OSVERSIONINFOA_SIZE,
            OSVERSIONINFOEXA_SIZE => OSVERSIONINFOEXA_SIZE,
            got => return Err(VersionExError::BadSize { got, at }),
        };
        // Check the whole struct is addressable *before* writing anything, so a
        // bad pointer cannot leave the guest with a half-filled structure.
        let region = mem
            .get_mut(at, total as usize)
            .ok_or(VersionExError::OutOfBounds { addr: at })?;

        let (major, minor, build, platform) = self.os_version_info();
        let put = |region: &mut [u8], off: u32, v: u32| {
            region[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
        };
        put(region, infoa_offset::SIZE, size);
        put(region, infoa_offset::MAJOR, major);
        put(region, infoa_offset::MINOR, minor);
        put(region, infoa_offset::BUILD, build);
        put(region, infoa_offset::PLATFORM, platform);
        // szCSDVersion stays empty: this host reports no service pack, and an
        // invented string would be exactly the plausible lie the layer avoids.
        let csd = &mut region[infoa_offset::CSD_VERSION as usize
            ..(infoa_offset::CSD_VERSION + infoa_offset::CSD_VERSION_LEN) as usize];
        csd.fill(0);
        Ok(())
    }
}

impl Default for WindowsVersion {
    fn default() -> Self {
        WindowsVersion::WIN2000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GuestMemory;

    fn info_at(data: &mut [u8], at: u32, size: u32) {
        data[at as usize..at as usize + 4].copy_from_slice(&size.to_le_bytes());
    }

    fn u32_at(data: &[u8], at: u32) -> u32 {
        let o = at as usize;
        u32::from_le_bytes(data[o..o + 4].try_into().unwrap())
    }

    #[test]
    fn the_reported_version_round_trips_through_osversioninfoa() {
        let mut data = vec![0u8; 0x1000];
        info_at(&mut data, 0x100, OSVERSIONINFOA_SIZE);
        let mut mem = GuestMemory::new(&mut data);
        WindowsVersion::WIN2000
            .write_os_version_info(&mut mem, 0x100)
            .expect("a correctly sized struct is writable");
        let d = mem.get(0x100, OSVERSIONINFOA_SIZE as usize).unwrap();
        assert_eq!(u32_at(d, infoa_offset::SIZE), OSVERSIONINFOA_SIZE);
        assert_eq!(u32_at(d, infoa_offset::MAJOR), 5);
        assert_eq!(u32_at(d, infoa_offset::MINOR), 0);
        assert_eq!(u32_at(d, infoa_offset::BUILD), 2195);
        assert_eq!(u32_at(d, infoa_offset::PLATFORM), 2, "VER_PLATFORM_WIN32_NT");
    }

    #[test]
    fn get_version_ex_agrees_with_get_version() {
        // Two APIs reporting different versions to one program is a bug that
        // shows up as inexplicable divergence much later.
        let v = WindowsVersion::WIN2000;
        let dword = v.version_dword();
        let (major, minor, build, platform) = v.os_version_info();
        assert_eq!((dword >> 8) & 0xff, major);
        assert_eq!(dword & 0xff, minor);
        assert_eq!(dword >> 16, build);
        assert_eq!(platform, Platform::Win32Nt as u32);
    }

    #[test]
    fn a_wrong_size_is_rejected_instead_of_partially_written() {
        let mut data = vec![0u8; 0x1000];
        info_at(&mut data, 0x100, 100); // not a real size
        let mut mem = GuestMemory::new(&mut data);
        let err = WindowsVersion::WIN2000
            .write_os_version_info(&mut mem, 0x100)
            .unwrap_err();
        assert!(matches!(err, VersionExError::BadSize { got: 100, .. }), "{err}");
        // Nothing may have been written: a half-filled struct is worse than none.
        let d = mem.get(0x104, 16).unwrap();
        assert_eq!(d, &[0u8; 16], "a rejected call must not scribble on the struct");
    }

    #[test]
    fn an_out_of_bounds_struct_is_reported() {
        let mut data = vec![0u8; 0x200];
        let mut mem = GuestMemory::new(&mut data);
        let err = WindowsVersion::WIN2000
            .write_os_version_info(&mut mem, 0xFFFF_FF00)
            .unwrap_err();
        assert!(matches!(err, VersionExError::OutOfBounds { .. }), "{err}");
    }

    #[test]
    fn the_extended_struct_is_zero_filled_beyond_the_common_prefix() {
        let mut data = vec![0u8; 0x1000];
        info_at(&mut data, 0x100, OSVERSIONINFOEXA_SIZE);
        // Pre-fill the extended area so a "did anything write there" check is
        // meaningful.
        for b in &mut data[0x100 + OSVERSIONINFOA_SIZE as usize..0x100 + OSVERSIONINFOEXA_SIZE as usize] {
            *b = 0xAA;
        }
        let mut mem = GuestMemory::new(&mut data);
        WindowsVersion::WIN2000
            .write_os_version_info(&mut mem, 0x100)
            .expect("the extended size is accepted");
        let d = mem
            .get(0x100, OSVERSIONINFOEXA_SIZE as usize)
            .unwrap();
        // The common prefix carries the version.
        assert_eq!(u32_at(d, infoa_offset::MAJOR), 5);
        assert_eq!(u32_at(d, infoa_offset::SIZE), OSVERSIONINFOEXA_SIZE);
    }

    #[test]
    fn no_service_pack_is_claimed() {
        let mut data = vec![0u8; 0x1000];
        info_at(&mut data, 0x100, OSVERSIONINFOA_SIZE);
        let mut mem = GuestMemory::new(&mut data);
        WindowsVersion::WIN2000.write_os_version_info(&mut mem, 0x100).unwrap();
        let csd = mem
            .get(
                0x100 + infoa_offset::CSD_VERSION,
                infoa_offset::CSD_VERSION_LEN as usize,
            )
            .unwrap();
        assert!(
            csd.iter().all(|&b| b == 0),
            "szCSDVersion must be empty: inventing a service pack is a plausible lie"
        );
    }
}
