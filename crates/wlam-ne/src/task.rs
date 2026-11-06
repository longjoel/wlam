//! Single-task Win16 startup and KERNEL ABI. Host API arguments are Pascal
//! words/far pointers, independent of the PE32 runtime's argument layout.
use crate::*;
use rsleigh_api::{Architecture, Decoder};
use std::collections::BTreeMap;
mod callbacks;
mod user;

const AX: u64 = 0;
const CX: u64 = 4;
const DX: u64 = 8;
const BX: u64 = 12;
const SP: u64 = 16;
const SI: u64 = 24;
const DI: u64 = 28;
const ES: u64 = 0x100;
const SS: u64 = 0x104;
const DS: u64 = 0x106;

#[derive(Debug, thiserror::Error)]
pub enum NeTaskError {
    #[error(transparent)]
    Image(#[from] NeError),
    #[error("unsupported Win16 API {module}!{symbol:?} at {cs:04x}:{ip:04x}")]
    UnsupportedApi {
        module: String,
        symbol: NeImportSymbol,
        cs: u16,
        ip: u16,
    },
    #[error("unsupported DOS function AX={0:04x}")]
    UnsupportedDos(u16),
    #[error("invalid Win16 task state: {0}")]
    State(String),
}

#[derive(Debug)]
pub enum NeTaskEvent {
    Wait,
    Instruction(NeStepResult),
    Api {
        module: String,
        symbol: NeImportSymbol,
    },
    Exit(u16),
    MessageBox {
        text: Vec<u8>,
        title: Vec<u8>,
        flags: u16,
    },
}

#[derive(Clone, Debug)]
struct Gate {
    module: String,
    symbol: NeImportSymbol,
}

/// A loaded NE task. The checked CPU remains the diagnostic execution backend;
/// the task/ABI layer is reusable by the recompilation backend.
pub struct NeTask {
    pub image: NeImage,
    pub loaded: NeLoadedImage,
    pub machine: NeMachine,
    pub host: wlam_host::runtime::Runtime,
    raw: Vec<u8>,
    decoder: Decoder,
    gates: BTreeMap<u16, Gate>,
    gate_selector: u16,
    context: NeTaskStartContext,
    initialized: bool,
    user_initialized: bool,
    heaps: BTreeMap<u16, LocalHeap>,
    resources: NeResourceManager,
    globals: BTreeMap<u16, (u16, u16)>, // handle -> (selector, locks)
    vectors: [NeFarAddress; 256],
    executable_path: Vec<u8>,
    thunks: BTreeMap<u32, u16>,
    handles: BTreeMap<u16, u32>,
    system_menus: BTreeMap<u16, u16>,
    menu_overrides: BTreeMap<u32, Vec<u8>>,
    graphics: Vec<u8>,
    arena: Vec<u8>,
    host_resources: BTreeMap<usize, u32>,
    files: BTreeMap<String, Vec<u8>>,
    callbacks: Vec<callbacks::CallbackFrame>,
    pending_host: Option<callbacks::HostCall>,
    exit: Option<u16>,
}

impl NeTask {
    pub fn load(raw: Vec<u8>, command_line: Vec<u8>) -> Result<Self, NeTaskError> {
        let image = NeImage::parse(&raw)?;
        let mut loaded = image.load_segments(&raw, 0x100)?;
        let gate_selector = loaded.add_region_auto(65536, false, true)?;
        let mut sites = BTreeMap::new();
        let mut gates = BTreeMap::new();
        let vectors = std::array::from_fn(|i| {
            let offset = 0x4000 + i as u16;
            gates.insert(
                offset,
                Gate {
                    module: "DOS_INTERRUPT".into(),
                    symbol: NeImportSymbol::Ordinal(i as u16),
                },
            );
            NeFarAddress {
                selector: gate_selector,
                offset,
            }
        });
        for segment in &image.segments {
            for r in &segment.relocations {
                let kind = r.flags & 3;
                if !matches!(kind, 1 | 2) {
                    continue;
                }
                let key = (kind, r.target1, r.target2);
                if sites.contains_key(&key) {
                    continue;
                }
                let module = String::from_utf8_lossy(
                    &image
                        .module_reference(r.target1)
                        .ok_or_else(|| NeTaskError::State("missing module".into()))?
                        .name,
                )
                .to_ascii_uppercase();
                let symbol = if kind == 1 {
                    NeImportSymbol::Ordinal(r.target2)
                } else {
                    NeImportSymbol::Name(
                        image
                            .imported_name(r.target2)
                            .ok_or_else(|| NeTaskError::State("missing export name".into()))?
                            .to_vec(),
                    )
                };
                // Absolute KERNEL exports are values, not callable gates. They
                // still use a mapped selector so pointer fixups remain checked.
                let value = match (module.as_str(), &symbol) {
                    ("KERNEL", NeImportSymbol::Ordinal(113)) => Some(3),
                    ("KERNEL", NeImportSymbol::Ordinal(114)) => Some(8),
                    ("KERNEL", NeImportSymbol::Ordinal(178)) => Some(0x413),
                    _ => None,
                };
                let offset = if let Some(value) = value {
                    value
                } else {
                    let offset = 0x8000u16
                        .checked_add(
                            u16::try_from(gates.len())
                                .map_err(|_| NeError::SelectorSpaceExhausted)?,
                        )
                        .filter(|v| *v < 0xffff)
                        .ok_or(NeError::SelectorSpaceExhausted)?;
                    gates.insert(offset, Gate { module, symbol });
                    offset
                };
                sites.insert(
                    key,
                    NeFarAddress {
                        selector: gate_selector,
                        offset,
                    },
                );
            }
        }
        loaded.apply_relocations(&image, |r| {
            sites.get(&(r.flags & 3, r.target1, r.target2)).copied()
        })?;
        let psp_selector = loaded.add_region_auto(256, true, false)?;
        let environment_selector = loaded.add_region_auto(256, true, false)?;
        loaded.write(
            environment_selector,
            0,
            b"PATH=C:\\WINDOWS\0\0\x01\0C:\\APP.EXE\0",
        )?;
        let context = NeTaskStartContext {
            psp_selector,
            instance: loaded
                .selector_for_segment(image.auto_data_segment)
                .ok_or_else(|| {
                    NeTaskError::State("EXE requires an automatic data segment".into())
                })?,
            previous_instance: 0,
            parent_psp: 0,
            environment_selector,
            dos_dispatcher: NeFarAddress {
                selector: gate_selector,
                offset: 0xffff,
            },
            command_line,
        };
        NeMachine::initialize_task_psp(&mut loaded, &context)?;
        let mut machine = NeMachine::new(&image, &loaded)?;
        machine.initialize_task_start(&image, &loaded, &context)?;
        let mut host =
            wlam_host::runtime::Runtime::with_allocation_base(crate::host_memory::ARENA_BASE);
        host.image_base = u32::from(context.instance);
        Ok(Self {
            image,
            loaded,
            machine,
            host,
            raw,
            decoder: Decoder::new(Architecture::X86_16Protected),
            gates,
            gate_selector,
            context,
            initialized: false,
            user_initialized: false,
            heaps: BTreeMap::new(),
            resources: NeResourceManager::default(),
            globals: BTreeMap::new(),
            vectors,
            executable_path: b"C:\\APP.EXE".to_vec(),
            thunks: BTreeMap::new(),
            handles: BTreeMap::new(),
            system_menus: BTreeMap::new(),
            menu_overrides: BTreeMap::new(),
            graphics: Vec::new(),
            arena: Vec::new(),
            host_resources: BTreeMap::new(),
            files: BTreeMap::new(),
            callbacks: Vec::new(),
            pending_host: None,
            exit: None,
        })
    }

    pub fn set_executable_path(&mut self, path: &[u8]) -> Result<(), NeTaskError> {
        let mut environment = b"PATH=C:\\WINDOWS\0\0\x01\0".to_vec();
        environment.extend_from_slice(path);
        environment.push(0);
        self.loaded
            .write(self.context.environment_selector, 0, &environment)?;
        self.executable_path = path.to_vec();
        Ok(())
    }
    pub fn register_file(&mut self, path: &[u8], bytes: Vec<u8>) {
        self.files.insert(crate::profile::path(path), bytes);
    }
    pub fn file_data(&self, path: &[u8]) -> Option<&[u8]> {
        self.files
            .get(&crate::profile::path(path))
            .map(Vec::as_slice)
    }

