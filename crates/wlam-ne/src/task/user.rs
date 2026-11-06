//! Win16 USER argument and packed-structure adapters.
use super::*;
impl NeTask {
    pub(super) fn user_basic(&mut self, ordinal: u16) -> Result<bool, NeTaskError> {
        match ordinal {
            414 => {
                let pointer = self.long_arg(0)?;
                let id = self.arg(4)?;
                let flags = self.arg(6)?;
                let item = self.arg(8)?;
                let menu = self.handle(self.arg(10)?)?;
                let text = if pointer == 0 {
                    Vec::new()
                } else {
                    self.string(pointer)?
                };
                let result = if menu == 0 {
                    0
                } else if let Some(bytes) = crate::templates::modify_menu_item(
                    self.menu_bytes(menu)?,
                    item,
                    flags & 0x400 != 0,
                    flags,
                    id,
                    &text,
                )
                .map_err(NeTaskError::State)?
                {
                    self.menu_overrides.insert(menu, bytes);
                    1
                } else {
                    0
                };
                self.finish(12, result)?;
            }
            412 | 413 => {
                let flags = self.arg(0)?;
                let item = self.arg(2)?;
                let menu = self.handle(self.arg(4)?)?;
                let result = if menu == 0 {
                    0
                } else if let Some(bytes) = crate::templates::remove_menu_item(
                    self.menu_bytes(menu)?,
                    item,
                    flags & 0x400 != 0,
                )
                .map_err(NeTaskError::State)?
                {
                    self.menu_overrides.insert(menu, bytes);
                    1
                } else {
                    0
                };
                self.finish(6, result)?;
            }
            31 | 272 => {
                let window = self.handle(self.arg(0)?)?;
                let style = self
                    .host
                    .windows
                    .get(window)
                    .map_err(|e| NeTaskError::State(format!("window state: {e:?}")))?
                    .style;
                self.finish(
                    2,
                    u32::from(
                        style
                            & if ordinal == 31 {
                                0x20000000
                            } else {
                                0x01000000
                            }
                            != 0,
                    ),
                )?;
            }
            232 => {
                let flags = u32::from(self.arg(0)?);
                let after = self.arg(10)?;
                let after = if flags & 4 != 0 {
                    0
                } else {
                    match after {
                        0 | 1 => u32::from(after),
                        0xfffe | 0xffff => after as i16 as i32 as u32,
                        _ => self.handle(after)?,
                    }
                };
                let window = self.handle(self.arg(12)?)?;
                let mut args = vec![window, after];
                for offset in [8, 6, 4, 2] {
                    args.push(self.arg(offset)? as i16 as i32 as u32);
                }
                args.push(flags);
                self.start_host("user32", "SetWindowPos", args, 14, false)?;
            }
            153 => {
                let flags = self.arg(0)?;
                if flags & 0x100 != 0 || flags & (0x200 | 0x1000) == 0 {
                    return Err(NeTaskError::State(
                        "Win16 ChangeMenu insertion/modification is not implemented".into(),
                    ));
                }
                let item = if flags & 0x200 != 0 || flags & 0x400 != 0 {
                    self.arg(8)?
                } else {
                    self.arg(2)?
                };
                let menu = self.handle(self.arg(10)?)?;
                let result = if menu == 0 {
                    0
                } else if let Some(bytes) = crate::templates::remove_menu_item(
                    self.menu_bytes(menu)?,
                    item,
                    flags & 0x400 != 0,
                )
                .map_err(NeTaskError::State)?
                {
                    self.menu_overrides.insert(menu, bytes);
                    1
                } else {
                    0
                };
                self.finish(12, result)?;
            }
            156 => {
                let revert = self.arg(0)? != 0;
                let guest_window = self.arg(2)?;
                let window = self.handle(guest_window)?;
                let style = self
                    .host
                    .windows
                    .get(window)
                    .map_err(|e| NeTaskError::State(format!("system menu window: {e:?}")))?
                    .style;
                let result = if revert {
                    self.system_menus.remove(&guest_window);
                    0
                } else if style & 0x80000 == 0 {
                    0
                } else if let Some(menu) = self.system_menus.get(&guest_window) {
                    *menu
                } else {
                    let mut raw = vec![0; 4];
                    for (id, name, enabled, end) in [
                        (0xf120u16, "&Restore", style & 0x21000000 != 0, false),
                        (0xf010, "&Move", true, false),
                        (0xf000, "&Size", style & 0x40000 != 0, false),
                        (0xf020, "Mi&nimize", style & 0x20000 != 0, false),
                        (0xf030, "Ma&ximize", style & 0x10000 != 0, false),
                        (0xf060, "&Close", true, true),
                    ] {
                        let flags = (if enabled { 0u16 } else { 1 }) | if end { 0x80 } else { 0 };
                        raw.extend_from_slice(&flags.to_le_bytes());
                        raw.extend_from_slice(&id.to_le_bytes());
                        raw.extend_from_slice(name.as_bytes());
                        raw.push(0);
                    }
                    let converted =
                        crate::templates::menu_to_win32(&raw).map_err(NeTaskError::State)?;
                    let address = crate::host_memory::HOST_BASE + self.graphics.len() as u32;
                    self.graphics.extend_from_slice(&converted);
                    let menu = self.map_handle(address)?;
                    self.system_menus.insert(guest_window, menu);
                    menu
                };
                self.finish(4, u32::from(result))?;
            }
            36 | 93 => {
                let capacity = u32::from(self.arg(0)?);
                let output = self.long_arg(2)?;
                let window = if ordinal == 36 {
                    self.handle(self.arg(6)?)?
                } else {
                    let id = u32::from(self.arg(6)?);
                    let dialog = self.handle(self.arg(8)?)?;
                    self.host_value("user32", "GetDlgItem", &[dialog, id])?
                };
                self.start_host(
                    "user32",
                    "SendMessageA",
                    vec![window, 0x0d, capacity, output],
                    if ordinal == 36 { 8 } else { 10 },
                    false,
                )?;
            }
            38 => {
                let window = self.handle(self.arg(0)?)?;
                self.start_host("user32", "SendMessageA", vec![window, 0x0e, 0, 0], 2, false)?;
            }
            96 => {
                let checked = u32::from(self.arg(0)?);
                let last = u32::from(self.arg(2)?);
                let first = u32::from(self.arg(4)?);
                let dialog = self.handle(self.arg(6)?)?;
                self.start_host(
                    "user32",
                    "CheckRadioButton",
                    vec![dialog, first, last, checked],
                    8,
                    false,
                )?;
            }
            125 | 127 => {
                let shift = if ordinal == 125 { 2 } else { 0 };
                let pointer = self.long_arg(shift)?;
                let window = self.handle(self.arg(shift + 4)?)?;
                let start = self.graphics.len();
                let address = if pointer == 0 {
                    0
                } else {
                    for value in self.rect16(pointer)? {
                        self.graphics
                            .extend_from_slice(&i32::from(value).to_le_bytes());
                    }
                    crate::host_memory::HOST_BASE + start as u32
                };
                let mut args = vec![window, address];
                if ordinal == 125 {
                    args.push(u32::from(self.arg(0)? != 0));
                }
                let value = self.host_value(
                    "user32",
                    if ordinal == 125 {
                        "InvalidateRect"
                    } else {
                        "ValidateRect"
                    },
                    &args,
                )?;
                self.graphics.truncate(start);
                self.finish(if ordinal == 125 { 8 } else { 6 }, value)?;
            }
            101 => {
                let dialog = self.handle(self.arg(10)?)?;
                let id = u32::from(self.arg(8)?);
                let message = self.arg(6)?;
                let (wp, lp) = self.message_from_win16(message, self.arg(4)?, self.long_arg(0)?)?;
                let child = self.host_value("user32", "GetDlgItem", &[dialog, id])?;
                let (message, wp, lp) = self.control_message16(child, message, wp, lp)?;
                self.start_host(
                    "user32",
                    "SendMessageA",
                    vec![child, u32::from(message), wp, lp],
                    12,
                    false,
                )?;
            }
            97 => {
                let check = u32::from(self.arg(0)?);
                let id = u32::from(self.arg(2)?);
                let dialog = self.handle(self.arg(4)?)?;
                self.start_host(
                    "user32",
                    "CheckDlgButton",
                    vec![dialog, id, check],
                    6,
                    false,
                )?;
            }
            98 => {
                let id = u32::from(self.arg(0)?);
                let dialog = self.handle(self.arg(2)?)?;
                let value = self.host_value("user32", "IsDlgButtonChecked", &[dialog, id])?;
                self.finish(4, value)?;
            }
            133 | 134 => {
                let writing = ordinal == 134;
                let shift = if writing { 2 } else { 0 };
                let index = self.arg(shift)? as i16;
                let window = self.handle(self.arg(shift + 2)?)?;
                let value = if writing { Some(self.arg(0)?) } else { None };
                let result = if index >= 0 {
                    u32::from(
                        self.host
                            .window_extra_word(window, index as u16, value)
                            .map_err(NeTaskError::State)?,
                    )
                } else {
                    if !matches!(index, -6 | -8 | -12) {
                        return Err(NeTaskError::State("invalid Win16 window word index".into()));
                    }
                    let mut args = vec![window, index as i32 as u32];
                    if let Some(value) = value {
                        args.push(if index == -8 {
                            self.handle(value)?
                        } else {
                            u32::from(value)
                        });
                    }
                    let previous = self.host_value(
                        "user32",
                        if writing {
                            "SetWindowLongA"
                        } else {
                            "GetWindowLongA"
                        },
                        &args,
                    )?;
                    if index == -8 {
                        u32::from(self.map_handle(previous)?)
                    } else {
                        previous & 0xffff
                    }
                };
                self.finish(if writing { 6 } else { 4 }, result)?;
            }
            108 | 109 => {
                let peek = ordinal == 109;
                let shift = if peek { 2 } else { 0 };
                let maximum = self.arg(shift)?;
                let minimum = self.arg(shift + 2)?;
                let window = self.handle(self.arg(shift + 4)?)?;
                let output = self.long_arg(shift + 6)?;
                self.loaded.translate(
                    (output >> 16) as u16,
                    output & 0xffff,
                    18,
                    NeAccess::Write,
                )?;
                let start = self.graphics.len();
                self.graphics.resize(start + 28, 0);
                let mut args = vec![
                    crate::host_memory::HOST_BASE + start as u32,
                    window,
                    u32::from(minimum),
                    u32::from(maximum),
                ];
                if peek {
                    args.push(u32::from(self.arg(0)?));
                }
                self.start_message_host(
                    if peek { "PeekMessageA" } else { "GetMessageA" },
                    args,
                    if peek { 12 } else { 10 },
                    Some(output),
                    start,
                )?;
            }
            113 | 114 => {
                let pointer = self.long_arg(0)?;
                let raw = self.loaded.read(
                    (pointer >> 16) as u16,
                    pointer & 0xffff,
                    18,
                    NeAccess::Read,
                )?;
                let short = |at: usize| u16::from_le_bytes(raw[at..at + 2].try_into().unwrap());
                let long = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
                let (wp, lp) = self.message_from_win16(short(2), short(4), long(6))?;
                let fields = [
                    self.handle(short(0))?,
                    u32::from(short(2)),
                    wp,
                    lp,
                    long(10),
                    short(14) as i16 as i32 as u32,
                    short(16) as i16 as i32 as u32,
                ];
                let start = self.graphics.len();
                for field in fields {
                    self.graphics.extend_from_slice(&field.to_le_bytes());
                }
                self.start_message_host(
                    if ordinal == 113 {
                        "TranslateMessage"
                    } else {
                        "DispatchMessageA"
                    },
                    vec![crate::host_memory::HOST_BASE + start as u32],
                    4,
                    None,
                    start,
                )?;
            }
            110 | 111 => {
                let window = self.handle(self.arg(8)?)?;
                let message = self.arg(6)?;
                let (wp, lp) = self.message_from_win16(message, self.arg(4)?, self.long_arg(0)?)?;
                let (message, wp, lp) = if ordinal == 111 {
                    self.control_message16(window, message, wp, lp)?
                } else {
                    (u32::from(message), wp, lp)
                };
                self.start_host(
                    "user32",
                    if ordinal == 110 {
                        "PostMessageA"
                    } else {
                        "SendMessageA"
                    },
                    vec![window, u32::from(message), wp, lp],
                    10,
                    false,
                )?;
            }
            34 => {
                let enabled = u32::from(self.arg(0)? != 0);
                let window = self.handle(self.arg(2)?)?;
                self.start_host("user32", "EnableWindow", vec![window, enabled], 4, false)?;
            }
            35 | 49 => {
                let window = self.handle(self.arg(0)?)?;
                let value = self.host_value(
                    "user32",
                    if ordinal == 35 {
                        "IsWindowEnabled"
                    } else {
                        "IsWindowVisible"
                    },
                    &[window],
                )?;
                self.finish(2, value)?;
            }
            157 => {
                let window = self.handle(self.arg(0)?)?;
                let menu = self.host_value("user32", "GetMenu", &[window])?;
                let menu = self.map_handle(menu)?;
                self.finish(2, u32::from(menu))?;
            }
            154 | 155 => {
                let flags = self.arg(0)?;
                let item = self.arg(2)?;
                let menu = self.handle(self.arg(4)?)?;
                if menu == 0 {
                    self.finish(6, 0xffff)?;
                    return Ok(true);
                }
                let mut bytes = self.menu_bytes(menu)?.to_vec();
                let found = crate::templates::menu_item_flags(&bytes, item, flags & 0x400 != 0)
                    .map_err(NeTaskError::State)?;
                let value = if let Some(offset) = found {
                    let old = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
                    let mask = if ordinal == 154 { 8 } else { 3 };
                    let next = (old & !mask) | (flags & mask);
                    bytes[offset..offset + 2].copy_from_slice(&next.to_le_bytes());
                    self.menu_overrides.insert(menu, bytes);
                    u32::from(old & mask)
                } else {
                    0xffff
                };
                self.finish(6, value)?;
            }
            81 => {
                let brush = self.handle(self.arg(0)?)?;
                let rect = self.rect16(self.long_arg(2)?)?;
                let dc = self.handle(self.arg(6)?)?;
                let start = self.graphics.len();
                for value in rect {
                    self.graphics
                        .extend_from_slice(&(i32::from(value)).to_le_bytes());
                }
                let address = crate::host_memory::HOST_BASE + start as u32;
                let value = self.host_value("user32", "FillRect", &[dc, address, brush])?;
                self.graphics.truncate(start);
                self.finish(8, value)?;
            }
            18 | 22 => {
                let window = self.handle(self.arg(0)?)?;
                self.start_host(
                    "user32",
                    if ordinal == 22 {
                        "SetFocus"
                    } else {
                        "SetCapture"
                    },
                    vec![window],
                    2,
                    true,
                )?;
            }
            19 | 23 => {
                self.start_host(
                    "user32",
                    if ordinal == 23 {
                        "GetFocus"
                    } else {
                        "ReleaseCapture"
                    },
                    Vec::new(),
                    0,
                    ordinal == 23,
                )?;
            }
            62 | 63 | 64 => {
                let (function, offsets, cleanup): (&str, &[u16], u16) = match ordinal {
                    62 => ("SetScrollPos", &[6, 4, 2, 0], 8),
                    63 => ("GetScrollPos", &[2, 0], 4),
                    _ => ("SetScrollRange", &[8, 6, 4, 2, 0], 10),
                };
                let mut args = Vec::new();
                for (i, offset) in offsets.iter().enumerate() {
                    let value = self.arg(*offset)?;
                    args.push(if i == 0 {
                        self.handle(value)?
                    } else {
                        value as i16 as i32 as u32
                    });
                }
                let result = self.host_value("user32", function, &args)?;
                self.finish(cleanup, result)?;
            }
            37 => {
                let text = self.long_arg(0)?;
                let window = self.handle(self.arg(4)?)?;
                self.start_host("user32", "SetWindowTextA", vec![window, text], 6, false)?;
            }
            39 | 40 => {
                let pointer = self.long_arg(0)?;
                let window = self.handle(self.arg(4)?)?;
                let start = self.graphics.len();
                self.graphics.resize(start + 64, 0);
                let address = crate::host_memory::HOST_BASE + start as u32;
                if ordinal == 40 {
                    let bytes = self.loaded.read(
                        (pointer >> 16) as u16,
                        pointer & 0xffff,
                        32,
                        NeAccess::Read,
                    )?;
                    let short =
                        |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
                    let fields = [
                        self.handle(short(0))?,
                        u32::from(short(2)),
                        short(4) as i16 as i32 as u32,
                        short(6) as i16 as i32 as u32,
                        short(8) as i16 as i32 as u32,
                        short(10) as i16 as i32 as u32,
                        u32::from(short(12)),
                        u32::from(short(14)),
                    ];
                    for (i, field) in fields.iter().enumerate() {
                        self.graphics[start + i * 4..start + i * 4 + 4]
                            .copy_from_slice(&field.to_le_bytes());
                    }
                    self.graphics[start + 32..start + 48].copy_from_slice(&bytes[16..32]);
                }
                let value = self.host_value(
                    "user32",
                    if ordinal == 39 {
                        "BeginPaint"
                    } else {
                        "EndPaint"
                    },
                    &[window, address],
                )?;
                let value = if ordinal == 39 {
                    let handle = self.map_handle(value)?;
                    let raw = self.graphics[start..start + 64].to_vec();
                    let mut converted = handle.to_le_bytes().to_vec();
                    for at in (4..32).step_by(4) {
                        converted.extend_from_slice(
                            &(u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as u16)
                                .to_le_bytes(),
                        );
                    }
                    converted.extend_from_slice(&raw[32..48]);
                    self.write_bytes(pointer, &converted)?;
                    u32::from(handle)
                } else {
                    value
                };
                self.graphics.truncate(start);
                self.finish(6, value)?;
            }
            71 => {
                let show = u32::from(self.arg(0)? != 0);
                let count = self.host_value("user32", "ShowCursor", &[show])?;
                self.finish(2, count)?;
            }
            135 | 136 => {
                let writing = ordinal == 136;
                let offset = if writing { 4 } else { 0 };
                let index = self.arg(offset)? as i16 as i32 as u32;
                let window = self.handle(self.arg(offset + 2)?)?;
                let mut args = vec![window, index];
                if writing {
                    let value = self.long_arg(0)?;
                    args.push(if index as i32 == -8 {
                        self.handle(value as u16)?
                    } else {
                        value
                    });
                }
                let value = self.host_value(
                    "user32",
                    if writing {
                        "SetWindowLongA"
                    } else {
                        "GetWindowLongA"
                    },
                    &args,
                )?;
                let value = if index as i32 == -8 {
                    u32::from(self.map_handle(value)?)
                } else {
                    value
                };
                self.finish(if writing { 8 } else { 4 }, value)?;
            }
            42 => {
                let command = self.arg(0)?;
                let window = self.handle(self.arg(2)?)?;
                self.start_host(
                    "user32",
                    "ShowWindow",
                    vec![window, u32::from(command)],
                    4,
                    false,
                )?;
            }
            124 => {
                let window = self.handle(self.arg(0)?)?;
                self.start_host("user32", "UpdateWindow", vec![window], 2, false)?;
            }
            56 => {
                let repaint = u32::from(self.arg(0)? != 0);
                let height = self.arg(2)? as i16 as i32 as u32;
                let width = self.arg(4)? as i16 as i32 as u32;
                let y = self.arg(6)? as i16 as i32 as u32;
                let x = self.arg(8)? as i16 as i32 as u32;
                let window = self.handle(self.arg(10)?)?;
                self.start_host(
                    "user32",
                    "MoveWindow",
                    vec![window, x, y, width, height, repaint],
                    12,
                    false,
                )?;
            }
            91 => {
                let id = self.arg(0)?;
                let window = self.handle(self.arg(2)?)?;
                let child = self.host_value("user32", "GetDlgItem", &[window, u32::from(id)])?;
                let child = self.map_handle(child)?;
                self.finish(4, u32::from(child))?;
            }
            92 => {
                let text = self.long_arg(0)?;
                let id = self.arg(4)?;
                let window = self.handle(self.arg(6)?)?;
                self.start_host(
                    "user32",
                    "SetDlgItemTextA",
                    vec![window, u32::from(id), text],
                    8,
                    false,
                )?;
            }
            87 => {
                let procedure = self.long_arg(0)?;
                let owner = self.handle(self.arg(4)?)?;
                let name = self.long_arg(6)?;
                let instance = self.arg(10)?;
                if instance != self.context.instance && instance != 0 {
                    return Err(NeTaskError::State(
                        "dialog instance is not the guest task".into(),
                    ));
                }
                let id = self.loaded.resource_identifier_from_pointer(name)?;
                let resource = self
                    .image
                    .find_resource(&NeResourceIdentifier::Ordinal(5), &id)
                    .ok_or_else(|| NeTaskError::State("Win16 dialog resource not found".into()))?;
                let bytes = crate::templates::dialog_to_win32(
                    self.image.resource_data(&self.raw, resource)?,
                )
                .map_err(NeTaskError::State)?;
                let host_id = 0x7000; // task-private resource slot; modal dialogs retain parsed templates
                let address = crate::host_memory::HOST_BASE + self.graphics.len() as u32;
                self.graphics.extend_from_slice(&bytes);
                self.host
                    .resources
                    .register(5, host_id, 0, address, bytes.len() as u32)
                    .map_err(NeTaskError::State)?;
                self.start_host(
                    "user32",
                    "DialogBoxParamA",
                    vec![self.host.image_base, host_id, owner, procedure, 0],
                    12,
                    false,
                )?;
            }
            88 => {
                let result = self.arg(0)? as i16 as i32 as u32;
                let window = self.handle(self.arg(2)?)?;
                let value = self.host_value("user32", "EndDialog", &[window, result])?;
                self.finish(4, value)?;
            }
            69 => {
                let cursor = self.handle(self.arg(0)?)?;
                let previous = self.host_value("user32", "SetCursor", &[cursor])?;
                let previous = self.map_handle(previous)?;
                self.finish(2, u32::from(previous))?;
            }
            32 | 33 => {
                let output = self.long_arg(0)?;
                let window = self.handle(self.arg(4)?)?;
                let function = if ordinal == 32 {
                    "GetWindowRect"
                } else {
                    "GetClientRect"
                };
                let start = self.graphics.len();
                self.graphics.resize(start + 16, 0);
                let address = crate::host_memory::HOST_BASE + start as u32;
                let value = self.host_value("user32", function, &[window, address])?;
                let mut rect = Vec::with_capacity(8);
                for item in self.graphics[start..start + 16].chunks_exact(4) {
                    rect.extend_from_slice(
                        &(i32::from_le_bytes(item.try_into().unwrap()) as i16).to_le_bytes(),
                    );
                }
                self.graphics.truncate(start);
                self.write_bytes(output, &rect)?;
                self.finish(6, value)?;
            }
            28 | 29 => {
                let pointer = self.long_arg(0)?;
                let window = self.handle(self.arg(4)?)?;
                let point = self.loaded.read(
                    (pointer >> 16) as u16,
                    pointer & 0xffff,
                    4,
                    NeAccess::Read,
                )?;
                let start = self.graphics.len();
                for p in point.chunks_exact(2) {
                    self.graphics.extend_from_slice(
                        &(i16::from_le_bytes(p.try_into().unwrap()) as i32).to_le_bytes(),
                    );
                }
                let address = crate::host_memory::HOST_BASE + start as u32;
                let value = self.host_value(
                    "user32",
                    if ordinal == 28 {
                        "ClientToScreen"
                    } else {
                        "ScreenToClient"
                    },
                    &[window, address],
                )?;
                let mut output = Vec::new();
                for p in self.graphics[start..start + 8].chunks_exact(4) {
                    output.extend_from_slice(
                        &(i32::from_le_bytes(p.try_into().unwrap()) as i16).to_le_bytes(),
                    );
                }
                self.graphics.truncate(start);
                self.write_bytes(pointer, &output)?;
                self.finish(6, value)?;
            }
            72 => {
                let pointer = self.long_arg(8)?;
                let mut rect = Vec::new();
                for offset in [6, 4, 2, 0] {
                    rect.extend_from_slice(&self.arg(offset)?.to_le_bytes());
                }
                self.write_bytes(pointer, &rect)?;
                self.finish(12, 1)?;
            }
            73 => {
                let pointer = self.long_arg(0)?;
                self.write_bytes(pointer, &[0; 8])?;
                self.finish(4, 1)?;
            }
            76 => {
                let point = self.long_arg(0)?;
                let rect = self.rect16(self.long_arg(4)?)?;
                let x = point as i16;
                let y = (point >> 16) as i16;
                self.finish(
                    8,
                    u32::from(x >= rect[0] && x < rect[2] && y >= rect[1] && y < rect[3]),
                )?;
            }
            77 => {
                let dy = self.arg(0)? as i16;
                let dx = self.arg(2)? as i16;
                let pointer = self.long_arg(4)?;
                let mut rect = self.rect16(pointer)?;
                for i in 0..4 {
                    rect[i] = rect[i].wrapping_add(if i % 2 == 0 { dx } else { dy });
                }
                let bytes = rect
                    .into_iter()
                    .flat_map(i16::to_le_bytes)
                    .collect::<Vec<_>>();
                self.write_bytes(pointer, &bytes)?;
                self.finish(8, 1)?;
            }
            79 => {
                let b = self.rect16(self.long_arg(0)?)?;
                let a = self.rect16(self.long_arg(4)?)?;
                let output = self.long_arg(8)?;
                let rect = [
                    a[0].max(b[0]),
                    a[1].max(b[1]),
                    a[2].min(b[2]),
                    a[3].min(b[3]),
                ];
                let nonempty = rect[0] < rect[2] && rect[1] < rect[3];
                let rect = if nonempty { rect } else { [0; 4] };
                self.write_bytes(
                    output,
                    &rect
                        .into_iter()
                        .flat_map(i16::to_le_bytes)
                        .collect::<Vec<_>>(),
                )?;
                self.finish(12, u32::from(nonempty))?;
            }
            47 => {
                let handle = self.arg(0)?;
                let valid = self
                    .handles
                    .get(&handle)
                    .is_some_and(|host| self.host.windows.get(*host).is_ok());
                self.finish(2, u32::from(valid))?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    pub(super) fn menu_bytes(&self, menu: u32) -> Result<&[u8], NeTaskError> {
        if let Some(bytes) = self.menu_overrides.get(&menu) {
            return Ok(bytes);
        }
        menu.checked_sub(crate::host_memory::HOST_BASE)
            .and_then(|offset| self.graphics.get(offset as usize..))
            .ok_or_else(|| NeTaskError::State("invalid Win16 menu template handle".into()))
    }
    pub(super) fn rect16(&self, address: u32) -> Result<[i16; 4], NeError> {
        let bytes =
            self.loaded
                .read((address >> 16) as u16, address & 0xffff, 8, NeAccess::Read)?;
        Ok(std::array::from_fn(|i| {
            i16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]])
        }))
    }
}
