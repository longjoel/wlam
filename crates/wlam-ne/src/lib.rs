//! Windows NE loading, selector-aware execution and an initial Win16 frontend.
//!
//! This crate parses DOS/NE metadata, materializes checked selector-backed
//! segments, applies supported relocation records, emits decoded 16-bit P-code
//! through C to Wasm, and supplies a single-task Win16 API subset. A checked
//! instruction-step CPU provides differential verification. Unsupported APIs
//! and CPU operations fail explicitly; this is not complete Win16 compatibility.

use std::cell::RefCell;
use std::rc::Rc;
use thiserror::Error;

extern crate alloc;

mod cpu;
pub use cpu::{NeCpuFlow, NeMachine, NeStepResult, NeTaskStartContext};
mod task;
pub use task::{NeTask, NeTaskError, NeTaskEvent};
pub mod compile;
mod bitmap;
mod host_memory;
mod templates;
mod profile;

const DOS_HEADER_SIZE: usize = 0x40;
const NE_HEADER_SIZE: usize = 0x40;
const NE_SEGMENT_SIZE: usize = 8;
const X86_MAX_INSTRUCTION_SIZE: usize = 15;
const NE_MOVABLE_ENTRY_MARKER: u8 = 0x3f;
const NE_INT_OPCODE: u8 = 0xcd;
const NE_RELOCINFO: u16 = 0x0100;
const RELOC_ADDITIVE: u8 = 0x04;
const RELOC_CHAIN_END: u16 = 0xffff;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeImage {
    pub ne_offset: u32,
    pub flags: u16,
    pub auto_data_segment: u16,
    pub heap_size: u16,
    pub stack_size: u16,
    pub initial_cs: u16,
    pub initial_ip: u16,
    pub initial_ss: u16,
    pub initial_sp: u16,
    pub alignment_shift: u16,
    pub segments: Vec<NeSegment>,
    pub entry_points: Vec<NeEntryPoint>,
    pub module_references: Vec<NeModuleReference>,
    pub imported_names: Vec<NeImportedName>,
    pub resources: Vec<NeResource>,
}

