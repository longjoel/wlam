//! Locale identity: `GetACP`, `GetOEMCP`, and the code-page information
//! derived from them.
//!
//! Hover resolves each character it wants to draw through a code page. Its
//! lookup routine starts by calling `GetACP`/`GetOEMCP` (through a helper that
//! treats `-2` as OEM and `-3` as ANSI). A made-up or zero answer sends it into
//! a code-page table lookup for a page the host does not have; the honest
//! answer is the page this host actually presents.
//!
//! The host presents as an English Windows 2000 installation, so the ANSI code
//! page is 1252 (Western European) and the OEM code page is 437 (United
//! States). Those are properties of the host, not values invented per call.

/// The ANSI code page (`GetACP`): Western European.
pub const ANSI_CODE_PAGE: u32 = 1252;
/// The OEM code page (`GetOEMCP`): United States.
pub const OEM_CODE_PAGE: u32 = 437;

/// Windows-1252's non-Latin-1 range, including the C1 control mappings.
const CP1252_HIGH: [u16; 32] = [
    0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039,
    0x152, 0x8d, 0x17d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x2dc,
    0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e, 0x178,
];
pub fn ansi_encode(value: u16) -> u8 {
    if let Some(index) = CP1252_HIGH.iter().position(|code| *code == value) {
        (index + 0x80) as u8
    } else if value < 0x80 || (0xa0..=0xff).contains(&value) {
        value as u8
    } else {
        b'?'
    }
}

/// One-byte case conversion for the host's English Windows-1252 profile.
/// Multi-character and unrepresentable Unicode mappings retain the original
/// byte; CharUpper/Lower cannot grow an ANSI character or buffer.
pub fn ansi_case(byte: u8, uppercase: bool) -> u8 {
    let value = if (0x80..0xa0).contains(&byte) {
        CP1252_HIGH[(byte - 0x80) as usize]
    } else {
        byte as u16
    };
    let character = char::from_u32(value as u32).unwrap();
    let mapped = if uppercase {
        let mut mapping = character.to_uppercase();
        let first = mapping.next().unwrap();
        if mapping.next().is_some() {
            return byte;
        }
        first
    } else {
        let mut mapping = character.to_lowercase();
        let first = mapping.next().unwrap();
        if mapping.next().is_some() {
            return byte;
        }
        first
    };
    if mapped as u32 > u16::MAX as u32 {
        return byte;
    }
    let encoded = ansi_encode(mapped as u16);
    if encoded == b'?' && mapped != '?' {
        byte
    } else {
        encoded
    }
}

/// ANSI conversion used by the guest CRT. Sizes include NUL only for -1 input.
/// https://learn.microsoft.com/windows/win32/api/stringapiset/nf-stringapiset-multibytetowidechar
pub fn multi_byte_to_wide(
    args: &[u32],
    memory: &mut dyn crate::memory::MemoryAccess,
) -> Result<u32, u32> {
    let (page, flags, input, length, output, capacity) = (
        args[0],
        args[1],
        args[2],
        args[3] as i32,
        args[4],
        args[5] as i32,
    );
    if !matches!(page, 0 | 1252) {
        return Err(87);
    }
    if flags & !9 != 0 {
        return Err(1004);
    }
    if length == 0 || length < -1 || capacity < 0 || input == 0 {
        return Err(87);
    }
    let mut wide = Vec::new();
    let limit = if length == -1 {
        memory.byte_len().saturating_sub(input as u64)
    } else {
        length as u64
    };
    for i in 0..limit {
        let at = input
            .checked_add(u32::try_from(i).map_err(|_| 87u32)?)
            .ok_or(87u32)?;
        let mut byte = [0];
        memory.read(at, &mut byte).map_err(|_| 87u32)?;
        let byte = byte[0];
        wide.push(if (0x80..0xa0).contains(&byte) {
            CP1252_HIGH[(byte - 0x80) as usize]
        } else {
            byte as u16
        });
        if length == -1 && byte == 0 {
            break;
        }
    }
    if length == -1 && wide.last() != Some(&0) {
        return Err(87);
    }
    let count = u32::try_from(wide.len()).map_err(|_| 87u32)?;
    if capacity == 0 {
        return Ok(count);
    }
    if output == 0 || (capacity as u32) < count {
        return Err(122);
    }
    let bytes = wide
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    memory.write(output, &bytes).map_err(|_| 87u32)?;
    Ok(count)
}

/// `CP_ACP`: the host's ANSI code page.
pub const CP_ACP: u32 = 0;
/// `CP_OEMCP`: the host's OEM code page.
pub const CP_OEMCP: u32 = 1;

