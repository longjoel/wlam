//! Resource directory (`.rsrc`) parsing.
//!
//! Windows resources form a three-level tree: *type* → *name/id* → *language*
//! → `IMAGE_RESOURCE_DATA_ENTRY`. We flatten that into a vector of entries so
//! callers can look up icons, menus and version blocks without knowing the
//! on-disk layout. Resource names may be numeric IDs or UTF-16 strings.

use crate::{Directory, ImageView, PeError};

/// A leaf resource node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceEntry {
    /// Resource type (`RT_ICON` = 3, `RT_MENU` = 4, `RT_STRING` = 6, ...).
    pub type_id: u16,
    /// Numeric name either as an ID (positive) or a named identifier.
    pub name: ResourceName,
    /// Language ID (`MAKELANGID`).
    pub lang: u16,
    /// RVA of the resource data.
    pub data_rva: u32,
    /// Size in bytes.
    pub size: u32,
    /// Code page from the data entry.
    pub codepage: u32,
}

/// A resource name: numeric ID or string name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceName {
    Id(u16),
    Name(String),
}

impl ResourceName {
    pub fn as_id(&self) -> Option<u16> {
        match self {
            ResourceName::Id(i) => Some(*i),
            ResourceName::Name(_) => None,
        }
    }
}

/// Flattened resource table.
#[derive(Debug, Default, Clone)]
pub struct ResourceTable {
    pub entries: Vec<ResourceEntry>,
}

impl ResourceTable {
    /// Numeric-named resources of a given type (the common case for icons
    /// and string tables).
    pub fn by_type(&self, type_id: u16) -> impl Iterator<Item = &ResourceEntry> {
        self.entries.iter().filter(move |e| e.type_id == type_id)
    }

    /// All entries with the given numeric name.
    pub fn by_name(&self, name: u16) -> impl Iterator<Item = &ResourceEntry> {
        self.entries
            .iter()
            .filter(move |e| e.name.as_id() == Some(name))
    }
}

const RT_HIGH_BIT: u32 = 0x8000_0000;

/// Walk the resource tree starting at `dir`.
pub(crate) fn parse_resources(
    image: &ImageView<'_>,
    dir: Option<Directory>,
) -> Result<ResourceTable, PeError> {
    let mut table = ResourceTable::default();
    let Some(dir) = dir else { return Ok(table) };
    let root = dir.rva;
    walk(
        image,
        root,
        root,
        0,
        &mut (0, ResourceName::Id(0), 0),
        &mut table,
    )?;
    Ok(table)
}