/// A protected-mode selector mapped onto owned guest bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeSelector {
    pub selector: u16,
    pub segment: u16,
    pub limit: u32,
    pub writable: bool,
    pub executable: bool,
    pub storage_index: usize,
    allocation_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeLoadedImage {
    pub selectors: Vec<NeSelector>,
    storage: Vec<Rc<RefCell<Vec<u8>>>>,
    next_allocation_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NeResourceInfoHandle(pub u16);

/// A movable Win16 global handle for a loaded resource. It is distinct from
/// the selector returned by `lock_resource`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NeGlobalResourceHandle(pub u16);

#[derive(Clone, Debug, PartialEq, Eq)]
struct NeResourceState {
    info_handle: NeResourceInfoHandle,
    resource_index: usize,
    loaded_selector: Option<u16>,
    loaded_allocation_id: Option<u64>,
    global_handle: Option<NeGlobalResourceHandle>,
    load_count: u16,
    lock_count: u16,
}

/// Win16 resource handles and lazy resource memory for one NE module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NeResourceManager {
    resources: Vec<NeResourceState>,
    next_info_handle: u16,
    next_global_handle: u16,
    free_global_handles: Vec<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeAccess {
    Read,
    Write,
    Execute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeFarAddress {
    pub selector: u16,
    pub offset: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NeImportSymbol {
    Ordinal(u16),
    Name(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeImportBinding {
    pub module: Vec<u8>,
    pub symbol: NeImportSymbol,
    pub address: NeFarAddress,
}

/// Explicit addresses for imported NE module exports. Module names compare
/// case-insensitively; exported names and ordinals compare exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NeImportResolver {
    bindings: Vec<NeImportBinding>,
}

impl NeImportResolver {
    pub fn bind_ordinal(&mut self, module: impl AsRef<[u8]>, ordinal: u16, address: NeFarAddress) {
        self.bindings.push(NeImportBinding {
            module: module.as_ref().to_vec(),
            symbol: NeImportSymbol::Ordinal(ordinal),
            address,
        });
    }

    pub fn bind_name(
        &mut self,
        module: impl AsRef<[u8]>,
        name: impl AsRef<[u8]>,
        address: NeFarAddress,
    ) {
        self.bindings.push(NeImportBinding {
            module: module.as_ref().to_vec(),
            symbol: NeImportSymbol::Name(name.as_ref().to_vec()),
            address,
        });
    }

    pub fn resolve(&self, image: &NeImage, relocation: &NeRelocation) -> Option<NeFarAddress> {
        let module_index = relocation.target1.checked_sub(1)? as usize;
        let module = image.module_references.get(module_index)?;
        let symbol = match relocation.flags & 0x03 {
            1 => NeImportSymbol::Ordinal(relocation.target2),
            2 => NeImportSymbol::Name(
                image
                    .imported_names
                    .iter()
                    .find(|name| name.offset == relocation.target2)?
                    .bytes
                    .clone(),
            ),
            _ => return None,
        };
        self.bindings
            .iter()
            .rev()
            .find(|binding| {
                binding.module.eq_ignore_ascii_case(&module.name) && binding.symbol == symbol
            })
            .map(|binding| binding.address)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeStartState {
    pub cs: u16,
    pub ip: u16,
    pub ss: u16,
    pub sp: u16,
}

#[derive(Clone, Debug)]
pub struct NeDecodedInstruction {
    pub cs: u16,
    pub ip: u16,
    pub next_ip: u16,
    pub instruction: rsleigh_api::Instruction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeEntryPoint {
    pub ordinal: u16,
    pub segment: u16,
    pub offset: u16,
    pub flags: u8,
    pub movable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeModuleReference {
    /// Zero-based position in the module-reference table. Relocation records
    /// encode this index as one-based `target1` and use `target1 - 1`.
    pub index: u16,
    pub imported_name_offset: u16,
    pub name: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeImportedName {
    /// Offset from the start of the Imported Names Table.
    pub offset: u16,
    /// Raw length-prefixed bytes; callers can decode with the guest code page.
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NeResourceIdentifier {
    Ordinal(u16),
    Name(Vec<u8>),
}

impl NeResourceIdentifier {
    /// Named Win16 resources compare without ASCII case; parsed names retain
    /// their original spelling. Numeric IDs remain a separate namespace.
    pub fn matches(&self, other:&Self)->bool {
        match (self,other) {
            (Self::Ordinal(a),Self::Ordinal(b))=>a==b,
            (Self::Name(a),Self::Name(b))=>a.eq_ignore_ascii_case(b),
            _=>false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeResource {
    pub resource_type: NeResourceIdentifier,
    pub name: NeResourceIdentifier,
    pub flags: u16,
    pub file_offset: u32,
    pub size: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeSegment {
    /// One-based NE segment number, as used by entry bundles and relocations.
    pub index: u16,
    /// File offset of initialized bytes, or `None` for a zero sector offset.
    pub file_offset: Option<u32>,
    /// Number of initialized bytes. Encoded zero means 64 KiB for a file-backed segment.
    pub file_size: u32,
    /// Minimum allocation size. Encoded zero means 64 KiB.
    pub minimum_allocation: u32,
    pub flags: u16,
    pub relocations: Vec<NeRelocation>,
}

/// A parsed 8-byte NE relocation record. `target1` and `target2` retain the
/// wire-format target words; interpreting them requires module/import tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeRelocation {
    pub source_type: u8,
    pub flags: u8,
    pub source_offset: u16,
    pub target1: u16,
    pub target2: u16,
    /// Segment-relative fixup locations linked from `source_offset`. Empty
    /// for additive records, which carry the value inline instead.
    pub chain_offsets: Vec<u16>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum NeError {
    #[error("input is not an MZ executable")]
    NotMz,
    #[error("input does not contain an NE header at e_lfanew {offset:#x}")]
    NotNe { offset: u32 },
    #[error("truncated or out-of-range field at file offset {offset:#x} (length {length:#x})")]
    OutOfBounds { offset: u64, length: u64 },
    #[error("NE alignment shift {shift} is outside the supported 0..=15 range")]
    InvalidAlignmentShift { shift: u16 },
    #[error("NE segment table has too many entries: {count}")]
    TooManySegments { count: u16 },
    #[error("NE segment {segment} has unsupported relocation source type {source_type:#x}")]
    InvalidRelocationSourceType { segment: u16, source_type: u8 },
    #[error("NE segment {segment} relocation source {offset:#x} with width {width} exceeds initialized data")]
    RelocationSourceOutOfBounds {
        segment: u16,
        offset: u16,
        width: u8,
    },
    #[error("NE segment {segment} relocation chain contains a cycle at {offset:#x}")]
    RelocationChainCycle { segment: u16, offset: u16 },
    #[error("malformed NE entry table at file offset {offset:#x}")]
    MalformedEntryTable { offset: u64 },
    #[error("NE entry ordinal {ordinal} refers to invalid segment {segment}")]
    EntrySegmentOutOfRange { ordinal: u16, segment: u16 },
    #[error("NE entry ordinal range overflows 16 bits")]
    EntryOrdinalOverflow,
    #[error("malformed NE imported-name table at file offset {offset:#x}")]
    MalformedImportedName { offset: u64 },
    #[error("malformed NE resource table at file offset {offset:#x}")]
    MalformedResourceTable { offset: u64 },
    #[error("unterminated resource identifier string at {selector:#x}:{offset:#06x}")]
    UnterminatedResourceIdentifier { selector: u16, offset: u16 },
    #[error("NE resource info handles are exhausted")]
    ResourceHandleSpaceExhausted,
    #[error("invalid NE resource info handle {handle:#06x}")]
    InvalidResourceInfoHandle { handle: u16 },
    #[error("invalid loaded NE resource handle {handle:#06x}")]
    InvalidGlobalResourceHandle { handle: u16 },
    #[error("NE resource load or lock count overflow")]
    ResourceUsageOverflow,
    #[error("NE segment {segment} relocation references module index {module_index}, but only {module_count} modules exist")]
    InvalidModuleReference {
        segment: u16,
        module_index: u16,
        module_count: u16,
    },
    #[error("NE segment {source_segment} relocation targets invalid segment {target_segment}")]
    InvalidRelocationSegment {
        source_segment: u16,
        target_segment: u16,
    },
    #[error("NE segment {source_segment} relocation target offset {target_offset:#x} exceeds target segment {target_segment} size {target_size:#x}")]
    RelocationTargetOffsetOutOfBounds {
        source_segment: u16,
        target_segment: u16,
        target_offset: u16,
        target_size: u32,
    },
    #[error("NE segment {source_segment} relocation targets missing entry ordinal {ordinal}")]
    InvalidRelocationEntryOrdinal { source_segment: u16, ordinal: u16 },
    #[error("segment {segment} is not present in the loaded image")]
    MissingSegment { segment: u16 },
    #[error("selector {selector:#x} is not present in the loaded image")]
    MissingSelector { selector: u16 },
    #[error("selector {selector:#x} denies {access:?} access")]
    SelectorAccessDenied { selector: u16, access: NeAccess },
    #[error("selector {selector:#x} range {offset:#x}..{end:#x} exceeds limit {limit:#x}")]
    SelectorLimitExceeded {
        selector: u16,
        offset: u32,
        end: u32,
        limit: u32,
    },
    #[error("selector allocation overflows while mapping NE segment {segment}")]
    SelectorAllocationOverflow { segment: u16 },
    #[error("selector allocation limit {limit:#x} exceeds the 64 KiB address space")]
    SelectorAllocationTooLarge { limit: u32 },
    #[error("selector {selector:#x} is already mapped")]
    DuplicateSelector { selector: u16 },
    #[error("relocation address {selector:#x}:{offset:#06x} is outside mapped selector memory")]
    InvalidRelocationAddress { selector: u16, offset: u16 },
    #[error("the null selector cannot map guest memory")]
    NullSelectorAllocation,
    #[error("no unused x86 selector remains for task memory")]
    SelectorSpaceExhausted,
    #[error("NE segment {segment} relocation has unsupported source type {source_type:#x}")]
    UnsupportedRelocationApplication { segment: u16, source_type: u8 },
    #[error("NE segment {segment} relocation target could not be resolved")]
    UnresolvedRelocationTarget { segment: u16 },
    #[error("import binding points outside mapped selector {selector:#x} at {offset:#x}")]
    InvalidImportedAddress { selector: u16, offset: u16 },
    #[error("instruction decoder is not configured for X86_16Protected")]
    WrongDecoderArchitecture,
    #[error("could not decode instruction at {selector:#x}:{offset:#06x}")]
    InstructionDecodeFailed { selector: u16, offset: u16 },
    #[error("unsupported P-code at operation {index}: {operation}")]
    UnsupportedPcode { index: usize, operation: String },
    #[error("invalid P-code selector pointer {pointer:#x}")]
    InvalidPcodeAddress { pointer: u64 },
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn bytes(&self, offset: u64, length: u64) -> Result<&[u8], NeError> {
        let start = usize::try_from(offset).map_err(|_| NeError::OutOfBounds { offset, length })?;
        let size = usize::try_from(length).map_err(|_| NeError::OutOfBounds { offset, length })?;
        let end = start
            .checked_add(size)
            .ok_or(NeError::OutOfBounds { offset, length })?;
        self.0
            .get(start..end)
            .ok_or(NeError::OutOfBounds { offset, length })
    }

    fn u16(&self, offset: u64) -> Result<u16, NeError> {
        let b = self.bytes(offset, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&self, offset: u64) -> Result<u32, NeError> {
        let b = self.bytes(offset, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

impl NeImage {
    /// Materialize every segment's declared allocation, copying initialized
    /// bytes and leaving the uninitialized tail zero-filled.
    pub fn load_segments(&self, raw: &[u8], first_selector: u16) -> Result<NeLoadedImage, NeError> {
        let mut selectors = Vec::with_capacity(self.segments.len());
        let mut storage = Vec::with_capacity(self.segments.len());
        for segment in &self.segments {
            let selector_index = u16::from(segment.index - 1)
                .checked_mul(8)
                .and_then(|delta| first_selector.checked_add(delta))
                .ok_or(NeError::SelectorAllocationOverflow {
                    segment: segment.index,
                })?;
            let selector = selector_index;
            let heap_extra = if segment.index == self.auto_data_segment {
                u32::from(self.heap_size)
            } else {
                0
            };
            let stack_extra = if segment.index == self.initial_ss {
                u32::from(self.stack_size).checked_add(4).ok_or(
                    NeError::SelectorAllocationOverflow {
                        segment: segment.index,
                    },
                )?
            } else {
                0
            };
            let allocation = segment
                .minimum_allocation
                .max(segment.file_size)
                .checked_add(heap_extra)
                .and_then(|size| size.checked_add(stack_extra))
                .ok_or(NeError::SelectorAllocationOverflow {
                    segment: segment.index,
                })?;
            let mut bytes = vec![0; allocation as usize];
            if let Some(file_offset) = segment.file_offset {
                let start = file_offset as usize;
                let end =
                    start
                        .checked_add(segment.file_size as usize)
                        .ok_or(NeError::OutOfBounds {
                            offset: u64::from(file_offset),
                            length: u64::from(segment.file_size),
                        })?;
                let data = raw.get(start..end).ok_or(NeError::OutOfBounds {
                    offset: u64::from(file_offset),
                    length: u64::from(segment.file_size),
                })?;
                bytes[..data.len()].copy_from_slice(data);
            }
            let storage_index = storage.len();
            storage.push(Rc::new(RefCell::new(bytes)));
            selectors.push(NeSelector {
                selector,
                segment: segment.index,
                limit: allocation,
                writable: segment.flags & 0x0001 != 0 && segment.flags & 0x0080 == 0,
                executable: segment.flags & 0x0001 == 0,
                storage_index,
                allocation_id: storage_index as u64 + 1,
            });
        }
        let next_allocation_id = selectors.len() as u64 + 1;
        Ok(NeLoadedImage {
            selectors,
            storage,
            next_allocation_id,
        })
    }

    /// Resolve the executable's initial segment:offset registers to runtime
    /// selectors after segments have been allocated and fixups applied.
    pub fn start_state(&self, loaded: &NeLoadedImage) -> Result<NeStartState, NeError> {
        let cs = loaded
            .selector_for_segment(self.initial_cs)
            .ok_or(NeError::MissingSegment {
                segment: self.initial_cs,
            })?;
        loaded.translate(cs, u32::from(self.initial_ip), 1, NeAccess::Execute)?;
        let ss = loaded
            .selector_for_segment(self.initial_ss)
            .ok_or(NeError::MissingSegment {
                segment: self.initial_ss,
            })?;
        let stack_pointer = if self.initial_sp == 0 {
            let stack_segment = self
                .initial_ss
                .checked_sub(1)
                .ok_or(NeError::MissingSegment {
                    segment: self.initial_ss,
                })?;
            let stack =
                self.segments
                    .get(usize::from(stack_segment))
                    .ok_or(NeError::MissingSegment {
                        segment: self.initial_ss,
                    })?;
            stack
                .minimum_allocation
                .max(stack.file_size)
                .checked_add(u32::from(self.stack_size))
                .ok_or(NeError::SelectorAllocationOverflow {
                    segment: self.initial_ss,
                })?
        } else {
            u32::from(self.initial_sp)
        };
        let sp = (stack_pointer & !1)
            .checked_add(4)
            .ok_or(NeError::SelectorAllocationOverflow {
                segment: self.initial_ss,
            })? as u16;
        // SP points at the next free stack byte, so one-past-limit is valid
        // for a downward-growing stack. A later push performs its own check.
        loaded.translate(ss, u32::from(sp), 0, NeAccess::Write)?;
        Ok(NeStartState {
            cs,
            ip: self.initial_ip,
            ss,
            sp,
        })
    }

    /// Resolve a relocation's one-based module-reference index.
    pub fn module_reference(&self, relocation_index: u16) -> Option<&NeModuleReference> {
        let index = relocation_index.checked_sub(1)?;
        self.module_references.get(usize::from(index))
    }

    /// Resolve an imported-name-table offset used by a name relocation.
    pub fn imported_name(&self, offset: u16) -> Option<&[u8]> {
        self.imported_names
            .iter()
            .find(|name| name.offset == offset)
            .map(|name| name.bytes.as_slice())
    }

    /// Return one resource's raw bytes from the original executable image.
    pub fn resource_data<'a>(
        &self,
        raw: &'a [u8],
        resource: &NeResource,
    ) -> Result<&'a [u8], NeError> {
        let start = usize::try_from(resource.file_offset).map_err(|_| NeError::OutOfBounds {
            offset: u64::from(resource.file_offset),
            length: u64::from(resource.size),
        })?;
        let size = usize::try_from(resource.size).map_err(|_| NeError::OutOfBounds {
            offset: u64::from(resource.file_offset),
            length: u64::from(resource.size),
        })?;
        let end = start.checked_add(size).ok_or(NeError::OutOfBounds {
            offset: u64::from(resource.file_offset),
            length: u64::from(resource.size),
        })?;
        raw.get(start..end).ok_or(NeError::OutOfBounds {
            offset: u64::from(resource.file_offset),
            length: u64::from(resource.size),
        })
    }

    /// Find a resource using Win16's case-insensitive named identifiers.
    pub fn find_resource(
        &self,
        resource_type: &NeResourceIdentifier,
        name: &NeResourceIdentifier,
    ) -> Option<&NeResource> {
        self.resources
            .iter()
            .find(|resource| resource.resource_type.matches(resource_type) && resource.name.matches(name))
    }

    /// Parse the MZ stub locator, NE header and segment table.
    pub fn parse(raw: &[u8]) -> Result<Self, NeError> {
        let reader = Reader(raw);
        if reader.bytes(0, 2)? != b"MZ" {
            return Err(NeError::NotMz);
        }
        reader.bytes(0, DOS_HEADER_SIZE as u64)?;
        let ne_offset = reader.u32(0x3c)?;
        let ne = u64::from(ne_offset);
        if reader.bytes(ne, 2)? != b"NE" {
            return Err(NeError::NotNe { offset: ne_offset });
        }
        reader.bytes(ne, NE_HEADER_SIZE as u64)?;

        let flags = reader.u16(ne + 0x0c)?;
        let auto_data_segment = reader.u16(ne + 0x0e)?;
        let heap_size = reader.u16(ne + 0x10)?;
        let stack_size = reader.u16(ne + 0x12)?;
        let initial_ip = reader.u16(ne + 0x14)?;
        let initial_cs = reader.u16(ne + 0x16)?;
        let initial_sp = reader.u16(ne + 0x18)?;
        let initial_ss = reader.u16(ne + 0x1a)?;
        let entry_table_offset = reader.u16(ne + 0x04)?;
        let entry_table_size = reader.u16(ne + 0x06)?;
        let segment_count = reader.u16(ne + 0x1c)?;
        let module_reference_count = reader.u16(ne + 0x1e)?;
        let resource_table_offset = reader.u16(ne + 0x24)?;
        let resident_names_offset = reader.u16(ne + 0x26)?;
        let module_reference_table_offset = reader.u16(ne + 0x28)?;
        let imported_names_table_offset = reader.u16(ne + 0x2a)?;
        let nonresident_names_offset = reader.u32(ne + 0x2c)?;
        if segment_count > 4096 {
            return Err(NeError::TooManySegments {
                count: segment_count,
            });
        }
        let segment_table = u64::from(reader.u16(ne + 0x22)?);
        let alignment_shift = reader.u16(ne + 0x32)?;
        if alignment_shift > 15 {
            return Err(NeError::InvalidAlignmentShift {
                shift: alignment_shift,
            });
        }

        let table_start = ne.checked_add(segment_table).ok_or(NeError::OutOfBounds {
            offset: ne,
            length: segment_table,
        })?;
        let table_size = u64::from(segment_count) * NE_SEGMENT_SIZE as u64;
        reader.bytes(table_start, table_size)?;

        let mut segments = Vec::with_capacity(segment_count as usize);
        for index in 0..segment_count {
            let record = table_start + u64::from(index) * NE_SEGMENT_SIZE as u64;
            let sector = reader.u16(record)?;
            let encoded_length = reader.u16(record + 2)?;
            let flags = reader.u16(record + 4)?;
            let encoded_allocation = reader.u16(record + 6)?;

            let file_size = if encoded_length == 0 {
                0x1_0000
            } else {
                u32::from(encoded_length)
            };
            let minimum_allocation = if encoded_allocation == 0 {
                0x1_0000
            } else {
                u32::from(encoded_allocation)
            };
            let file_offset = if sector == 0 {
                None
            } else {
                let offset = u64::from(sector)
                    .checked_shl(u32::from(alignment_shift))
                    .ok_or(NeError::OutOfBounds {
                        offset: u64::from(sector),
                        length: u64::from(alignment_shift),
                    })?;
                reader.bytes(offset, u64::from(file_size))?;
                Some(u32::try_from(offset).map_err(|_| NeError::OutOfBounds {
                    offset,
                    length: u64::from(file_size),
                })?)
            };
            let relocations = if flags & NE_RELOCINFO != 0 {
                let Some(file_offset) = file_offset else {
                    return Err(NeError::OutOfBounds {
                        offset: u64::from(sector),
                        length: u64::from(file_size),
                    });
                };
                let relocation_table = u64::from(file_offset)
                    .checked_add(u64::from(file_size))
                    .ok_or(NeError::OutOfBounds {
                        offset: u64::from(file_offset),
                        length: u64::from(file_size),
                    })?;
                parse_relocations(
                    &reader,
                    index + 1,
                    u64::from(file_offset),
                    file_size,
                    relocation_table,
                )?
            } else {
                Vec::new()
            };
            segments.push(NeSegment {
                index: index + 1,
                file_offset,
                file_size: if file_offset.is_none() { 0 } else { file_size },
                minimum_allocation,
                flags,
                relocations,
            });
        }
        let entry_points =
            parse_entry_table(&reader, ne, entry_table_offset, entry_table_size, &segments)?;
        let (module_references, imported_names) = parse_import_tables(
            &reader,
            ne,
            module_reference_table_offset,
            module_reference_count,
            imported_names_table_offset,
            entry_table_offset,
            nonresident_names_offset,
            &segments,
        )?;
        let resources = parse_resources(
            &reader,
            ne,
            resource_table_offset,
            &[
                resident_names_offset,
                module_reference_table_offset,
                imported_names_table_offset,
                entry_table_offset,
                reader.u16(ne + 0x22)?,
            ],
        )?;
        validate_internal_relocations(&segments, &entry_points)?;
        Ok(Self {
            ne_offset,
            flags,
            auto_data_segment,
            heap_size,
            stack_size,
            initial_cs,
            initial_ip,
            initial_ss,
            initial_sp,
            alignment_shift,
            segments,
            entry_points,
            module_references,
            imported_names,
            resources,
        })
    }
}

impl NeLoadedImage {
    /// An independent memory snapshot retaining selector alias relationships.
    pub fn deep_clone(&self) -> Self {
        Self {
            selectors: self.selectors.clone(),
            storage: self.storage.iter().map(|s| Rc::new(RefCell::new(s.borrow().clone()))).collect(),
            next_allocation_id: self.next_allocation_id,
        }
    }
    /// Add owned task or host memory that does not come from an NE segment,
    /// such as a PSP. The selector uses the same checked access path as NE
    /// segments and is initialized to zero.
    pub fn add_region(
        &mut self,
        selector: u16,
        limit: u32,
        writable: bool,
        executable: bool,
    ) -> Result<(), NeError> {
        if selector == 0 {
            return Err(NeError::NullSelectorAllocation);
        }
        if limit > 0x1_0000 {
            return Err(NeError::SelectorAllocationTooLarge { limit });
        }
        if self
            .selectors
            .iter()
            .any(|entry| entry.selector == selector)
        {
            return Err(NeError::DuplicateSelector { selector });
        }
        let storage_index = self.storage.len();
        let allocation_id = self.next_allocation_id;
        self.next_allocation_id = self
            .next_allocation_id
            .checked_add(1)
            .ok_or(NeError::SelectorSpaceExhausted)?;
        self.storage
            .push(Rc::new(RefCell::new(vec![0; limit as usize])));
        self.selectors.push(NeSelector {
            selector,
            segment: 0,
            limit,
            writable,
            executable,
            storage_index,
            allocation_id,
        });
        Ok(())
    }

    /// Allocate a task-owned selector after the highest selector already in
    /// use. LDT selectors advance in eight-byte descriptor slots.
    pub fn add_region_auto(
        &mut self,
        limit: u32,
        writable: bool,
        executable: bool,
    ) -> Result<u16, NeError> {
        let selector = match self.selectors.iter().map(|entry| entry.selector).max() {
            Some(last) => last.checked_add(8).ok_or(NeError::SelectorSpaceExhausted)?,
            None => 8,
        };
        self.add_region(selector, limit, writable, executable)?;
        Ok(selector)
    }

    /// Remove a selector mapping and release its backing bytes when no alias
    /// still refers to them.
    pub fn remove_region(&mut self, selector: u16) -> Result<(), NeError> {
        let index = self
            .selectors
            .iter()
            .position(|entry| entry.selector == selector)
            .ok_or(NeError::MissingSelector { selector })?;
        let storage_index = self.selectors[index].storage_index;
        self.selectors.remove(index);
        if !self
            .selectors
            .iter()
            .any(|entry| entry.storage_index == storage_index)
        {
            self.storage.remove(storage_index);
            for entry in &mut self.selectors {
                if entry.storage_index > storage_index {
                    entry.storage_index -= 1;
                }
            }
        }
        Ok(())
    }

    /// Decode a Win16 resource ID argument: a null selector encodes an
    /// integer resource ID, while a non-null selector points to a C string.
    pub fn resource_identifier_from_pointer(
        &self,
        pointer: u32,
    ) -> Result<NeResourceIdentifier, NeError> {
        let selector = (pointer >> 16) as u16;
        let offset = pointer as u16;
        if selector == 0 {
            return Ok(NeResourceIdentifier::Ordinal(offset & 0x7fff));
        }
        let descriptor = self
            .selectors
            .iter()
            .find(|descriptor| descriptor.selector == selector)
            .ok_or(NeError::MissingSelector { selector })?;
        let remaining = descriptor.limit.checked_sub(u32::from(offset)).ok_or(
            NeError::SelectorLimitExceeded {
                selector,
                offset: u32::from(offset),
                end: u32::from(offset),
                limit: descriptor.limit,
            },
        )?;
        let bytes = self.read(selector, u32::from(offset), remaining, NeAccess::Read)?;
        let Some(end) = bytes.iter().position(|byte| *byte == 0) else {
            return Err(NeError::UnterminatedResourceIdentifier { selector, offset });
        };
        Ok(NeResourceIdentifier::Name(bytes[..end].to_vec()))
    }

    /// Fetch and decode one Win16 instruction through the checked selector
    /// map. A segment with at least 64 KiB wraps instruction fetch at
    /// IP=0xffff; smaller segments stop at their allocation limit.
    pub fn decode_next(
        &self,
        decoder: &mut rsleigh_api::Decoder,
        cs: u16,
        ip: u16,
    ) -> Result<NeDecodedInstruction, NeError> {
        if decoder.architecture() != rsleigh_api::Architecture::X86_16Protected {
            return Err(NeError::WrongDecoderArchitecture);
        }
        let selector = self
            .selectors
            .iter()
            .find(|entry| entry.selector == cs)
            .ok_or(NeError::MissingSelector { selector: cs })?;
        let mut bytes = Vec::with_capacity(X86_MAX_INSTRUCTION_SIZE);
        for index in 0..X86_MAX_INSTRUCTION_SIZE {
            let offset = u32::from(ip).wrapping_add(index as u32);
            let offset = if offset > u16::MAX as u32 && selector.limit >= 0x1_0000 {
                offset & 0xffff
            } else {
                offset
            };
            if offset >= selector.limit {
                break;
            }
            let byte = self.read(cs, offset, 1, NeAccess::Execute)?;
            bytes.push(byte[0]);
        }
        let address = (u64::from(cs) << 16) | u64::from(ip);
        let instruction =
            decoder
                .decode(&bytes, address)
                .map_err(|_| NeError::InstructionDecodeFailed {
                    selector: cs,
                    offset: ip,
                })?;
        let length =
            u16::try_from(instruction.len).map_err(|_| NeError::InstructionDecodeFailed {
                selector: cs,
                offset: ip,
            })?;
        if length == 0 || usize::from(length) > bytes.len() {
            return Err(NeError::InstructionDecodeFailed {
                selector: cs,
                offset: ip,
            });
        }
        Ok(NeDecodedInstruction {
            cs,
            ip,
            next_ip: ip.wrapping_add(length),
            instruction,
        })
    }

    /// Apply internal NE fixups and imports resolved by the caller. The
    /// resolver is called only for imported ordinal/name records; returning
    /// `None` makes loading fail rather than installing a guessed address.
    pub fn apply_relocations<F>(
        &mut self,
        image: &NeImage,
        mut resolve_import: F,
    ) -> Result<(), NeError>
    where
        F: FnMut(&NeRelocation) -> Option<NeFarAddress>,
    {
        // Stage writes so a malformed or unresolved later relocation cannot
        // leave a partially relocated image behind.
        let mut staged: Vec<Vec<u8>> = self
            .storage
            .iter()
            .map(|storage| storage.borrow().clone())
            .collect();
        for source in &image.segments {
            let source_selector =
                self.selector_for_segment(source.index)
                    .ok_or(NeError::MissingSegment {
                        segment: source.index,
                    })?;
            for relocation in &source.relocations {
                let target_kind = relocation.flags & 0x03;
                if target_kind == 3 {
                    // OSFIXUP records are ignored by Windows-compatible
                    // loaders that do not provide the historical x87 patch.
                    continue;
                }
                let target = if target_kind == 0 {
                    if relocation.target1 & 0x00ff == 0x00ff {
                        let entry = image
                            .entry_points
                            .iter()
                            .find(|entry| entry.ordinal == relocation.target2)
                            .ok_or(NeError::InvalidRelocationEntryOrdinal {
                                source_segment: source.index,
                                ordinal: relocation.target2,
                            })?;
                        NeFarAddress {
                            selector: self.selector_for_segment(entry.segment).ok_or(
                                NeError::MissingSegment {
                                    segment: entry.segment,
                                },
                            )?,
                            offset: entry.offset,
                        }
                    } else {
                        let segment = relocation.target1;
                        if segment == 0 {
                            return Err(NeError::InvalidRelocationSegment {
                                source_segment: source.index,
                                target_segment: segment,
                            });
                        }
                        NeFarAddress {
                            selector: self.selector_for_segment(segment).ok_or(
                                NeError::InvalidRelocationSegment {
                                    source_segment: source.index,
                                    target_segment: segment,
                                },
                            )?,
                            offset: relocation.target2,
                        }
                    }
                } else {
                    resolve_import(relocation).ok_or(NeError::UnresolvedRelocationTarget {
                        segment: source.index,
                    })?
                };
                let mapped_target = self
                    .selectors
                    .iter()
                    .find(|selector| selector.selector == target.selector)
                    .is_some_and(|selector| u32::from(target.offset) < selector.limit);
                if !mapped_target {
                    return Err(NeError::InvalidRelocationAddress {
                        selector: target.selector,
                        offset: target.offset,
                    });
                }

                let source_offsets = if relocation.flags & RELOC_ADDITIVE != 0 {
                    std::slice::from_ref(&relocation.source_offset)
                } else {
                    &relocation.chain_offsets
                };
                let source_descriptor = self
                    .selectors
                    .iter()
                    .find(|selector| selector.selector == source_selector)
                    .expect("segment selector was resolved");
                let source_index = source_descriptor.storage_index;
                let bytes = &mut staged[source_index];
                for &offset in source_offsets {
                    let offset = usize::from(offset);
                    match relocation.source_type {
                        0 => {
                            let value = bytes[offset];
                            bytes[offset] = if relocation.flags & RELOC_ADDITIVE != 0 {
                                value.wrapping_add(target.offset as u8)
                            } else {
                                target.offset as u8
                            };
                        }
                        2 => {
                            let value = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                            let patched = if relocation.flags & RELOC_ADDITIVE != 0 {
                                value.wrapping_add(target.selector)
                            } else {
                                target.selector
                            };
                            bytes[offset..offset + 2].copy_from_slice(&patched.to_le_bytes());
                        }
                        3 => {
                            let old_offset = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                            let patched_offset = if relocation.flags & RELOC_ADDITIVE != 0 {
                                old_offset.wrapping_add(target.offset)
                            } else {
                                target.offset
                            };
                            let old_selector =
                                u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]);
                            let patched_selector = if relocation.flags & RELOC_ADDITIVE != 0 {
                                old_selector.wrapping_add(target.selector)
                            } else {
                                target.selector
                            };
                            bytes[offset..offset + 2]
                                .copy_from_slice(&patched_offset.to_le_bytes());
                            bytes[offset + 2..offset + 4]
                                .copy_from_slice(&patched_selector.to_le_bytes());
                        }
                        5 => {
                            let old_offset = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                            let patched = if relocation.flags & RELOC_ADDITIVE != 0 {
                                old_offset.wrapping_add(target.offset)
                            } else {
                                target.offset
                            };
                            bytes[offset..offset + 2].copy_from_slice(&patched.to_le_bytes());
                        }
                        source_type => {
                            return Err(NeError::UnsupportedRelocationApplication {
                                segment: source.index,
                                source_type,
                            });
                        }
                    }
                }
            }
        }
        for (storage, bytes) in self.storage.iter().zip(staged) {
            *storage.borrow_mut() = bytes;
        }
        Ok(())
    }

    /// Apply all fixups using the provided explicit export bindings.
    pub fn apply_imports(
        &mut self,
        image: &NeImage,
        resolver: &NeImportResolver,
    ) -> Result<(), NeError> {
        for source in &image.segments {
            for relocation in &source.relocations {
                if !matches!(relocation.flags & 0x03, 1 | 2) {
                    continue;
                }
                let Some(address) = resolver.resolve(image, relocation) else {
                    continue;
                };
                let mapped = self
                    .selectors
                    .iter()
                    .find(|selector| selector.selector == address.selector)
                    .is_some_and(|selector| u32::from(address.offset) < selector.limit);
                if !mapped {
                    return Err(NeError::InvalidImportedAddress {
                        selector: address.selector,
                        offset: address.offset,
                    });
                }
            }
        }
        self.apply_relocations(image, |relocation| resolver.resolve(image, relocation))
    }

    /// Add a selector alias with the same bounds and permissions as an
    /// existing selector. Both descriptors refer to the same backing bytes.
    pub fn add_alias(&mut self, existing: u16, alias: u16) -> Result<(), NeError> {
        if self.selectors.iter().any(|s| s.selector == alias) {
            return Err(NeError::DuplicateSelector { selector: alias });
        }
        let mut descriptor = self
            .selectors
            .iter()
            .find(|s| s.selector == existing)
            .cloned()
            .ok_or(NeError::MissingSelector { selector: existing })?;
        descriptor.selector = alias;
        self.selectors.push(descriptor);
        Ok(())
    }

    pub fn selector_for_segment(&self, segment: u16) -> Option<u16> {
        self.selectors
            .iter()
            .find(|s| s.segment == segment)
            .map(|s| s.selector)
    }

    pub fn translate(
        &self,
        selector: u16,
        offset: u32,
        length: u32,
        access: NeAccess,
    ) -> Result<std::ops::Range<usize>, NeError> {
        let descriptor = self
            .selectors
            .iter()
            .find(|s| s.selector == selector)
            .ok_or(NeError::MissingSelector { selector })?;
        if (access == NeAccess::Write && !descriptor.writable)
            || (access == NeAccess::Execute && !descriptor.executable)
        {
            return Err(NeError::SelectorAccessDenied { selector, access });
        }
        let end = offset
            .checked_add(length)
            .ok_or(NeError::SelectorLimitExceeded {
                selector,
                offset,
                end: u32::MAX,
                limit: descriptor.limit,
            })?;
        if end > descriptor.limit {
            return Err(NeError::SelectorLimitExceeded {
                selector,
                offset,
                end,
                limit: descriptor.limit,
            });
        }
        Ok(offset as usize..end as usize)
    }

    pub fn read(
        &self,
        selector: u16,
        offset: u32,
        length: u32,
        access: NeAccess,
    ) -> Result<Vec<u8>, NeError> {
        let range = self.translate(selector, offset, length, access)?;
        let descriptor = self
            .selectors
            .iter()
            .find(|s| s.selector == selector)
            .expect("translate resolved selector");
        Ok(self.storage[descriptor.storage_index].borrow()[range].to_vec())
    }

    pub fn write(&mut self, selector: u16, offset: u32, data: &[u8]) -> Result<(), NeError> {
        let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let range = self.translate(selector, offset, length, NeAccess::Write)?;
        let descriptor = self
            .selectors
            .iter()
            .find(|s| s.selector == selector)
            .expect("translate resolved selector");
        self.storage[descriptor.storage_index].borrow_mut()[range].copy_from_slice(data);
        Ok(())
    }
}

impl NeResourceManager {
    /// Find a resource by exact NE type and name identifiers, returning a
    /// stable Win16 HRSRC-like handle.
    pub fn find_resource(
        &mut self,
        image: &NeImage,
        resource_type: &NeResourceIdentifier,
        name: &NeResourceIdentifier,
    ) -> Result<Option<NeResourceInfoHandle>, NeError> {
        let Some(resource_index) = image.resources.iter().position(|resource| {
            resource.resource_type.matches(resource_type) && resource.name.matches(name)
        }) else {
            return Ok(None);
        };
        if let Some(state) = self
            .resources
            .iter()
            .find(|state| state.resource_index == resource_index)
        {
            return Ok(Some(state.info_handle));
        }
        let handle = self.next_info_handle.max(1);
        if handle >= 0x8000 {
            return Err(NeError::ResourceHandleSpaceExhausted);
        }
        let Some(next) = handle.checked_add(1) else {
            return Err(NeError::ResourceHandleSpaceExhausted);
        };
        self.next_info_handle = next;
        let info_handle = NeResourceInfoHandle(handle);
        self.resources.push(NeResourceState {
            info_handle,
            resource_index,
            loaded_selector: None,
            loaded_allocation_id: None,
            global_handle: None,
            load_count: 0,
            lock_count: 0,
        });
        Ok(Some(info_handle))
    }

    /// Find a resource from Win16 pointer arguments (`MAKEINTRESOURCE` or
    /// selector:offset C strings) in name-then-type order.
    pub fn find_resource_from_pointers(
        &mut self,
        image: &NeImage,
        loaded: &NeLoadedImage,
        name: u32,
        resource_type: u32,
    ) -> Result<Option<NeResourceInfoHandle>, NeError> {
        let name = loaded.resource_identifier_from_pointer(name)?;
        let resource_type = loaded.resource_identifier_from_pointer(resource_type)?;
        self.find_resource(image, &resource_type, &name)
    }

    /// Return the size recorded for a resource without loading its bytes.
    pub fn size_of_resource(
        &self,
        image: &NeImage,
        handle: NeResourceInfoHandle,
    ) -> Result<u32, NeError> {
        let state = self.info_state(handle)?;
        image
            .resources
            .get(state.resource_index)
            .map(|resource| resource.size)
            .ok_or(NeError::InvalidResourceInfoHandle { handle: handle.0 })
    }

    /// Load a resource into selector-backed guest memory. Repeated loads of
    /// the same resource share its global handle and increment its use count.
    pub fn load_resource(
        &mut self,
        image: &NeImage,
        raw: &[u8],
        loaded: &mut NeLoadedImage,
        handle: NeResourceInfoHandle,
    ) -> Result<Option<NeGlobalResourceHandle>, NeError> {
        let state_index = self
            .resources
            .iter()
            .position(|state| state.info_handle == handle)
            .ok_or(NeError::InvalidResourceInfoHandle { handle: handle.0 })?;
        if let Some(selector) = self.resources[state_index].loaded_selector {
            if loaded.selectors.iter().any(|mapped| {
                mapped.selector == selector
                    && Some(mapped.allocation_id)
                        == self.resources[state_index].loaded_allocation_id
            }) {
                self.resources[state_index].load_count = self.resources[state_index]
                    .load_count
                    .checked_add(1)
                    .ok_or(NeError::ResourceUsageOverflow)?;
                return self.resources[state_index]
                    .global_handle
                    .map(Some)
                    .ok_or(NeError::InvalidGlobalResourceHandle { handle: selector });
            }
            if let Some(global_handle) = self.resources[state_index].global_handle.take() {
                self.free_global_handles.push(global_handle.0);
            }
            self.resources[state_index].loaded_selector = None;
            self.resources[state_index].loaded_allocation_id = None;
            self.resources[state_index].load_count = 0;
            self.resources[state_index].lock_count = 0;
        }

        let resource = image
            .resources
            .get(self.resources[state_index].resource_index)
            .ok_or(NeError::InvalidResourceInfoHandle { handle: handle.0 })?;
        let data = image.resource_data(raw, resource)?;
        let selector = loaded.add_region_auto(resource.size, true, false)?;
        let allocation_id = loaded
            .selectors
            .iter()
            .find(|mapped| mapped.selector == selector)
            .ok_or(NeError::MissingSelector { selector })?
            .allocation_id;
        if let Err(error) = loaded.write(selector, 0, data) {
            let _ = loaded.remove_region(selector);
            return Err(error);
        }
        let global_handle = match self.allocate_global_handle() {
            Ok(handle) => handle,
            Err(error) => {
                let _ = loaded.remove_region(selector);
                return Err(error);
            }
        };
        self.resources[state_index].loaded_selector = Some(selector);
        self.resources[state_index].loaded_allocation_id = Some(allocation_id);
        self.resources[state_index].global_handle = Some(global_handle);
        self.resources[state_index].load_count = 1;
        Ok(Some(global_handle))
    }

    /// Lock a loaded resource and return its selector:0 far pointer.
    pub fn lock_resource(
        &mut self,
        loaded: &NeLoadedImage,
        handle: NeGlobalResourceHandle,
    ) -> Result<Option<NeFarAddress>, NeError> {
        let Some(state) = self
            .resources
            .iter_mut()
            .find(|state| state.global_handle == Some(handle) && state.load_count != 0)
        else {
            return Err(NeError::InvalidGlobalResourceHandle { handle: handle.0 });
        };
        let selector = state
            .loaded_selector
            .ok_or(NeError::InvalidGlobalResourceHandle { handle: handle.0 })?;
        if !loaded.selectors.iter().any(|mapped| {
            mapped.selector == selector && Some(mapped.allocation_id) == state.loaded_allocation_id
        }) {
            return Err(NeError::InvalidGlobalResourceHandle { handle: handle.0 });
        }
        state.lock_count = state
            .lock_count
            .checked_add(1)
            .ok_or(NeError::ResourceUsageOverflow)?;
        Ok(Some(NeFarAddress {
            selector,
            offset: 0,
        }))
    }

    /// Release one lock. Matches Win16 GlobalUnlock: return true only when
    /// the block remains locked after the count is decremented.
    pub fn unlock_resource(&mut self, handle: NeGlobalResourceHandle) -> bool {
        let Some(state) = self
            .resources
            .iter_mut()
            .find(|state| state.global_handle == Some(handle))
        else {
            return false;
        };
        if state.lock_count == 0 {
            return false;
        }
        state.lock_count -= 1;
        state.lock_count != 0
    }

    /// Release one load reference and discard its guest selector when no
    /// resource locks or load references remain.
    pub fn free_resource(
        &mut self,
        loaded: &mut NeLoadedImage,
        handle: NeGlobalResourceHandle,
    ) -> Result<bool, NeError> {
        let Some(index) = self
            .resources
            .iter()
            .position(|state| state.global_handle == Some(handle) && state.load_count != 0)
        else {
            return Err(NeError::InvalidGlobalResourceHandle { handle: handle.0 });
        };
        if self.resources[index].lock_count != 0 {
            return Ok(false);
        }
        if self.resources[index].load_count == 1 {
            let selector = self.resources[index]
                .loaded_selector
                .ok_or(NeError::InvalidGlobalResourceHandle { handle: handle.0 })?;
            let allocation_id = self.resources[index].loaded_allocation_id;
            if !loaded.selectors.iter().any(|mapped| {
                mapped.selector == selector && Some(mapped.allocation_id) == allocation_id
            }) {
                return Err(NeError::InvalidGlobalResourceHandle { handle: handle.0 });
            }
            loaded.remove_region(selector)?;
            self.resources[index].load_count = 0;
            self.resources[index].loaded_selector = None;
            self.resources[index].loaded_allocation_id = None;
            self.resources[index].global_handle = None;
            self.free_global_handles.push(handle.0);
        } else {
            self.resources[index].load_count -= 1;
        }
        Ok(true)
    }

    fn allocate_global_handle(&mut self) -> Result<NeGlobalResourceHandle, NeError> {
        if let Some(handle) = self.free_global_handles.pop() {
            return Ok(NeGlobalResourceHandle(handle));
        }
        let handle = self.next_global_handle.max(0x8000);
        let next = handle
            .checked_add(1)
            .ok_or(NeError::ResourceHandleSpaceExhausted)?;
        if self.resources.iter().any(|state| {
            state
                .global_handle
                .is_some_and(|existing| existing.0 == handle)
        }) {
            return Err(NeError::ResourceHandleSpaceExhausted);
        }
        self.next_global_handle = next;
        Ok(NeGlobalResourceHandle(handle))
    }

    fn info_state(&self, handle: NeResourceInfoHandle) -> Result<&NeResourceState, NeError> {
        self.resources
            .iter()
            .find(|state| state.info_handle == handle)
            .ok_or(NeError::InvalidResourceInfoHandle { handle: handle.0 })
    }
}

fn validate_internal_relocations(
    segments: &[NeSegment],
    entry_points: &[NeEntryPoint],
) -> Result<(), NeError> {
    for source in segments {
        for relocation in &source.relocations {
            if relocation.flags & 0x03 != 0 {
                continue;
            }
            if relocation.target1 & 0x00ff == 0x00ff {
                if !entry_points
                    .iter()
                    .any(|entry| entry.ordinal == relocation.target2)
                {
                    return Err(NeError::InvalidRelocationEntryOrdinal {
                        source_segment: source.index,
                        ordinal: relocation.target2,
                    });
                }
                continue;
            }
            let target_segment = relocation.target1;
            let Some(target) = segments
                .get(usize::from(target_segment.saturating_sub(1)))
                .filter(|_| target_segment != 0)
            else {
                return Err(NeError::InvalidRelocationSegment {
                    source_segment: source.index,
                    target_segment,
                });
            };
            let target_size = target.minimum_allocation.max(target.file_size);
            if u32::from(relocation.target2) >= target_size {
                return Err(NeError::RelocationTargetOffsetOutOfBounds {
                    source_segment: source.index,
                    target_segment,
                    target_offset: relocation.target2,
                    target_size,
                });
            }
        }
    }
    Ok(())
}

fn parse_import_tables(
    reader: &Reader<'_>,
    ne_offset: u64,
    module_table_offset: u16,
    module_count: u16,
    names_table_offset: u16,
    entry_table_offset: u16,
    nonresident_names_offset: u32,
    segments: &[NeSegment],
) -> Result<(Vec<NeModuleReference>, Vec<NeImportedName>), NeError> {
    let module_table = ne_offset + u64::from(module_table_offset);
    if module_count > 0 && u64::from(module_table_offset) < NE_HEADER_SIZE as u64 {
        return Err(NeError::MalformedImportedName {
            offset: module_table,
        });
    }
    reader.bytes(module_table, u64::from(module_count) * 2)?;

    let names_start = ne_offset + u64::from(names_table_offset);
    let mut names_end = reader.0.len() as u64;
    let entry_start = ne_offset + u64::from(entry_table_offset);
    if entry_table_offset >= names_table_offset && entry_table_offset != 0 {
        names_end = names_end.min(entry_start);
    }
    let nonresident_start = u64::from(nonresident_names_offset);
    if nonresident_names_offset != 0 && nonresident_start > names_start {
        names_end = names_end.min(nonresident_start);
    }
    if (module_count > 0
        || segments
            .iter()
            .flat_map(|s| &s.relocations)
            .any(|reloc| reloc.flags & 0x03 == 2))
        && u64::from(names_table_offset) < NE_HEADER_SIZE as u64
    {
        return Err(NeError::MalformedImportedName {
            offset: names_start,
        });
    }
    if names_end < names_start {
        return Err(NeError::MalformedImportedName {
            offset: names_start,
        });
    }

    let mut required_offsets = std::collections::BTreeSet::new();
    let mut module_name_offsets = Vec::with_capacity(module_count as usize);
    for i in 0..module_count {
        let name_offset = reader.u16(module_table + u64::from(i) * 2)?;
        module_name_offsets.push(name_offset);
        required_offsets.insert(name_offset);
    }
    for segment in segments {
        for relocation in &segment.relocations {
            match relocation.flags & 0x03 {
                1 | 2 if relocation.target1 == 0 || relocation.target1 > module_count => {
                    return Err(NeError::InvalidModuleReference {
                        segment: segment.index,
                        module_index: relocation.target1,
                        module_count,
                    });
                }
                2 => {
                    required_offsets.insert(relocation.target2);
                }
                _ => {}
            }
        }
    }

    let mut imported_names = Vec::with_capacity(required_offsets.len());
    for offset in required_offsets {
        imported_names.push(read_imported_name(reader, names_start, names_end, offset)?);
    }
    let module_references = module_name_offsets
        .into_iter()
        .enumerate()
        .map(|(index, imported_name_offset)| {
            let name = imported_names
                .iter()
                .find(|name| name.offset == imported_name_offset)
                .expect("module name offset was collected")
                .bytes
                .clone();
            NeModuleReference {
                index: index as u16,
                imported_name_offset,
                name,
            }
        })
        .collect();
    Ok((module_references, imported_names))
}

fn parse_resources(
    reader: &Reader<'_>,
    ne_offset: u64,
    table_offset: u16,
    following_table_offsets: &[u16],
) -> Result<Vec<NeResource>, NeError> {
    if table_offset == 0 {
        return Ok(Vec::new());
    }
    let table_start = ne_offset + u64::from(table_offset);
    let table_end = following_table_offsets
        .iter()
        .copied()
        .filter(|offset| *offset > table_offset)
        .min()
        .map(|offset| ne_offset + u64::from(offset))
        .unwrap_or(reader.0.len() as u64);
    if table_end <= table_start {
        return Err(NeError::MalformedResourceTable {
            offset: table_start,
        });
    }
    reader
        .bytes(table_start, table_end - table_start)
        .map_err(|_| NeError::MalformedResourceTable {
            offset: table_start,
        })?;
    let shift =
        u32::from(
            reader
                .u16(table_start)
                .map_err(|_| NeError::MalformedResourceTable {
                    offset: table_start,
                })?,
        );
    if shift > 31 {
        return Err(NeError::MalformedResourceTable {
            offset: table_start,
        });
    }

    let mut cursor = table_start + 2;
    let mut resources = Vec::new();
    loop {
        if cursor + 2 > table_end {
            return Err(NeError::MalformedResourceTable { offset: cursor });
        }
        let type_id = reader
            .u16(cursor)
            .map_err(|_| NeError::MalformedResourceTable { offset: cursor })?;
        cursor += 2;
        if type_id == 0 {
            break;
        }
        if cursor + 6 > table_end {
            return Err(NeError::MalformedResourceTable { offset: cursor });
        }
        let count = reader
            .u16(cursor)
            .map_err(|_| NeError::MalformedResourceTable { offset: cursor })?;
        cursor += 6; // count and four reserved bytes
        let resource_type = parse_resource_identifier(reader, table_start, table_end, type_id)?;

        for _ in 0..count {
            if cursor + 12 > table_end {
                return Err(NeError::MalformedResourceTable { offset: cursor });
            }
            let offset_units = reader
                .u16(cursor)
                .map_err(|_| NeError::MalformedResourceTable { offset: cursor })?;
            let size_units = u64::from(
                reader
                    .u16(cursor + 2)
                    .map_err(|_| NeError::MalformedResourceTable { offset: cursor + 2 })?,
            );
            let flags = reader
                .u16(cursor + 4)
                .map_err(|_| NeError::MalformedResourceTable { offset: cursor + 4 })?;
            let name_id = reader
                .u16(cursor + 6)
                .map_err(|_| NeError::MalformedResourceTable { offset: cursor + 6 })?;
            let name = parse_resource_identifier(reader, table_start, table_end, name_id)?;
            let file_offset = u64::from(offset_units)
                .checked_shl(shift)
                .and_then(|offset| u32::try_from(offset).ok())
                .ok_or(NeError::MalformedResourceTable { offset: cursor })?;
            let size = size_units
                .checked_shl(shift)
                .and_then(|size| u32::try_from(size).ok())
                .ok_or(NeError::MalformedResourceTable { offset: cursor + 2 })?;
            reader
                .bytes(u64::from(file_offset), u64::from(size))
                .map_err(|_| NeError::OutOfBounds {
                    offset: u64::from(file_offset),
                    length: u64::from(size),
                })?;
            resources.push(NeResource {
                resource_type: resource_type.clone(),
                name,
                flags,
                file_offset,
                size,
            });
            cursor += 12;
        }
    }
    Ok(resources)
}

fn parse_resource_identifier(
    reader: &Reader<'_>,
    table_start: u64,
    table_end: u64,
    encoded: u16,
) -> Result<NeResourceIdentifier, NeError> {
    if encoded & 0x8000 != 0 {
        return Ok(NeResourceIdentifier::Ordinal(encoded & 0x7fff));
    }
    let start = table_start
        .checked_add(u64::from(encoded))
        .filter(|start| *start < table_end)
        .ok_or(NeError::MalformedResourceTable {
            offset: table_start + u64::from(encoded),
        })?;
    let length = u64::from(
        reader
            .bytes(start, 1)
            .map_err(|_| NeError::MalformedResourceTable { offset: start })?[0],
    );
    let name_start = start + 1;
    let name_end = name_start
        .checked_add(length)
        .filter(|end| *end <= table_end)
        .ok_or(NeError::MalformedResourceTable { offset: start })?;
    Ok(NeResourceIdentifier::Name(
        reader
            .bytes(name_start, name_end - name_start)
            .map_err(|_| NeError::MalformedResourceTable { offset: start })?
            .to_vec(),
    ))
}

fn read_imported_name(
    reader: &Reader<'_>,
    table_start: u64,
    table_end: u64,
    offset: u16,
) -> Result<NeImportedName, NeError> {
    let start = table_start + u64::from(offset);
    if start >= table_end {
        return Err(NeError::MalformedImportedName { offset: start });
    }
    let length = u64::from(reader.bytes(start, 1)?[0]);
    let name_start = start + 1;
    if name_start
        .checked_add(length)
        .is_none_or(|end| end > table_end)
    {
        return Err(NeError::MalformedImportedName { offset: start });
    }
    Ok(NeImportedName {
        offset,
        bytes: reader.bytes(name_start, length)?.to_vec(),
    })
}

fn parse_entry_table(
    reader: &Reader<'_>,
    ne_offset: u64,
    table_offset: u16,
    table_size: u16,
    segments: &[NeSegment],
) -> Result<Vec<NeEntryPoint>, NeError> {
    if table_size == 0 {
        return Ok(Vec::new());
    }
    if u64::from(table_offset) < NE_HEADER_SIZE as u64 {
        return Err(NeError::MalformedEntryTable {
            offset: ne_offset + u64::from(table_offset),
        });
    }
    let start = ne_offset + u64::from(table_offset);
    let table = reader.bytes(start, u64::from(table_size))?;
    let mut cursor = 0usize;
    let mut ordinal = 1u32;
    let mut entries = Vec::new();

    loop {
        let Some(&count) = table.get(cursor) else {
            return Err(NeError::MalformedEntryTable {
                offset: start + cursor as u64,
            });
        };
        cursor += 1;
        if count == 0 {
            break;
        }
        let Some(&segment_indicator) = table.get(cursor) else {
            return Err(NeError::MalformedEntryTable {
                offset: start + cursor as u64,
            });
        };
        cursor += 1;

        if segment_indicator == 0 {
            ordinal = ordinal
                .checked_add(u32::from(count))
                .ok_or(NeError::EntryOrdinalOverflow)?;
            continue;
        }

        for _ in 0..count {
            let (segment, offset, flags, movable) = if segment_indicator == 0xff {
                let Some(data) = table.get(cursor..cursor + 6) else {
                    return Err(NeError::MalformedEntryTable {
                        offset: start + cursor as u64,
                    });
                };
                if data[1] != NE_INT_OPCODE || data[2] != NE_MOVABLE_ENTRY_MARKER {
                    return Err(NeError::MalformedEntryTable {
                        offset: start + cursor as u64 + 1,
                    });
                }
                let offset = u16::from_le_bytes([data[4], data[5]]);
                (u16::from(data[3]), offset, data[0], true)
            } else {
                let Some(data) = table.get(cursor..cursor + 3) else {
                    return Err(NeError::MalformedEntryTable {
                        offset: start + cursor as u64,
                    });
                };
                let offset = u16::from_le_bytes([data[1], data[2]]);
                (u16::from(segment_indicator), offset, data[0], false)
            };
            cursor += if movable { 6 } else { 3 };
            let ordinal16 = u16::try_from(ordinal).map_err(|_| NeError::EntryOrdinalOverflow)?;
            let Some(target_segment) = segments
                .get(usize::from(segment.saturating_sub(1)))
                .filter(|_| segment != 0)
            else {
                return Err(NeError::EntrySegmentOutOfRange {
                    ordinal: ordinal16,
                    segment,
                });
            };
            let allocation = target_segment
                .minimum_allocation
                .max(target_segment.file_size);
            if u32::from(offset) >= allocation {
                return Err(NeError::MalformedEntryTable {
                    offset: start + cursor as u64,
                });
            }
            entries.push(NeEntryPoint {
                ordinal: ordinal16,
                segment,
                offset,
                flags,
                movable,
            });
            ordinal = ordinal
                .checked_add(1)
                .ok_or(NeError::EntryOrdinalOverflow)?;
        }
    }
    Ok(entries)
}

fn parse_relocations(
    reader: &Reader<'_>,
    segment: u16,
    data_offset: u64,
    data_size: u32,
    table_offset: u64,
) -> Result<Vec<NeRelocation>, NeError> {
    let count = reader.u16(table_offset)?;
    let records_start = table_offset + 2;
    reader.bytes(records_start, u64::from(count) * 8)?;
    let mut relocations = Vec::with_capacity(count as usize);
    for i in 0..count {
        let record = records_start + u64::from(i) * 8;
        let source_type = reader.bytes(record, 1)?[0] & 0x7f;
        let flags = reader.bytes(record + 1, 1)?[0];
        let source_offset = reader.u16(record + 2)?;
        let target1 = reader.u16(record + 4)?;
        let target2 = reader.u16(record + 6)?;
        let width = match source_type {
            0 => 1,
            2 | 5 => 2,
            3 | 13 => 4,
            11 => 6,
            _ => {
                return Err(NeError::InvalidRelocationSourceType {
                    segment,
                    source_type,
                })
            }
        };
        ensure_source_range(segment, source_offset, width, data_size)?;

        let mut chain_offsets = Vec::new();
        if flags & RELOC_ADDITIVE == 0 {
            let mut current = source_offset;
            let mut seen = std::collections::BTreeSet::new();
            loop {
                ensure_source_range(segment, current, width.max(2), data_size)?;
                if !seen.insert(current) {
                    return Err(NeError::RelocationChainCycle {
                        segment,
                        offset: current,
                    });
                }
                chain_offsets.push(current);
                let link = data_offset + u64::from(current);
                let next = reader.u16(link)?;
                if next == RELOC_CHAIN_END || next == current {
                    break;
                }
                current = next;
            }
        }
        relocations.push(NeRelocation {
            source_type,
            flags,
            source_offset,
            target1,
            target2,
            chain_offsets,
        });
    }
    Ok(relocations)
}

fn ensure_source_range(
    segment: u16,
    offset: u16,
    width: u8,
    data_size: u32,
) -> Result<(), NeError> {
    if u32::from(offset) + u32::from(width) > data_size {
        return Err(NeError::RelocationSourceOutOfBounds {
            segment,
            offset,
            width,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_resource_lookup_ignores_case_and_shares_resource_handles() {
        let mut image=NeImage::parse(&fixture([0,0,0,0],0x148)).unwrap();
        image.resources.push(NeResource {
            resource_type:NeResourceIdentifier::Name(b"MENU".to_vec()),
            name:NeResourceIdentifier::Name(b"CASTLE".to_vec()),
            flags:0,file_offset:0,size:0,
        });
        let kind=NeResourceIdentifier::Name(b"Menu".to_vec());
        let name=NeResourceIdentifier::Name(b"Castle".to_vec());
        assert!(image.find_resource(&kind,&name).is_some());
        assert!(image.find_resource(&kind,&NeResourceIdentifier::Ordinal(1)).is_none());
        let mut manager=NeResourceManager::default();
        let mixed=manager.find_resource(&image,&kind,&name).unwrap().unwrap();
        let upper=manager.find_resource(&image,&image.resources[0].resource_type,&image.resources[0].name).unwrap().unwrap();
        assert_eq!(mixed,upper);
        assert_eq!(image.resources[0].name,NeResourceIdentifier::Name(b"CASTLE".to_vec()));
    }

    fn decode_on_large_stack(
        raw: Vec<u8>,
        first_selector: u16,
        cs: u16,
        ip: u16,
    ) -> Result<NeDecodedInstruction, NeError> {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let image = NeImage::parse(&raw)?;
                let loaded = image.load_segments(&raw, first_selector)?;
                let mut decoder =
                    rsleigh_api::Decoder::new(rsleigh_api::Architecture::X86_16Protected);
                loaded.decode_next(&mut decoder, cs, ip)
            })
            .unwrap()
            .join()
            .unwrap()
    }

    fn fixture(segment: [u16; 4], file_len: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; file_len.max(0x100 + 0x40 + 8)];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x100u32.to_le_bytes());
        let ne = 0x100;
        bytes[ne..ne + 2].copy_from_slice(b"NE");
        bytes[ne + 0x0e..ne + 0x10].copy_from_slice(&1u16.to_le_bytes());
        bytes[ne + 0x14..ne + 0x16].copy_from_slice(&0x1234u16.to_le_bytes());
        bytes[ne + 0x16..ne + 0x18].copy_from_slice(&2u16.to_le_bytes());
        bytes[ne + 0x18..ne + 0x1a].copy_from_slice(&0xff00u16.to_le_bytes());
        bytes[ne + 0x1a..ne + 0x1c].copy_from_slice(&3u16.to_le_bytes());
        bytes[ne + 0x1c..ne + 0x1e].copy_from_slice(&1u16.to_le_bytes());
        bytes[ne + 0x22..ne + 0x24].copy_from_slice(&0x40u16.to_le_bytes());
        bytes[ne + 0x32..ne + 0x34].copy_from_slice(&0u16.to_le_bytes());
        for (i, word) in segment.iter().enumerate() {
            let at = ne + 0x40 + i * 2;
            bytes[at..at + 2].copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn entry_fixture(table: &[u8]) -> Vec<u8> {
        let start = 0x148;
        let mut bytes = fixture([0, 0, 0, 0], start + table.len());
        bytes[0x104..0x106].copy_from_slice(&0x48u16.to_le_bytes());
        bytes[0x106..0x108].copy_from_slice(&(table.len() as u16).to_le_bytes());
        bytes[start..start + table.len()].copy_from_slice(table);
        bytes
    }

    fn module_fixture() -> Vec<u8> {
        let mut bytes = fixture([0, 0, 0, 0], 0x157);
        bytes[0x11e..0x120].copy_from_slice(&2u16.to_le_bytes());
        bytes[0x128..0x12a].copy_from_slice(&0x48u16.to_le_bytes());
        bytes[0x12a..0x12c].copy_from_slice(&0x4cu16.to_le_bytes());
        bytes[0x104..0x106].copy_from_slice(&0x56u16.to_le_bytes());
        bytes[0x106..0x108].copy_from_slice(&1u16.to_le_bytes());
        bytes[0x148..0x14a].copy_from_slice(&0u16.to_le_bytes());
        bytes[0x14a..0x14c].copy_from_slice(&5u16.to_le_bytes());
        bytes[0x14c..0x156]
            .copy_from_slice(&[4, b'K', b'E', b'R', b'N', 4, b'U', b'S', b'E', b'R']);
        bytes[0x156] = 0;
        bytes
    }

    fn relocation_fixture(flags: u8, first_link: u16, second_link: u16, source: u16) -> Vec<u8> {
        let mut bytes = fixture([0x148, 8, NE_RELOCINFO, 0x20], 0x15a);
        bytes[0x149..0x14b].copy_from_slice(&first_link.to_le_bytes());
        bytes[0x14c..0x14e].copy_from_slice(&second_link.to_le_bytes());
        bytes[0x150..0x152].copy_from_slice(&1u16.to_le_bytes());
        bytes[0x152] = 5;
        bytes[0x153] = flags;
        bytes[0x154..0x156].copy_from_slice(&source.to_le_bytes());
        bytes[0x156..0x158].copy_from_slice(&1u16.to_le_bytes());
        bytes[0x158..0x15a].copy_from_slice(&0x10u16.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_header_and_segment_metadata() {
        let image = NeImage::parse(&fixture([0x80, 4, 0x0001, 0x1000], 0x1000)).unwrap();
        assert_eq!((image.initial_cs, image.initial_ip), (2, 0x1234));
        assert_eq!((image.initial_ss, image.initial_sp), (3, 0xff00));
        assert_eq!(image.auto_data_segment, 1);
        assert_eq!(
            image.segments[0],
            NeSegment {
                index: 1,
                file_offset: Some(0x80),
                file_size: 4,
                minimum_allocation: 0x1000,
                flags: 0x0001,
                relocations: Vec::new(),
            }
        );
    }

    #[test]
    fn parses_relocation_records_and_source_chain() {
        let image = NeImage::parse(&relocation_fixture(0, 4, RELOC_CHAIN_END, 1)).unwrap();
        assert_eq!(
            image.segments[0].relocations,
            vec![NeRelocation {
                source_type: 5,
                flags: 0,
                source_offset: 1,
                target1: 1,
                target2: 0x10,
                chain_offsets: vec![1, 4],
            }]
        );
    }

    #[test]
    fn applies_internal_offset_fixups_to_every_chain_member() {
        let raw = relocation_fixture(0, 4, RELOC_CHAIN_END, 1);
        let image = NeImage::parse(&raw).unwrap();
        let mut loaded = image.load_segments(&raw, 0x28).unwrap();
        loaded.apply_relocations(&image, |_| None).unwrap();
        assert_eq!(loaded.read(0x28, 1, 2, NeAccess::Read).unwrap(), [0x10, 0]);
        assert_eq!(loaded.read(0x28, 4, 2, NeAccess::Read).unwrap(), [0x10, 0]);
    }

    #[test]
    fn resolves_movable_internal_relocation_ordinals() {
        let mut raw = relocation_fixture(0, RELOC_CHAIN_END, RELOC_CHAIN_END, 1);
        raw[0x152] = 3; // 32-bit far pointer
        raw[0x156..0x158].copy_from_slice(&0x00ffu16.to_le_bytes());
        raw[0x158..0x15a].copy_from_slice(&1u16.to_le_bytes());
        raw[0x104..0x106].copy_from_slice(&0x60u16.to_le_bytes());
        raw[0x106..0x108].copy_from_slice(&9u16.to_le_bytes());
        raw.resize(0x169, 0);
        raw[0x160..0x169].copy_from_slice(&[1, 0xff, 1, 0xcd, 0x3f, 1, 0x10, 0, 0]);

        let image = NeImage::parse(&raw).unwrap();
        let mut loaded = image.load_segments(&raw, 0x28).unwrap();
        loaded.apply_relocations(&image, |_| None).unwrap();
        assert_eq!(
            loaded.read(0x28, 1, 4, NeAccess::Read).unwrap(),
            [0x10, 0, 0x28, 0]
        );
    }

    #[test]
    fn additive_offset_fixups_add_the_inline_value_without_chain_walk() {
        let mut raw = relocation_fixture(RELOC_ADDITIVE, 5, 0, 1);
        raw[0x158..0x15a].copy_from_slice(&0x10u16.to_le_bytes());
        let image = NeImage::parse(&raw).unwrap();
        let mut loaded = image.load_segments(&raw, 0x28).unwrap();
        loaded.apply_relocations(&image, |_| None).unwrap();
        assert_eq!(loaded.read(0x28, 1, 2, NeAccess::Read).unwrap(), [0x15, 0]);
    }

    #[test]
    fn applies_selector_and_far_pointer_relocations() {
        let mut selector_raw = relocation_fixture(0, 4, RELOC_CHAIN_END, 1);
        selector_raw[0x152] = 2;
        let selector_image = NeImage::parse(&selector_raw).unwrap();
        let mut selector_loaded = selector_image.load_segments(&selector_raw, 0x28).unwrap();
        selector_loaded
            .apply_relocations(&selector_image, |_| None)
            .unwrap();
        assert_eq!(
            selector_loaded.read(0x28, 1, 2, NeAccess::Read).unwrap(),
            [0x28, 0]
        );
        assert_eq!(
            selector_loaded.read(0x28, 4, 2, NeAccess::Read).unwrap(),
            [0x28, 0]
        );

        let mut pointer_raw = relocation_fixture(0, 4, RELOC_CHAIN_END, 0);
        pointer_raw[0x148..0x14a].copy_from_slice(&4u16.to_le_bytes());
        pointer_raw[0x152] = 3;
        let pointer_image = NeImage::parse(&pointer_raw).unwrap();
        let mut pointer_loaded = pointer_image.load_segments(&pointer_raw, 0x28).unwrap();
        pointer_loaded
            .apply_relocations(&pointer_image, |_| None)
            .unwrap();
        assert_eq!(
            pointer_loaded.read(0x28, 0, 4, NeAccess::Read).unwrap(),
            [0x10, 0, 0x28, 0]
        );
        assert_eq!(
            pointer_loaded.read(0x28, 4, 4, NeAccess::Read).unwrap(),
            [0x10, 0, 0x28, 0]
        );
    }

    #[test]
    fn import_relocations_use_the_caller_resolver() {
        let mut raw = relocation_fixture(1, RELOC_CHAIN_END, RELOC_CHAIN_END, 1);
        raw[0x152] = 3;
        raw[0x156..0x158].copy_from_slice(&1u16.to_le_bytes());
        raw[0x158..0x15a].copy_from_slice(&7u16.to_le_bytes());
        raw[0x11e..0x120].copy_from_slice(&1u16.to_le_bytes());
        raw[0x128..0x12a].copy_from_slice(&0x5au16.to_le_bytes());
        raw[0x12a..0x12c].copy_from_slice(&0x5cu16.to_le_bytes());
        raw[0x104..0x106].copy_from_slice(&0x63u16.to_le_bytes());
        raw[0x106..0x108].copy_from_slice(&1u16.to_le_bytes());
        raw.resize(0x164, 0);
        raw[0x15a..0x15c].copy_from_slice(&0u16.to_le_bytes());
        raw[0x15c..0x163].copy_from_slice(&[1, b'M', 4, b'P', b'r', b'o', b'c']);

        let image = NeImage::parse(&raw).unwrap();
        let mut loaded = image.load_segments(&raw, 0x28).unwrap();
        // Imported far pointers must refer to mapped guest memory, just as
        // the API gates in the real-application startup trace do.
        assert!(matches!(
            loaded.apply_relocations(&image, |_| Some(NeFarAddress {
                selector: 0x88,
                offset: 0x1234,
            })),
            Err(NeError::InvalidRelocationAddress {
                selector: 0x88,
                offset: 0x1234,
            })
        ));
        loaded.add_region(0x88, 0x1235, false, true).unwrap();
        loaded
            .apply_relocations(&image, |relocation| {
                assert_eq!(relocation.target1, 1); // one-based module reference
                assert_eq!(relocation.target2, 7);
                Some(NeFarAddress {
                    selector: 0x88,
                    offset: 0x1234,
                })
            })
            .unwrap();
        assert_eq!(
            loaded.read(0x28, 1, 4, NeAccess::Read).unwrap(),
            [0x34, 0x12, 0x88, 0]
        );

        let mut unresolved = image.load_segments(&raw, 0x28).unwrap();
        assert!(matches!(
            unresolved.apply_relocations(&image, |_| None),
            Err(NeError::UnresolvedRelocationTarget { segment: 1 })
        ));
    }

    #[test]
    fn failed_relocation_application_does_not_commit_earlier_fixups() {
        let raw = relocation_fixture(0, 4, RELOC_CHAIN_END, 1);
        let mut image = NeImage::parse(&raw).unwrap();
        image.segments[0].relocations.push(NeRelocation {
            source_type: 11,
            flags: 0,
            source_offset: 0,
            target1: 1,
            target2: 0,
            chain_offsets: vec![0],
        });
        let mut loaded = image.load_segments(&raw, 0x28).unwrap();
        let before = loaded.read(0x28, 1, 2, NeAccess::Read).unwrap();
        assert!(matches!(
            loaded.apply_relocations(&image, |_| None),
            Err(NeError::UnsupportedRelocationApplication {
                source_type: 11,
                ..
            })
        ));
        assert_eq!(loaded.read(0x28, 1, 2, NeAccess::Read).unwrap(), before);
    }

    #[test]
    fn validates_internal_segment_offsets_and_movable_entry_ordinals() {
        let mut bad_segment = relocation_fixture(0, RELOC_CHAIN_END, RELOC_CHAIN_END, 1);
        bad_segment[0x156..0x158].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            NeImage::parse(&bad_segment),
            Err(NeError::InvalidRelocationSegment {
                source_segment: 1,
                target_segment: 2
            })
        ));

        let mut bad_offset = relocation_fixture(0, RELOC_CHAIN_END, RELOC_CHAIN_END, 1);
        bad_offset[0x158..0x15a].copy_from_slice(&0x20u16.to_le_bytes());
        assert!(matches!(
            NeImage::parse(&bad_offset),
            Err(NeError::RelocationTargetOffsetOutOfBounds {
                source_segment: 1,
                target_segment: 1,
                target_offset: 0x20,
                target_size: 0x20
            })
        ));

        fn movable_target(ordinal: u16) -> Vec<u8> {
            let mut bytes = relocation_fixture(0, RELOC_CHAIN_END, RELOC_CHAIN_END, 1);
            bytes[0x156..0x158].copy_from_slice(&0x00ffu16.to_le_bytes());
            bytes[0x158..0x15a].copy_from_slice(&ordinal.to_le_bytes());
            bytes[0x104..0x106].copy_from_slice(&0x5au16.to_le_bytes());
            bytes[0x106..0x108].copy_from_slice(&9u16.to_le_bytes());
            bytes.resize(0x163, 0);
            bytes[0x15a..0x163].copy_from_slice(&[1, 0xff, 1, 0xcd, 0x3f, 1, 0x10, 0, 0]);
            bytes
        }
        let valid = NeImage::parse(&movable_target(1)).unwrap();
        assert_eq!(valid.entry_points[0].ordinal, 1);
        assert!(matches!(
            NeImage::parse(&movable_target(2)),
            Err(NeError::InvalidRelocationEntryOrdinal {
                source_segment: 1,
                ordinal: 2
            })
        ));
    }

    #[test]
    fn parses_module_references_and_their_imported_names() {
        let image = NeImage::parse(&module_fixture()).unwrap();
        assert_eq!(
            image.module_references,
            vec![
                NeModuleReference {
                    index: 0,
                    imported_name_offset: 0,
                    name: b"KERN".to_vec()
                },
                NeModuleReference {
                    index: 1,
                    imported_name_offset: 5,
                    name: b"USER".to_vec()
                },
            ]
        );
        assert_eq!(
            image.imported_names,
            vec![
                NeImportedName {
                    offset: 0,
                    bytes: b"KERN".to_vec()
                },
                NeImportedName {
                    offset: 5,
                    bytes: b"USER".to_vec()
                },
            ]
        );
        assert_eq!(
            image
                .module_reference(1)
                .map(|module| module.name.as_slice()),
            Some(b"KERN".as_slice())
        );
        assert_eq!(
            image
                .module_reference(2)
                .map(|module| module.name.as_slice()),
            Some(b"USER".as_slice())
        );
        assert_eq!(image.module_reference(0), None);
        assert_eq!(image.module_reference(3), None);
        assert_eq!(image.imported_name(5), Some(b"USER".as_slice()));
    }

    #[test]
    fn import_name_relocations_resolve_to_preserved_imported_name_bytes() {
        let mut bytes = relocation_fixture(2, RELOC_CHAIN_END, RELOC_CHAIN_END, 1);
        bytes[0x156..0x158].copy_from_slice(&1u16.to_le_bytes());
        bytes[0x158..0x15a].copy_from_slice(&2u16.to_le_bytes());
        bytes[0x11e..0x120].copy_from_slice(&1u16.to_le_bytes());
        bytes[0x128..0x12a].copy_from_slice(&0x5au16.to_le_bytes());
        bytes[0x12a..0x12c].copy_from_slice(&0x5cu16.to_le_bytes());
        bytes[0x104..0x106].copy_from_slice(&0x63u16.to_le_bytes());
        bytes[0x106..0x108].copy_from_slice(&1u16.to_le_bytes());
        bytes.resize(0x164, 0);
        bytes[0x15a..0x15c].copy_from_slice(&0u16.to_le_bytes());
        bytes[0x15c..0x163].copy_from_slice(&[1, b'M', 4, b'P', b'r', b'o', b'c']);
        bytes[0x163] = 0;

        let image = NeImage::parse(&bytes).unwrap();
        assert_eq!(image.module_references[0].name, b"M");
        assert_eq!(
            image.imported_names,
            vec![
                NeImportedName {
                    offset: 0,
                    bytes: b"M".to_vec()
                },
                NeImportedName {
                    offset: 2,
                    bytes: b"Proc".to_vec()
                },
            ]
        );
    }

    #[test]
    fn rejects_imported_names_outside_table_and_invalid_module_indices() {
        let mut bad_name = module_fixture();
        bad_name[0x148..0x14a].copy_from_slice(&0x100u16.to_le_bytes());
        assert!(matches!(
            NeImage::parse(&bad_name),
            Err(NeError::MalformedImportedName { .. })
        ));

        let mut bad_module = relocation_fixture(2, RELOC_CHAIN_END, RELOC_CHAIN_END, 1);
        bad_module[0x156..0x158].copy_from_slice(&0u16.to_le_bytes());
        bad_module[0x11e..0x120].copy_from_slice(&1u16.to_le_bytes());
        bad_module[0x128..0x12a].copy_from_slice(&0x5au16.to_le_bytes());
        bad_module[0x12a..0x12c].copy_from_slice(&0x5cu16.to_le_bytes());
        bad_module.resize(0x15e, 0);
        bad_module[0x15a..0x15c].copy_from_slice(&0u16.to_le_bytes());
        bad_module[0x15c..0x15e].copy_from_slice(&[1, b'M']);
        assert!(matches!(
            NeImage::parse(&bad_module),
            Err(NeError::InvalidModuleReference {
                module_index: 0,
                module_count: 1,
                ..
            })
        ));
        bad_module[0x156..0x158].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            NeImage::parse(&bad_module),
            Err(NeError::InvalidModuleReference {
                module_index: 2,
                module_count: 1,
                ..
            })
        ));
    }

    #[test]
    fn additive_relocation_does_not_treat_source_as_a_chain() {
        let image = NeImage::parse(&relocation_fixture(RELOC_ADDITIVE, 0, 0, 1)).unwrap();
        assert!(image.segments[0].relocations[0].chain_offsets.is_empty());
    }

    #[test]
    fn rejects_relocation_chain_cycles_and_out_of_range_sources() {
        assert!(matches!(
            NeImage::parse(&relocation_fixture(0, 4, 1, 1)),
            Err(NeError::RelocationChainCycle {
                segment: 1,
                offset: 1
            })
        ));
        assert!(matches!(
            NeImage::parse(&relocation_fixture(0, RELOC_CHAIN_END, RELOC_CHAIN_END, 7)),
            Err(NeError::RelocationSourceOutOfBounds {
                segment: 1,
                offset: 7,
                ..
            })
        ));

        let mut narrow_chain = relocation_fixture(0, 4, RELOC_CHAIN_END, 1);
        narrow_chain[0x142..0x144].copy_from_slice(&6u16.to_le_bytes());
        narrow_chain[0x152] = 3; // pointer32 needs four bytes per chain member
        narrow_chain.copy_within(0x150..0x15a, 0x14e);
        assert!(matches!(
            NeImage::parse(&narrow_chain),
            Err(NeError::RelocationSourceOutOfBounds {
                offset: 4,
                width: 4,
                ..
            })
        ));
    }

    #[test]
    fn rejects_truncated_relocation_records() {
        let mut bytes = relocation_fixture(0, RELOC_CHAIN_END, RELOC_CHAIN_END, 1);
        bytes.truncate(0x159);
        assert!(matches!(
            NeImage::parse(&bytes),
            Err(NeError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn applies_alignment_shift_to_segment_sector_offsets() {
        let mut bytes = fixture([0x20, 4, 0x0001, 0x1000], 0x204);
        bytes[0x132..0x134].copy_from_slice(&4u16.to_le_bytes());
        let image = NeImage::parse(&bytes).unwrap();
        assert_eq!(image.segments[0].file_offset, Some(0x200));
        assert_eq!(image.segments[0].file_size, 4);
    }

    #[test]
    fn zero_encoded_lengths_represent_64k_allocation_and_segment() {
        let image = NeImage::parse(&fixture([1, 0, 0, 0], 0x1_0040)).unwrap();
        assert_eq!(image.segments[0].file_size, 0x1_0000);
        assert_eq!(image.segments[0].minimum_allocation, 0x1_0000);
    }

    #[test]
    fn zero_sector_offset_is_uninitialized_storage_not_file_data() {
        let image = NeImage::parse(&fixture([0, 0, 0, 0x200], 0x148)).unwrap();
        assert_eq!(image.segments[0].file_offset, None);
        assert_eq!(image.segments[0].file_size, 0);
        assert_eq!(image.segments[0].minimum_allocation, 0x200);
    }

    #[test]
    fn loads_segments_with_zero_fill_and_checked_selector_access() {
        let raw = fixture([0x80, 4, 0x0001, 8], 0x84);
        let image = NeImage::parse(&raw).unwrap();
        let mut loaded = image.load_segments(&raw, 0x28).unwrap();
        assert_eq!(loaded.selector_for_segment(1), Some(0x28));
        assert_eq!(
            loaded.read(0x28, 0, 4, NeAccess::Read).unwrap(),
            [0, 0, 0, 0]
        );
        loaded.write(0x28, 2, &[0xaa, 0xbb]).unwrap();
        assert_eq!(
            loaded.read(0x28, 0, 4, NeAccess::Read).unwrap(),
            [0, 0, 0xaa, 0xbb]
        );
        assert!(matches!(
            loaded.read(0x28, 7, 2, NeAccess::Read),
            Err(NeError::SelectorLimitExceeded { .. })
        ));
        let code = fixture([0x80, 4, 0x0080, 8], 0x84);
        let code_image = NeImage::parse(&code).unwrap();
        let mut code_loaded = code_image.load_segments(&code, 0x38).unwrap();
        assert_eq!(
            code_loaded.read(0x38, 0, 1, NeAccess::Execute).unwrap(),
            [0]
        );
        assert!(matches!(
            code_loaded.write(0x38, 0, &[1]),
            Err(NeError::SelectorAccessDenied {
                access: NeAccess::Write,
                ..
            })
        ));
        assert!(matches!(
            loaded.read(0x29, 0, 1, NeAccess::Read),
            Err(NeError::MissingSelector { .. })
        ));

        let mut two_segments = fixture([0, 0, 0, 8], 0x150);
        two_segments[0x11c..0x11e].copy_from_slice(&2u16.to_le_bytes());
        two_segments[0x148..0x150].copy_from_slice(&[0, 0, 0, 0, 1, 0, 8, 0]);
        let two_segment_image = NeImage::parse(&two_segments).unwrap();
        let two_segment_loaded = two_segment_image
            .load_segments(&two_segments, 0x28)
            .unwrap();
        assert_eq!(two_segment_loaded.selector_for_segment(1), Some(0x28));
        assert_eq!(two_segment_loaded.selector_for_segment(2), Some(0x30));
    }

    #[test]
    fn resolves_initial_registers_to_loaded_code_and_stack_selectors() {
        let mut raw = fixture([0x80, 4, 0x0080, 0x20], 0x1_0080);
        raw[0x11c..0x11e].copy_from_slice(&2u16.to_le_bytes());
        raw[0x114..0x116].copy_from_slice(&1u16.to_le_bytes());
        raw[0x116..0x118].copy_from_slice(&1u16.to_le_bytes());
        raw[0x118..0x11a].copy_from_slice(&0x20u16.to_le_bytes());
        raw[0x11a..0x11c].copy_from_slice(&2u16.to_le_bytes());
        raw[0x148..0x150].copy_from_slice(&[0, 0, 0, 0, 1, 0, 0x20, 0]);
        let image = NeImage::parse(&raw).unwrap();
        let loaded = image.load_segments(&raw, 0x28).unwrap();
        assert_eq!(
            image.start_state(&loaded).unwrap(),
            NeStartState {
                cs: 0x28,
                ip: 1,
                ss: 0x30,
                sp: 0x24,
            }
        );

        let mut bad_ip = image.clone();
        bad_ip.initial_ip = 0x20;
        assert!(matches!(
            bad_ip.start_state(&loaded),
            Err(NeError::SelectorLimitExceeded { .. })
        ));
        let mut bad_sp = image.clone();
        bad_sp.initial_sp = 0x21;
        let mut short_stack_image = image.clone();
        short_stack_image.segments[1].minimum_allocation = 0x10;
        let short_stack_loaded = short_stack_image.load_segments(&raw, 0x28).unwrap();
        assert!(matches!(
            bad_sp.start_state(&short_stack_loaded),
            Err(NeError::SelectorLimitExceeded { .. })
        ));
    }

    #[test]
    fn decodes_instruction_bytes_through_executable_selector() {
        let mut raw = fixture([0x80, 4, 0x0080, 4], 0x84);
        raw[0x80..0x84].copy_from_slice(&[0xb8, 0x34, 0x12, 0xc3]);
        let image = NeImage::parse(&raw).unwrap();
        let loaded = image.load_segments(&raw, 0x28).unwrap();
        let decoded = decode_on_large_stack(raw.clone(), 0x28, 0x28, 0).unwrap();
        assert_eq!((decoded.cs, decoded.ip, decoded.next_ip), (0x28, 0, 3));
        assert_eq!(decoded.instruction.len, 3);
        assert!(decoded.instruction.disassembly.contains("AX"));
        assert!(matches!(
            loaded.decode_next(
                &mut rsleigh_api::Decoder::new(rsleigh_api::Architecture::X86_32),
                0x28,
                0,
            ),
            Err(NeError::WrongDecoderArchitecture)
        ));
    }

    #[test]
    fn steps_far_call_return_and_register_immediate() {
        let mut raw = fixture([0x80, 16, 0x0080, 0x20], 0x1_0080);
        raw[0x11c..0x11e].copy_from_slice(&2u16.to_le_bytes());
        raw[0x10e..0x110].copy_from_slice(&2u16.to_le_bytes());
        raw[0x128..0x12a].copy_from_slice(&0x48u16.to_le_bytes());
        raw[0x12a..0x12c].copy_from_slice(&0x48u16.to_le_bytes());
        raw[0x12c..0x12e].copy_from_slice(&0x48u16.to_le_bytes());
        raw[0x114..0x116].copy_from_slice(&0u16.to_le_bytes());
        raw[0x116..0x118].copy_from_slice(&1u16.to_le_bytes());
        raw[0x118..0x11a].copy_from_slice(&0x20u16.to_le_bytes());
        raw[0x11a..0x11c].copy_from_slice(&2u16.to_le_bytes());
        raw[0x148..0x150].copy_from_slice(&[0, 0, 0, 0, 1, 0, 0x20, 0]);
        raw[0x140..0x148].copy_from_slice(&[0x80, 0, 0, 0, 0x80, 0, 0x20, 0]);
        raw[0x80..0x90].copy_from_slice(&[
            0x9a, 0x09, 0x00, 0x28, 0x00, // call far 0028:0009
            0xb8, 0x21, 0x43, // mov ax, 4321h
            0x90, // nop
            0xcb, // retf
            0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
        ]);

        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let image = NeImage::parse(&raw).unwrap();
                let mut loaded = image.load_segments(&raw, 0x28).unwrap();
                let mut decoder =
                    rsleigh_api::Decoder::new(rsleigh_api::Architecture::X86_16Protected);
                let mut machine = NeMachine::new(&image, &loaded).unwrap();

                let call = machine.step(&mut loaded, &mut decoder).unwrap();
                assert_eq!(call.flow, NeCpuFlow::Call);
                assert_eq!((call.next_cs, call.next_ip), (0x28, 9));
                assert_eq!(machine.register(0x10, 2).unwrap(), 0x20);

                let ret = machine.step(&mut loaded, &mut decoder).unwrap();
                assert_eq!(ret.flow, NeCpuFlow::Return);
                assert_eq!((ret.next_cs, ret.next_ip), (0x28, 5));
                assert_eq!(machine.register(0x10, 2).unwrap(), 0x24);

                let mov = machine.step(&mut loaded, &mut decoder).unwrap();
                assert_eq!(mov.flow, NeCpuFlow::Sequential);
                assert_eq!((mov.next_cs, mov.next_ip), (0x28, 8));
                assert_eq!(machine.register(0, 2).unwrap(), 0x4321);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn fetch_decode_wraps_ip_inside_full_64k_segment() {
        let mut raw = fixture([0x80, 0, 0x0080, 0], 0x1_0080);
        raw[0x80..0x82].copy_from_slice(&[0x34, 0x12]);
        raw[0x1_007f] = 0xb8;
        let decoded = decode_on_large_stack(raw, 0x28, 0x28, 0xffff).unwrap();
        assert_eq!(decoded.instruction.len, 3);
        assert_eq!(decoded.next_ip, 2);
    }

    #[test]
    fn selector_aliases_share_backing_storage() {
        let raw = fixture([0, 0, 1, 8], 0x148);
        let image = NeImage::parse(&raw).unwrap();
        let mut loaded = image.load_segments(&raw, 0x30).unwrap();
        loaded.add_alias(0x30, 0x40).unwrap();
        loaded.write(0x40, 7, &[0x5a]).unwrap();
        assert_eq!(loaded.read(0x30, 7, 1, NeAccess::Read).unwrap(), [0x5a]);
    }

    #[test]
    fn rejects_segment_bytes_outside_file() {
        assert!(matches!(
            NeImage::parse(&fixture([0x80, 0x100, 0, 0x100], 0x148)),
            Err(NeError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn rejects_bad_signature_and_alignment_shift() {
        assert_eq!(NeImage::parse(b"not mz"), Err(NeError::NotMz));
        let mut bytes = fixture([0, 0, 0, 0], 0x148);
        bytes[0x100..0x102].copy_from_slice(b"PE");
        assert_eq!(
            NeImage::parse(&bytes),
            Err(NeError::NotNe { offset: 0x100 })
        );

        let mut bytes = fixture([0, 0, 0, 0], 0x148);
        bytes[0x132..0x134].copy_from_slice(&16u16.to_le_bytes());
        assert_eq!(
            NeImage::parse(&bytes),
            Err(NeError::InvalidAlignmentShift { shift: 16 })
        );
    }

    #[test]
    fn parses_fixed_and_movable_entry_bundles_with_ordinals() {
        let fixed = entry_fixture(&[
            2, 1, // two fixed entries in segment 1
            1, 0x10, 0, 0, 0x20, 0, 0, // flags+offsets, then end marker
        ]);
        let image = NeImage::parse(&fixed).unwrap();
        assert_eq!(
            image.entry_points,
            vec![
                NeEntryPoint {
                    ordinal: 1,
                    segment: 1,
                    offset: 0x10,
                    flags: 1,
                    movable: false
                },
                NeEntryPoint {
                    ordinal: 2,
                    segment: 1,
                    offset: 0x20,
                    flags: 0,
                    movable: false
                },
            ]
        );

        let movable = entry_fixture(&[1, 0xff, 1, 0xcd, 0x3f, 1, 0x30, 0, 0]);
        let image = NeImage::parse(&movable).unwrap();
        assert_eq!(
            image.entry_points,
            vec![NeEntryPoint {
                ordinal: 1,
                segment: 1,
                offset: 0x30,
                flags: 1,
                movable: true,
            }]
        );
    }

    #[test]
    fn unused_entry_bundles_advance_ordinals_and_bad_entries_are_rejected() {
        let image = NeImage::parse(&entry_fixture(&[
            2, 0, // skip ordinals 1 and 2
            1, 1, 1, 0x10, 0, // fixed ordinal 3
            0,
        ]))
        .unwrap();
        assert_eq!(image.entry_points[0].ordinal, 3);

        assert!(matches!(
            NeImage::parse(&entry_fixture(&[1, 0xff, 1, 0, 0x3f, 1, 0x10, 0, 0])),
            Err(NeError::MalformedEntryTable { .. })
        ));
        assert!(matches!(
            NeImage::parse(&entry_fixture(&[1, 2, 1, 0x10, 0, 0])),
            Err(NeError::EntrySegmentOutOfRange { .. })
        ));

        let mut outside = entry_fixture(&[1, 1, 1, 0x10, 0, 0]);
        outside[0x146..0x148].copy_from_slice(&0x10u16.to_le_bytes());
        assert!(matches!(
            NeImage::parse(&outside),
            Err(NeError::MalformedEntryTable { .. })
        ));
    }

    #[test]
    fn rejects_segment_table_outside_file() {
        let mut bytes = fixture([0, 0, 0, 0], 0x148);
        bytes[0x122..0x124].copy_from_slice(&0xfff0u16.to_le_bytes());
        assert!(matches!(
            NeImage::parse(&bytes),
            Err(NeError::OutOfBounds { .. })
        ));
    }
}