    pub fn is_api_address(&self, selector: u16) -> bool {
        selector == self.gate_selector
    }
    fn write_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), NeError> {
        self.loaded
            .write((address >> 16) as u16, address & 0xffff, bytes)
    }
    fn string(&self, address: u32) -> Result<Vec<u8>, NeTaskError> {
        let selector = (address >> 16) as u16;
        let offset = address & 0xffff;
        let limit = self
            .loaded
            .selectors
            .iter()
            .find(|s| s.selector == selector)
            .ok_or(NeError::MissingSelector { selector })?
            .limit;
        let bytes = self.loaded.read(
            selector,
            offset,
            limit.saturating_sub(offset),
            NeAccess::Read,
        )?;
        let end = bytes
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| NeTaskError::State("unterminated guest string".into()))?;
        Ok(bytes[..end].to_vec())
    }

    pub fn step(&mut self) -> Result<NeTaskEvent, NeTaskError> {
        if self.pending_host.is_some() {
            return self.resume_host_wait();
        }
        if let Some(code) = self.exit {
            return Ok(NeTaskEvent::Exit(code));
        }
        if self.machine.cs() != self.gate_selector {
            return Ok(NeTaskEvent::Instruction(
                self.machine.step(&mut self.loaded, &mut self.decoder)?,
            ));
        }
        if self.machine.ip() == 0x3fff {
            return self.resume_callback();
        }
        let gate = if self.machine.ip() == 0xffff {
            Gate {
                module: "KERNEL".into(),
                symbol: NeImportSymbol::Ordinal(102),
            }
        } else {
            self.gates
                .get(&self.machine.ip())
                .cloned()
                .ok_or_else(|| NeTaskError::State("call into non-export gate address".into()))?
        };
        if gate.module == "KERNEL" {
            if let NeImportSymbol::Ordinal(ordinal) = gate.symbol {
                if self.kernel(ordinal)? {
                    return Ok(NeTaskEvent::Api {
                        module: gate.module,
                        symbol: gate.symbol,
                    });
                }
            }
        }
        if gate.module == "GDI" {
            if let NeImportSymbol::Ordinal(ordinal) = gate.symbol {
                if self.gdi(ordinal)? {
                    return Ok(NeTaskEvent::Api {
                        module: gate.module,
                        symbol: gate.symbol,
                    });
                }
            }
        }
        if gate.module == "USER" {
            if let NeImportSymbol::Ordinal(ordinal) = gate.symbol {
                if self.user_basic(ordinal)? {
                    return Ok(NeTaskEvent::Api {
                        module: gate.module,
                        symbol: gate.symbol,
                    });
                }
            }
            if matches!(gate.symbol, NeImportSymbol::Ordinal(107 | 122)) {
                let window = self.handle(self.arg(8)?)?;
                let message = u32::from(self.arg(6)?);
                let mut wparam = u32::from(self.arg(4)?);
                let mut lparam = self.long_arg(0)?;
                if message == 0x111 {
                    wparam |= lparam & 0xffff0000;
                    lparam = self.handle(lparam as u16)?;
                }
                if gate.symbol == NeImportSymbol::Ordinal(107) {
                    let result = self.host_value(
                        "user32",
                        "DefWindowProcA",
                        &[window, message, wparam, lparam],
                    )?;
                    self.finish(10, result)?;
                } else {
                    self.start_host(
                        "user32",
                        "CallWindowProcA",
                        vec![self.long_arg(10)?, window, message, wparam, lparam],
                        14,
                        false,
                    )?;
                }
                return Ok(NeTaskEvent::Api {
                    module: gate.module,
                    symbol: gate.symbol,
                });
            }
            if gate.symbol == NeImportSymbol::Ordinal(41) {
                let parent = self.handle(self.arg(8)?)?;
                let style = self.long_arg(18)?;
                let menu = if style & 0x40000000 != 0 { u32::from(self.arg(6)?) }
                    else { self.handle(self.arg(6)?)? };
                let default_x = self.arg(16)? == 0x8000;
                let default_size = self.arg(12)? == 0x8000;
                let overlapped = style & 0xc0000000 == 0;
                let x = if default_x {
                    0
                } else {
                    self.arg(16)? as i16 as i32 as u32
                };
                let y = if default_x && overlapped {
                    0
                } else {
                    self.arg(14)? as i16 as i32 as u32
                };
                let width = if default_size {
                    if overlapped {
                        self.host_value("user32", "GetSystemMetrics", &[0])?
                    } else {
                        0
                    }
                } else {
                    self.arg(12)? as i16 as i32 as u32
                };
                let height = if default_size {
                    if overlapped {
                        self.host_value("user32", "GetSystemMetrics", &[1])?
                    } else {
                        0
                    }
                } else {
                    self.arg(10)? as i16 as i32 as u32
                };
                let args = vec![
                    0,
                    self.long_arg(26)?,
                    self.long_arg(22)?,
                    style,
                    x,
                    y,
                    width,
                    height,
                    parent,
                    menu,
                    u32::from(self.arg(4)?),
                    self.long_arg(0)?,
                ];
                self.start_host("user32", "CreateWindowExA", args, 30, true)?;
                return Ok(NeTaskEvent::Api {
                    module: gate.module,
                    symbol: gate.symbol,
                });
            }
            if gate.symbol == NeImportSymbol::Ordinal(57) {
                let pointer = self.long_arg(0)?;
                let bytes = self.loaded.read(
                    (pointer >> 16) as u16,
                    pointer & 0xffff,
                    26,
                    NeAccess::Read,
                )?;
                let word = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                let long = |offset: usize| {
                    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
                };
                let procedure = long(2);
                self.loaded.translate(
                    (procedure >> 16) as u16,
                    procedure & 0xffff,
                    1,
                    NeAccess::Execute,
                )?;
                let brush = word(16);
                let brush = if brush < 0x1000 {
                    u32::from(brush)
                } else {
                    self.handle(brush)?
                };
                let fields = [
                    u32::from(word(0)),
                    procedure,
                    word(6) as i16 as i32 as u32,
                    word(8) as i16 as i32 as u32,
                    u32::from(word(10)),
                    self.handle(word(12))?,
                    self.handle(word(14))?,
                    brush,
                    long(18),
                    long(22),
                ];
                let start = self.graphics.len();
                let address = crate::host_memory::HOST_BASE + start as u32;
                for field in fields {
                    self.graphics.extend_from_slice(&field.to_le_bytes());
                }
                let atom = self.host_value("user32", "RegisterClassA", &[address])?;
                self.graphics.truncate(start);
                self.finish(4, atom)?;
                return Ok(NeTaskEvent::Api {
                    module: gate.module,
                    symbol: gate.symbol,
                });
            }
            if gate.symbol == NeImportSymbol::Ordinal(1) {
                let flags = self.arg(0)?;
                let title = self.string(self.long_arg(2)?)?;
                let text = self.string(self.long_arg(6)?)?;
                self.arg(10)?;
                return Ok(NeTaskEvent::MessageBox { text, title, flags });
            }
            if gate.symbol == NeImportSymbol::Ordinal(150) {
                let name = self.long_arg(0)?;
                let instance = self.arg(4)?;
                let id = self.loaded.resource_identifier_from_pointer(name)?;
                let resource = self
                    .image
                    .find_resource(&NeResourceIdentifier::Ordinal(4), &id);
                let result = if (instance == 0 || instance == self.context.instance)
                    && resource.is_some()
                {
                    let resource = resource.unwrap();
                    let converted = crate::templates::menu_to_win32(
                        self.image.resource_data(&self.raw, resource)?,
                    )
                    .map_err(NeTaskError::State)?;
                    let address = crate::host_memory::HOST_BASE
                        + u32::try_from(self.graphics.len())
                            .map_err(|_| NeTaskError::State("graphics memory exhausted".into()))?;
                    self.graphics.extend_from_slice(&converted);
                    self.map_handle(address)?
                } else {
                    0
                };
                self.finish(6, u32::from(result))?;
                return Ok(NeTaskEvent::Api {
                    module: gate.module,
                    symbol: gate.symbol,
                });
            }
            if let NeImportSymbol::Ordinal(ordinal @ (66 | 68)) = gate.symbol {
                let result = if ordinal == 66 {
                    let window = self.handle(self.arg(0)?)?;
                    let dc = self.host_value("user32", "GetDC", &[window])?;
                    let handle = self.map_handle(dc)?;
                    u32::from(handle)
                } else {
                    let dc = self.handle(self.arg(0)?)?;
                    let window = self.handle(self.arg(2)?)?;
                    self.host_value("user32", "ReleaseDC", &[window, dc])?
                };
                self.finish(if ordinal == 66 { 2 } else { 4 }, result)?;
                return Ok(NeTaskEvent::Api {
                    module: gate.module,
                    symbol: gate.symbol,
                });
            }
            if let NeImportSymbol::Ordinal(ordinal @ (173 | 174)) = gate.symbol {
                let name = self.long_arg(0)?;
                let instance = self.arg(4)?;
                if instance == self.context.instance {
                    for index in 0..self.image.resources.len() {
                        let resource = &self.image.resources[index];
                        if matches!(
                            resource.resource_type,
                            NeResourceIdentifier::Ordinal(1 | 3 | 12 | 14)
                        ) && !self.host_resources.contains_key(&index)
                        {
                            if let (
                                NeResourceIdentifier::Ordinal(kind),
                                NeResourceIdentifier::Ordinal(id),
                            ) = (&resource.resource_type, &resource.name)
                            {
                                let kind = *kind;
                                let id = *id;
                                let bytes = self.image.resource_data(&self.raw, resource)?.to_vec();
                                let address = crate::host_memory::HOST_BASE
                                    + u32::try_from(self.graphics.len()).map_err(|_| {
                                        NeTaskError::State("graphics memory exhausted".into())
                                    })?;
                                self.graphics.extend_from_slice(&bytes);
                                self.host
                                    .resources
                                    .register(
                                        u32::from(kind),
                                        u32::from(id),
                                        0,
                                        address,
                                        bytes.len() as u32,
                                    )
                                    .map_err(NeTaskError::State)?;
                                self.host_resources.insert(index, address);
                            }
                        }
                    }
                }
                let function = if ordinal == 173 {
                    "LoadCursorA"
                } else {
                    "LoadIconA"
                };
                let object = self.host_value("user32", function, &[u32::from(instance), name])?;
                let handle = self.map_handle(object)?;
                self.finish(6, u32::from(handle))?;
                return Ok(NeTaskEvent::Api {
                    module: gate.module,
                    symbol: gate.symbol,
                });
            }
            if let NeImportSymbol::Ordinal(ordinal @ (179 | 180)) = gate.symbol {
                let index = self.arg(0)? as i16 as i32 as u32;
                let name = if ordinal == 179 {
                    "GetSystemMetrics"
                } else {
                    "GetSysColor"
                };
                let result = self.host_value("user32", name, &[index])?;
                self.finish(2, result)?;
                return Ok(NeTaskEvent::Api {
                    module: gate.module,
                    symbol: gate.symbol,
                });
            }
        }
        if gate.module == "USER" && gate.symbol == NeImportSymbol::Ordinal(5) {
            let instance = self.arg(0)?;
            if !self.initialized || instance != self.context.instance {
                self.finish(2, 0)?;
            } else {
                // Single-task USER initialization creates the task's queue;
                // no windows or messages exist before the application adds them.
                self.user_initialized = self.host.windows.reserve_queue(8);
                self.finish(2, u32::from(self.user_initialized))?;
            }
            return Ok(NeTaskEvent::Api {
                module: gate.module,
                symbol: gate.symbol,
            });
        }
        if gate.module == "GDI" && gate.symbol == NeImportSymbol::Ordinal(87) {
            let index = self.arg(0)?;
            let object = self
                .host
                .gdi
                .stock_object(u32::from(index))
                .map_err(|e| NeTaskError::State(format!("GetStockObject: {e:?}")))?;
            let handle = self.map_handle(object)?;
            self.finish(2, u32::from(handle))?;
            return Ok(NeTaskEvent::Api {
                module: gate.module,
                symbol: gate.symbol,
            });
        }
        if gate.module == "GDI" && gate.symbol == NeImportSymbol::Ordinal(60) {
            let bitmap = self.arg(0)?;
            let bitmap = *self
                .handles
                .get(&bitmap)
                .ok_or_else(|| NeTaskError::State("invalid bitmap handle".into()))?;
            let memory = crate::host_memory::TaskMemory {
                loaded: &mut self.loaded,
                graphics: &mut self.graphics,
                arena: &mut self.arena,
            };
            let object = self
                .host
                .gdi
                .pattern_brush(&memory, bitmap)
                .map_err(|e| NeTaskError::State(format!("CreatePatternBrush: {e:?}")))?;
            let handle = self.map_handle(object)?;
            self.finish(2, u32::from(handle))?;
            return Ok(NeTaskEvent::Api {
                module: gate.module,
                symbol: gate.symbol,
            });
        }
        if gate.module == "USER" && gate.symbol == NeImportSymbol::Ordinal(175) {
            let name = self.long_arg(0)?;
            let instance = self.arg(4)?;
            let name = self.loaded.resource_identifier_from_pointer(name)?;
            let resource = self
                .image
                .find_resource(&NeResourceIdentifier::Ordinal(2), &name);
            let result = if (instance == self.context.instance || instance == 0)
                && resource.is_some()
            {
                let resource = resource.unwrap();
                let bitmap = crate::bitmap::decode(self.image.resource_data(&self.raw, resource)?)
                    .map_err(NeTaskError::State)?;
                let offset = u32::try_from(self.graphics.len())
                    .map_err(|_| NeTaskError::State("graphics memory exhausted".into()))?;
                let address = crate::host_memory::HOST_BASE
                    .checked_add(offset)
                    .ok_or_else(|| NeTaskError::State("graphics address overflow".into()))?;
                self.graphics.extend_from_slice(&bitmap);
                let memory = crate::host_memory::TaskMemory {
                    loaded: &mut self.loaded,
                    graphics: &mut self.graphics,
                    arena: &mut self.arena,
                };
                let object = self
                    .host
                    .gdi
                    .bitmap(&memory, address, address + 40)
                    .map_err(|e| NeTaskError::State(format!("LoadBitmap: {e:?}")))?;
                self.map_handle(object)?
            } else {
                0
            };
            self.finish(6, u32::from(result))?;
            return Ok(NeTaskEvent::Api {
                module: gate.module,
                symbol: gate.symbol,
            });
        }
        Err(NeTaskError::UnsupportedApi {
            module: gate.module,
            symbol: gate.symbol,
            cs: self.machine.cs(),
            ip: self.machine.ip(),
        })
    }

    fn map_handle(&mut self, host: u32) -> Result<u16, NeTaskError> {
        if host == 0 {
            return Ok(0);
        }
        if let Some((&handle, _)) = self.handles.iter().find(|(_, value)| **value == host) {
            return Ok(handle);
        }
        let handle = self
            .handles
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0xfff)
            .checked_add(1)
            .ok_or_else(|| NeTaskError::State("Win16 handle space exhausted".into()))?;
        self.handles.insert(handle, host);
        Ok(handle)
    }

    /// Resume a message box only after the UI has supplied an actual button.
    pub fn answer_message_box(&mut self, result: u16) -> Result<(), NeTaskError> {
        let gate = self.gates.get(&self.machine.ip());
        if self.machine.cs() != self.gate_selector
            || !gate.is_some_and(|g| g.module == "USER" && g.symbol == NeImportSymbol::Ordinal(1))
        {
            return Err(NeTaskError::State("no pending message box".into()));
        }
        let valid: &[u16] = match self.arg(0)? & 15 {
            0 => &[1],
            1 => &[1, 2],
            2 => &[3, 4, 5],
            3 => &[6, 7, 2],
            4 => &[6, 7],
            5 => &[4, 2],
            6 => &[2, 10, 11],
            _ => &[],
        };
        if !valid.contains(&result) {
            return Err(NeTaskError::State("invalid message-box button".into()));
        }
        self.finish(12, u32::from(result))?;
        Ok(())
    }

    fn handle(&self, guest: u16) -> Result<u32, NeTaskError> {
        if guest == 0 {
            return Ok(0);
        }
        self.handles
            .get(&guest)
            .copied()
            .ok_or_else(|| NeTaskError::State(format!("invalid Win16 object handle {guest:04x}")))
    }
    fn host_value(&mut self, library: &str, name: &str, args: &[u32]) -> Result<u32, NeTaskError> {
        let mut memory = crate::host_memory::TaskMemory {
            loaded: &mut self.loaded,
            graphics: &mut self.graphics,
            arena: &mut self.arena,
        };
        let result = self
            .host
            .call(library, name, args, &mut memory)
            .map_err(NeTaskError::State)?;
        match result {
            wlam_host::runtime::Effect::Return(value) => Ok(value),
            other => Err(NeTaskError::State(format!(
                "unexpected host effect from {name}: {other:?}"
            ))),
        }
    }
    fn gdi(&mut self, ordinal: u16) -> Result<bool, NeTaskError> {
        match ordinal {
            19 | 20 => {
                let y = self.arg(0)? as i16 as i32 as u32;
                let x = self.arg(2)? as i16 as i32 as u32;
                let dc = self.handle(self.arg(4)?)?;
                let value = if ordinal == 19 { self.host_value("gdi32", "LineTo", &[dc, x, y])? }
                else {
                    let start = self.graphics.len();
                    self.graphics.resize(start + 8, 0);
                    let address = crate::host_memory::HOST_BASE + start as u32;
                    self.host_value("gdi32", "MoveToEx", &[dc, x, y, address])?;
                    let old_x = u32::from_le_bytes(self.graphics[start..start + 4].try_into().unwrap());
                    let old_y = u32::from_le_bytes(self.graphics[start + 4..start + 8].try_into().unwrap());
                    self.graphics.truncate(start);
                    (old_x & 0xffff) | ((old_y & 0xffff) << 16)
                };
                self.finish(6, value)?;
            }
            27 => {
                let mut args = vec![self.handle(self.arg(8)?)?];
                for offset in [6, 4, 2, 0] { args.push(self.arg(offset)? as i16 as i32 as u32); }
                let value = self.host_value("gdi32", "Rectangle", &args)?;
                self.finish(10, value)?;
            }
            91 => {
                let count = self.arg(0)? as i16 as i32 as u32;
                let text = self.long_arg(2)?;
                let dc = self.handle(self.arg(6)?)?;
                let start = self.graphics.len();
                self.graphics.resize(start + 8, 0);
                let output = crate::host_memory::HOST_BASE + start as u32;
                let success = self.host_value("gdi32", "GetTextExtentPoint32A", &[dc, text, count, output])?;
                let width = u32::from_le_bytes(self.graphics[start..start + 4].try_into().unwrap());
                let height = u32::from_le_bytes(self.graphics[start + 4..start + 8].try_into().unwrap());
                self.graphics.truncate(start);
                self.finish(8, if success == 0 { 0 } else { (width & 0xffff) | ((height & 0xffff) << 16) })?;
            }
            33 => {
                let count = u32::from(self.arg(0)?);
                let text = self.long_arg(2)?;
                let y = self.arg(6)? as i16 as i32 as u32;
                let x = self.arg(8)? as i16 as i32 as u32;
                let dc = self.handle(self.arg(10)?)?;
                let value = self.host_value("gdi32", "TextOutA", &[dc, x, y, text, count])?;
                self.finish(12, value)?;
            }
            1 | 9 => {
                let color = self.long_arg(0)?;
                let dc = self.handle(self.arg(4)?)?;
                let value = self.host_value("gdi32", if ordinal == 1 { "SetBkColor" } else { "SetTextColor" }, &[dc, color])?;
                self.finish(6, value)?;
            }
            2 => {
                let mode = u32::from(self.arg(0)?);
                let dc = self.handle(self.arg(2)?)?;
                let value = self.host_value("gdi32", "SetBkMode", &[dc, mode])?;
                self.finish(4, value)?;
            }
            29 | 34 | 35 => {
                let (name, offsets, handles, cleanup): (&str, &[u16], &[usize], u16) = match ordinal {
                    29 => ("PatBlt", &[12, 10, 8, 6, 4], &[0], 14),
                    34 => ("BitBlt", &[18, 16, 14, 12, 10, 8, 6, 4], &[0, 5], 20),
                    _ => ("StretchBlt", &[22, 20, 18, 16, 14, 12, 10, 8, 6, 4], &[0, 5], 24),
                };
                let mut args = Vec::new();
                for (i, offset) in offsets.iter().enumerate() {
                    let value = self.arg(*offset)?;
                    args.push(if handles.contains(&i) { self.handle(value)? } else { value as i16 as i32 as u32 });
                }
                args.push(self.long_arg(0)?);
                let value = self.host_value("gdi32", name, &args)?;
                self.finish(cleanup, value)?;
            }
            82 => {
                let output = self.long_arg(0)?;
                let capacity = self.arg(4)? as i16;
                let object = self.handle(self.arg(6)?)?;
                let Some(wlam_host::gdi::ObjectInfo::Bitmap { width, height, stride, bit_count, .. }) = self.host.gdi.object_info(object) else {
                    return Err(NeTaskError::State("Win16 GetObject layout not implemented for this object type".into()));
                };
                let mut bytes = Vec::with_capacity(14);
                for field in [0, width as u16, height as u16, stride as u16] { bytes.extend_from_slice(&field.to_le_bytes()); }
                bytes.extend_from_slice(&[1, bit_count as u8]);
                bytes.extend_from_slice(&0u32.to_le_bytes()); // Win16 LoadBitmap/CreateCompatibleBitmap expose DDBs; host DIB storage is private
                let count = if capacity <= 0 { 0 } else { (capacity as usize).min(bytes.len()) };
                if count != 0 { self.write_bytes(output, &bytes[..count])?; }
                self.finish(8, count as u32)?;
            }
            51 => {
                let height = self.arg(0)? as i16;
                let width = self.arg(2)? as i16;
                let dc = self.handle(self.arg(4)?)?;
                if !self.host.gdi.is_dc(dc) {
                    return Err(NeTaskError::State(
                        "CreateCompatibleBitmap requires a DC".into(),
                    ));
                }
                let result = if height <= 0 || width <= 0 {
                    0
                } else {
                    let size = (width as usize)
                        .checked_mul(height as usize)
                        .and_then(|n| n.checked_mul(4))
                        .filter(|n| *n <= 64 * 1024 * 1024)
                        .ok_or_else(|| NeTaskError::State("compatible bitmap too large".into()))?;
                    let start = self.graphics.len();
                    let address = crate::host_memory::HOST_BASE
                        .checked_add(
                            u32::try_from(start).map_err(|_| {
                                NeTaskError::State("graphics memory exhausted".into())
                            })?,
                        )
                        .ok_or_else(|| NeTaskError::State("graphics address overflow".into()))?;
                    self.graphics.resize(start + size, 0);
                    let memory = crate::host_memory::TaskMemory {
                        loaded: &mut self.loaded,
                        graphics: &mut self.graphics,
                        arena: &mut self.arena,
                    };
                    let object = self
                        .host
                        .gdi
                        .bitmap_pixels(&memory, width as u32, height as u32, address, 32)
                        .map_err(|e| {
                            NeTaskError::State(format!("CreateCompatibleBitmap: {e:?}"))
                        })?;
                    self.map_handle(object)?
                };
                self.finish(6, u32::from(result))?;
            }
            80 => {
                let index = self.arg(0)?;
                let dc = self.handle(self.arg(2)?)?;
                let value = self.host_value("gdi32", "GetDeviceCaps", &[dc, u32::from(index)])?;
                self.finish(4, value)?;
            }
            93 => {
                let output = self.long_arg(0)?;
                let dc = self.handle(self.arg(4)?)?;
                let mark = self.graphics.len();
                self.graphics.resize(mark + 56, 0);
                let address = crate::host_memory::HOST_BASE + mark as u32;
                let value = self.host_value("gdi32", "GetTextMetricsA", &[dc, address])?;
                if value != 0 {
                    let source = &self.graphics[mark..mark + 56];
                    let mut metrics = vec![0; 31];
                    for index in 0..11 {
                        let value = i32::from_le_bytes(
                            source[index * 4..index * 4 + 4].try_into().unwrap(),
                        ) as i16;
                        metrics[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
                    }
                    metrics[22..31].copy_from_slice(&source[44..53]);
                    self.write_bytes(output, &metrics)?;
                }
                self.graphics.truncate(mark);
                self.finish(6, value)?;
            }
            57 => {
                let pointer = self.long_arg(0)?;
                let bytes = self.loaded.read(
                    (pointer >> 16) as u16,
                    pointer & 0xffff,
                    50,
                    NeAccess::Read,
                )?;
                let height = i16::from_le_bytes([bytes[0], bytes[1]]) as i32;
                let width = i16::from_le_bytes([bytes[2], bytes[3]]) as i32;
                let weight = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
                let face = &bytes[18..];
                let end = face.iter().position(|b| *b == 0).unwrap_or(face.len());
                let font = self
                    .host
                    .gdi
                    .font(height, width, weight, bytes[10] != 0, &face[..end])
                    .map_err(|e| NeTaskError::State(format!("CreateFontIndirect: {e:?}")))?;
                let handle = self.map_handle(font)?;
                self.finish(4, u32::from(handle))?;
            }
            69 | 68 => {
                let guest = self.arg(0)?;
                let result = if let Some(&object) = self.handles.get(&guest) {
                    let result = if ordinal == 69 {
                        self.host.gdi.delete_object(object)
                    } else {
                        self.host.gdi.delete_dc(object)
                    };
                    if result.is_ok() {
                        self.handles.remove(&guest);
                        1
                    } else {
                        0
                    }
                } else {
                    0
                };
                self.finish(2, result)?;
            }
            52 => {
                self.handle(self.arg(0)?)?;
                let dc = self
                    .host
                    .gdi
                    .memory_dc()
                    .map_err(|e| NeTaskError::State(format!("CreateCompatibleDC: {e:?}")))?;
                let handle = self.map_handle(dc)?;
                self.finish(2, u32::from(handle))?;
            }
            45 => {
                let object = self.handle(self.arg(0)?)?;
                let dc = self.handle(self.arg(2)?)?;
                let previous = self
                    .host
                    .gdi
                    .select(dc, object)
                    .map_err(|e| NeTaskError::State(format!("SelectObject: {e:?}")))?;
                let previous = self.map_handle(previous)?;
                self.finish(4, u32::from(previous))?;
            }
            61 => {
                let color = self.long_arg(0)?;
                let width = self.arg(4)? as i16;
                let style = self.arg(6)? as i16;
                let object = self
                    .host
                    .gdi
                    .pen(style as u32, width.unsigned_abs() as u32, color)
                    .map_err(|e| NeTaskError::State(format!("CreatePen: {e:?}")))?;
                let handle = self.map_handle(object)?;
                self.finish(8, u32::from(handle))?;
            }
            66 => {
                let color = self.long_arg(0)?;
                let object = self
                    .host
                    .gdi
                    .solid_brush(color)
                    .map_err(|e| NeTaskError::State(format!("CreateSolidBrush: {e:?}")))?;
                let handle = self.map_handle(object)?;
                self.finish(4, u32::from(handle))?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn reg(&self, reg: u64) -> Result<u16, NeError> {
        Ok(self.machine.register(reg, 2)? as u16)
    }
    fn set(&mut self, reg: u64, value: u16) -> Result<(), NeError> {
        self.machine.set_register(reg, 2, u64::from(value))
    }
    fn word(&self, selector: u16, offset: u16) -> Result<u16, NeError> {
        let bytes = self
            .loaded
            .read(selector, u32::from(offset), 2, NeAccess::Read)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }
    fn put(&mut self, selector: u16, offset: u16, value: u16) -> Result<(), NeError> {
        self.loaded
            .write(selector, u32::from(offset), &value.to_le_bytes())
    }
    fn arg(&self, offset: u16) -> Result<u16, NeError> {
        self.word(
            self.reg(SS)?,
            self.reg(SP)?.wrapping_add(4).wrapping_add(offset),
        )
    }
    fn long_arg(&self, offset: u16) -> Result<u32, NeError> {
        Ok(u32::from(self.arg(offset)?) | (u32::from(self.arg(offset + 2)?) << 16))
    }
    fn finish(&mut self, args: u16, result: u32) -> Result<(), NeError> {
        self.set(AX, result as u16)?;
        self.set(DX, (result >> 16) as u16)?;
        self.ret(args)
    }
    fn ret(&mut self, args: u16) -> Result<(), NeError> {
        let ss = self.reg(SS)?;
        let sp = self.reg(SP)?;
        let ip = self.word(ss, sp)?;
        let cs = self.word(ss, sp.wrapping_add(2))?;
        self.set(SP, sp.wrapping_add(4).wrapping_add(args))?;
        self.machine
            .set_register(0x284, 4, (u64::from(cs) << 16) | u64::from(ip))
    }
    fn kernel(&mut self, ordinal: u16) -> Result<bool, NeTaskError> {
        match ordinal {
            91 => self.init_task()?,
            127..=129 => {
                let filename = self.long_arg(0)?;
                let filename = if filename == 0 {
                    b"C:\\WINDOWS\\WIN.INI".to_vec()
                } else {
                    self.string(filename)?
                };
                let path = crate::profile::path(&filename);
                let mut profile = crate::profile::Profile::parse(
                    self.files.get(&path).map(Vec::as_slice).unwrap_or(&[]),
                );
                if ordinal == 127 {
                    let default = self.arg(4)?;
                    let key = self.string(self.long_arg(6)?)?;
                    let section = self.string(self.long_arg(10)?)?;
                    let value = profile
                        .value(&section, &key)
                        .map(|v| String::from_utf8_lossy(v))
                        .and_then(|v| {
                            let v = v.trim();
                            if v.starts_with("0x") || v.starts_with("0X") {
                                i32::from_str_radix(&v[2..], 16).ok()
                            } else {
                                v.parse::<i32>().ok()
                            }
                        })
                        .map(|v| v as u16)
                        .unwrap_or(default);
                    self.finish(14, u32::from(value))?;
                } else if ordinal == 128 {
                    let count = usize::from(self.arg(4)?);
                    let output = self.long_arg(6)?;
                    let default = self.long_arg(10)?;
                    let key = self.long_arg(14)?;
                    let section = self.long_arg(18)?;
                    let mut value = Vec::new();
                    let multiple = section == 0 || key == 0;
                    if section == 0 {
                        for (name, _) in &profile.sections {
                            value.extend_from_slice(name);
                            value.push(0);
                        }
                    } else {
                        let section = self.string(section)?;
                        if key == 0 {
                            if let Some(keys) = profile.keys(&section) {
                                for (name, _) in keys {
                                    value.extend_from_slice(name);
                                    value.push(0);
                                }
                            }
                        } else {
                            let key = self.string(key)?;
                            value = if let Some(value) = profile.value(&section, &key) {
                                value.to_vec()
                            } else if default != 0 {
                                self.string(default)?
                            } else {
                                Vec::new()
                            };
                        }
                    }
                    let result = if count == 0 {
                        0
                    } else if multiple {
                        let length = value.len();
                        value.push(0);
                        if length == 0 {
                            value.push(0);
                        }
                        if value.len() > count {
                            value.truncate(count);
                            value[count - 1] = 0;
                            if count > 1 {
                                value[count - 2] = 0;
                            }
                            count.saturating_sub(2)
                        } else {
                            length.saturating_sub(1)
                        }
                    } else {
                        let length = value.len().min(count - 1);
                        value.truncate(length);
                        value.push(0);
                        length
                    };
                    if count != 0 {
                        self.write_bytes(output, &value)?;
                    }
                    self.finish(22, result as u32)?;
                } else {
                    let value = self.long_arg(4)?;
                    let key = self.long_arg(8)?;
                    let section = self.long_arg(12)?;
                    if section != 0 {
                        let section = self.string(section)?;
                        let key = if key == 0 {
                            None
                        } else {
                            Some(self.string(key)?)
                        };
                        let value = if value == 0 {
                            None
                        } else {
                            Some(self.string(value)?)
                        };
                        profile.set(&section, key.as_deref(), value.as_deref());
                        self.files.insert(path, profile.serialize());
                    }
                    self.finish(16, 1)?;
                }
            }
            47 => {
                let pointer = self.long_arg(0)?;
                let value = if pointer == 0 {
                    self.context.instance
                } else if pointer <= 0xffff {
                    let handle = pointer as u16;
                    if handle == self.context.instance
                        || (0xf000..0xf000 + self.image.module_references.len() as u16)
                            .contains(&handle)
                    {
                        handle
                    } else {
                        0
                    }
                } else {
                    let name = self.string(pointer)?;
                    let normalized = module_stem(&name);
                    let resident_offset = self
                        .raw
                        .get(
                            self.image.ne_offset as usize + 0x26
                                ..self.image.ne_offset as usize + 0x28,
                        )
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .unwrap_or(0);
                    let resident = if resident_offset != 0 {
                        let at = self.image.ne_offset as usize + usize::from(resident_offset);
                        self.raw
                            .get(at)
                            .and_then(|length| self.raw.get(at + 1..at + 1 + usize::from(*length)))
                    } else {
                        None
                    };
                    if normalized == module_stem(&self.executable_path)
                        || resident.is_some_and(|name| normalized == module_stem(name))
                    {
                        self.context.instance
                    } else {
                        self.image
                            .module_references
                            .iter()
                            .position(|module| module_stem(&module.name) == normalized)
                            .map(|index| 0xf000 + index as u16)
                            .unwrap_or(0)
                    }
                };
                self.finish(4, u32::from(value))?;
            }
            3 => self.finish(0, 0x0616_0a03)?, // DOS 6.22, Windows 3.10
            132 => self.finish(0, 0x413)?,
            131 => self.finish(0, u32::from(self.context.environment_selector) << 16)?,
            49 => {
                let count = usize::from(self.arg(0)?);
                let buffer = self.long_arg(2)?;
                let module = self.arg(6)?;
                let value = if module == 0 || module == self.context.instance {
                    let length = count.saturating_sub(1).min(self.executable_path.len());
                    if count != 0 {
                        let mut path = self.executable_path[..length].to_vec();
                        path.push(0);
                        self.write_bytes(buffer, &path)?;
                    }
                    length as u32
                } else {
                    0
                };
                self.finish(8, value)?;
            }
            88 => {
                let source = self.long_arg(0)?;
                let destination = self.long_arg(4)?;
                let mut bytes = self.string(source)?;
                bytes.push(0);
                self.write_bytes(destination, &bytes)?;
                self.finish(8, destination)?;
            }
            90 => {
                let address = self.long_arg(0)?;
                let length = self.string(address)?.len();
                self.finish(4, length as u32)?;
            }
            51 => {
                let _instance = self.arg(0)?;
                let procedure = self.long_arg(2)?;
                self.loaded.translate(
                    (procedure >> 16) as u16,
                    procedure & 0xffff,
                    1,
                    NeAccess::Execute,
                )?;
                let selector = self.loaded.add_region_auto(8, true, true)?;
                let mut code = vec![0xb8];
                code.extend_from_slice(&self.reg(DS)?.to_le_bytes());
                code.push(0xea);
                code.extend_from_slice(&procedure.to_le_bytes());
                self.loaded.write(selector, 0, &code)?;
                let thunk = u32::from(selector) << 16;
                self.thunks.insert(thunk, selector);
                self.finish(6, thunk)?;
            }
            52 => {
                let thunk = self.long_arg(0)?;
                if let Some(selector) = self.thunks.remove(&thunk) {
                    self.loaded.remove_region(selector)?;
                }
                self.finish(4, 0)?;
            }
            23 | 24 => {
                let selector = self.arg(0)?;
                self.finish(
                    2,
                    u32::from(if selector == 0xffff {
                        self.reg(DS)?
                    } else {
                        selector
                    }),
                )?;
            }
            30 => {
                self.arg(0)?;
                self.finish(2, 0)?;
            } // single cooperative task, no posted events
            102 => {
                self.dos()?;
                if self.exit.is_none() {
                    self.ret(0)?;
                }
            }
            5 => {
                let size = self.arg(0)?;
                let flags = self.arg(2)?;
                let ds = self.reg(DS)?;
                let handle = if let Some(heap) = self.heaps.get_mut(&ds) {
                    heap.allocate(&mut self.loaded, flags, size)?
                } else {
                    0
                };
                self.finish(4, u32::from(handle))?;
            }
            6 => {
                let flags = self.arg(0)?;
                let size = self.arg(2)?;
                let handle = self.arg(4)?;
                let ds = self.reg(DS)?;
                let result = if let Some(heap) = self.heaps.get_mut(&ds) { heap.reallocate(&mut self.loaded, handle, flags, size)? } else { 0 };
                self.finish(6, u32::from(result))?;
            }
            7..=10 => {
                let handle = self.arg(0)?;
                let ds = self.reg(DS)?;
                let value = if let Some(heap) = self.heaps.get_mut(&ds) {
                    match ordinal {
                        7 => heap.free(&mut self.loaded, handle)?,
                        8 => heap.lock(&mut self.loaded, handle)?,
                        9 => heap.unlock(&mut self.loaded, handle)?,
                        _ => heap.size(handle),
                    }
                } else {
                    0
                };
                let value = if ordinal == 8 && value != 0 {
                    (u32::from(ds) << 16) | u32::from(value)
                } else {
                    u32::from(value)
                };
                self.finish(2, value)?;
            }
            25 => {
                let _minimum = self.long_arg(0)?;
                // Allocations have independent backing storage, so there is no
                // fragmentation to compact. Report the largest block the current
                // single-selector GlobalAlloc implementation can actually supply.
                let available = self.loaded.selectors.iter().map(|s| s.selector).max()
                    .is_none_or(|last| last.checked_add(8).is_some());
                self.finish(4, if available { 65536 } else { 0 })?;
            }
            15 => {
                let size = self.long_arg(0)?;
                let flags = self.arg(4)?;
                let value = if size > 65536 {
                    0
                } else {
                    let sel = self.loaded.add_region_auto(size.max(1), true, false)?;
                    let handle = if flags & 2 != 0 { sel - 1 } else { sel };
                    self.globals.insert(handle, (sel, 0));
                    handle
                };
                self.finish(6, u32::from(value))?;
            }
            17..=20 => {
                let handle = self.arg(0)?;
                let selector = self.globals.get(&handle).map(|v| v.0).unwrap_or(handle);
                let mapped = self
                    .loaded
                    .selectors
                    .iter()
                    .find(|s| s.selector == selector)
                    .map(|s| s.limit);
                let value = match ordinal {
                    17 => {
                        if self.globals.remove(&handle).is_some() {
                            self.loaded.remove_region(selector)?;
                            0
                        } else {
                            u32::from(handle)
                        }
                    }
                    18 => {
                        if mapped.is_some() {
                            if let Some(v) = self.globals.get_mut(&handle) {
                                v.1 = v.1.saturating_add(1);
                            }
                            u32::from(selector) << 16
                        } else {
                            0
                        }
                    }
                    19 => {
                        if let Some(v) = self.globals.get_mut(&handle) {
                            v.1 = v.1.saturating_sub(1);
                            u32::from(v.1 != 0)
                        } else {
                            0
                        }
                    }
                    _ => mapped.unwrap_or(0),
                };
                self.finish(2, value)?;
            }
            60 => {
                let resource_type = self.long_arg(0)?;
                let name = self.long_arg(4)?;
                let _module = self.arg(8)?;
                let id = self.loaded.resource_identifier_from_pointer(name)?;
                let kind = self
                    .loaded
                    .resource_identifier_from_pointer(resource_type)?;
                let value = self
                    .resources
                    .find_resource(&self.image, &kind, &id)?
                    .map(|v| v.0)
                    .unwrap_or(0);
                self.finish(10, u32::from(value))?;
            }
            61 => {
                let info = NeResourceInfoHandle(self.arg(0)?);
                let _module = self.arg(2)?;
                let value = self
                    .resources
                    .load_resource(&self.image, &self.raw, &mut self.loaded, info)?
                    .map(|v| v.0)
                    .unwrap_or(0);
                self.finish(4, u32::from(value))?;
            }
            62 => {
                let handle = NeGlobalResourceHandle(self.arg(0)?);
                let value = self
                    .resources
                    .lock_resource(&self.loaded, handle)?
                    .map(|v| (u32::from(v.selector) << 16) | u32::from(v.offset))
                    .unwrap_or(0);
                self.finish(2, value)?;
            }
            63 => {
                let handle = NeGlobalResourceHandle(self.arg(0)?);
                let value = self.resources.free_resource(&mut self.loaded, handle)?;
                self.finish(2, u32::from(!value))?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn init_task(&mut self) -> Result<(), NeTaskError> {
        if self.initialized {
            return Err(NeTaskError::State("InitTask called twice".into()));
        }
        let ds = self.reg(DS)?;
        let ss = self.reg(SS)?;
        let top = self.reg(SP)?.wrapping_add(4);
        let limit = top.saturating_sub(self.reg(BX)?).saturating_add(150);
        self.put(ds, 10, limit)?;
        self.put(ds, 12, top)?;
        self.put(ds, 14, top)?;
        let heap_size = self.reg(CX)?;
        if heap_size != 0 {
            let heap = LocalHeap::new(&mut self.loaded, ds, heap_size)?;
            self.heaps.insert(ds, heap);
        }
        self.ret(0)?;
        let sp = self.reg(SP)?.wrapping_sub(2);
        self.put(ss, sp, 0)?;
        self.set(SP, sp)?;
        self.set(AX, 1)?;
        self.set(CX, limit)?;
        self.set(DX, 1)?;
        self.set(SI, self.context.previous_instance)?;
        self.set(DI, self.context.instance)?;
        self.set(ES, self.context.psp_selector)?;
        let mut skip = 0;
        while self
            .context
            .command_line
            .get(skip)
            .is_some_and(|b| matches!(b, b' ' | b'\t'))
        {
            skip += 1;
        }
        self.set(
            BX,
            if self.context.command_line.is_empty() {
                0x80
            } else {
                0x81 + skip.min(126) as u16
            },
        )?;
        self.initialized = true;
        Ok(())
    }
    fn dos(&mut self) -> Result<(), NeTaskError> {
        let ax = self.reg(AX)?;
        match ax >> 8 {
            0x2a|0x2c=>{
                let start=self.graphics.len();self.graphics.resize(start+16,0);
                let address=crate::host_memory::HOST_BASE+start as u32;
                self.host_value("kernel32","GetLocalTime",&[address])?;
                let time=self.graphics[start..start+16].chunks_exact(2).map(|b|u16::from_le_bytes([b[0],b[1]])).collect::<Vec<_>>();
                self.graphics.truncate(start);
                if ax>>8==0x2a {self.set(AX,(ax&0xff00)|time[2])?;self.set(CX,time[0])?;self.set(DX,(time[1]<<8)|time[3])?;}
                else {self.set(CX,(time[4]<<8)|time[5])?;self.set(DX,(time[6]<<8)|(time[7]/10))?;}
            }
            0x30 => {
                self.set(AX, 0x1606)?;
                self.set(BX, 0xff00)?;
                self.set(CX, 0)?;
            }
            0x35 => {
                let address = self.vectors[usize::from(ax & 0xff)];
                self.set(ES, address.selector)?;
                self.set(BX, address.offset)?;
            }
            0x25 => {
                let address = NeFarAddress {
                    selector: self.reg(DS)?,
                    offset: self.reg(DX)?,
                };
                self.loaded.translate(
                    address.selector,
                    u32::from(address.offset),
                    1,
                    NeAccess::Execute,
                )?;
                self.vectors[usize::from(ax & 0xff)] = address;
            }
            0x4c => self.exit = Some(ax & 0xff),
            _ => return Err(NeTaskError::UnsupportedDos(ax)),
        }
        Ok(())
    }
}

fn module_stem(name: &[u8]) -> Vec<u8> {
    let name = name
        .rsplit(|c| matches!(c, b'/' | b'\\'))
        .next()
        .unwrap_or(name);
    let mut name = name.to_ascii_uppercase();
    if name.ends_with(b".EXE") || name.ends_with(b".DLL") {
        name.truncate(name.len() - 4);
    }
    name
}

#[derive(Debug)]
struct Arena {
    length: u16,
    used: bool,
}
#[derive(Debug, Clone)]
struct LocalBlock {
    data: u16,
    size: u16,
    locks: u8,
    table: Option<u16>,
}
#[derive(Debug)]
struct LocalHeap {
    selector: u16,
    info: u16,
    first: u16,
    last: u16,
    arenas: BTreeMap<u16, Arena>,
    blocks: BTreeMap<u16, LocalBlock>,
}
impl LocalHeap {
    fn new(memory: &mut NeLoadedImage, selector: u16, size: u16) -> Result<Self, NeTaskError> {
        let limit = memory
            .selectors
            .iter()
            .find(|s| s.selector == selector)
            .ok_or(NeError::MissingSelector { selector })?
            .limit
            .min(65535);
        let end = limit.saturating_sub(1);
        let first = (end.saturating_sub(u32::from(size)).max(16) + 3) & !3;
        let last = end.saturating_sub(12) & !3;
        let free = first + 12 + 48;
        if free + 12 >= last {
            return Err(NeTaskError::State("local heap too small".into()));
        }
        let first = first as u16;
        let last = last as u16;
        let free = free as u16;
        let info = first + 16;
        let mut heap = Self {
            selector,
            info,
            first,
            last,
            arenas: BTreeMap::new(),
            blocks: BTreeMap::new(),
        };
        heap.arenas.insert(
            first,
            Arena {
                length: 12,
                used: true,
            },
        );
        heap.arenas.insert(
            first + 12,
            Arena {
                length: 48,
                used: true,
            },
        );
        heap.arenas.insert(
            free,
            Arena {
                length: last - free,
                used: false,
            },
        );
        heap.arenas.insert(
            last,
            Arena {
                length: 12,
                used: false,
            },
        );
        memory.write(selector, u32::from(info), &[0; 42])?;
        for (offset, value) in [
            (6, first),
            (10, last),
            (24, 32),
            (36, 512),
            (38, last - free),
            (40, 0x484c),
        ] {
            heap.put(memory, info + offset, value)?;
        }
        heap.put(memory, 6, info)?;
        heap.sync(memory)?;
        Ok(heap)
    }
    fn put(&self, memory: &mut NeLoadedImage, offset: u16, value: u16) -> Result<(), NeError> {
        memory.write(self.selector, u32::from(offset), &value.to_le_bytes())
    }
    fn sync(&self, memory: &mut NeLoadedImage) -> Result<(), NeError> {
        let nodes: Vec<_> = self.arenas.iter().collect();
        let free: Vec<u16> = std::iter::once(self.first)
            .chain(self.arenas.iter().filter(|(_, a)| !a.used).map(|(&p, _)| p))
            .collect();
        for (i, (&at, arena)) in nodes.iter().enumerate() {
            let previous = if i == 0 { at } else { *nodes[i - 1].0 };
            let next = if i + 1 == nodes.len() {
                at
            } else {
                *nodes[i + 1].0
            };
            self.put(memory, at, previous | u16::from(arena.used))?;
            self.put(memory, at + 2, next)?;
            if !arena.used || at == self.first {
                let index = free.iter().position(|p| *p == at).unwrap();
                self.put(memory, at + 4, arena.length)?;
                self.put(memory, at + 6, free[index.saturating_sub(1)])?;
                self.put(memory, at + 8, free[(index + 1).min(free.len() - 1)])?;
            }
        }
        self.put(memory, self.info + 4, nodes.len() as u16)
    }
    fn reserve(&mut self, size: u16) -> Option<u16> {
        let needed = ((u32::from(size).max(1) + 4 + 3) & !3).max(12);
        let (&at, arena) = self
            .arenas
            .iter()
            .find(|(p, a)| **p != self.last && !a.used && u32::from(a.length) >= needed)?;
        let length = arena.length;
        let used = if u32::from(length) - needed >= 12 {
            needed as u16
        } else {
            length
        };
        self.arenas.insert(
            at,
            Arena {
                length: used,
                used: true,
            },
        );
        if used < length {
            self.arenas.insert(
                at + used,
                Arena {
                    length: length - used,
                    used: false,
                },
            );
        }
        Some(at + 4)
    }
    fn release(&mut self, pointer: u16) {
        if let Some(arena) = self.arenas.get_mut(&(pointer - 4)) {
            arena.used = false;
        }
        loop {
            let entries: Vec<_> = self
                .arenas
                .iter()
                .map(|(&p, a)| (p, a.length, a.used))
                .collect();
            let pair = entries
                .windows(2)
                .find(|w| !w[0].2 && !w[1].2 && w[1].0 != self.last);
            let Some(w) = pair else {
                break;
            };
            let (at, len, _) = w[0];
            let (next, extra, _) = w[1];
            self.arenas.get_mut(&at).unwrap().length = len + extra;
            self.arenas.remove(&next);
        }
    }
    fn allocate(
        &mut self,
        memory: &mut NeLoadedImage,
        flags: u16,
        size: u16,
    ) -> Result<u16, NeError> {
        let movable = flags & 2 != 0;
        let Some(data_size) = size.checked_add(if movable { 2 } else { 0 }) else {
            return Ok(0);
        };
        let Some(raw) = self.reserve(data_size) else {
            return Ok(0);
        };
        let data = raw + if movable { 2 } else { 0 };
        let table = if movable {
            match self.reserve(8) {
                Some(p) => Some(p),
                None => {
                    self.release(raw);
                    self.sync(memory)?;
                    return Ok(0);
                }
            }
        } else {
            None
        };
        let handle = table.map(|p| p + 2).unwrap_or(data);
        if movable {
            self.put(memory, raw, handle)?;
            self.put(memory, handle, data)?;
            self.put(memory, handle + 2, 0)?;
        }
        if flags & 0x40 != 0 {
            memory.write(self.selector, u32::from(data), &vec![0; usize::from(size)])?;
        }
        self.blocks.insert(
            handle,
            LocalBlock {
                data,
                size,
                locks: 0,
                table,
            },
        );
        self.sync(memory)?;
        Ok(handle)
    }
    fn reallocate(&mut self, memory: &mut NeLoadedImage, handle: u16, flags: u16, size: u16) -> Result<u16, NeError> {
        let Some(block) = self.blocks.get(&handle).cloned() else { return Ok(0); };
        if flags & 0x80 != 0 { return Ok(handle); }
        let movable = block.table.is_some();
        if size == 0 {
            if !movable || block.locks != 0 { return Ok(0); }
            if block.data != 0 { self.release(block.data - 2); }
            let entry = self.blocks.get_mut(&handle).unwrap();
            entry.data = 0; entry.size = 0;
            self.put(memory, handle, 0)?;
            self.put(memory, handle + 2, 0x40)?;
            self.sync(memory)?;
            return Ok(handle);
        }
        let Some(data_size) = size.checked_add(if movable { 2 } else { 0 }) else { return Ok(0); };
        let needed = ((u32::from(data_size) + 7) & !3).max(12);
        let old_raw = if block.data == 0 { None } else { Some(block.data - if movable { 2 } else { 0 }) };
        if let Some(raw) = old_raw {
            let at = raw - 4;
            let current = self.arenas[&at].length;
            let next_at = at + current;
            let next_length = self.arenas.get(&next_at).filter(|arena| next_at != self.last && !arena.used).map_or(0, |arena| arena.length);
            let available = if needed > u32::from(current) { u32::from(current) + u32::from(next_length) } else { u32::from(current) };
            if needed <= available {
                if needed > u32::from(current) && next_length != 0 { self.arenas.remove(&next_at); }
                let used = if available - needed >= 12 { needed } else { available } as u16;
                self.arenas.get_mut(&at).unwrap().length = used;
                if u32::from(used) < available {
                    let free_at = at + used;
                    self.arenas.insert(free_at, Arena { length: (available - u32::from(used)) as u16, used: false });
                    self.release(free_at + 4);
                }
                if flags & 0x40 != 0 && size > block.size {
                    memory.write(self.selector, u32::from(block.data) + u32::from(block.size), &vec![0; usize::from(size - block.size)])?;
                }
                self.blocks.get_mut(&handle).unwrap().size = size;
                self.sync(memory)?;
                return Ok(handle);
            }
        }
        if (movable && block.locks != 0) || (!movable && flags & 2 == 0) { return Ok(0); }
        let Some(raw) = self.reserve(data_size) else { return Ok(0); };
        let data = raw + if movable { 2 } else { 0 };
        let count = size.min(block.size);
        let previous = if count == 0 { Vec::new() } else { memory.read(self.selector, u32::from(block.data), u32::from(count), NeAccess::Read)? };
        if flags & 0x40 != 0 { memory.write(self.selector, u32::from(data), &vec![0; usize::from(size)])?; }
        memory.write(self.selector, u32::from(data), &previous)?;
        if let Some(old_raw) = old_raw { self.release(old_raw); }
        let result = if movable { handle } else { data };
        self.blocks.remove(&handle);
        self.blocks.insert(result, LocalBlock { data, size, locks: block.locks, table: block.table });
        if movable { self.put(memory, raw, handle)?; self.put(memory, handle, data)?; self.put(memory, handle + 2, u16::from(block.locks) << 8)?; }
        self.sync(memory)?;
        Ok(result)
    }
    fn free(&mut self, memory: &mut NeLoadedImage, handle: u16) -> Result<u16, NeError> {
        if handle == 0 {
            return Ok(0);
        }
        let Some(block) = self.blocks.remove(&handle) else {
            return Ok(handle);
        };
        if block.data != 0 { self.release(block.data - if block.table.is_some() { 2 } else { 0 }); }
        if let Some(table) = block.table {
            self.release(table);
        }
        self.sync(memory)?;
        Ok(0)
    }
    fn lock(&mut self, memory: &mut NeLoadedImage, handle: u16) -> Result<u16, NeError> {
        let Some(block) = self.blocks.get_mut(&handle) else {
            return Ok(0);
        };
        if block.data == 0 { return Ok(0); }
        if block.table.is_some() {
            block.locks = block.locks.saturating_add(1).min(254);
            memory.write(self.selector, u32::from(handle + 3), &[block.locks])?;
        }
        Ok(block.data)
    }
    fn unlock(&mut self, memory: &mut NeLoadedImage, handle: u16) -> Result<u16, NeError> {
        let Some(block) = self.blocks.get_mut(&handle) else {
            return Ok(0);
        };
        if block.table.is_none() {
            return Ok(0);
        }
        block.locks = block.locks.saturating_sub(1);
        memory.write(self.selector, u32::from(handle + 3), &[block.locks])?;
        Ok(u16::from(block.locks))
    }
    fn size(&self, handle: u16) -> u16 {
        self.blocks.get(&handle).map(|b| b.size).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_far_call_to_init_task_returns_registers_and_a_zero_task_frame() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut raw = vec![0; 0x250];
                raw[..2].copy_from_slice(b"MZ");
                raw[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
                raw[0x40..0x42].copy_from_slice(b"NE");
                let put = |raw: &mut [u8], offset: usize, value: u16| {
                    raw[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
                };
                for (offset, value) in [
                    (0x04, 0x5a),
                    (0x06, 1),
                    (0x0c, 0x302),
                    (0x0e, 2),
                    (0x10, 0x1000),
                    (0x12, 0x100),
                    (0x16, 1),
                    (0x1a, 2),
                    (0x1c, 2),
                    (0x1e, 1),
                    (0x22, 0x40),
                    (0x28, 0x50),
                    (0x2a, 0x52),
                ] {
                    put(&mut raw, 0x40 + offset, value);
                }
                for (offset, value) in [
                    (0x80, 0x200),
                    (0x82, 64),
                    (0x84, 0x100),
                    (0x86, 64),
                    (0x88, 0),
                    (0x8a, 0),
                    (0x8c, 1),
                    (0x8e, 256),
                ] {
                    put(&mut raw, offset, value);
                }
                raw[0x92..0x99].copy_from_slice(b"\x06KERNEL");
                raw[0x200..0x20c].copy_from_slice(&[
                    0x33, 0xed, 0x55, 0x9a, 0xff, 0xff, 0, 0, 0x8c, 0x06, 0x20, 0,
                ]);
                put(&mut raw, 0x240, 1);
                raw[0x242..0x24a].copy_from_slice(&[3, 1, 4, 0, 1, 0, 91, 0]);
                let mut task = NeTask::load(raw, b" /test".to_vec()).unwrap();
                for _ in 0..3 {
                    assert!(matches!(task.step().unwrap(), NeTaskEvent::Instruction(_)));
                }
                assert!(matches!(
                    task.step().unwrap(),
                    NeTaskEvent::Api {
                        symbol: NeImportSymbol::Ordinal(91),
                        ..
                    }
                ));
                assert_eq!((task.machine.cs(), task.machine.ip()), (0x100, 8));
                assert_eq!(task.reg(SP).unwrap(), 512);
                assert_eq!(task.word(0x108, 512).unwrap(), 0);
                assert_eq!(task.reg(AX).unwrap(), 1);
                assert_eq!(task.reg(CX).unwrap(), 408);
                assert_eq!(task.reg(DX).unwrap(), 1);
                assert_eq!(task.reg(DI).unwrap(), 0x108);
                assert_eq!(task.reg(SI).unwrap(), 0);
                assert_eq!(task.reg(BX).unwrap(), 0x82);
                assert!(task.word(0x108, 6).unwrap() > 512);
                task.step().unwrap();
                assert_eq!(task.word(0x108, 0x20).unwrap(), task.context.psp_selector);
                // GlobalCompact must advertise a block GlobalAlloc can supply,
                // while both APIs honor their different Pascal argument sizes.
                task.set(SP, 0x200).unwrap();
                task.put(0x108, 0x200, 8).unwrap();
                task.put(0x108, 0x202, 0x100).unwrap();
                task.put(0x108, 0x204, 0).unwrap();
                task.put(0x108, 0x206, 0).unwrap();
                assert!(task.kernel(25).unwrap());
                assert_eq!((task.reg(DX).unwrap(), task.reg(AX).unwrap()), (1, 0));
                assert_eq!(task.reg(SP).unwrap(), 0x208);
                task.set(SP, 0x200).unwrap();
                task.put(0x108, 0x206, 1).unwrap();
                task.put(0x108, 0x208, 2).unwrap();
                task.kernel(15).unwrap();
                let handle = task.reg(AX).unwrap();
                let selector = task.globals[&handle].0;
                assert_eq!(task.loaded.read(selector, 65535, 1, NeAccess::Read).unwrap(), [0]);
                assert_eq!(task.reg(SP).unwrap(), 0x20a);
            })
            .unwrap()
            .join()
            .unwrap();
    }
    #[test]
    fn local_reallocation_preserves_handles_locks_bytes_and_zero_initialized_growth() {
        let mut memory = NeLoadedImage { selectors: Vec::new(), storage: Vec::new(), next_allocation_id: 1 };
        memory.add_region(0x28, 0x8000, true, false).unwrap();
        let mut heap = LocalHeap::new(&mut memory, 0x28, 0x2000).unwrap();
        let fixed = heap.allocate(&mut memory, 0x40, 32).unwrap();
        let neighbor = heap.allocate(&mut memory, 0x40, 32).unwrap();
        memory.write(0x28, u32::from(fixed), &[0x5a; 32]).unwrap();
        memory.write(0x28, u32::from(neighbor), &[0x66; 32]).unwrap();
        assert_eq!(heap.reallocate(&mut memory, fixed, 0x40, 128).unwrap(), 0);
        let moved = heap.reallocate(&mut memory, fixed, 0x42, 128).unwrap();
        assert_ne!(moved, 0); assert_ne!(moved, fixed);
        assert_eq!(memory.read(0x28, u32::from(moved), 32, NeAccess::Read).unwrap(), [0x5a; 32]);
        assert_eq!(memory.read(0x28, u32::from(moved) + 32, 96, NeAccess::Read).unwrap(), [0; 96]);
        let movable = heap.allocate(&mut memory, 0x42, 32).unwrap();
        let barrier = heap.allocate(&mut memory, 0x40, 64).unwrap();
        assert_ne!(barrier, 0);
        let original = heap.lock(&mut memory, movable).unwrap();
        memory.write(0x28, u32::from(original), &[0x77; 32]).unwrap();
        assert_eq!(heap.reallocate(&mut memory, movable, 0x42, 256).unwrap(), 0);
        heap.unlock(&mut memory, movable).unwrap();
        assert_eq!(heap.reallocate(&mut memory, movable, 0x42, 256).unwrap(), movable);
        let resized = heap.lock(&mut memory, movable).unwrap();
        assert_eq!(memory.read(0x28, u32::from(resized), 32, NeAccess::Read).unwrap(), [0x77; 32]);
        assert_eq!(memory.read(0x28, u32::from(resized) + 32, 224, NeAccess::Read).unwrap(), [0; 224]);
        heap.unlock(&mut memory, movable).unwrap();
        assert_eq!(heap.reallocate(&mut memory, movable, 2, 0).unwrap(), movable);
        assert_eq!(heap.lock(&mut memory, movable).unwrap(), 0);
        assert_eq!(heap.reallocate(&mut memory, movable, 0x42, 64).unwrap(), movable);
        assert_eq!(heap.size(movable), 64);
        assert_eq!(memory.read(0x28, u32::from(neighbor), 32, NeAccess::Read).unwrap(), [0x66; 32]);
        memory.add_region(0x30, 0x8000, true, false).unwrap();
        let mut tail_heap = LocalHeap::new(&mut memory, 0x30, 0x2000).unwrap();
        let tail = tail_heap.allocate(&mut memory, 0x40, 16).unwrap();
        assert_eq!(tail_heap.reallocate(&mut memory, tail, 0x40, 64).unwrap(), tail);
        assert_eq!(tail_heap.reallocate(&mut memory, tail, 0x40, 8).unwrap(), tail);
        assert_eq!(tail_heap.size(tail), 8);
    }
    #[test]
    fn local_heap_reuses_freed_storage_without_corrupting_locked_blocks() {
        let mut memory = NeLoadedImage {
            selectors: Vec::new(),
            storage: Vec::new(),
            next_allocation_id: 1,
        };
        memory.add_region(0x28, 0x8000, true, false).unwrap();
        let mut heap = LocalHeap::new(&mut memory, 0x28, 0x2000).unwrap();
        let fixed = heap.allocate(&mut memory, 0x40, 100).unwrap();
        let movable = heap.allocate(&mut memory, 0x42, 100).unwrap();
        assert_eq!(fixed & 3, 0);
        assert_eq!(movable & 3, 2);
        assert_eq!(
            memory
                .read(0x28, u32::from(fixed), 100, NeAccess::Read)
                .unwrap(),
            [0; 100]
        );
        let data = heap.lock(&mut memory, movable).unwrap();
        memory.write(0x28, u32::from(data), &[0x5a; 100]).unwrap();
        assert_eq!(heap.lock(&mut memory, movable).unwrap(), data);
        assert_eq!(heap.unlock(&mut memory, movable).unwrap(), 1);
        assert_eq!(heap.free(&mut memory, fixed).unwrap(), 0);
        assert_eq!(heap.allocate(&mut memory, 0, 100).unwrap(), fixed);
        assert_eq!(
            memory
                .read(0x28, u32::from(data), 100, NeAccess::Read)
                .unwrap(),
            [0x5a; 100]
        );
        assert_eq!(heap.unlock(&mut memory, movable).unwrap(), 0);
        assert_eq!(heap.free(&mut memory, movable).unwrap(), 0);
        assert_eq!(heap.free(&mut memory, movable).unwrap(), movable);
    }
}