/// `sizeof(CPINFO)` as Windows writes it: a `UINT`, two default-character
/// bytes, and twelve lead-byte bytes, with the struct's 4-byte alignment
/// padding at the end.
pub const CPINFO_SIZE: u32 = 20;

/// Field offsets in `CPINFO`.
pub mod cpinfo_offset {
    pub const MAX_CHAR_SIZE: u32 = 0x00;
    pub const DEFAULT_CHAR: u32 = 0x04;
    pub const LEAD_BYTE: u32 = 0x06;
}

/// What `GetCPInfo` reports for a single-byte code page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpInfo {
    /// Bytes per character: 1 for an SBCS page.
    pub max_char_size: u32,
    /// The replacement character Windows uses for unmappable input.
    pub default_char: [u8; 2],
    /// Lead-byte ranges, terminated by a zero pair. Empty for SBCS.
    pub lead_byte: [u8; 12],
}

impl CpInfo {
    /// The information for a code page this host actually presents.
    ///
    /// Both the symbolic aliases (`CP_ACP`, `CP_OEMCP`) and their concrete
    /// values are accepted, because callers use both. An unknown page is
    /// `None`: the host must not describe a character set it does not have.
    pub fn for_code_page(code_page: u32) -> Option<Self> {
        match code_page {
            CP_ACP | ANSI_CODE_PAGE | CP_OEMCP | OEM_CODE_PAGE => Some(CpInfo {
                max_char_size: 1,
                // '?' is the default character for the Western pages.
                default_char: [b'?', 0],
                lead_byte: [0; 12],
            }),
            _ => None,
        }
    }

    /// Write the structure at `at`.
    ///
    /// The whole `sizeof(CPINFO)` range is validated before any byte is
    /// written, so a bad pointer cannot leave a partially described page.
    pub fn write(&self, mem: &mut dyn crate::memory::MemoryAccess, at: u32) -> bool {
        let mut region = [0u8; CPINFO_SIZE as usize];
        region.fill(0);
        region[cpinfo_offset::MAX_CHAR_SIZE as usize..cpinfo_offset::MAX_CHAR_SIZE as usize + 4]
            .copy_from_slice(&self.max_char_size.to_le_bytes());
        region[cpinfo_offset::DEFAULT_CHAR as usize..cpinfo_offset::DEFAULT_CHAR as usize + 2]
            .copy_from_slice(&self.default_char);
        region[cpinfo_offset::LEAD_BYTE as usize..cpinfo_offset::LEAD_BYTE as usize + 12]
            .copy_from_slice(&self.lead_byte);
        mem.write(at, &region).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_pages_are_the_pages_this_host_claims() {
        // 1252 is the ANSI page for the English Windows install this host
        // presents; 437 is its OEM counterpart. A caller that looks either up
        // must find a table it already knows.
        assert_eq!(ANSI_CODE_PAGE, 1252);
        assert_eq!(OEM_CODE_PAGE, 437);
        assert_ne!(ANSI_CODE_PAGE, OEM_CODE_PAGE);
    }

    #[test]
    fn cpinfo_describes_a_single_byte_western_page() {
        let mut data = vec![0xAAu8; 0x100];
        let mut mem = crate::GuestMemory::new(&mut data);
        let info = CpInfo::for_code_page(ANSI_CODE_PAGE).unwrap();
        assert!(info.write(&mut mem, 0x20));
        assert_eq!(mem.read_u32(0x20), Some(1));
        assert_eq!(mem.get(0x24, 2), Some(&b"?\0"[..]));
        // No lead bytes for an SBCS page.
        assert_eq!(mem.get(0x26, 12), Some(&[0u8; 12][..]));
    }

    #[test]
    fn symbolic_and_concrete_code_pages_agree() {
        let ansi = CpInfo::for_code_page(CP_ACP).unwrap();
        assert_eq!(ansi, CpInfo::for_code_page(ANSI_CODE_PAGE).unwrap());
        let oem = CpInfo::for_code_page(CP_OEMCP).unwrap();
        assert_eq!(oem, CpInfo::for_code_page(OEM_CODE_PAGE).unwrap());
    }

    #[test]
    fn an_unknown_code_page_is_refused_rather_than_described() {
        assert_eq!(CpInfo::for_code_page(932), None);
        assert_eq!(CpInfo::for_code_page(65001), None);
    }

    #[test]
    fn an_out_of_bounds_write_changes_nothing() {
        let mut data = vec![0xAAu8; 0x30];
        let mut mem = crate::GuestMemory::new(&mut data);
        let info = CpInfo::for_code_page(ANSI_CODE_PAGE).unwrap();
        assert!(
            !info.write(&mut mem, 0x20),
            "20 bytes at 0x20 overruns 0x30"
        );
        assert_eq!(mem.get(0x20, 0x10), Some(&[0xAAu8; 0x10][..]));
    }
}
