//! Cooperative host continuations with real Win16 Pascal callback frames.
use super::*;
use wlam_host::{memory::MemoryAccess, runtime::Effect};

#[derive(Clone)]
pub(super) struct HostCall {
    library: String,
    name: String,
    args: Vec<u32>,
    argument_bytes: u16,
    handle_result: bool,
    return_value: Option<u32>,
    message_output: Option<u32>,
    scratch: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn create_window_runs_pascal_callbacks_with_dgroup_and_converted_structures() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut raw = vec![0; 0x300];
                raw[..2].copy_from_slice(b"MZ");
                raw[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
                raw[0x40..0x42].copy_from_slice(b"NE");
                let put = |raw: &mut [u8], at: usize, value: u16| {
                    raw[at..at + 2].copy_from_slice(&value.to_le_bytes())
                };
                for (offset, value) in [
                    (0x0e, 2),
                    (0x16, 1),
                    (0x18, 0x1000),
                    (0x1a, 2),
                    (0x1c, 2),
                    (0x22, 0x40),
                ] {
                    put(&mut raw, 0x40 + offset, value);
                }
                for (offset, value) in [
                    (0x80, 0x200),
                    (0x82, 256),
                    (0x84, 0),
                    (0x86, 256),
                    (0x88, 0),
                    (0x8a, 0),
                    (0x8c, 1),
                    (0x8e, 4096),
                ] {
                    put(&mut raw, offset, value);
                }
                let code = [
                    0x55, 0x8b, 0xec, 0x8b, 0x46, 0x0c, 0xa3, 0x20, 0, 0x8b, 0x46, 0x0e, 0xa3,
                    0x22, 0, 0x83, 0x7e, 0x0c, 5, 0x74, 10, 0xc4, 0x5e, 6, 0x26, 0x8b, 0x47, 0x0c,
                    0xa3, 0x2a, 0, 0xff, 6, 0x28, 0, 0xb8, 1, 0, 0x33, 0xd2, 0x5d, 0xca, 10, 0,
                ];
                raw[0x220..0x220 + code.len()].copy_from_slice(&code);
                let mut task = NeTask::load(raw, Vec::new()).unwrap();
                task.loaded.write(0x108, 0x100, b"Test\0").unwrap();
                task.loaded.write(0x108, 0x108, b"Title\0").unwrap();
                let thunk_selector = task.loaded.add_region_auto(8, true, true).unwrap();
                let thunk = u32::from(thunk_selector) << 16;
                task.loaded
                    .write(thunk_selector, 0, &[0xb8, 8, 1, 0xea, 0x20, 0, 0, 1])
                    .unwrap();
                task.thunks.insert(thunk, thunk_selector);
                task.graphics.extend_from_slice(b"Test\0Title\0");
                task.host.windows.register(b"Test".to_vec(), thunk).unwrap();
                task.set(SP, 0x200).unwrap();
                task.set(0x14, 0x700).unwrap();
                task.put(0x108, 0x200, 0x90).unwrap();
                task.put(0x108, 0x202, 0x100).unwrap();
                task.machine
                    .set_register(0x284, 4, (u64::from(task.gate_selector) << 16) | 0x8000)
                    .unwrap();
                task.start_host(
                    "user32",
                    "CreateWindowExA",
                    vec![
                        0,
                        crate::host_memory::HOST_BASE,
                        crate::host_memory::HOST_BASE + 5,
                        0,
                        0,
                        0,
                        100,
                        100,
                        0,
                        0,
                        0x108,
                        0,
                    ],
                    30,
                    true,
                )
                .unwrap();
                let create = task.long_arg(0).unwrap();
                let raw = task
                    .loaded
                    .read((create >> 16) as u16, 22, 8, NeAccess::Read)
                    .unwrap();
                let title = u32::from_le_bytes(raw[..4].try_into().unwrap());
                let class = u32::from_le_bytes(raw[4..].try_into().unwrap());
                assert_eq!(task.string(title).unwrap(), b"Title");
                assert_eq!(task.string(class).unwrap(), b"Test");
                for _ in 0..100 {
                    if task.machine.cs() == 0x100 && task.machine.ip() == 0x90 {
                        break;
                    }
                    task.step().unwrap();
                }
                assert_eq!((task.machine.cs(), task.machine.ip()), (0x100, 0x90));
                assert_eq!(task.reg(SP).unwrap(), 0x222);
                assert_eq!(task.reg(0x14).unwrap(), 0x700);
                assert_eq!(task.reg(DS).unwrap(), 0x108);
                assert!(task.callbacks.is_empty());
                assert_eq!(task.word(0x108, 0x20).unwrap(), 5); // final initial-size notification
                assert_eq!(task.word(0x108, 0x22).unwrap(), task.reg(AX).unwrap());
                assert_eq!(task.word(0x108, 0x28).unwrap(), 3); // NCCREATE, CREATE, SIZE
                assert_eq!(task.word(0x108, 0x2a).unwrap(), 100); // CREATESTRUCT16.cx
                let window = task.handle(task.reg(AX).unwrap()).unwrap();
                assert_eq!(task.host.windows.get(window).unwrap().width, 100);
                assert!(task.host.gdi.surface(window).is_some());
                task.set(SP, 0x200).unwrap();
                task.start_host(
                    "user32",
                    "CreateWindowExA",
                    vec![
                        0, 0x01080100, 0x01080108, 0x40000000, 0, 0, 20, 20, window, 1, 0x108, 0,
                    ],
                    30,
                    true,
                )
                .unwrap();
                for _ in 0..100 {
                    if task.machine.cs() == 0x100 && task.machine.ip() == 0x90 {
                        break;
                    }
                    task.step().unwrap();
                }
                let child = task.handle(task.reg(AX).unwrap()).unwrap();
                assert_eq!(task.host.windows.get(child).unwrap().id, 1);
                assert_eq!(task.host.windows.get(child).unwrap().parent, window);
                let guest_window = task.map_handle(window).unwrap();
                task.set(SP, 0x200).unwrap();
                task.put(0x108, 0x204, 0x300).unwrap();
                task.put(0x108, 0x206, 0x108).unwrap();
                task.put(0x108, 0x208, guest_window).unwrap();
                task.put(0x108, 0x320, 0xbeef).unwrap();
                assert!(task.user_basic(39).unwrap());
                assert_eq!(task.word(0x108, 0x300).unwrap(), task.reg(AX).unwrap());
                assert_eq!(task.word(0x108, 0x308).unwrap(), 100);
                assert_eq!(task.word(0x108, 0x30a).unwrap(), 100);
                assert_eq!(task.word(0x108, 0x320).unwrap(), 0xbeef);
                task.set(SP, 0x200).unwrap();
                assert!(task.user_basic(40).unwrap());
                assert!(!task.host.windows.get(window).unwrap().painting);
                let dc = task.host.gdi.memory_dc().unwrap();
                let dc = task.map_handle(dc).unwrap();
                task.set(SP, 0x200).unwrap();
                for (offset, value) in [(0x204, 3), (0x206, 5), (0x208, dc)] {
                    task.put(0x108, offset, value).unwrap();
                }
                task.gdi(51).unwrap();
                let bitmap = task.reg(AX).unwrap();
                task.set(SP, 0x200).unwrap();
                for (offset, value) in [
                    (0x204, 0x300),
                    (0x206, 0x108),
                    (0x208, 14),
                    (0x20a, bitmap),
                    (0x30e, 0xbeef),
                ] {
                    task.put(0x108, offset, value).unwrap();
                }
                task.gdi(82).unwrap();
                assert_eq!(task.reg(AX).unwrap(), 14);
                assert_eq!(task.word(0x108, 0x302).unwrap(), 5);
                assert_eq!(task.word(0x108, 0x304).unwrap(), 3);
                assert_eq!(task.word(0x108, 0x308).unwrap(), 0x2001);
                assert_eq!(task.word(0x108, 0x30a).unwrap(), 0);
                assert_eq!(task.word(0x108, 0x30c).unwrap(), 0);
                assert_eq!(task.word(0x108, 0x30e).unwrap(), 0xbeef);
                task.set(SP, 0x200).unwrap();
                for (offset, value) in [
                    (0x204, 0),
                    (0x206, 20),
                    (0x208, 5),
                    (0x20a, 0),
                    (0x20c, guest_window),
                ] {
                    task.put(0x108, offset, value).unwrap();
                }
                task.user_basic(64).unwrap();
                assert_eq!(task.reg(SP).unwrap(), 0x20e);
                task.set(SP, 0x200).unwrap();
                for (offset, value) in [(0x204, 0), (0x206, 90), (0x208, 0), (0x20a, guest_window)]
                {
                    task.put(0x108, offset, value).unwrap();
                }
                task.user_basic(62).unwrap();
                assert_eq!(task.reg(AX).unwrap(), 5);
                task.set(SP, 0x200).unwrap();
                task.put(0x108, 0x204, 0).unwrap();
                task.put(0x108, 0x206, guest_window).unwrap();
                task.user_basic(63).unwrap();
                assert_eq!(task.reg(AX).unwrap(), 20);
                task.loaded.write(0x108, 0x380, b"LISTBOX\0").unwrap();
                task.set(SP, 0x200).unwrap();
                task.start_host(
                    "user32",
                    "CreateWindowExA",
                    vec![
                        0, 0x01080380, 0, 0x50000001, 0, 0, 100, 100, window, 77, 0x108, 0,
                    ],
                    30,
                    true,
                )
                .unwrap();
                let listbox = task.handle(task.reg(AX).unwrap()).unwrap();
                for text in [0x01080100, 0x01080108] {
                    let (message, wp, lp) =
                        task.control_message16(listbox, 0x401, 0, text).unwrap();
                    task.host_value("user32", "SendMessageA", &[listbox, message, wp, lp])
                        .unwrap();
                }
                let (message, wp, lp) = task.control_message16(listbox, 0x40c, 0, 0).unwrap();
                assert_eq!(
                    task.host_value("user32", "SendMessageA", &[listbox, message, wp, lp])
                        .unwrap(),
                    2
                );
                let (message, wp, lp) = task.control_message16(listbox, 0x407, 0xffff, 0).unwrap();
                assert_eq!(wp, u32::MAX);
                task.host_value("user32", "SendMessageA", &[listbox, message, wp, lp])
                    .unwrap();
                let (message, wp, lp) = task.control_message16(listbox, 0x409, 0, 0).unwrap();
                assert_eq!(
                    task.host_value("user32", "SendMessageA", &[listbox, message, wp, lp])
                        .unwrap(),
                    u32::MAX
                );
                assert_eq!(
                    task.control_message16(window, 0x401, 42, 0).unwrap(),
                    (0x401, 42, 0)
                );
                task.host.windows.validate_paint(listbox);
                // MSG16 packs handles/notification codes into words and LPARAM.
                task.set(SP, 0x200).unwrap();
                for (offset, value) in [
                    (0x204, 0),
                    (0x206, 0),
                    (0x208, 0),
                    (0x20a, 0x300),
                    (0x20c, 0x108),
                    (0x312, 0xbeef),
                ] {
                    task.put(0x108, offset, value).unwrap();
                }
                task.host
                    .windows
                    .post(wlam_host::window::Message {
                        window,
                        message: 0x111,
                        wparam: (7 << 16) | 42,
                        lparam: child,
                    })
                    .unwrap();
                let graphics_length = task.graphics.len();
                task.user_basic(108).unwrap();
                assert_eq!(task.word(0x108, 0x300).unwrap(), guest_window);
                assert_eq!(task.word(0x108, 0x302).unwrap(), 0x111);
                assert_eq!(task.word(0x108, 0x304).unwrap(), 42);
                assert_eq!(
                    task.word(0x108, 0x306).unwrap(),
                    task.map_handle(child).unwrap()
                );
                assert_eq!(task.word(0x108, 0x308).unwrap(), 7);
                assert_eq!(task.word(0x108, 0x312).unwrap(), 0xbeef);
                assert_eq!(task.graphics.len(), graphics_length);
                // Polling a wait must not consume the Pascal far return.
                task.set(SP, 0x200).unwrap();
                let saved = task.machine.register_bytes().to_vec();
                task.user_basic(108).unwrap();
                assert!(task.is_waiting());
                assert!(matches!(task.step().unwrap(), NeTaskEvent::Wait));
                assert_eq!(task.machine.register_bytes(), saved);
                task.host.windows.post_quit(7);
                task.step().unwrap();
                assert!(!task.is_waiting());
                assert_eq!(task.reg(SP).unwrap(), 0x20e);
                assert_eq!(task.reg(AX).unwrap(), 0);
                assert_eq!(task.word(0x108, 0x302).unwrap(), 18);
                assert_eq!(task.word(0x108, 0x304).unwrap(), 7);
                assert_eq!(task.word(0x108, 0x312).unwrap(), 0xbeef);
                assert_eq!(task.graphics.len(), graphics_length);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
pub(super) struct CallbackFrame {
    machine: NeMachine,
    token: u64,
    call: HostCall,
    converted: Vec<(u16, u32, u32)>,
    word_result: bool,
}

impl NeTask {
    pub(super) fn control_message16(
        &self,
        window: u32,
        message: u16,
        wp: u32,
        lp: u32,
    ) -> Result<(u32, u32, u32), NeTaskError> {
        if !self
            .host
            .windows
            .get(window)
            .is_ok_and(|window| window.procedure == 0xffff_ff02)
        {
            return Ok((u32::from(message), wp, lp));
        }
        let mut wp = wp;
        let translated = match self.host.window_class(window) {
            Some(b"LISTBOX") if (0x401..=0x423).contains(&message) => {
                let table = [
                    0x180, 0x181, 0x182, 0x183, 0x184, 0x185, 0x186, 0x187, 0x188, 0x189, 0x18a,
                    0x18b, 0x18c, 0x18d, 0x18e, 0x18f, 0x190, 0x191, 0x192, 0x193, 0x194, 0x195,
                    0x196, 0x197, 0x198, 0x199, 0x19a, 0x19b, 0x19c, 0x19d, 0x19e, 0x19f, 0x1a0,
                    0x1a1, 0x1a2,
                ];
                if matches!(
                    message,
                    0x402
                        | 0x403
                        | 0x407
                        | 0x408
                        | 0x40a
                        | 0x40b
                        | 0x40d
                        | 0x410
                        | 0x41a
                        | 0x41b
                        | 0x423
                ) {
                    wp = wp as u16 as i16 as i32 as u32;
                }
                table[usize::from(message - 0x401)]
            }
            Some(b"BUTTON") if (0x400..=0x404).contains(&message) => {
                u32::from(message - 0x400) + 0xf0
            }
            _ => u32::from(message),
        };
        Ok((translated, wp, lp))
    }
    pub(super) fn message_to_win16(
        &mut self,
        message: u32,
        wp: u32,
        lp: u32,
    ) -> Result<(u16, u32), NeTaskError> {
        Ok(if message == 0x111 {
            (
                wp as u16,
                u32::from(self.map_handle(lp)?) | (wp & 0xffff0000),
            )
        } else if matches!(message, 7 | 8) {
            (self.map_handle(wp)?, lp)
        } else {
            (wp as u16, lp)
        })
    }
    pub(super) fn message_from_win16(
        &self,
        message: u16,
        wp: u16,
        lp: u32,
    ) -> Result<(u32, u32), NeTaskError> {
        Ok(if message == 0x111 {
            (u32::from(wp) | (lp & 0xffff0000), self.handle(lp as u16)?)
        } else if matches!(message, 7 | 8) {
            (self.handle(wp)?, lp)
        } else {
            (u32::from(wp), lp)
        })
    }
    /// Live guest HWNDs and their corresponding shared-host windows.
    pub fn window_handles(&self) -> Vec<(u16, u32)> {
        self.handles
            .iter()
            .filter_map(|(guest, host)| self.host.windows.get(*host).ok().map(|_| (*guest, *host)))
            .collect()
    }

    pub fn post_key(&mut self, window: u16, key: u16, down: bool) -> Result<(), NeTaskError> {
        let window = self.handle(window)?;
        self.host
            .windows
            .post(wlam_host::window::Message {
                window,
                message: if down { 0x100 } else { 0x101 },
                wparam: u32::from(key),
                lparam: if down { 1 } else { 0xc0000001 },
            })
            .map_err(|e| NeTaskError::State(format!("keyboard input: {e:?}")))
    }
    pub fn post_menu_command(&mut self, window: u16, command: u16) -> Result<(), NeTaskError> {
        let mut window = self.handle(window)?;
        let mut menu = self.host_value("user32", "GetMenu", &[window])?;
        for _ in 0..64 {
            if menu != 0 {
                break;
            }
            let parent = self
                .host
                .windows
                .get(window)
                .map_err(|e| NeTaskError::State(format!("menu owner: {e:?}")))?
                .parent;
            if parent == 0 {
                break;
            }
            window = parent;
            menu = self.host_value("user32", "GetMenu", &[window])?;
        }
        let bytes = self.menu_bytes(menu)?;
        let offset = crate::templates::menu_item_flags(bytes, command, false)
            .map_err(NeTaskError::State)?
            .ok_or_else(|| NeTaskError::State("menu command not found".into()))?;
        if u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) & 3 != 0 {
            return Err(NeTaskError::State("menu command is disabled".into()));
        }
        self.host
            .post(wlam_host::window::Message {
                window,
                message: 0x111,
                wparam: u32::from(command),
                lparam: 0,
            })
            .map_err(NeTaskError::State)
    }
    pub fn set_dialog_text(
        &mut self,
        dialog: u16,
        control: u16,
        text: &[u8],
    ) -> Result<(), NeTaskError> {
        if text.contains(&0) {
            return Err(NeTaskError::State("embedded NUL in control text".into()));
        }
        let dialog = self.handle(dialog)?;
        let start = self.graphics.len();
        self.graphics.extend_from_slice(text);
        self.graphics.push(0);
        let result = self.host_value(
            "user32",
            "SetDlgItemTextA",
            &[
                dialog,
                u32::from(control),
                crate::host_memory::HOST_BASE + start as u32,
            ],
        );
        self.graphics.truncate(start);
        if result? == 0 {
            return Err(NeTaskError::State("control text update failed".into()));
        }
        Ok(())
    }

    pub fn is_waiting(&self) -> bool {
        self.pending_host.is_some()
    }

    pub(super) fn resume_host_wait(&mut self) -> Result<NeTaskEvent, NeTaskError> {
        let call = self
            .pending_host
            .take()
            .ok_or_else(|| NeTaskError::State("missing host wait".into()))?;
        let effect = self.call_host(&call)?;
        self.host_effect(effect, call)?;
        if self.is_waiting() {
            Ok(NeTaskEvent::Wait)
        } else {
            Ok(NeTaskEvent::Api {
                module: "USER_CONTINUATION".into(),
                symbol: NeImportSymbol::Name(b"resume".to_vec()),
            })
        }
    }
    fn host_read(&mut self, address: u32, length: usize) -> Result<Vec<u8>, NeTaskError> {
        let mut bytes = vec![0; length];
        let memory = crate::host_memory::TaskMemory {
            loaded: &mut self.loaded,
            graphics: &mut self.graphics,
            arena: &mut self.arena,
        };
        memory
            .read(address, &mut bytes)
            .map_err(|e| NeTaskError::State(format!("host structure read: {e:?}")))?;
        Ok(bytes)
    }
    fn host_write(&mut self, address: u32, bytes: &[u8]) -> Result<(), NeTaskError> {
        let mut memory = crate::host_memory::TaskMemory {
            loaded: &mut self.loaded,
            graphics: &mut self.graphics,
            arena: &mut self.arena,
        };
        memory
            .write(address, bytes)
            .map_err(|e| NeTaskError::State(format!("host structure write: {e:?}")))
    }
    pub(super) fn start_host(
        &mut self,
        library: &str,
        name: &str,
        args: Vec<u32>,
        argument_bytes: u16,
        handle_result: bool,
    ) -> Result<(), NeTaskError> {
        let call = HostCall {
            library: library.into(),
            name: name.into(),
            args,
            argument_bytes,
            handle_result,
            return_value: None,
            message_output: None,
            scratch: None,
        };
        let effect = self.call_host(&call)?;
        self.host_effect(effect, call)
    }
    fn call_host(&mut self, call: &HostCall) -> Result<Effect, NeTaskError> {
        let mut memory = crate::host_memory::TaskMemory {
            loaded: &mut self.loaded,
            graphics: &mut self.graphics,
            arena: &mut self.arena,
        };
        self.host
            .call(&call.library, &call.name, &call.args, &mut memory)
            .map_err(|error| {
                NeTaskError::State(format!(
                    "{}!{}({:x?}): {error}",
                    call.library, call.name, call.args
                ))
            })
    }
    pub(super) fn start_message_host(
        &mut self,
        name: &str,
        args: Vec<u32>,
        argument_bytes: u16,
        output: Option<u32>,
        scratch: usize,
    ) -> Result<(), NeTaskError> {
        let call = HostCall {
            library: "user32".into(),
            name: name.into(),
            args,
            argument_bytes,
            handle_result: false,
            return_value: None,
            message_output: output,
            scratch: Some(scratch),
        };
        let effect = self.call_host(&call)?;
        self.host_effect(effect, call)
    }
    fn host_effect(&mut self, mut effect: Effect, call: HostCall) -> Result<(), NeTaskError> {
        for _ in 0..1024 {
            match effect {
                Effect::Return(value) => {
                    if call.name == "CreateWindowExA" && value != 0 {
                        let window =
                            self.host.windows.get(value).map_err(|e| {
                                NeTaskError::State(format!("created window: {e:?}"))
                            })?;
                        let size = (window.width & 0xffff) | ((window.height & 0xffff) << 16);
                        let notification = HostCall {
                            library: "user32".into(),
                            name: "SendMessageA".into(),
                            args: vec![value, 5, 0, size],
                            argument_bytes: call.argument_bytes,
                            handle_result: call.handle_result,
                            return_value: Some(value),
                            message_output: None,
                            scratch: None,
                        };
                        let effect = self.call_host(&notification)?;
                        return self.host_effect(effect, notification);
                    }
                    let value = call.return_value.unwrap_or(value);
                    if let Some(output) = call.message_output {
                        if value != 0 || call.name == "GetMessageA" {
                            let raw = self.host_read(call.args[0], 28)?;
                            let long =
                                |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
                            let mut bytes = self.map_handle(long(0))?.to_le_bytes().to_vec();
                            bytes.extend_from_slice(&(long(4) as u16).to_le_bytes());
                            let (wp, lp) = self.message_to_win16(long(4), long(8), long(12))?;
                            bytes.extend_from_slice(&wp.to_le_bytes());
                            bytes.extend_from_slice(&lp.to_le_bytes());
                            bytes.extend_from_slice(&long(16).to_le_bytes());
                            bytes.extend_from_slice(&(long(20) as i16).to_le_bytes());
                            bytes.extend_from_slice(&(long(24) as i16).to_le_bytes());
                            self.write_bytes(output, &bytes)?;
                        }
                    }
                    if let Some(start) = call.scratch {
                        // Guest callbacks may append persistent bitmap/resource
                        // storage after the temporary MSG. Preserve those bytes.
                        if self.graphics.len() == start + 28 {
                            self.graphics.truncate(start);
                        }
                    }
                    let value = if call.handle_result {
                        u32::from(self.map_handle(value)?)
                    } else {
                        value
                    };
                    self.finish(call.argument_bytes, value)?;
                    return Ok(());
                }
                Effect::Invoke {
                    token,
                    procedure,
                    args,
                } => return self.enter_callback(token, procedure, &args, call),
                Effect::Retry => effect = self.call_host(&call)?,
                Effect::Wait => {
                    self.pending_host = Some(call);
                    return Ok(());
                }
                Effect::Exit(code) => {
                    self.exit = Some(code as u16);
                    return Ok(());
                }
                other => {
                    return Err(NeTaskError::State(format!(
                        "unhandled Win16 host continuation: {other:?}"
                    )))
                }
            }
        }
        Err(NeTaskError::State("host retry budget exhausted".into()))
    }
    fn enter_callback(
        &mut self,
        token: u64,
        procedure: u32,
        args: &[u32],
        call: HostCall,
    ) -> Result<(), NeTaskError> {
        if args.len() != 4 {
            return Err(NeTaskError::State(format!(
                "unsupported callback signature: {} arguments",
                args.len()
            )));
        }
        if std::env::var_os("WLAM_NE_TRACE").is_some() {
            eprintln!("Win16 callback {procedure:08x}: {args:x?}");
        }
        let (procedure, instance) = if self.thunks.contains_key(&procedure) {
            let code = self
                .loaded
                .read((procedure >> 16) as u16, 0, 8, NeAccess::Read)?;
            (
                u32::from_le_bytes(code[4..8].try_into().unwrap()),
                u16::from_le_bytes(code[1..3].try_into().unwrap()),
            )
        } else {
            (procedure, self.context.instance)
        };
        self.loaded.translate(
            (procedure >> 16) as u16,
            procedure & 0xffff,
            1,
            NeAccess::Execute,
        )?;
        let saved = self.machine.clone();
        let window = self.map_handle(args[0])?;
        let message = u16::try_from(args[1])
            .map_err(|_| NeTaskError::State("callback message exceeds Win16 range".into()))?;
        let mut wparam = args[2] as u16;
        if matches!(message, 7 | 8) {
            wparam = self.map_handle(args[2])?;
        }
        let mut lparam = args[3];
        let mut converted = Vec::new();
        if matches!(message, 1 | 0x81) && lparam != 0 {
            let bytes = self.host_read(lparam, 48)?;
            let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            let mut guest = Vec::with_capacity(34);
            guest.extend_from_slice(&word(0).to_le_bytes());
            guest.extend_from_slice(&(word(4) as u16).to_le_bytes());
            let menu = if word(32) & 0x40000000 != 0 {
                word(8) as u16
            } else {
                self.map_handle(word(8))?
            };
            guest.extend_from_slice(&menu.to_le_bytes());
            guest.extend_from_slice(&self.map_handle(word(12))?.to_le_bytes());
            for at in [16, 20, 24, 28] {
                guest.extend_from_slice(&(word(at) as i16).to_le_bytes());
            }
            for at in [32, 36, 40, 44] {
                let mut value = word(at);
                if matches!(at, 36 | 40) && value >= crate::host_memory::HOST_BASE {
                    let original = value;
                    let mut string = Vec::new();
                    for index in 0..65536u32 {
                        let byte = self.host_read(
                            original.checked_add(index).ok_or_else(|| {
                                NeTaskError::State("callback string overflow".into())
                            })?,
                            1,
                        )?[0];
                        string.push(byte);
                        if byte == 0 {
                            break;
                        }
                    }
                    if string.last() != Some(&0) {
                        return Err(NeTaskError::State("unterminated callback string".into()));
                    }
                    let selector = self
                        .loaded
                        .add_region_auto(string.len() as u32, true, false)?;
                    self.loaded.write(selector, 0, &string)?;
                    converted.push((selector, original, 0));
                    value = u32::from(selector) << 16;
                }
                guest.extend_from_slice(&value.to_le_bytes());
            }
            let selector = self
                .loaded
                .add_region_auto(guest.len() as u32, true, false)?;
            self.loaded.write(selector, 0, &guest)?;
            converted.push((selector, lparam, u32::from(message)));
            lparam = u32::from(selector) << 16;
        } else if message == 0x111 {
            // Win16 WM_COMMAND packs control/notification in LPARAM.
            wparam = args[2] as u16;
            lparam = u32::from(self.map_handle(args[3])?) | (args[2] & 0xffff0000);
        }
        let ss = self.reg(SS)?;
        let mut sp = self.reg(SP)?;
        for (value, size) in [
            (u32::from(window), 2usize),
            (u32::from(message), 2),
            (u32::from(wparam), 2),
            (lparam, 4),
        ] {
            sp = sp.wrapping_sub(size as u16);
            self.loaded
                .write(ss, u32::from(sp), &value.to_le_bytes()[..size])?;
        }
        sp = sp.wrapping_sub(2);
        self.put(ss, sp, self.gate_selector)?;
        sp = sp.wrapping_sub(2);
        self.put(ss, sp, 0x3fff)?;
        self.set(SP, sp)?;
        self.set(DS, instance)?;
        self.set(AX, instance)?;
        self.machine.set_register(0x284, 4, u64::from(procedure))?;
        self.callbacks.push(CallbackFrame {
            machine: saved,
            token,
            call,
            converted,
            word_result: self
                .host
                .windows
                .get(args[0])
                .is_ok_and(|window| window.procedure == 0xffff_ff01),
        });
        Ok(())
    }
    pub(super) fn resume_callback(&mut self) -> Result<NeTaskEvent, NeTaskError> {
        let frame = self
            .callbacks
            .pop()
            .ok_or_else(|| NeTaskError::State("callback return without a continuation".into()))?;
        if self.reg(SP)? != frame.machine.register(SP, 2)? as u16
            || self.reg(SS)? != frame.machine.register(SS, 2)? as u16
        {
            return Err(NeTaskError::State(format!(
                "unbalanced Win16 callback stack: SS:SP={:04x}:{:04x}, expected {:04x}:{:04x}",
                self.reg(SS)?,
                self.reg(SP)?,
                frame.machine.register(SS, 2)?,
                frame.machine.register(SP, 2)?
            )));
        }
        let result = u32::from(self.reg(AX)?)
            | if frame.word_result {
                0
            } else {
                u32::from(self.reg(DX)?) << 16
            };
        let strings: BTreeMap<u16, u32> = frame
            .converted
            .iter()
            .filter(|(_, _, message)| *message == 0)
            .map(|(selector, address, _)| (*selector, *address))
            .collect();
        for (selector, address, message) in frame.converted {
            if matches!(message, 1 | 0x81) {
                let bytes = self.loaded.read(selector, 0, 34, NeAccess::Read)?;
                let short = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
                let long = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                let original_pointer = |value: u32| {
                    strings
                        .get(&((value >> 16) as u16))
                        .map_or(value, |original| original + (value & 0xffff))
                };
                let fields = [
                    long(0),
                    u32::from(short(4)),
                    if long(18) & 0x40000000 != 0 {
                        u32::from(short(6))
                    } else {
                        self.handle(short(6))?
                    },
                    self.handle(short(8))?,
                    short(10) as i16 as i32 as u32,
                    short(12) as i16 as i32 as u32,
                    short(14) as i16 as i32 as u32,
                    short(16) as i16 as i32 as u32,
                    long(18),
                    original_pointer(long(22)),
                    original_pointer(long(26)),
                    long(30),
                ];
                let output = fields
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>();
                self.host_write(address, &output)?;
            }
            self.loaded.remove_region(selector)?;
        }
        self.machine = frame.machine;
        let effect = {
            let mut memory = crate::host_memory::TaskMemory {
                loaded: &mut self.loaded,
                graphics: &mut self.graphics,
                arena: &mut self.arena,
            };
            self.host
                .resume_with_memory(frame.token, result, &mut memory)
                .map_err(NeTaskError::State)?
        };
        self.host_effect(effect, frame.call)?;
        Ok(NeTaskEvent::Api {
            module: "USER_CALLBACK".into(),
            symbol: NeImportSymbol::Name(b"return".to_vec()),
        })
    }
}