/// `root_rva` is the resource data directory itself; all offsets inside the
/// tree are relative to it, *not* to the sub-directory being walked.
fn walk(
    image: &ImageView<'_>,
    root_rva: u32,
    dir_rva: u32,
    depth: usize,
    path: &mut (u16, ResourceName, u16),
    out: &mut ResourceTable,
) -> Result<(), PeError> {
    // Bound the nesting depth to keep a malformed image from recursing
    // forever.
    if depth > 3 {
        return Err(PeError::BadResources {
            reason: "resource tree deeper than 3 levels".into(),
        });
    }
    let named = image.u16(dir_rva + 12)? as u32;
    let id_entries = image.u16(dir_rva + 14)? as u32;
    let total = named + id_entries;
    for i in 0..total {
        let ent = dir_rva + 16 + i * 8;
        let name_field = image.u32(ent)?;
        let offset_field = image.u32(ent + 4)?;
        let is_name_string = name_field & RT_HIGH_BIT != 0;
        let name = if is_name_string {
            let s_rva = root_rva + (name_field & !RT_HIGH_BIT);
            let len = image.u16(s_rva)? as u32;
            let bytes = image.bytes(s_rva + 2, len * 2)?;
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            ResourceName::Name(String::from_utf16_lossy(&units))
        } else {
            ResourceName::Id((name_field & 0xffff) as u16)
        };
        // `level` is which component of the (type, name, lang) path this
        // entry names; it is *not* the recursion depth, because a data entry
        // hangs off the third level while being visited at depth 2.
        let level = depth;
        match level {
            0 => path.0 = name.as_id().unwrap_or(u16::MAX),
            1 => path.1 = name,
            _ => path.2 = name.as_id().unwrap_or(0),
        }
        if offset_field & RT_HIGH_BIT != 0 {
            let sub = root_rva + (offset_field & !RT_HIGH_BIT);
            walk(image, root_rva, sub, depth + 1, path, out)?;
            // Restore this level's component so sibling entries are not
            // polluted by the subtree just visited. Level 0 (type) must also
            // be cleared: reaching depth 1 means the path has slipped a level,
            // and clearing keeps such siblings from inheriting a stale type.
            if level == 0 {
                path.0 = 0;
            } else if level == 1 {
                path.1 = ResourceName::Id(0);
            } else if level == 2 {
                path.2 = 0;
            }
        } else {
            // IMAGE_RESOURCE_DATA_ENTRY: DataRVA, Size, CodePage, Reserved.
            let data = root_rva + offset_field;
            out.entries.push(ResourceEntry {
                type_id: path.0,
                name: path.1.clone(),
                lang: path.2,
                data_rva: image.u32(data)?,
                size: image.u32(data + 4)?,
                codepage: image.u32(data + 8)?,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Section;

    #[test]
    fn resource_name_helpers() {
        assert_eq!(ResourceName::Id(3).as_id(), Some(3));
        assert_eq!(ResourceName::Name("x".into()).as_id(), None);
    }

    /// Build a three-level resource tree where all offsets are relative to the
    /// *root* and verify entries resolve. Regression test for a bug where
    /// offsets were added to the current sub-directory's RVA, which broke on
    /// every real DLL with resources below the top level.
    #[test]
    fn offsets_are_root_relative() {
        let mut data = vec![0u8; 0x200];
        // Section virtual address is 0x100, so file offset `data[i]` is RVA
        // `0x100 + i`. Writing at `data[root + n]` would land at RVA
        // `0x100 + 0x100 + n`, i.e. past the real header.
        let _ = data.len();
        // Every offset below is file-offset based: RVA 0x100 + file offset.
        // Root directory: one ID entry, type ICON (3) -> sub dir at +0x30.
        data[12..14].copy_from_slice(&0u16.to_le_bytes()); // named
        data[14..16].copy_from_slice(&1u16.to_le_bytes()); // ids
        data[16..20].copy_from_slice(&3u32.to_le_bytes()); // type id
        data[20..24].copy_from_slice(&0x8000_0030u32.to_le_bytes());
        // Sub directory: name 101 -> leaf dir at +0x50.
        let sub = 0x30;
        data[sub + 14..sub + 16].copy_from_slice(&1u16.to_le_bytes());
        data[sub + 16..sub + 20].copy_from_slice(&101u32.to_le_bytes());
        data[sub + 20..sub + 24].copy_from_slice(&0x8000_0050u32.to_le_bytes());
        // Leaf directory: lang 1033 -> data entry at +0x70.
        let leaf = 0x50;
        data[leaf + 14..leaf + 16].copy_from_slice(&1u16.to_le_bytes());
        data[leaf + 16..leaf + 20].copy_from_slice(&1033u32.to_le_bytes());
        data[leaf + 20..leaf + 24].copy_from_slice(&0x0070u32.to_le_bytes());
        // Data entry: data at RVA 0x400, 0x20 bytes, cp1252.
        let entry = 0x70;
        data[entry..entry + 4].copy_from_slice(&0x400u32.to_le_bytes());
        data[entry + 4..entry + 8].copy_from_slice(&0x20u32.to_le_bytes());
        data[entry + 8..entry + 12].copy_from_slice(&1252u32.to_le_bytes());

        let section = Section {
            name: ".rsrc".into(),
            virtual_address: 0x100,
            virtual_size: data.len() as u32,
            file_offset: 0,
            file_size: data.len() as u32,
            characteristics: 0x4000_0040,
            data,
        };
        let raw: Vec<u8> = vec![0u8; 0x200];
        let sections = vec![section];
        let image = ImageView {
            raw: &raw,
            sections: &sections,
            image_base: 0,
            is64: false,
        };
        let dir = Directory {
            index: Directory::RESOURCE,
            rva: 0x100,
            size: 0x100,
        };
        let table = parse_resources(&image, Some(dir)).expect("tree parses");
        assert_eq!(table.entries.len(), 1);
        let e = &table.entries[0];
        assert_eq!(e.type_id, 3);
        assert_eq!(e.name.as_id(), Some(101));
        assert_eq!(e.lang, 1033);
        assert_eq!(e.data_rva, 0x400);
        assert_eq!(e.size, 0x20);
        assert_eq!(e.codepage, 1252);
    }
}
