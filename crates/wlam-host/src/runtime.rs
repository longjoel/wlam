//! Engine-independent dispatcher for the first interactive Win32 slice.
use crate::{
    gdi::{Gdi, Rect},
    memory::MemoryAccess,
    window::{Action, Message, Windows},
};
use std::collections::BTreeMap;
pub const DESKTOP_WINDOW: u32 = 0x10000001;

const DEFAULT_PALETTE_COLORS: [u32; 20] = [
    0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080,
    0xc0c0c0, 0xc0dcc0, 0xa6caf0, 0xfffbf0, 0xa0a0a4, 0x808080, 0xff0000,
    0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
];
fn initial_system_palette() -> [[u8; 4]; 256] {
    let mut colors = [[0; 4]; 256];
    for (index, color) in colors.iter_mut().enumerate() {
        *color = [index as u8, index as u8, index as u8, 0];
    }
    for (index, rgb) in DEFAULT_PALETTE_COLORS.into_iter().enumerate() {
        colors[if index < 10 { index } else { index + 236 }] =
            [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0];
    }
    colors
}

fn classic_window_margins(style: u32, has_menu: bool) -> (i32, i32, i32, i32) {
    const WS_BORDER: u32 = 0x0080_0000;
    const WS_DLGFRAME: u32 = 0x0040_0000;
    const WS_CAPTION: u32 = WS_BORDER | WS_DLGFRAME;
    const WS_THICKFRAME: u32 = 0x0004_0000;
    let frame = if style & WS_THICKFRAME != 0 {
        4
    } else if style & WS_DLGFRAME != 0 {
        3
    } else if style & WS_BORDER != 0 {
        1
    } else {
        0
    };
    let caption = if style & WS_CAPTION == WS_CAPTION {
        19
    } else {
        0
    };
    let menu = if has_menu { 19 } else { 0 };
    (frame, frame + caption + menu, frame, frame)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    Return(u32),
    Invoke {
        token: u64,
        procedure: u32,
        args: Vec<u32>,
    },
    Wait,
    Exit(u32),
    MessageBox {
        text: Vec<u8>,
        title: Vec<u8>,
        flags: u32,
    },
}
#[derive(Debug)]
enum Finish {
    Plain,
    Accelerator,
    Redraw { window: u32, dc: u32 },
    Create(u32),
    Destroy(u32),
    Update,
}
#[derive(Debug, Clone)]
struct HookContext {
    chain: Vec<u32>,
    index: usize,
}
#[derive(Debug)]
enum CallbackFinish {
    Return,
    DialogInit(u32),
    DialogHook {
        window: u32,
        procedure: u32,
        init_parameter: u32,
        storage: u32,
    },
    DialogNcCreate {
        window: u32,
        dialog_procedure: u32,
        init_parameter: u32,
        storage: u32,
    },
    DialogCreate {
        window: u32,
        dialog_procedure: u32,
        init_parameter: u32,
        storage: u32,
    },
    DialogChildren {
        window: u32,
        dialog_procedure: u32,
        init_parameter: u32,
        children: Vec<u32>,
        index: usize,
    },
    DialogChildHook(DialogChildContext),
    DialogChildNcCreate(DialogChildContext),
    DialogChildCreate(DialogChildContext),
    DialogBoxMessage {
        key: DialogKey,
    },
    Create {
        action: Action,
        parameter: u32,
        create_parameter: u32,
        window: u32,
    },
}
#[derive(Debug)]
struct DialogChildContext {
    window: u32,
    dialog_procedure: u32,
    init_parameter: u32,
    children: Vec<u32>,
    index: usize,
    child: u32,
    storage: u32,
}
type DialogKey = (u32, u32, u32, u32, u32);
#[derive(Debug, Clone, Copy)]
struct ModalDialog {
    window: u32,
    procedure: u32,
    result: Option<u32>,
}
#[derive(Debug, Default, Clone)]
struct ListBoxState {
    items: Vec<Vec<u8>>,
    selection: i32,
}
#[derive(Debug)]
struct CallbackPending {
    context: Option<HookContext>,
    finish: CallbackFinish,
}
#[derive(Debug)]
struct WindowData {
    class: Vec<u8>,
    instance: u32,
    parent: u32,
    id: u32,
    style: u32,
    ex_style: u32,
    user_data: u32,
    extra: Vec<u8>,
    x: i32,
    y: i32,
    text: Vec<u8>,
}
#[derive(Debug)]
pub struct Runtime {
    pub windows: Windows,
    pub gdi: Gdi,
    pub gl_context: crate::gl_context::GlContexts,
    pub opengl: crate::opengl::OpenGl,
    pending: BTreeMap<u64, Finish>,
    paints: BTreeMap<u32, u32>,
    arena: crate::arena::Arena,
    bitmap_allocations: BTreeMap<u32, u32>,
    pub image_base: u32,
    pub version: crate::WindowsVersion,
    thread: crate::thread::ThreadState,
    virtual_memory: crate::virtual_memory::VirtualMemory,
    critical_sections: crate::sync::CriticalSections,
    process: crate::startup::Process,
    stdio: crate::stdio::Stdio,
    pub modules: crate::modules::Modules,
    exception_filter: u32,
    cursor: (i32, i32),
    keys: [u8; 256],
    atoms: crate::atoms::Atoms,
    code_ranges: Vec<(u64, u64)>,
    common_dcs: BTreeMap<u32, u32>,
    cursors: BTreeMap<u32, u32>,
    active_cursor: u32,
    focused_window: u32,
    registered_messages: crate::atoms::Atoms,
    error_mode: u32,
    events: crate::events::Events,
    message_hooks: crate::objects::Handles<(u32, u32, u32)>,
    hook_order: Vec<u32>,
    classes: BTreeMap<Vec<u8>, (u16, [u32; 10])>,
    pub resources: crate::resources::Resources,
    icons: crate::objects::Handles<(u32, u32)>,
    shared_icons: BTreeMap<(u32, u32), u32>,
    palettes: crate::objects::Handles<Vec<[u8; 4]>>,
    default_palette: Option<u32>,
    display_bits: u32,
    system_palette: [[u8; 4]; 256],
    callbacks: BTreeMap<u64, CallbackPending>,
    callback_stack: Vec<u64>,
    next_callback: u64,
    clock_ms: u64,
    timers: BTreeMap<(u32, u32), (u64, u32, u32)>,
    next_timer: u32,
    window_data: BTreeMap<u32, WindowData>,
    menus: BTreeMap<u32, u32>,
    files: crate::files::VirtualFiles,
    modal_dialogs: BTreeMap<DialogKey, ModalDialog>,
    modal_windows: BTreeMap<u32, DialogKey>,
    pending_modal: Option<DialogKey>,
    list_boxes: BTreeMap<u32, ListBoxState>,
}
impl Default for Runtime {
    fn default() -> Self {
        let mut gdi = Gdi::default();
        gdi.create_surface(DESKTOP_WINDOW, 640, 480)
            .expect("virtual desktop dimensions");
        Self {
            windows: Windows::default(),
            gdi,
            gl_context: crate::gl_context::GlContexts::default(),
            opengl: crate::opengl::OpenGl::default(),
            pending: BTreeMap::new(),
            paints: BTreeMap::new(),
            arena: crate::arena::Arena::default(),
            bitmap_allocations: BTreeMap::new(),
            image_base: 0x400000,
            version: crate::WindowsVersion::WIN2000,
            thread: crate::thread::ThreadState::default(),
            virtual_memory: crate::virtual_memory::VirtualMemory::default(),
            critical_sections: crate::sync::CriticalSections::default(),
            process: crate::startup::Process::default(),
            stdio: crate::stdio::Stdio::default(),
            modules: crate::modules::Modules::default(),
            exception_filter: 0,
            cursor: (0, 0),
            keys: [0; 256],
            atoms: crate::atoms::Atoms::default(),
            code_ranges: Vec::new(),
            common_dcs: BTreeMap::new(),
            cursors: BTreeMap::new(),
            active_cursor: 0,
            focused_window: 0,
            registered_messages: crate::atoms::Atoms::default(),
            error_mode: 0,
            events: crate::events::Events::default(),
            message_hooks: crate::objects::Handles::with_namespace(7),
            hook_order: Vec::new(),
            classes: BTreeMap::new(),
            resources: crate::resources::Resources::default(),
            icons: crate::objects::Handles::with_namespace(9),
            shared_icons: BTreeMap::new(),
            palettes: crate::objects::Handles::with_namespace(11),
            default_palette: None,
            display_bits: 32,
            system_palette: initial_system_palette(),
            callbacks: BTreeMap::new(),
            callback_stack: Vec::new(),
            next_callback: 1 << 63,
            clock_ms: 0,
            timers: BTreeMap::new(),
            next_timer: 1,
            window_data: BTreeMap::new(),
            menus: BTreeMap::new(),
            files: crate::files::VirtualFiles::default(),
            modal_dialogs: BTreeMap::new(),
            modal_windows: BTreeMap::new(),
            pending_modal: None,
            list_boxes: BTreeMap::new(),
        }
    }
}
fn read(memory: &dyn MemoryAccess, address: u32) -> Result<u32, String> {
    memory
        .read_word(address)
        .map_err(|e| format!("guest read {e:?}"))
}
fn write(memory: &mut dyn MemoryAccess, address: u32, value: u32) -> Result<(), String> {
    memory
        .write_word(address, value)
        .map_err(|e| format!("guest write {e:?}"))
}
fn read_rect(memory: &dyn MemoryAccess, address: u32) -> Result<[i32; 4], String> {
    let mut raw = [0u8; 16];
    memory.read(address, &mut raw).map_err(|e| format!("RECT read: {e:?}"))?;
    Ok(std::array::from_fn(|i| i32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap())))
}
fn write_rect(memory: &mut dyn MemoryAccess, address: u32, rect: [i32; 4]) -> Result<(), String> {
    let mut raw = [0u8; 16];
    for (i, edge) in rect.iter().enumerate() {
        raw[i * 4..i * 4 + 4].copy_from_slice(&edge.to_le_bytes());
    }
    memory.write(address, &raw).map_err(|e| format!("RECT write: {e:?}"))
}
fn string(memory: &dyn MemoryAccess, address: u32) -> Result<Vec<u8>, String> {
    let mut value = Vec::new();
    for i in 0..4096 {
        let mut b = [0];
        memory
            .read(
                address.checked_add(i).ok_or("string address overflow")?,
                &mut b,
            )
            .map_err(|e| format!("string read {e:?}"))?;
        if b[0] == 0 {
            return Ok(value);
        }
        value.push(b[0]);
    }
    Err("guest string exceeds 4096 bytes".into())
}
pub fn read_c_string(memory: &dyn MemoryAccess, address: u32) -> Result<Vec<u8>, String> {
    string(memory, address)
}
fn format_wsprintf(format: &[u8], values: &[u32], memory: &dyn MemoryAccess) -> Result<Vec<u8>, String> {
    let mut output=Vec::new();let mut arg=0usize;let mut at=0usize;
    while at<format.len() {
        if format[at]!=b'%' {output.push(format[at]);at+=1;continue;}
        at+=1;if at==format.len(){break}if format[at]==b'%' {output.push(b'%');at+=1;continue;}
        let mut left=false;let mut zero=false;
        while at<format.len()&&matches!(format[at],b'-'|b'0'|b'#'|b' '|b'+') {left|=format[at]==b'-';zero|=format[at]==b'0';at+=1;}
        let mut width=0usize;while at<format.len()&&format[at].is_ascii_digit(){width=width.saturating_mul(10).saturating_add((format[at]-b'0')as usize).min(1024);at+=1;}
        let mut precision=None;if at<format.len()&&format[at]==b'.'{at+=1;let mut p=0usize;while at<format.len()&&format[at].is_ascii_digit(){p=p.saturating_mul(10).saturating_add((format[at]-b'0')as usize).min(1024);at+=1;}precision=Some(p);}
        while at<format.len()&&matches!(format[at],b'l'|b'h'|b'I'|b'z'|b'w'){at+=1;}
        let kind=*format.get(at).ok_or("incomplete wsprintf format")?;at+=1;
        let value=*values.get(arg).ok_or("not enough wsprintf arguments")?;arg+=1;
        let mut piece=match kind {
            b'x'=>format!("{value:x}").into_bytes(),b'X'=>format!("{value:X}").into_bytes(),
            b'd'|b'i'=>format!("{}",value as i32).into_bytes(),b'u'=>value.to_string().into_bytes(),
            b'p'=>format!("{value:08x}").into_bytes(),b'c'=>vec![value as u8],
            b's'|b'S'=>if value==0{Vec::new()}else{string(memory,value)?},
            other=>vec![other],
        };
        if let Some(p)=precision {if matches!(kind,b's'|b'S')&&piece.len()>p{piece.truncate(p);}}
        let padding=width.saturating_sub(piece.len()).min(1024);if !left&&padding>0 {let pad=if zero{b'0'}else{b' '};output.extend(std::iter::repeat(pad).take(padding));}
        output.extend(piece);if left&&padding>0{output.extend(std::iter::repeat(b' ').take(padding));}
        if output.len()>1023 {output.truncate(1023);break;}
    }
    Ok(output)
}
impl Runtime {
    fn gl_window_for_dc(&self, hdc: u32) -> Option<u32> {
        let hwnd = self
            .common_dcs
            .get(&hdc)
            .copied()
            .filter(|hwnd| *hwnd != 0)
            .or_else(|| self.gdi.window_for_dc(hdc))?;
        self.windows.get(hwnd).ok()?;
        self.gdi.surface(hwnd)?;
        Some(hwnd)
    }

    /// Current WGL binding as `(HGLRC, HDC, HWND)` for the renderer.
    pub fn current_gl_context(&self) -> Option<(u32, u32, u32)> {
        self.gl_context.current()
    }

    /// Enable browser-oriented ordered GL batch capture. Native execution
    /// leaves this disabled and continues to rasterize directly into RGBA.
    pub fn set_gl_gpu_capture(&mut self, enabled: bool) {
        self.opengl.set_gpu_capture(enabled);
    }

    pub fn set_gl_gpu_mode(&mut self, enabled: bool) {
        self.opengl.set_gpu_mode(enabled);
    }

    /// Drain clip-space GL operations for the window bound to the current
    /// context. The returned dimensions describe the current drawable surface.
    pub fn take_gl_gpu_ops(
        &mut self,
        window: u32,
    ) -> Option<(u32, u32, Vec<crate::gl_renderer::GpuOp>)> {
        let (context, _, bound_window) = self.gl_context.current()?;
        if bound_window != window {
            return None;
        }
        let surface = self.gdi.surface(window)?;
        Some((
            surface.width,
            surface.height,
            self.opengl.take_gpu_ops(context),
        ))
    }

    pub fn current_gl_window(&self) -> u32 {
        self.gl_context.current().map_or(0, |(_, _, window)| window)
    }

    /// Validate and return the active WGL binding before executing GL work.
    pub fn require_gl_context(&self) -> Result<(u32, u32, u32), String> {
        let (hglrc, hdc, hwnd) = self.gl_context.require_current().map_err(str::to_owned)?;
        if self.gl_window_for_dc(hdc) != Some(hwnd) {
            return Err("OpenGL current DC no longer belongs to its window".into());
        }
        Ok((hglrc, hdc, hwnd))
    }

    /// Consume the last successful SwapBuffers boundary for presentation.
    pub fn take_gl_swap(&mut self) -> Option<(u32, u32, u32)> {
        self.gl_context.take_swap()
    }

    pub fn register_file(&mut self, path: &[u8], bytes: Vec<u8>) {
        self.files.insert(path, bytes);
    }
    /// Select the guest display profile before executing the program. Browser
    /// presentation remains RGBA, including for the indexed guest display.
    pub fn set_display_bits(&mut self, bits: u32) -> Result<(), String> {
        if !matches!(bits, 8 | 32) { return Err("display depth must be 8 or 32".into()); }
        self.display_bits = bits;
        Ok(())
    }
    pub fn next_timer_delay(&self) -> u32 {
        self.timers
            .values()
            .map(|(due, _, _)| due.saturating_sub(self.clock_ms).min(u32::MAX as u64) as u32)
            .min()
            .unwrap_or(u32::MAX)
    }
    pub fn advance_time(&mut self, elapsed: u32) -> Result<(), String> {
        self.clock_ms = self.clock_ms.saturating_add(elapsed as u64);
        let mut due = Vec::new();
        for (&(window, id), timer) in &mut self.timers {
            while timer.0 <= self.clock_ms {
                due.push((window, id, timer.2));
                timer.0 = timer.0.saturating_add(timer.1 as u64);
            }
        }
        for (window, id, callback) in due {
            self.post(Message {
                window,
                message: 0x113,
                wparam: id,
                lparam: callback,
            })?;
        }
        Ok(())
    }
    fn static_window_proc(
        &mut self,
        window: u32,
        message: u32,
        wparam: u32,
        lparam: u32,
        memory: &mut dyn MemoryAccess,
    ) -> Result<u32, String> {
        if !self.window_data.contains_key(&window) {
            return Ok(0);
        }
        if self
            .window_data
            .get(&window)
            .is_some_and(|data| data.class == b"LISTBOX")
        {
            let state = self.list_boxes.entry(window).or_default();
            return Ok(match message {
                0x180 => {
                    state.items.push(if lparam == 0 {
                        Vec::new()
                    } else {
                        string(memory, lparam)?
                    });
                    state.items.len() as u32 - 1
                }
                0x184 => {
                    state.items.clear();
                    state.selection = -1;
                    0
                }
                0x186 => {
                    let previous = state.selection;
                    state.selection = if wparam as i32 >= 0 && (wparam as usize) < state.items.len()
                    {
                        wparam as i32
                    } else {
                        -1
                    };
                    previous as u32
                }
                0x188 => state.selection as u32,
                0x18b => state.items.len() as u32,
                0x18a => state
                    .items
                    .get(wparam as usize)
                    .map_or(u32::MAX, |item| item.len() as u32),
                0x189 => {
                    if let Some(item) = state.items.get(wparam as usize) {
                        let bytes = &item[..item.len().min(1023)];
                        memory
                            .write(lparam, bytes)
                            .map_err(|e| format!("listbox text: {e:?}"))?;
                        memory
                            .write(lparam + bytes.len() as u32, &[0])
                            .map_err(|e| format!("listbox text NUL: {e:?}"))?;
                        bytes.len() as u32
                    } else {
                        u32::MAX
                    }
                }
                0x18f | 0x18c => {
                    let query = if lparam == 0 {
                        Vec::new()
                    } else {
                        string(memory, lparam)?
                    };
                    let start = if (wparam as i32) < 0 {
                        0
                    } else {
                        (wparam as usize + 1).min(state.items.len())
                    };
                    let found = state
                        .items
                        .iter()
                        .enumerate()
                        .skip(start)
                        .find(|(_, item)| {
                            item.len() >= query.len()
                                && item[..query.len()].eq_ignore_ascii_case(&query)
                        })
                        .map(|(index, _)| index as i32);
                    if let Some(index) = found {
                        state.selection = index;
                        index as u32
                    } else {
                        u32::MAX
                    }
                }
                0x0f => {
                    self.paint_static_control(window)?;
                    0
                }
                0x81 => 1,
                0x14 => 1,
                0x83 => 0,
                _ => 0,
            });
        }
        match message {
            0x81 => Ok(1),
            0x0c => {
                let text = if lparam == 0 {
                    Vec::new()
                } else {
                    string(memory, lparam)?
                };
                self.window_data.get_mut(&window).unwrap().text = text;
                Ok(1)
            }
            0x0d => {
                let text = self.window_data.get(&window).unwrap().text.clone();
                if lparam == 0 {
                    return Ok(text.len() as u32);
                }
                let bytes = &text[..text.len().min(wparam.saturating_sub(1) as usize)];
                memory
                    .write(lparam, bytes)
                    .map_err(|e| format!("static WM_GETTEXT: {e:?}"))?;
                memory
                    .write(lparam + bytes.len() as u32, &[0])
                    .map_err(|e| format!("static WM_GETTEXT NUL: {e:?}"))?;
                Ok(bytes.len() as u32)
            }
            0x0e => Ok(self
                .window_data
                .get(&window)
                .map_or(0, |d| d.text.len() as u32)),
            0x0f => {
                self.paint_static_control(window)?;
                Ok(0)
            }
            0x14 => Ok(1),
            0x83 => Ok(0),
            _ => Ok(0),
        }
    }
    fn paint_static_control(&mut self, window: u32) -> Result<(), String> {
        self.windows
            .begin_paint(window)
            .map_err(|e| format!("static BeginPaint: {e:?}"))?;
        let rendered = self.render_static(window);
        let ended = self
            .windows
            .end_paint(window)
            .map_err(|e| format!("static EndPaint: {e:?}"));
        rendered?;
        ended?;
        Ok(())
    }
    fn render_static(&mut self, window: u32) -> Result<(), String> {
        let data = self
            .window_data
            .get(&window)
            .ok_or("unknown static control")?;
        let class = data.class.clone();
        let style = data.style;
        let text = data.text.clone();
        let parent = data.parent;
        let (x, y) = (data.x, data.y);
        let (width, height) = {
            let w = self
                .windows
                .get(window)
                .map_err(|e| format!("static dimensions: {e:?}"))?;
            (w.width, w.height)
        };
        self.gdi
            .fill_surface(
                window,
                Rect {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                },
                0xc0c0c0,
            )
            .map_err(|e| format!("static background: {e:?}"))?;
        match class.as_slice() {
            b"BUTTON" => {
                self.gdi
                    .fill_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: 1,
                        },
                        0xffffff,
                    )
                    .map_err(|e| format!("button top border: {e:?}"))?;
                self.gdi
                    .fill_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: 1,
                            bottom: height as i32,
                        },
                        0xffffff,
                    )
                    .map_err(|e| format!("button left border: {e:?}"))?;
                self.gdi
                    .fill_surface(
                        window,
                        Rect {
                            left: 1,
                            top: height as i32 - 1,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        0x808080,
                    )
                    .map_err(|e| format!("button bottom border: {e:?}"))?;
                self.gdi
                    .fill_surface(
                        window,
                        Rect {
                            left: width as i32 - 1,
                            top: 1,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        0x808080,
                    )
                    .map_err(|e| format!("button right border: {e:?}"))?;
                self.gdi
                    .draw_default_text(
                        window,
                        0,
                        ((height as i32 - 14) / 2).max(0),
                        width,
                        &text,
                        0,
                    )
                    .map_err(|e| format!("button text: {e:?}"))?;
            }
            b"EDIT" => {
                self.gdi
                    .fill_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        0xffffff,
                    )
                    .map_err(|e| format!("edit background: {e:?}"))?;
                self.gdi
                    .frame_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        0x808080,
                    )
                    .map_err(|e| format!("edit frame: {e:?}"))?;
                self.gdi
                    .draw_default_text(window, 0, 2, width, &text, 0)
                    .map_err(|e| format!("edit text: {e:?}"))?;
            }
            b"LISTBOX" => {
                self.gdi
                    .fill_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        0xffffff,
                    )
                    .map_err(|e| format!("listbox background: {e:?}"))?;
                self.gdi
                    .frame_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        0x808080,
                    )
                    .map_err(|e| format!("listbox frame: {e:?}"))?;
                let items = self.list_boxes.get(&window).cloned().unwrap_or_default();
                let rows = (height / 16).max(1) as usize;
                let first = if items.selection < rows as i32 {
                    0
                } else {
                    items.selection as usize - rows + 1
                };
                for row in 0..rows {
                    let Some(item) = items.items.get(first + row) else {
                        break;
                    };
                    let top = row as i32 * 16;
                    if first + row == items.selection as usize {
                        self.gdi
                            .fill_surface(
                                window,
                                Rect {
                                    left: 1,
                                    top,
                                    right: width as i32 - 1,
                                    bottom: (top + 16).min(height as i32 - 1),
                                },
                                0x800000,
                            )
                            .map_err(|e| format!("listbox selection: {e:?}"))?;
                    }
                    self.gdi
                        .draw_default_text(
                            window,
                            0,
                            top,
                            width,
                            item,
                            if first + row == items.selection as usize {
                                0xffffff
                            } else {
                                0
                            },
                        )
                        .map_err(|e| format!("listbox item: {e:?}"))?;
                }
            }
            b"STATIC" => match style & 15 {
                7 => self
                    .gdi
                    .frame_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        0,
                    )
                    .map_err(|e| format!("static frame: {e:?}"))?,
                1 | 2 | 3 => self
                    .gdi
                    .draw_default_text(
                        window,
                        0,
                        ((height as i32 - 14) / 2).max(0),
                        width,
                        &text,
                        0,
                    )
                    .map_err(|e| format!("static text: {e:?}"))?,
                0 => {}
                style => return Err(format!("unsupported STATIC style {style}")),
            },
            _ => {
                return Err(format!(
                    "unsupported dialog child class {}",
                    String::from_utf8_lossy(&class)
                ))
            }
        }
        if parent != 0 {
            self.gdi
                .composite(parent, window, x, y)
                .map_err(|e| format!("static composition: {e:?}"))?;
        }
        Ok(())
    }
    pub fn compose_children(&mut self, parent: u32) -> Result<(), String> {
        let children = self
            .window_data
            .iter()
            .filter_map(|(&handle, data)| {
                (data.parent == parent).then_some((handle, data.x, data.y))
            })
            .collect::<Vec<_>>();
        for (child, x, y) in children {
            self.gdi
                .composite(parent, child, x, y)
                .map_err(|e| format!("child composition: {e:?}"))?;
        }
        Ok(())
    }
    fn cancel_dialog(&mut self, window: u32) -> Result<(), String> {
        let children = self
            .window_data
            .iter()
            .filter_map(|(&handle, data)| (data.parent == window).then_some(handle))
            .collect::<Vec<_>>();
        for child in children {
            self.window_data.remove(&child);
            self.gdi
                .remove_surface(child)
                .map_err(|e| format!("cancel dialog control: {e:?}"))?;
            self.windows.cancel_create(child);
        }
        self.window_data.remove(&window);
        self.gdi
            .remove_surface(window)
            .map_err(|e| format!("cancel dialog surface: {e:?}"))?;
        self.windows.cancel_create(window);
        Ok(())
    }
    fn finish_modal(&mut self, key: DialogKey) -> Result<Effect, String> {
        let Some(state) = self.modal_dialogs.get(&key).copied() else {
            return Err("unknown modal dialog".into());
        };
        if let Some(result) = state.result {
            self.modal_dialogs.remove(&key);
            self.modal_windows.remove(&state.window);
            self.cancel_dialog(state.window)?;
            Ok(Effect::Return(result))
        } else {
            Ok(Effect::Wait)
        }
    }
    fn callback(
        &mut self,
        procedure: u32,
        args: Vec<u32>,
        context: Option<HookContext>,
        finish: CallbackFinish,
    ) -> Result<Effect, String> {
        self.next_callback = self
            .next_callback
            .checked_add(1)
            .ok_or("callback tokens exhausted")?;
        let token = self.next_callback;
        self.callbacks
            .insert(token, CallbackPending { context, finish });
        self.callback_stack.push(token);
        Ok(Effect::Invoke {
            token,
            procedure,
            args,
        })
    }
    fn hook(
        &mut self,
        chain: Vec<u32>,
        mut index: usize,
        code: u32,
        wparam: u32,
        lparam: u32,
        finish: CallbackFinish,
    ) -> Result<Effect, String> {
        while index < chain.len() {
            if let Some((_, procedure, _)) = self.message_hooks.get(chain[index]).copied() {
                return self.callback(
                    procedure,
                    vec![code, wparam, lparam],
                    Some(HookContext { chain, index }),
                    finish,
                );
            }
            index += 1;
        }
        Ok(Effect::Return(0))
    }
    pub fn register_code(&mut self, address: u32, length: u32) {
        self.code_ranges
            .push((address as u64, address as u64 + length as u64));
    }
    fn allocate(&mut self, memory: &dyn MemoryAccess, size: u32) -> Result<u32, String> {
        self.arena.allocate(size, memory.byte_len())
    }
    fn action(&mut self, action: Action, finish: Finish) -> Result<Effect, String> {
        match action {
            Action::Invoke {
                token,
                procedure,
                args,
            } => {
                self.pending.insert(token, finish);
                Ok(Effect::Invoke {
                    token,
                    procedure: self
                        .windows
                        .get(args[0])
                        .map(|w| w.procedure)
                        .unwrap_or(procedure),
                    args: args.to_vec(),
                })
            }
            Action::Wait => Ok(Effect::Wait),
            Action::Return(value) => {
                if let Finish::Create(parameter) = &finish {
                    self.arena.release(*parameter)?;
                }
                match finish {
                    Finish::Create(_) if value != 0 => {
                        let w = self.windows.get(value).map_err(|e| format!("{e:?}"))?;
                        if self.gdi.surface(value).is_none() {
                            self.gdi
                                .create_surface(value, w.width, w.height)
                                .map_err(|e| format!("{e:?}"))?;
                        }
                    }
                    Finish::Destroy(window) if value != 0 => {
                        self.gl_context.window_destroyed(window);
                        self.window_data.remove(&window);
                        self.gdi
                            .remove_surface(window)
                            .map_err(|e| format!("{e:?}"))?;
                    }
                    Finish::Redraw { window, dc } => {
                        self.gdi.delete_dc(dc).map_err(|e| format!("redraw DC: {e:?}"))?;
                        self.common_dcs.remove(&dc);
                        let action = self.windows.send(Message { window, message: 15, wparam: 0, lparam: 0 })
                            .map_err(|e| format!("redraw paint: {e:?}"))?;
                        return self.action(action, Finish::Update);
                    }
                    Finish::Update | Finish::Accelerator => return Ok(Effect::Return(1)),
                    _ => {}
                }
                Ok(Effect::Return(value))
            }
        }
    }
    fn continue_dialog_children(
        &mut self,
        window: u32,
        dialog_procedure: u32,
        init_parameter: u32,
        children: Vec<u32>,
        index: usize,
        memory: &mut dyn MemoryAccess,
    ) -> Result<Effect, String> {
        if index >= children.len() {
            return self.callback(
                dialog_procedure,
                vec![window, 0x110, 0, init_parameter],
                None,
                CallbackFinish::DialogInit(window),
            );
        }
        let child = children[index];
        let data = self
            .window_data
            .get(&child)
            .ok_or("dialog child data missing")?;
        let (class, title, instance, id, style, ex_style, x, y) = (
            data.class.clone(),
            data.text.clone(),
            data.instance,
            data.id,
            data.style,
            data.ex_style,
            data.x,
            data.y,
        );
        let child_window = self
            .windows
            .get(child)
            .map_err(|e| format!("dialog child window missing: {e:?}"))?;
        let (width, height) = (child_window.width, child_window.height);
        let storage = self.allocate(memory, 80)?;
        let class_ptr = storage + 56;
        let title_ptr = storage + 64;
        let cbt = storage + 68;
        memory
            .write(class_ptr, &[class.as_slice(), &[0]].concat())
            .map_err(|e| format!("child class string: {e:?}"))?;
        memory
            .write(title_ptr, &[title.as_slice(), &[0]].concat())
            .map_err(|e| format!("child title string: {e:?}"))?;
        let fields = [
            0, instance, id, window, height, width, y as u32, x as u32, style, title_ptr,
            class_ptr, ex_style,
        ];
        for (i, value) in fields.iter().enumerate() {
            write(memory, storage + i as u32 * 4, *value)?;
        }
        write(memory, cbt, storage)?;
        write(memory, cbt + 4, 0)?;
        let context = DialogChildContext {
            window,
            dialog_procedure,
            init_parameter,
            children,
            index,
            child,
            storage,
        };
        let chain = self
            .hook_order
            .iter()
            .rev()
            .copied()
            .filter(|handle| {
                self.message_hooks
                    .get(*handle)
                    .is_some_and(|(kind, _, _)| *kind == 5)
            })
            .collect::<Vec<_>>();
        if chain.is_empty() {
            self.create_dialog_child(context, memory)
        } else {
            self.hook(
                chain,
                0,
                3,
                child,
                cbt,
                CallbackFinish::DialogChildHook(context),
            )
        }
    }
    fn create_dialog_child(
        &mut self,
        context: DialogChildContext,
        memory: &mut dyn MemoryAccess,
    ) -> Result<Effect, String> {
        let procedure = self
            .windows
            .get(context.child)
            .map_err(|e| format!("child procedure: {e:?}"))?
            .procedure;
        if procedure == 0xffff_ff02 || procedure == 0xffff_ff01 {
            self.arena.release(context.storage)?;
            return self.continue_dialog_children(
                context.window,
                context.dialog_procedure,
                context.init_parameter,
                context.children,
                context.index + 1,
                memory,
            );
        }
        self.callback(
            procedure,
            vec![context.child, 0x81, 0, context.storage],
            None,
            CallbackFinish::DialogChildNcCreate(context),
        )
    }
    pub fn resume(&mut self, token: u64, value: u32) -> Result<Effect, String> {
        self.resume_inner(token, value, None)
    }
    pub fn resume_with_memory(
        &mut self,
        token: u64,
        value: u32,
        memory: &mut dyn MemoryAccess,
    ) -> Result<Effect, String> {
        self.resume_inner(token, value, Some(memory))
    }
    fn resume_inner(
        &mut self,
        token: u64,
        value: u32,
        mut memory: Option<&mut dyn MemoryAccess>,
    ) -> Result<Effect, String> {
        if let Some(pending) = self.callbacks.remove(&token) {
            if self.callback_stack.pop() != Some(token) {
                return Err("callback continuation order".into());
            }
            return match pending.finish {
                CallbackFinish::Return => Ok(Effect::Return(value)),
                CallbackFinish::DialogInit(window) => {
                    if let Some(key) = self.modal_windows.get(&window).copied() {
                        self.finish_modal(key)
                    } else {
                        Ok(Effect::Return(window))
                    }
                }
                CallbackFinish::DialogBoxMessage { key } => self.finish_modal(key),
                CallbackFinish::DialogHook {
                    window,
                    procedure,
                    init_parameter,
                    storage,
                } => {
                    if value != 0 {
                        self.arena.release(storage)?;
                        return Ok(Effect::Return(0));
                    }
                    let wndproc = self
                        .windows
                        .get(window)
                        .map_err(|e| format!("dialog procedure: {e:?}"))?
                        .procedure;
                    self.callback(
                        wndproc,
                        vec![window, 0x81, 0, storage],
                        None,
                        CallbackFinish::DialogNcCreate {
                            window,
                            dialog_procedure: procedure,
                            init_parameter,
                            storage,
                        },
                    )
                }
                CallbackFinish::DialogNcCreate {
                    window,
                    dialog_procedure,
                    init_parameter,
                    storage,
                } => {
                    if value == 0 {
                        self.arena.release(storage)?;
                        self.cancel_dialog(window)?;
                        return Ok(Effect::Return(0));
                    }
                    let wndproc = self
                        .windows
                        .get(window)
                        .map_err(|e| format!("dialog procedure: {e:?}"))?
                        .procedure;
                    self.callback(
                        wndproc,
                        vec![window, 1, 0, storage],
                        None,
                        CallbackFinish::DialogCreate {
                            window,
                            dialog_procedure,
                            init_parameter,
                            storage,
                        },
                    )
                }
                CallbackFinish::DialogCreate {
                    window,
                    dialog_procedure,
                    init_parameter,
                    storage,
                } => {
                    self.arena.release(storage)?;
                    if value == u32::MAX {
                        self.cancel_dialog(window)?;
                        return Ok(Effect::Return(0));
                    }
                    let children = self
                        .window_data
                        .iter()
                        .filter_map(|(handle, data)| (data.parent == window).then_some(*handle))
                        .collect();
                    self.continue_dialog_children(
                        window,
                        dialog_procedure,
                        init_parameter,
                        children,
                        0,
                        memory
                            .as_deref_mut()
                            .ok_or("dialog continuation requires guest memory")?,
                    )
                }
                CallbackFinish::DialogChildren {
                    window,
                    dialog_procedure,
                    init_parameter,
                    children,
                    index,
                } => self.continue_dialog_children(
                    window,
                    dialog_procedure,
                    init_parameter,
                    children,
                    index,
                    memory
                        .as_deref_mut()
                        .ok_or("dialog continuation requires guest memory")?,
                ),
                CallbackFinish::DialogChildHook(context) => {
                    if value != 0 {
                        self.arena.release(context.storage)?;
                        return self.continue_dialog_children(
                            context.window,
                            context.dialog_procedure,
                            context.init_parameter,
                            context.children,
                            context.index + 1,
                            memory
                                .as_deref_mut()
                                .ok_or("dialog continuation requires guest memory")?,
                        );
                    }
                    self.create_dialog_child(
                        context,
                        memory
                            .as_deref_mut()
                            .ok_or("dialog continuation requires guest memory")?,
                    )
                }
                CallbackFinish::DialogChildNcCreate(context) => {
                    if value == 0 {
                        self.arena.release(context.storage)?;
                        return self.continue_dialog_children(
                            context.window,
                            context.dialog_procedure,
                            context.init_parameter,
                            context.children,
                            context.index + 1,
                            memory
                                .as_deref_mut()
                                .ok_or("dialog continuation requires guest memory")?,
                        );
                    }
                    let procedure = self
                        .windows
                        .get(context.child)
                        .map_err(|e| format!("child procedure: {e:?}"))?
                        .procedure;
                    self.callback(
                        procedure,
                        vec![context.child, 1, 0, context.storage],
                        None,
                        CallbackFinish::DialogChildCreate(context),
                    )
                }
                CallbackFinish::DialogChildCreate(context) => {
                    self.arena.release(context.storage)?;
                    self.continue_dialog_children(
                        context.window,
                        context.dialog_procedure,
                        context.init_parameter,
                        context.children,
                        context.index + 1,
                        memory
                            .as_deref_mut()
                            .ok_or("dialog continuation requires guest memory")?,
                    )
                }
                CallbackFinish::Create {
                    action,
                    parameter,
                    create_parameter,
                    window,
                } => {
                    self.arena.release(parameter)?;
                    if value != 0 {
                        self.windows.cancel_create(window);
                        self.window_data.remove(&window);
                        if self.gdi.surface(window).is_some() {
                            self.gdi.remove_surface(window).map_err(|e| format!("cancel window surface: {e:?}"))?;
                        }
                        self.action(Action::Return(0), Finish::Create(create_parameter))
                    } else {
                        self.action(action, Finish::Create(create_parameter))
                    }
                }
            };
        }
        let finish = self
            .pending
            .remove(&token)
            .ok_or("unknown runtime continuation")?;
        let action = self
            .windows
            .resume(token, value)
            .map_err(|e| format!("window continuation: {e:?}"))?;
        self.action(action, finish)
    }
    pub fn post(&mut self, message: Message) -> Result<(), String> {
        if matches!(message.message, 0x200..=0x209) {
            self.cursor = (
                message.lparam as i16 as i32,
                (message.lparam >> 16) as i16 as i32,
            );
        }
        self.windows
            .post(message)
            .map_err(|e| format!("post message: {e:?}"))
    }

    /// Resize a browser-backed guest window and deliver the normal Win32
    /// WM_SIZE notification before its next message dispatch.
    pub fn resize_window(&mut self, window: u32, width: u32, height: u32) -> Result<(), String> {
        let current = self
            .windows
            .get(window)
            .map_err(|e| format!("resize window: {e:?}"))?;
        if (current.width, current.height) == (width, height) {
            return Ok(());
        }
        let (x, y) = (current.x, current.y);
        self.windows
            .move_window(window, x, y, width, height)
            .map_err(|e| format!("resize window: {e:?}"))?;
        self.gdi
            .resize_surface(window, width, height)
            .map_err(|e| format!("resize surface: {e:?}"))?;
        self.windows
            .post(Message {
                window,
                message: crate::window::WM_SIZE,
                wparam: 0, // SIZE_RESTORED
                lparam: (width.min(u16::MAX as u32)) | (height.min(u16::MAX as u32) << 16),
            })
            .map_err(|e| format!("post WM_SIZE: {e:?}"))
    }

    pub fn call(
        &mut self,
        dll: &str,
        name: &str,
        args: &[u32],
        memory: &mut dyn MemoryAccess,
    ) -> Result<Effect, String> {
        let library = dll.to_ascii_lowercase();
        let library = library.strip_suffix(".dll").unwrap_or(&library);
        let signature = wlam_pcode::abi::signature(library, name)
            .ok_or_else(|| format!("unsupported import {dll}!{name}"))?;
        let variadic_wsprintf = library == "user32" && name == "wsprintfA";
        if (!variadic_wsprintf && args.len() != signature.argument_bytes as usize / 4)
            || (variadic_wsprintf && args.len() < 2) {
            return Err(format!("wrong argument count for {dll}!{name}"));
        }
        if (library == "opengl32" && name.starts_with("gl")) || library == "glu32" {
            if library == "opengl32" && name == "glGetError" {
                return Ok(Effect::Return(
                    self.current_gl_context()
                        .map_or(0, |(context, _, _)| self.opengl.get_error(context)),
                ));
            }
            let (context, _, window) = self.require_gl_context()?;
            let result = self.opengl.call(context, window, name, args, memory, &self.gdi)?;
            return Ok(Effect::Return(result));
        }
        let a = |i: usize| args[i];
        let value = match (library, name) {
            ("kernel32", "lstrcmpA") => match string(memory, a(0))?.cmp(&string(memory, a(1))?) {
                std::cmp::Ordering::Less => u32::MAX,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            },
            ("user32", "wsprintfA") => {
                let output=format_wsprintf(&string(memory,a(1))?,&args[2..],memory)?;
                let end=a(0).checked_add(output.len() as u32+1).ok_or("wsprintf buffer overflow")?;
                if end as u64>memory.byte_len(){return Err("wsprintf output outside guest memory".into());}
                let mut bytes=output.clone();bytes.push(0);memory.write(a(0),&bytes).map_err(|e|format!("wsprintf output: {e:?}"))?;
                output.len() as u32
            }
            ("user32", "LoadStringA") if a(0) == 0 || a(0) == self.image_base => {
                if (a(3) as i32) <= 0 {
                    0
                } else if let Some(resource) = self.resources.named(6, (a(1) >> 4) + 1) {
                    let end = resource
                        .address
                        .checked_add(resource.size)
                        .ok_or("string resource overflow")?;
                    let mut pointer = resource.address;
                    let mut text = Vec::new();
                    for index in 0..=(a(1) & 15) {
                        if pointer.checked_add(2).is_none_or(|p| p > end) {
                            return Err("truncated string resource".into());
                        }
                        let mut length = [0; 2];
                        memory
                            .read(pointer, &mut length)
                            .map_err(|e| format!("string resource: {e:?}"))?;
                        let length = u16::from_le_bytes(length) as u32;
                        pointer += 2;
                        if pointer.checked_add(length * 2).is_none_or(|p| p > end) {
                            return Err("truncated string resource text".into());
                        }
                        if index == (a(1) & 15) {
                            for offset in 0..length.min(a(3) - 1) {
                                let mut unit = [0; 2];
                                memory
                                    .read(pointer + offset * 2, &mut unit)
                                    .map_err(|e| format!("string resource text: {e:?}"))?;
                                text.push(crate::locale::ansi_encode(u16::from_le_bytes(unit)));
                            }
                        }
                        pointer += length * 2;
                    }
                    let count = text.len() as u32;
                    text.push(0);
                    memory
                        .write(a(2), &text)
                        .map_err(|e| format!("LoadString output: {e:?}"))?;
                    count
                } else {
                    self.thread.last_error = 1814;
                    0
                }
            }
            ("kernel32", "lstrlenA") => {
                if a(0) == 0 {
                    0
                } else {
                    string(memory, a(0))?.len() as u32
                }
            }
            ("kernel32", "lstrcpyA" | "lstrcatA") => {
                let mut text = if name == "lstrcatA" {
                    string(memory, a(0))?
                } else {
                    Vec::new()
                };
                text.extend(string(memory, a(1))?);
                text.push(0);
                memory
                    .write(a(0), &text)
                    .map_err(|e| format!("string copy: {e:?}"))?;
                a(0)
            }
            ("user32", "GetWindowLongA" | "SetWindowLongA") => {
                let old = if a(1) as i32 == -4 {
                    let procedure = self
                        .windows
                        .get(a(0))
                        .map_err(|e| format!("window procedure: {e:?}"))?
                        .procedure;
                    if name == "SetWindowLongA" {
                        self.windows
                            .set_procedure(a(0), a(2))
                            .map_err(|e| format!("window procedure: {e:?}"))?;
                    }
                    procedure
                } else {
                    let data = self
                        .window_data
                        .get_mut(&a(0))
                        .ok_or("invalid window data handle")?;
                    let slot = match a(1) as i32 {
                        -6 => Some(&mut data.instance),
                        -8 => Some(&mut data.parent),
                        -12 => Some(&mut data.id),
                        -16 => Some(&mut data.style),
                        -20 => Some(&mut data.ex_style),
                        -21 => Some(&mut data.user_data),
                        _ => None,
                    };
                    if let Some(slot) = slot {
                        if name == "SetWindowLongA" {
                            std::mem::replace(slot, a(2))
                        } else {
                            *slot
                        }
                    } else {
                        let offset = a(1) as usize;
                        if offset
                            .checked_add(4)
                            .is_none_or(|end| end > data.extra.len())
                        {
                            self.thread.last_error = 1413;
                            return Ok(Effect::Return(0));
                        }
                        let old =
                            u32::from_le_bytes(data.extra[offset..offset + 4].try_into().unwrap());
                        if name == "SetWindowLongA" {
                            data.extra[offset..offset + 4].copy_from_slice(&a(2).to_le_bytes());
                        }
                        old
                    }
                };
                old
            }
            ("user32", "GetParent") => {
                self.window_data
                    .get(&a(0))
                    .ok_or("invalid parent window")?
                    .parent
            }
            ("user32", "GetDlgCtrlID") => self
                .windows
                .get(a(0))
                .map_or(0, |window| window.id),
            ("user32", "GetForegroundWindow") => self.windows.foreground_window(),
            ("user32", "SetFocus") => {
                if a(0) != 0 && !self.window_data.contains_key(&a(0)) {
                    self.thread.last_error = 1400;
                    0
                } else {
                    std::mem::replace(&mut self.focused_window, a(0))
                }
            }
            ("user32", "GetMenu") => {
                if !self.window_data.contains_key(&a(0)) { return Err("GetMenu: invalid window".into()); }
                self.menus.get(&a(0)).copied().unwrap_or(0)
            }
            ("user32", "SetMenu") => {
                if !self.window_data.contains_key(&a(0)) { return Err("SetMenu: invalid window".into()); }
                if a(1)==0 { self.menus.remove(&a(0)); } else { self.menus.insert(a(0),a(1)); }
                1
            }
            ("user32", "GetClassNameA") => {
                let class = &self
                    .window_data
                    .get(&a(0))
                    .ok_or("invalid class window")?
                    .class;
                crate::startup::write_cstr(memory, a(1), a(2), class)
                    .map_err(|e| e.to_string())?
                    .0
            }
            ("user32", "CallNextHookEx") => {
                let context = self
                    .callback_stack
                    .iter()
                    .rev()
                    .find_map(|token| self.callbacks.get(token)?.context.clone());
                if let Some(context) = context {
                    return self.hook(
                        context.chain,
                        context.index + 1,
                        a(1),
                        a(2),
                        a(3),
                        CallbackFinish::Return,
                    );
                }
                0
            }
            ("user32", "CallWindowProcA") => {
                if let Some((dll, procedure)) = self.modules.import_at(a(0)) {
                    return self.call(&dll, &procedure, &args[1..], memory);
                }
                if a(0) == 0xffff_ff02 {
                    return Ok(Effect::Return(self.static_window_proc(
                        a(1),
                        a(2),
                        a(3),
                        a(4),
                        memory,
                    )?));
                }
                if a(0) == 0xffff_ff01 {
                    if a(2) == crate::window::WM_PAINT {
                        self.windows.validate_paint(a(1));
                    }
                    return Ok(Effect::Return(Windows::default_result(a(2))));
                }
                return self.callback(
                    a(0),
                    vec![a(1), a(2), a(3), a(4)],
                    None,
                    CallbackFinish::Return,
                );
            }
            ("kernel32", "FindResourceA") if a(0) == 0 || a(0) == self.image_base => {
                if a(1) > 0xffff || a(2) > 0xffff {
                    return Err("named resource lookup is not implemented".into());
                }
                self.resources.find(a(2), a(1)).unwrap_or_else(|| {
                    self.thread.last_error = 1814;
                    0
                })
            }
            ("kernel32", "LoadResource") if a(0) == 0 || a(0) == self.image_base => {
                if self.resources.get(a(1)).is_some() {
                    a(1)
                } else {
                    self.thread.last_error = 6;
                    0
                }
            }
            ("kernel32", "LockResource") => self.resources.get(a(0)).map_or(0, |r| r.address),
            ("kernel32", "SizeofResource") if a(0) == 0 || a(0) == self.image_base => {
                self.resources.get(a(1)).map_or(0, |r| r.size)
            }
            ("kernel32", "FreeResource") => 0, // Win32 resource mappings remain resident.
            ("user32", "LoadIconA") => {
                if let Some(handle) = self.shared_icons.get(&(a(0), a(1))) {
                    *handle
                } else {
                    let source = if a(0) == 0 && matches!(a(1), 32512..=32518) {
                        Some((0, a(1)))
                    } else if a(0) == self.image_base && a(1) <= 0xffff {
                        if let Some(group) = self.resources.named(14, a(1)) {
                            let mut header = [0; 6];
                            memory
                                .read(group.address, &mut header)
                                .map_err(|e| format!("icon group: {e:?}"))?;
                            let count = u16::from_le_bytes([header[4], header[5]]) as u32;
                            if count == 0 || 6 + count * 14 > group.size {
                                return Err("invalid icon group".into());
                            }
                            let mut chosen = None;
                            for i in 0..count {
                                let mut entry = [0; 14];
                                memory
                                    .read(group.address + 6 + i * 14, &mut entry)
                                    .map_err(|e| format!("icon entry: {e:?}"))?;
                                let id = u16::from_le_bytes([entry[12], entry[13]]) as u32;
                                if chosen.is_none() || entry[0] == 32 && entry[1] == 32 {
                                    chosen = self.resources.named(3, id);
                                }
                                if entry[0] == 32 && entry[1] == 32 {
                                    break;
                                }
                            }
                            chosen.map(|r| (r.address, r.size))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(source) = source {
                        let handle = self.icons.insert(source).ok_or("icon handles exhausted")?;
                        self.shared_icons.insert((a(0), a(1)), handle);
                        handle
                    } else {
                        self.thread.last_error = 1814;
                        0
                    }
                }
            }
            ("user32", "LoadMenuA") => {
                if a(0) != 0 && a(0) != self.image_base { return Err("LoadMenuA: unsupported module".into()); }
                if a(1) > 0xffff { return Err("LoadMenuA: named menu resources are unsupported".into()); }
                self.resources.named(4,a(1)).map_or_else(||{self.thread.last_error=1813;0},|resource|resource.address)
            }
            ("user32", "LoadAcceleratorsA") => {
                if a(0) != 0 && a(0) != self.image_base { return Err("LoadAcceleratorsA: unsupported module".into()); }
                if a(1) > 0xffff { return Err("LoadAcceleratorsA: named accelerator resources are unsupported".into()); }
                self.resources.named(9, a(1)).map_or_else(|| { self.thread.last_error = 1813; 0 }, |resource| resource.address)
            }
            ("user32", "TranslateAcceleratorA") => {
                let message = read(memory, a(2) + 4)?;
                if !matches!(message, 0x100 | 0x104 | 0x102 | 0x106) {
                    return Ok(Effect::Return(0));
                }
                let key = read(memory, a(2) + 8)?;
                let table = self.resources.at_address(9, a(1))
                    .ok_or("TranslateAcceleratorA: invalid accelerator table")?;
                if table.size % 8 != 0 { return Err("malformed accelerator table".into()); }
                let modifiers = [(16, 4), (17, 8), (18, 16)].into_iter()
                    .fold(0, |flags, (key, flag)| flags | if self.keys[key] & 0x80 != 0 { flag } else { 0 });
                for offset in (0..table.size).step_by(8) {
                    let entry = table.address.checked_add(offset).ok_or("accelerator address overflow")?;
                    let flags_key = read(memory, entry)?;
                    let flags = flags_key & 0xffff;
                    let virtual_key = flags & 1 != 0;
                    if flags_key >> 16 == key && flags & 0x1c == modifiers
                        && virtual_key == matches!(message, 0x100 | 0x104) {
                        let command = read(memory, entry + 4)? & 0xffff;
                        let action = self.windows.send(Message {
                            window: a(0), message: 0x111, wparam: command | 0x10000, lparam: 0,
                        }).map_err(|e| format!("accelerator dispatch: {e:?}"))?;
                        return self.action(action, Finish::Accelerator);
                    }
                    if flags & 0x80 != 0 { break; }
                }
                0
            }
            ("user32", "SetWindowsHookExA") => {
                if !matches!(a(0), u32::MAX | 5)
                    || a(3) != 1
                    || (a(2) != 0 && a(2) != self.image_base)
                {
                    return Err(
                        "only the current thread's WH_MSGFILTER and WH_CBT hooks are supported"
                            .into(),
                    );
                }
                if !self
                    .code_ranges
                    .iter()
                    .any(|(start, end)| *start <= a(1) as u64 && (a(1) as u64) < *end)
                {
                    self.thread.last_error = 87;
                    0
                } else {
                    let handle = self
                        .message_hooks
                        .insert((a(0), a(1), a(3)))
                        .ok_or("hook handles exhausted")?;
                    self.hook_order.push(handle);
                    handle
                }
            }
            ("user32", "UnhookWindowsHookEx") => {
                self.hook_order.retain(|handle| *handle != a(0));
                self.message_hooks.remove(a(0)).is_some() as u32
            }
            ("kernel32", "SetErrorMode") => std::mem::replace(&mut self.error_mode, a(0)),
            ("user32", "SetMessageQueue") => self.windows.reserve_queue(a(0)) as u32,
            ("comctl32", "#17" | "InitCommonControls") => {
                self.windows.initialize_common_controls();
                0
            }
            ("user32", "RegisterWindowMessageA") => {
                match self.registered_messages.add(&string(memory, a(0))?) {
                    Ok(id) => id as u32,
                    Err(code) => {
                        self.thread.last_error = code;
                        0
                    }
                }
            }
            ("user32", "LoadCursorA") if a(0) == 0 => {
                let id = a(1);
                if matches!(id,32512..=32516|32640..=32646|32648..=32651) {
                    let handle = 0x60010000 | (id - 32512 + 1);
                    self.cursors.insert(handle, id);
                    handle
                } else {
                    self.thread.last_error = 1814;
                    0
                }
            }
            ("user32", "SetCursor") => {
                if a(0) != 0 && !self.cursors.contains_key(&a(0)) {
                    return Err("invalid cursor handle".into());
                }
                std::mem::replace(&mut self.active_cursor, a(0))
            }
            ("gdi32", "CreatePen") => self
                .gdi
                .pen(a(0), a(1), a(2))
                .map_err(|e| format!("pen: {e:?}"))?,
            ("gdi32", "CreateFontIndirectA") => {
                let mut logfont = [0; 60];
                memory.read(a(0), &mut logfont).map_err(|e| format!("LOGFONTA: {e:?}"))?;
                let height = i32::from_le_bytes(logfont[0..4].try_into().unwrap());
                let width = i32::from_le_bytes(logfont[4..8].try_into().unwrap());
                let weight = u32::from_le_bytes(logfont[16..20].try_into().unwrap());
                self.gdi.font(height, width, weight, logfont[20] != 0)
                    .map_err(|e| format!("font: {e:?}"))?
            }
            ("gdi32", "CreatePalette") => {
                let mut header = [0; 4];
                memory
                    .read(a(0), &mut header)
                    .map_err(|e| format!("palette header: {e:?}"))?;
                let version = u16::from_le_bytes([header[0], header[1]]);
                let count = u16::from_le_bytes([header[2], header[3]]) as usize;
                if version != 0x300 || count == 0 || count > 256 {
                    return Err("invalid logical palette header".into());
                }
                let mut raw = vec![0; count * 4];
                memory
                    .read(a(0) + 4, &mut raw)
                    .map_err(|e| format!("palette entries: {e:?}"))?;
                let entries = raw
                    .chunks_exact(4)
                    .map(|e| [e[0], e[1], e[2], e[3]])
                    .collect();
                self.palettes
                    .insert(entries)
                    .ok_or("palette handle table full")?
            }
            ("gdi32", "SelectPalette") => {
                if !self.gdi.is_dc(a(0)) {
                    return Err("invalid DC or palette handle".into());
                }
                if a(1) == 0 {
                    return Ok(Effect::Return(0));
                }
                if self.palettes.get(a(1)).is_none() {
                    return Err("invalid DC or palette handle".into());
                }
                let Effect::Return(default) = self.call("gdi32", "GetStockObject", &[15], memory)? else { unreachable!() };
                self.gdi.dc_state_mut(a(0)).map_err(|e| format!("palette DC: {e:?}"))?.palette.replace(a(1)).unwrap_or(default)
            }
            ("gdi32", "RealizePalette") => {
                if !self.gdi.is_dc(a(0)) {
                    return Err("invalid palette DC".into());
                }
                if self.display_bits != 8 { return Ok(Effect::Return(0)); }
                let Some(handle) = self.gdi.dc_state(a(0)).map_err(|e| format!("palette DC: {e:?}"))?.palette else { return Ok(Effect::Return(0)); };
                let entries = self.palettes.get(handle).ok_or("invalid selected palette")?;
                let mut used = [false; 256];
                let mut changed = 0;
                for entry in entries {
                    // PC_EXPLICIT entries name a physical palette index rather
                    // than an RGB color. Static system colors stay reserved.
                    if entry[3] & 2 != 0 { continue; }
                    if entry[3] & 5 == 0 {
                        if let Some(index) = self.system_palette.iter().position(|color| color[..3] == entry[..3]) {
                            used[index] = true;
                            continue;
                        }
                    }
                    let Some(next) = (10..246).find(|index| !used[*index]) else { break; };
                    let color = [entry[0], entry[1], entry[2], 0];
                    if self.system_palette[next] != color { changed += 1; }
                    self.system_palette[next] = color;
                    used[next] = true;
                }
                changed
            }
            ("gdi32", "GetPaletteEntries") => {
                let entries = self.palettes.get(a(0)).ok_or("invalid palette handle")?;
                let start = a(1) as usize;
                let count = (a(2) as usize).min(entries.len().saturating_sub(start));
                if a(3) != 0 {
                    let bytes = entries
                        .get(start..start + count)
                        .ok_or("palette index out of range")?
                        .iter()
                        .flatten()
                        .copied()
                        .collect::<Vec<_>>();
                    memory
                        .write(a(3), &bytes)
                        .map_err(|e| format!("palette output: {e:?}"))?;
                }
                count as u32
            }
            ("gdi32", "SetPaletteEntries") => {
                let entries = self
                    .palettes
                    .get_mut(a(0))
                    .ok_or("invalid palette handle")?;
                let start = a(1) as usize;
                let count = (a(2) as usize).min(entries.len().saturating_sub(start));
                let mut bytes = vec![0; count * 4];
                memory
                    .read(a(3), &mut bytes)
                    .map_err(|e| format!("palette input: {e:?}"))?;
                for i in 0..count {
                    entries[start + i].copy_from_slice(&bytes[i * 4..i * 4 + 4]);
                }
                count as u32
            }
            ("gdi32", "GetNearestPaletteIndex") => {
                let entries = self.palettes.get(a(0)).ok_or("invalid palette handle")?;
                let color = a(1);
                entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| {
                        let dr = e[0] as i32 - (color & 255) as i32;
                        let dg = e[1] as i32 - ((color >> 8) & 255) as i32;
                        let db = e[2] as i32 - ((color >> 16) & 255) as i32;
                        dr * dr + dg * dg + db * db
                    })
                    .map_or(u32::MAX, |(i, _)| i as u32)
            }
            ("gdi32", "GetSystemPaletteEntries") => {
                if !self.gdi.is_dc(a(0)) {
                    return Err("invalid palette DC".into());
                }
                if self.display_bits != 8 { return Ok(Effect::Return(0)); }
                if a(3) == 0 { return Ok(Effect::Return(256)); }
                let start = (a(1) as usize).min(256);
                let count = (a(2) as usize).min(256 - start);
                let bytes: Vec<u8> = self.system_palette[start..start + count].iter().flatten().copied().collect();
                memory.write(a(3), &bytes).map_err(|e| format!("system palette output: {e:?}"))?;
                count as u32
            }
            ("user32", "GetSysColor") => {
                const CLASSIC: [u32; 25] = [
                    0xc0c0c0, 0x808000, 0x800000, 0x808080, 0xc0c0c0, 0xffffff, 0, 0, 0, 0xffffff,
                    0xc0c0c0, 0xc0c0c0, 0x808080, 0x800000, 0xffffff, 0xc0c0c0, 0x808080, 0x808080,
                    0, 0xc0c0c0, 0xffffff, 0x404040, 0xdfdfdf, 0, 0xe1ffff,
                ];
                *CLASSIC
                    .get(a(0) as usize)
                    .ok_or("unsupported system color index")?
            }
            ("gdi32", "CreateSolidBrush") => self
                .gdi
                .solid_brush(a(0))
                .map_err(|e| format!("solid brush: {e:?}"))?,
            ("gdi32", "SaveDC") => self.gdi.save_dc(a(0)).map_err(|e| format!("SaveDC: {e:?}"))?,
            ("gdi32", "RestoreDC") => self.gdi.restore_dc(a(0), a(1) as i32).map_err(|e| format!("RestoreDC: {e:?}"))? as u32,
            ("gdi32", "GetTextColor" | "GetBkColor" | "GetBkMode" | "GetTextAlign" | "GetStretchBltMode" | "GetROP2"
                | "SetTextColor" | "SetBkColor" | "SetBkMode" | "SetTextAlign" | "SetStretchBltMode" | "SetROP2") => {
                let state = self.gdi.dc_state_mut(a(0)).map_err(|e| format!("DC attribute: {e:?}"))?;
                let value = if name.starts_with("Set") { a(1) } else { 0 };
                let slot = match name {
                    "GetTextColor" | "SetTextColor" => &mut state.text_color,
                    "GetBkColor" | "SetBkColor" => &mut state.background_color,
                    "GetBkMode" | "SetBkMode" => {
                        if name.starts_with("Set") && !matches!(value, 1 | 2) { return Ok(Effect::Return(0)); }
                        &mut state.background_mode
                    }
                    "GetTextAlign" | "SetTextAlign" => {
                        if name.starts_with("Set") && (value & !31 != 0 || !matches!(value & 6, 0 | 2 | 6) || !matches!(value & 24, 0 | 8 | 24)) { return Err("unsupported text alignment".into()); }
                        &mut state.text_align
                    }
                    "GetStretchBltMode" | "SetStretchBltMode" => {
                        if name.starts_with("Set") && !(1..=4).contains(&value) { return Ok(Effect::Return(0)); }
                        &mut state.stretch_mode
                    }
                    _ => {
                        if name.starts_with("Set") && !(1..=16).contains(&value) { return Ok(Effect::Return(0)); }
                        &mut state.rop2
                    }
                };
                if name.starts_with("Set") {
                    if matches!(name, "SetTextColor" | "SetBkColor") && value & 0xff000000 != 0 { return Err("palette-index COLORREF not yet supported".into()); }
                    std::mem::replace(slot, value)
                } else { *slot }
            }
            ("gdi32", "IntersectClipRect") => self.gdi.intersect_clip(a(0), Rect { left: a(1) as i32, top: a(2) as i32, right: a(3) as i32, bottom: a(4) as i32 }).map_err(|e| format!("clip: {e:?}"))?,
            ("gdi32", "GetClipBox") => {
                let clip = self.gdi.clip_bounds(a(0)).map_err(|e| format!("clip: {e:?}"))?;
                write_rect(memory, a(1), [clip.left, clip.top, clip.right, clip.bottom])?;
                if clip.left >= clip.right || clip.top >= clip.bottom { 1 } else { 2 }
            }
            ("gdi32", "TextOutA" | "ExtTextOutA") => {
                let extended = name == "ExtTextOutA";
                let (pointer, count) = if extended { (a(5), a(6)) } else { (a(3), a(4)) };
                if count > 8192 { return Err("text output exceeds 8192 bytes".into()); }
                let mut text = vec![0; count as usize];
                memory.read(pointer, &mut text).map_err(|e| format!("text input: {e:?}"))?;
                let options = if extended { a(3) } else { 0 };
                let rect = if extended && a(4) != 0 {
                    let [left, top, right, bottom] = read_rect(memory, a(4))?;
                    Some(Rect { left, top, right, bottom })
                } else { None };
                let advances = if extended && a(7) != 0 {
                    Some((0..count).map(|i| read(memory, a(7).checked_add(i * 4).ok_or("text advance overflow")?).map(|v| v as i32)).collect::<Result<Vec<_>, String>>()?)
                } else { None };
                self.gdi.text_out(memory, a(0), a(1) as i32, a(2) as i32, &text, options, rect, advances.as_deref()).map_err(|e| format!("text output: {e:?}"))?;
                1
            }
            ("gdi32", "GetTextExtentPointA") => {
                if !self.gdi.is_dc(a(0)) {
                    return Err("text extent: invalid DC".into());
                }
                if a(2) > 65536 {
                    return Err("text extent exceeds supported length".into());
                }
                let mut text = vec![0; a(2) as usize];
                memory
                    .read(a(1), &mut text)
                    .map_err(|e| format!("text extent: {e:?}"))?;
                let (width, height) = crate::font::extent(&text);
                let mut size = [0; 8];
                size[..4].copy_from_slice(&width.to_le_bytes());
                size[4..].copy_from_slice(&height.to_le_bytes());
                memory
                    .write(a(3), &size)
                    .map_err(|e| format!("text size: {e:?}"))?;
                1
            }
            ("gdi32", "GetTextMetricsA") => {
                if !self.gdi.is_dc(a(0)) {
                    return Err("text metrics: invalid DC".into());
                }
                memory
                    .write(a(1), &crate::font::metrics())
                    .map_err(|e| format!("text metrics: {e:?}"))?;
                1
            }
            ("user32", "GetDesktopWindow") => DESKTOP_WINDOW,
            ("user32", "GetDC" | "GetWindowDC") => {
                let target = if a(0) == 0 { DESKTOP_WINDOW } else { a(0) };
                if self.gdi.surface(target).is_none() {
                    let window = self.windows.get(target).map_err(|e| format!("GetDC: invalid window: {e:?}"))?;
                    self.gdi.create_surface(target, window.width, window.height)
                        .map_err(|e| format!("GetDC surface: {e:?}"))?;
                }
                let surface = self.gdi.surface(target).ok_or("GetDC: invalid window")?;
                let clip = Rect {
                    left: 0,
                    top: 0,
                    right: surface.width as i32,
                    bottom: surface.height as i32,
                };
                let dc = self
                    .gdi
                    .paint_dc(target, clip)
                    .map_err(|e| format!("GetDC: {e:?}"))?;
                self.common_dcs.insert(dc, a(0));
                dc
            }
            ("user32", "ReleaseDC") => {
                if self.common_dcs.get(&a(1)) != Some(&a(0)) {
                    0
                } else {
                    self.gdi
                        .delete_dc(a(1))
                        .map_err(|e| format!("ReleaseDC: {e:?}"))?;
                    self.common_dcs.remove(&a(1));
                    1
                }
            }
            ("gdi32", "ChoosePixelFormat") => {
                if self.gl_window_for_dc(a(0)).is_none() || a(1) == 0 {
                    return Ok(Effect::Return(0));
                }
                let mut bytes = [0u8; crate::gl_context::PFD_SIZE];
                memory
                    .read(a(1), &mut bytes)
                    .map_err(|e| format!("ChoosePixelFormat PFD read: {e:?}"))?;
                let Some(request) = crate::gl_context::PixelFormatDescriptor::decode(&bytes) else {
                    return Ok(Effect::Return(0));
                };
                self.gl_context.choose_pixel_format(request)
            }
            ("gdi32", "DescribePixelFormat") => {
                if self.gl_window_for_dc(a(0)).is_none() {
                    return Ok(Effect::Return(0));
                }
                let Some(descriptor) = self.gl_context.descriptor(a(1)) else {
                    return Ok(Effect::Return(0));
                };
                if a(2) != 0 {
                    if a(3) == 0 {
                        return Ok(Effect::Return(0));
                    }
                    let bytes = descriptor.encode();
                    let length = (a(2) as usize).min(bytes.len());
                    memory
                        .write(a(3), &bytes[..length])
                        .map_err(|e| format!("DescribePixelFormat PFD write: {e:?}"))?;
                }
                self.gl_context.pixel_format_count()
            }
            ("gdi32", "SetPixelFormat") => {
                let Some(hwnd) = self.gl_window_for_dc(a(0)) else {
                    return Ok(Effect::Return(0));
                };
                if a(1) != crate::gl_context::PIXEL_FORMAT || a(2) == 0 {
                    return Ok(Effect::Return(0));
                }
                let mut bytes = [0u8; crate::gl_context::PFD_SIZE];
                memory
                    .read(a(2), &mut bytes)
                    .map_err(|e| format!("SetPixelFormat PFD read: {e:?}"))?;
                let Some(descriptor) = crate::gl_context::PixelFormatDescriptor::decode(&bytes)
                else {
                    return Ok(Effect::Return(0));
                };
                if !crate::gl_context::accepts_descriptor(descriptor) {
                    return Ok(Effect::Return(0));
                }
                self.gl_context.set_pixel_format(hwnd, a(1)) as u32
            }
            ("gdi32", "GetPixelFormat") => {
                let Some(hwnd) = self.gl_window_for_dc(a(0)) else {
                    return Ok(Effect::Return(0));
                };
                self.gl_context.pixel_format(hwnd)
            }
            ("gdi32", "SwapBuffers") => {
                let Some(hwnd) = self.gl_window_for_dc(a(0)) else {
                    return Ok(Effect::Return(0));
                };
                let swapped = self.gl_context.swap_buffers(a(0), hwnd);
                if swapped {
                    if let Some((context, _, window)) = self.take_gl_swap() {
                        self.opengl.present(context, window, &mut self.gdi)?;
                    }
                }
                swapped as u32
            }
            ("opengl32", "wglCreateContext") => {
                let Some(hwnd) = self.gl_window_for_dc(a(0)) else {
                    return Ok(Effect::Return(0));
                };
                let format = self.gl_context.pixel_format(hwnd);
                self.gl_context.create_context(format)
            }
            ("opengl32", "wglMakeCurrent") => {
                if a(1) == 0 {
                    return Ok(Effect::Return(self.gl_context.make_current(0, 0, 0) as u32));
                }
                let Some(hwnd) = self.gl_window_for_dc(a(0)) else {
                    return Ok(Effect::Return(0));
                };
                self.gl_context.make_current(a(0), hwnd, a(1)) as u32
            }
            ("opengl32", "wglDeleteContext") => {
                let deleted = self.gl_context.delete_context(a(0));
                if deleted { self.opengl.context_destroyed(a(0)); }
                deleted as u32
            }
            ("gdi32", "GetDeviceCaps") => {
                if !self.gdi.is_dc(a(0)) {
                    return Err("GetDeviceCaps: invalid DC".into());
                }
                match a(1) {
                    0 => 0x300,
                    2 => 1,
                    4 => 169,
                    6 => 127,
                    8 => 640,
                    10 => 480,
                    12 => self.display_bits,
                    108 => 32,
                    14 => 1,
                    24 => if self.display_bits == 8 { 20 } else { u32::MAX },
                    38 => 0x41 | if self.display_bits == 8 { 0x100 } else { 0 },
                    88 | 90 => 96,
                    104 => if self.display_bits == 8 { 256 } else { 0 },
                    106 => if self.display_bits == 8 { 20 } else { 0 },
                    index => return Err(format!("unsupported virtual display capability {index}")),
                }
            }
            ("user32", "GetSystemMetrics") => match a(0) {
                0 | 16 | 78 => 640,
                1 | 79 => 480,
                17 => 461,
                2 | 3 | 9 | 10 | 20 | 21 => 17,
                4 | 15 => 19,
                5 | 6 => 1,
                7 | 8 => 3,
                11..=14 => 32,
                18 | 22 | 23 | 40 | 41 | 42 | 44 | 67 | 70 | 76 | 77 | 4096 => 0,
                19 | 80 | 81 => 1,
                28 | 34 => 112,
                29 | 35 => 27,
                30 | 31 => 18,
                32 | 33 | 36 | 37 | 68 | 69 => 4,
                38 | 39 => 75,
                43 => 3,
                45 | 46 => 2,
                49 | 50 => 16,
                51..=53 => 15,
                54 | 55 => 13,
                index => return Err(format!("unsupported virtual desktop metric {index}")),
            },
            ("kernel32", "IsBadCodePtr") => {
                (!self
                    .code_ranges
                    .iter()
                    .any(|(start, end)| *start <= a(0) as u64 && (a(0) as u64) < *end))
                    as u32
            }
            ("kernel32", "GlobalAddAtomA") => {
                if (1..0xc000).contains(&a(0)) {
                    a(0)
                } else {
                    match self.atoms.add(&string(memory, a(0))?) {
                        Ok(id) => id as u32,
                        Err(code) => {
                            self.thread.last_error = code;
                            0
                        }
                    }
                }
            }
            ("kernel32", "GlobalDeleteAtom") => {
                if !self.atoms.delete(a(0) as u16) {
                    self.thread.last_error = 6;
                }
                0
            }
            ("kernel32", "GlobalGetAtomNameA") => {
                if let Some(name) = self.atoms.name(a(0) as u16) {
                    crate::startup::write_cstr(memory, a(1), a(2), &name)
                        .map_err(|e| e.to_string())?
                        .0
                } else {
                    self.thread.last_error = 6;
                    0
                }
            }
            ("kernel32", "CreateEventA") => {
                if a(0) != 0 {
                    return Err("event security attributes are not implemented".into());
                }
                let name = if a(3) == 0 {
                    None
                } else {
                    Some(string(memory, a(3))?)
                };
                match self.events.create(name, a(1) != 0, a(2) != 0) {
                    Some((handle, exists)) => {
                        self.thread.last_error = if exists { 183 } else { 0 };
                        handle
                    }
                    None => {
                        self.thread.last_error = 8;
                        0
                    }
                }
            }
            ("kernel32", "SetEvent" | "ResetEvent") => {
                if self.events.signal(a(0), name == "SetEvent").is_some() {
                    1
                } else {
                    self.thread.last_error = 6;
                    0
                }
            }
            ("kernel32", "CloseHandle") => {
                if self.files.close(a(0)) || self.events.close(a(0)) {
                    1
                } else {
                    self.thread.last_error = 6;
                    0
                }
            }
            ("kernel32", "CreateFileA") => {
                const GENERIC_WRITE: u32 = 0x4000_0000;
                const OPEN_EXISTING: u32 = 3;
                let path = string(memory, a(0))?;
                if a(1) & GENERIC_WRITE != 0 || a(4) != OPEN_EXISTING {
                    self.thread.last_error = 5;
                    u32::MAX
                } else if let Some(handle) = self.files.open(&path) {
                    handle
                } else {
                    self.thread.last_error = 2;
                    u32::MAX
                }
            }
            ("kernel32", "OpenFile") => {
                let path = string(memory, a(0))?;
                let handle = if a(1) == 0 {
                    None
                } else {
                    self.files.open(&path)
                };
                if a(1) != 0 {
                    let mut structure = [0u8; 136];
                    structure[0] = 136;
                    structure[1] = 1;
                    if handle.is_none() {
                        structure[2..4].copy_from_slice(&2u16.to_le_bytes());
                    }
                    let path_len = path.len().min(127);
                    structure[8..8 + path_len].copy_from_slice(&path[..path_len]);
                    memory
                        .write(a(1), &structure)
                        .map_err(|e| format!("OpenFile OFSTRUCT: {e:?}"))?;
                }
                handle.unwrap_or(u32::MAX)
            }
            ("kernel32", "ReadFile") => {
                let data = if let Some(bytes) = self.files.read(a(0), a(2) as usize) {
                    Some(bytes)
                } else {
                    self.stdio.read(a(0), a(2) as usize)
                };
                if let Some(bytes) = data {
                    memory
                        .write(a(1), &bytes)
                        .map_err(|e| format!("ReadFile buffer: {e:?}"))?;
                    if a(3) != 0 {
                        write(memory, a(3), bytes.len() as u32)?;
                    }
                    1
                } else {
                    self.thread.last_error = 6;
                    0
                }
            }
            ("kernel32", "WriteFile") => {
                let mut bytes = vec![0; a(2) as usize];
                memory
                    .read(a(1), &mut bytes)
                    .map_err(|e| format!("WriteFile buffer: {e:?}"))?;
                if self.stdio.write(a(0), &bytes) {
                    if a(3) != 0 {
                        write(memory, a(3), bytes.len() as u32)?;
                    }
                    1
                } else {
                    self.thread.last_error = 5;
                    0
                }
            }
            ("kernel32", "SetFilePointer") => {
                let high = if a(2) == 0 {
                    0i64
                } else {
                    read(memory, a(2))? as i32 as i64
                };
                let distance = (high << 32) | (a(1) as i32 as i64 & 0xffff_ffff);
                match self.files.seek(a(0), distance, a(3)) {
                    Some(position) => {
                        if a(2) != 0 {
                            write(memory, a(2), 0)?;
                        }
                        position
                    }
                    None => u32::MAX,
                }
            }
            ("kernel32", "WaitForSingleObject") => match self.events.poll(a(0)) {
                Some(true) => 0,
                Some(false) if a(1) == 0 => 258,
                Some(false) => return Err("blocking event waits require a guest scheduler".into()),
                None => {
                    self.thread.last_error = 6;
                    u32::MAX
                }
            },
            ("kernel32", "GlobalMemoryStatus") => {
                // Deterministic virtual machine capacity, independent of the
                // browser machine's physical memory. Account for host-managed
                // guest allocations; the entire linear address space is backed.
                let total = memory.byte_len().min(u32::MAX as u64) as u32;
                let used = self.arena.allocated_bytes() + self.virtual_memory.committed_bytes();
                let available = (total as u64).saturating_sub(used) as u32;
                let load = if total == 0 {
                    0
                } else {
                    ((total - available) as u64 * 100 / total as u64) as u32
                };
                let fields = [
                    32, load, total, available, total, available, total, available,
                ];
                let mut info = [0u8; 32];
                for (offset, value) in fields.into_iter().enumerate() {
                    info[offset * 4..offset * 4 + 4].copy_from_slice(&value.to_le_bytes());
                }
                memory
                    .write(a(0), &info)
                    .map_err(|e| format!("memory status: {e:?}"))?;
                0
            }
            ("kernel32", "GetLocalTime") => {
                // Stable local epoch for deterministic demos: 2000-01-01 00:00:00,
                // advanced by the VM's virtual clock.
                let seconds = self.clock_ms / 1000;
                let day_seconds = seconds % 86_400;
                let mut time = [0u8; 16];
                // 2000-01-01 was Saturday (SYSTEMTIME weekday 6).
                for (offset, value) in [2000u16, 1, 6, 1, (day_seconds / 3600) as u16,
                    ((day_seconds / 60) % 60) as u16, (day_seconds % 60) as u16,
                    (self.clock_ms % 1000) as u16].into_iter().enumerate() {
                    time[offset * 2..offset * 2 + 2].copy_from_slice(&value.to_le_bytes());
                }
                memory.write(a(0), &time).map_err(|e| format!("local time: {e:?}"))?;
                0
            }
            ("kernel32", "GetTimeZoneInformation") => {
                // TIME_ZONE_ID_UNKNOWN with a zero-bias UTC virtual clock.
                memory.write(a(0), &[0; 172]).map_err(|e| format!("time zone: {e:?}"))?;
                0
            }
            ("kernel32", "GetSystemInfo") => {
                // Virtual i486, one processor, 4 KiB pages, 64 KiB allocation
                // granularity. Report the Win32 guest rather than the host CPU.
                let mut info = [0u8; 36];
                for (offset, value) in [
                    (4, 4096u32),
                    (8, 0x10000),
                    (12, 0xfffeffff),
                    (16, 1),
                    (20, 1),
                    (24, 486),
                    (28, 65536),
                ] {
                    info[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                info[32..34].copy_from_slice(&4u16.to_le_bytes());
                memory
                    .write(a(0), &info)
                    .map_err(|e| format!("system information: {e:?}"))?;
                0
            }
            ("user32", "MessageBoxA") => {
                if a(0) != 0 {
                    self.windows
                        .get(a(0))
                        .map_err(|e| format!("dialog owner: {e:?}"))?;
                }
                if a(3) & 15 > 6 || a(3) & 0x4000 != 0 {
                    return Err("unsupported message box buttons".into());
                }
                return Ok(Effect::MessageBox {
                    text: if a(1) == 0 {
                        Vec::new()
                    } else {
                        string(memory, a(1))?
                    },
                    title: if a(2) == 0 {
                        b"Error".to_vec()
                    } else {
                        string(memory, a(2))?
                    },
                    flags: a(3),
                });
            }
            ("user32", "GetKeyState") => {
                let state = *self.keys.get(a(0) as usize).ok_or("invalid virtual key")?;
                ((if state & 0x80 != 0 { 0x8000 } else { 0 }) | (state & 1) as u16) as i16 as i32
                    as u32
            }
            ("user32", "GetCursorPos") => {
                let mut point = [0; 8];
                point[..4].copy_from_slice(&self.cursor.0.to_le_bytes());
                point[4..].copy_from_slice(&self.cursor.1.to_le_bytes());
                memory
                    .write(a(0), &point)
                    .map_err(|e| format!("cursor position: {e:?}"))?;
                1
            }
            ("kernel32", "SetUnhandledExceptionFilter") => {
                std::mem::replace(&mut self.exception_filter, a(0))
            }
            ("kernel32", "MultiByteToWideChar") => {
                match crate::locale::multi_byte_to_wide(args, memory) {
                    Ok(value) => value,
                    Err(code) => {
                        self.thread.last_error = code;
                        0
                    }
                }
            }
            ("kernel32", "LoadLibraryA" | "GetModuleHandleA") if a(0) != 0 => {
                let bytes = string(memory, a(0))?;
                let module_name =
                    std::str::from_utf8(&bytes).map_err(|_| "non-ASCII module name")?;
                let handle = if name == "LoadLibraryA" {
                    self.modules.load(module_name)
                } else {
                    self.modules.handle(module_name)
                };
                handle.unwrap_or_else(|| {
                    self.thread.last_error = 126;
                    0
                })
            }
            ("kernel32", "FreeLibrary") => {
                let success = self.modules.free(a(0));
                if !success {
                    self.thread.last_error = 6;
                }
                success as u32
            }
            ("kernel32", "GetProcAddress") => {
                let export = if a(1) <= 0xffff {
                    format!("#{}", a(1))
                } else {
                    String::from_utf8(string(memory, a(1))?).map_err(|_| "non-ASCII export name")?
                };
                self.modules.procedure(a(0), &export).unwrap_or_else(|| {
                    self.thread.last_error = 127;
                    0
                })
            }
            ("kernel32", "GetStartupInfoA") => {
                crate::startup::StartupInfo::default()
                    .write(memory, a(0))
                    .map_err(|e| e.to_string())?;
                0
            }
            ("kernel32", "GetCommandLineA") => self
                .process
                .command_line(memory)
                .ok_or("command line memory")?,
            ("kernel32", "GetEnvironmentStrings") => self
                .process
                .environment_strings(memory)
                .ok_or("environment memory")?,
            ("kernel32", "GetModuleFileNameA") if a(0) == 0 || a(0) == self.image_base => {
                let (written, truncated) = crate::startup::write_cstr(
                    memory,
                    a(1),
                    a(2),
                    crate::startup::MODULE_FILE_NAME,
                )
                .map_err(|e| e.to_string())?;
                if truncated {
                    a(2)
                } else {
                    written
                }
            }
            // The browser guest has no host filesystem for profile files. Win32
            // returns the caller's default when the requested INI file or key
            // is absent, which is the case for Hover's optional PROGRAM.INI.
            ("kernel32", "GetPrivateProfileIntA") => a(2),
            ("kernel32", "GetCurrentDirectoryA") => {
                let current = b"C:\\";
                if a(1) == 0 || (a(1) as u64).saturating_add(a(0) as u64) > memory.byte_len() {
                    self.thread.last_error = 998;
                    0
                } else if a(0) <= current.len() as u32 {
                    (current.len() + 1) as u32
                } else {
                    memory
                        .write(a(1), current)
                        .map_err(|e| format!("current directory: {e:?}"))?;
                    memory
                        .write(a(1) + current.len() as u32, &[0])
                        .map_err(|e| format!("current directory NUL: {e:?}"))?;
                    current.len() as u32
                }
            }
            // The isolated browser guest starts with an empty registry. A
            // missing software key is reported using Win32's normal status so
            // the program can fall back to its built-in settings.
            ("advapi32", "RegOpenKeyA" | "RegOpenKeyExA") => 2,
            ("advapi32", "RegCloseKey") => {
                if a(0) <= 0x8000_0000 {
                    6
                } else {
                    0
                }
            }
            ("winmm", "waveOutGetNumDevs" | "midiOutGetNumDevs") => 0,
            ("winmm", "joyGetDevCapsA" | "joyGetPos") => 6,
            ("winmm", "timeGetTime") => self.clock_ms as u32,
            // No multimedia timer can have ID zero. Applications commonly
            // cancel their zero-initialized timer during game reset.
            ("winmm", "timeKillEvent") if a(0) == 0 => 97, // TIMERR_NOCANDO
            ("kernel32", "GetTickCount") => self.clock_ms as u32,
            ("wlam.x87", "sin" | "cos" | "tan") => {
                let mut input = [0; 8];
                memory
                    .read(0x7000, &mut input)
                    .map_err(|e| format!("x87 input: {e:?}"))?;
                let value = f64::from_le_bytes(input);
                let result = match name {
                    "sin" => libm::sin(value),
                    "cos" => libm::cos(value),
                    _ => libm::tan(value),
                };
                memory
                    .write(0x7008, &result.to_le_bytes())
                    .map_err(|e| format!("x87 result: {e:?}"))?;
                0
            }
            ("wlam.x87", "atan2") => {
                let mut y = [0; 8];
                let mut x = [0; 8];
                memory
                    .read(0x7000, &mut y)
                    .map_err(|e| format!("FPATAN y: {e:?}"))?;
                memory
                    .read(0x7008, &mut x)
                    .map_err(|e| format!("FPATAN x: {e:?}"))?;
                let result = libm::atan2(f64::from_le_bytes(y), f64::from_le_bytes(x));
                memory
                    .write(0x7010, &result.to_le_bytes())
                    .map_err(|e| format!("FPATAN result: {e:?}"))?;
                0
            }
            ("user32", "SetRect") => {
                let values = [a(1), a(2), a(3), a(4)];
                for (index, value) in values.into_iter().enumerate() {
                    write(memory, a(0) + index as u32 * 4, value)?;
                }
                1
            }
            ("user32", "SetRectEmpty") => {
                memory
                    .write(a(0), &[0; 16])
                    .map_err(|e| format!("empty RECT: {e:?}"))?;
                1
            }
            ("user32", "CopyRect") => {
                if a(0) == 0 || a(1) == 0 { return Ok(Effect::Return(0)); }
                let rect = read_rect(memory, a(1))?;
                write_rect(memory, a(0), rect)?;
                1
            }
            ("user32", "EqualRect") => {
                if a(0) == 0 || a(1) == 0 { return Ok(Effect::Return(0)); }
                (read_rect(memory, a(0))? == read_rect(memory, a(1))?) as u32
            }
            ("user32", "OffsetRect") => {
                if a(0) == 0 { return Ok(Effect::Return(0)); }
                let mut rect = read_rect(memory, a(0))?;
                rect[0] = rect[0].wrapping_add(a(1) as i32);
                rect[2] = rect[2].wrapping_add(a(1) as i32);
                rect[1] = rect[1].wrapping_add(a(2) as i32);
                rect[3] = rect[3].wrapping_add(a(2) as i32);
                write_rect(memory, a(0), rect)?;
                1
            }
            ("user32", "InflateRect") => {
                if a(0) == 0 { return Ok(Effect::Return(0)); }
                let mut rect = read_rect(memory, a(0))?;
                rect[0] = rect[0].wrapping_sub(a(1) as i32);
                rect[1] = rect[1].wrapping_sub(a(2) as i32);
                rect[2] = rect[2].wrapping_add(a(1) as i32);
                rect[3] = rect[3].wrapping_add(a(2) as i32);
                write_rect(memory, a(0), rect)?;
                1
            }
            ("user32", "IntersectRect" | "UnionRect") => {
                if a(0) == 0 || a(1) == 0 || a(2) == 0 { return Ok(Effect::Return(0)); }
                let lhs = read_rect(memory, a(1))?;
                let rhs = read_rect(memory, a(2))?;
                let out = if name == "IntersectRect" {
                    [lhs[0].max(rhs[0]), lhs[1].max(rhs[1]), lhs[2].min(rhs[2]), lhs[3].min(rhs[3])]
                } else if lhs[0] >= lhs[2] || lhs[1] >= lhs[3] {
                    rhs
                } else if rhs[0] >= rhs[2] || rhs[1] >= rhs[3] {
                    lhs
                } else {
                    [lhs[0].min(rhs[0]), lhs[1].min(rhs[1]), lhs[2].max(rhs[2]), lhs[3].max(rhs[3])]
                };
                write_rect(memory, a(0), out)?;
                if name == "IntersectRect" { (out[0] < out[2] && out[1] < out[3]) as u32 } else { 1 }
            }
            ("user32", "PtInRect") => {
                if a(0) == 0 { return Ok(Effect::Return(0)); }
                let rect = read_rect(memory, a(0))?;
                let x = a(1) as i32;
                let y = a(2) as i32;
                (x >= rect[0] && x < rect[2] && y >= rect[1] && y < rect[3]) as u32
            }
            ("kernel32", "GetACP") => crate::locale::ANSI_CODE_PAGE,
            ("kernel32", "GetOEMCP") => crate::locale::OEM_CODE_PAGE,
            ("kernel32", "GetCPInfo") => crate::locale::CpInfo::for_code_page(a(0))
                .is_some_and(|info| info.write(memory, a(1)))
                as u32,
            ("kernel32", "GetStdHandle") => self.stdio.get_std_handle(a(0)),
            ("kernel32", "GetFileType") => {
                if self.files.contains_handle(a(0)) {
                    1
                } else {
                    self.stdio.file_type(a(0))
                }
            }
            ("kernel32", "SetStdHandle") => self.stdio.set_std_handle(a(0), a(1)) as u32,
            ("kernel32", "SetHandleCount") => a(0),
            (
                "kernel32",
                "InitializeCriticalSection"
                | "EnterCriticalSection"
                | "TryEnterCriticalSection"
                | "LeaveCriticalSection"
                | "DeleteCriticalSection",
            ) => self.critical_sections.call(name, a(0), memory)?,
            ("kernel32", "VirtualAlloc" | "VirtualFree") => {
                let result = if name == "VirtualAlloc" {
                    self.virtual_memory.allocate(a(0), a(1), a(2), a(3), memory)
                } else {
                    self.virtual_memory.free(a(0), a(1), a(2))
                };
                match result {
                    Ok(value) => value,
                    Err(error) => {
                        self.thread.last_error = error.code();
                        0
                    }
                }
            }
            ("kernel32", "GetVersion") => self.version.version_dword(),
            ("kernel32", "MulDiv") => {
                let numerator = (a(0) as i32 as i64) * (a(1) as i32 as i64);
                let denominator = a(2) as i32 as i64;
                if denominator == 0 {
                    u32::MAX
                } else {
                    let mut quotient = numerator / denominator;
                    let remainder = numerator % denominator;
                    if remainder.abs() * 2 >= denominator.abs() {
                        quotient += if (numerator < 0) ^ (denominator < 0) { -1 } else { 1 };
                    }
                    if quotient < i32::MIN as i64 || quotient > i32::MAX as i64 {
                        u32::MAX
                    } else {
                        quotient as i32 as u32
                    }
                }
            }
            ("kernel32", "GetModuleHandleA") if a(0) == 0 => self.image_base,
            (
                "kernel32",
                "GetLastError" | "SetLastError" | "GetCurrentThread" | "GetCurrentProcess"
                | "GetCurrentThreadId" | "SetThreadPriority" | "TlsAlloc" | "TlsFree" | "TlsGetValue" | "TlsSetValue",
            ) => self.thread.call(name, args)?,
            ("kernel32", "ExitProcess") => return Ok(Effect::Exit(a(0))),
            ("user32", "RegisterClassA" | "RegisterClassExA") => {
                let address = if name == "RegisterClassExA" {
                    if read(memory, a(0))? != 48 {
                        return Err("unsupported WNDCLASSEXA size".into());
                    }
                    a(0).checked_add(4).ok_or("class address overflow")?
                } else {
                    a(0)
                };
                let mut fields = [0; 10];
                for (i, field) in fields.iter_mut().enumerate() {
                    *field = read(memory, address + i as u32 * 4)?;
                }
                let class = string(memory, fields[9])?;
                if self.classes.contains_key(&class) {
                    self.thread.last_error = 1410;
                    0
                } else {
                    self.windows
                        .register(class.clone(), fields[1])
                        .map_err(|e| format!("register class: {e:?}"))?;
                    let atom = u16::try_from(self.classes.len() + 1)
                        .map_err(|_| "class atoms exhausted")?;
                    self.classes.insert(class, (atom, fields));
                    atom as u32
                }
            }
            ("user32", "GetClassInfoA") => {
                let class = if a(1) <= 0xffff {
                    self.classes
                        .iter()
                        .find(|(_, (atom, _))| *atom as u32 == a(1))
                        .map(|(name, _)| name.clone())
                        .unwrap_or_default()
                } else {
                    string(memory, a(1))?
                };
                if let Some((_, fields)) = self
                    .classes
                    .get(&class)
                    .filter(|(_, fields)| a(0) == 0 || a(0) == fields[4])
                {
                    let bytes = fields
                        .iter()
                        .flat_map(|field| field.to_le_bytes())
                        .collect::<Vec<_>>();
                    memory
                        .write(a(2), &bytes)
                        .map_err(|e| format!("class information: {e:?}"))?;
                    1
                } else {
                    self.thread.last_error = 1411;
                    0
                }
            }
            ("user32", "DialogBoxParamA") => {
                let key = (a(0), a(1), a(2), a(3), a(4));
                if let Some(state) = self.modal_dialogs.get(&key).copied() {
                    if state.result.is_some() {
                        return self.finish_modal(key);
                    }
                    let Some(message) = self.windows.peek(true) else {
                        return Ok(Effect::Wait);
                    };
                    let mut msg = message.message;
                    let mut wp = message.wparam;
                    let lp = message.lparam;
                    if msg == 0x100 && matches!(wp, 13 | 27) {
                        msg = 0x111;
                        wp = if wp == 13 { 1 } else { 2 };
                    } else if msg == 0x201 {
                        let px = lp as i16 as i32;
                        let py = (lp >> 16) as i16 as i32;
                        if let Some(id) = self.window_data.iter().find_map(|(child, data)| {
                            if data.parent != state.window || data.class != b"BUTTON" {
                                return None;
                            }
                            let w = self.windows.get(*child).ok()?;
                            (px >= data.x
                                && py >= data.y
                                && px < data.x + w.width as i32
                                && py < data.y + w.height as i32)
                                .then_some(data.id)
                        }) {
                            msg = 0x111;
                            wp = id;
                        }
                    }
                    return self.callback(
                        state.procedure,
                        vec![state.window, msg, wp, lp],
                        None,
                        CallbackFinish::DialogBoxMessage { key },
                    );
                }
                self.pending_modal = Some(key);
                return match self.call("USER32.dll", "CreateDialogParamA", args, memory) {
                    Ok(effect) => Ok(effect),
                    Err(error) => {
                        self.pending_modal = None;
                        Err(error)
                    }
                };
            }
            ("user32", "EndDialog") => {
                if let Some(key) = self.modal_windows.get(&a(0)).copied() {
                    if let Some(dialog) = self.modal_dialogs.get_mut(&key) {
                        dialog.result = Some(a(1));
                        1
                    } else {
                        0
                    }
                } else {
                    0
                }
            }
            ("user32", "CreateDialogParamA") => {
                if a(0) != 0 && a(0) != self.image_base {
                    return Err("dialog template module is not the guest image".into());
                }
                if a(2) != 0 {
                    self.windows
                        .get(a(2))
                        .map_err(|e| format!("dialog owner: {e:?}"))?;
                }
                let id = if a(1) <= 0xffff {
                    a(1)
                } else {
                    return Err("named dialog templates are not implemented".into());
                };
                let resource = self.resources.named(5, id).ok_or_else(|| {
                    self.thread.last_error = 1814;
                    "dialog template resource not found".to_string()
                })?;
                let mut bytes = vec![0; resource.size as usize];
                memory
                    .read(resource.address, &mut bytes)
                    .map_err(|e| format!("dialog template read: {e:?}"))?;
                let template = crate::dialog::DialogTemplate::parse(&bytes)
                    .map_err(|e| format!("dialog template: {e:?}"))?;
                if template.extended {
                    return Err("extended dialog templates are not yet supported".into());
                }
                let width = ((template.cx as f32 * 7.0 / 4.0).round() as i32).max(1) as u32;
                let height = ((template.cy as f32 * 14.0 / 8.0).round() as i32).max(1) as u32;
                let x = if template.x == -1 {
                    (640 - width as i32) / 2
                } else {
                    (template.x as i32 * 7 / 4)
                };
                let y = if template.y == -1 {
                    (480 - height as i32) / 2
                } else {
                    (template.y as i32 * 14 / 8)
                };
                let window = self
                    .windows
                    .create_builtin(0xffff_ff01, a(2), 0, x, y, width, height, template.style)
                    .map_err(|e| format!("dialog window: {e:?}"))?;
                self.gdi
                    .create_surface(window, width, height)
                    .map_err(|e| format!("dialog surface: {e:?}"))?;
                self.gdi
                    .fill_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        0xc0c0c0,
                    )
                    .map_err(|e| format!("dialog background: {e:?}"))?;
                self.window_data.insert(
                    window,
                    WindowData {
                        class: b"#32770".to_vec(),
                        instance: a(0),
                        parent: a(2),
                        id: 0,
                        style: template.style,
                        ex_style: template.ex_style,
                        user_data: 0,
                        extra: vec![0; 32],
                        x,
                        y,
                        text: Vec::new(),
                    },
                );
                for control in template.controls {
                    let class = match control.class {
                        crate::dialog::OrdinalOrString::Ordinal(128) => b"BUTTON".to_vec(),
                        crate::dialog::OrdinalOrString::Ordinal(129) => b"EDIT".to_vec(),
                        crate::dialog::OrdinalOrString::Ordinal(130) => b"STATIC".to_vec(),
                        crate::dialog::OrdinalOrString::Ordinal(131) => b"LISTBOX".to_vec(),
                        crate::dialog::OrdinalOrString::String(v) => String::from_utf16(&v)
                            .map_err(|_| "invalid control class")?
                            .into_bytes(),
                        _ => return Err("unsupported dialog control class".into()),
                    };
                    if !matches!(
                        class.as_slice(),
                        b"BUTTON" | b"EDIT" | b"STATIC" | b"LISTBOX"
                    ) {
                        return Err(format!(
                            "unsupported dialog control class {}",
                            String::from_utf8_lossy(&class)
                        ));
                    }
                    let cx = (control.cx as i32 * 7 / 4).max(1) as u32;
                    let cy = (control.cy as i32 * 14 / 8).max(if control.style & 15 == 1 {
                        20
                    } else {
                        1
                    }) as u32;
                    let cx = cx.min(width);
                    let cy = cy.min(height);
                    let px = control.x as i32 * 7 / 4;
                    let py = control.y as i32 * 14 / 8;
                    let child = self
                        .windows
                        .create_builtin(
                            0xffff_ff02,
                            window,
                            control.id,
                            px,
                            py,
                            cx,
                            cy,
                            control.style,
                        )
                        .map_err(|e| format!("static control: {e:?}"))?;
                    self.gdi
                        .create_surface(child, cx, cy)
                        .map_err(|e| format!("static surface: {e:?}"))?;
                    self.gdi
                        .fill_surface(
                            child,
                            Rect {
                                left: 0,
                                top: 0,
                                right: cx as i32,
                                bottom: cy as i32,
                            },
                            0xc0c0c0,
                        )
                        .map_err(|e| format!("static background: {e:?}"))?;
                    if control.style & 15 == 7 {
                        self.gdi
                            .frame_surface(
                                child,
                                Rect {
                                    left: 0,
                                    top: 0,
                                    right: cx as i32,
                                    bottom: cy as i32,
                                },
                                0,
                            )
                            .map_err(|e| format!("static frame: {e:?}"))?;
                    }
                    let text = match control.title {
                        crate::dialog::OrdinalOrString::String(v) => String::from_utf16(&v)
                            .map_err(|_| "invalid static title")?
                            .into_bytes(),
                        _ => Vec::new(),
                    };
                    self.window_data.insert(
                        child,
                        WindowData {
                            class,
                            instance: a(0),
                            parent: window,
                            id: control.id,
                            style: control.style,
                            ex_style: control.ex_style,
                            user_data: 0,
                            extra: Vec::new(),
                            x: px,
                            y: py,
                            text,
                        },
                    );
                    if self
                        .window_data
                        .get(&child)
                        .is_some_and(|data| data.class == b"LISTBOX")
                    {
                        self.list_boxes.insert(
                            child,
                            ListBoxState {
                                items: Vec::new(),
                                selection: -1,
                            },
                        );
                    }
                }
                let procedure = a(3);
                let init_parameter = a(4);
                if let Some(key) = self.pending_modal.take() {
                    self.modal_dialogs.insert(
                        key,
                        ModalDialog {
                            window,
                            procedure,
                            result: None,
                        },
                    );
                    self.modal_windows.insert(window, key);
                }
                let chain = self
                    .hook_order
                    .iter()
                    .rev()
                    .copied()
                    .filter(|handle| {
                        self.message_hooks
                            .get(*handle)
                            .is_some_and(|(kind, _, _)| *kind == 5)
                    })
                    .collect::<Vec<_>>();
                if !chain.is_empty() {
                    let storage = self.allocate(memory, 80)?;
                    let class_ptr = storage + 56;
                    let title_ptr = storage + 64;
                    let cbt = storage + 68;
                    memory
                        .write(class_ptr, b"#32770\0")
                        .map_err(|e| format!("dialog class string: {e:?}"))?;
                    let title = match template.title {
                        crate::dialog::OrdinalOrString::String(v) => String::from_utf16(&v)
                            .map_err(|_| "invalid dialog title")?
                            .into_bytes(),
                        _ => Vec::new(),
                    };
                    memory
                        .write(title_ptr, &[title.as_slice(), &[0]].concat())
                        .map_err(|e| format!("dialog title: {e:?}"))?;
                    let fields = [
                        init_parameter,
                        a(0),
                        0,
                        a(2),
                        height,
                        width,
                        y as u32,
                        x as u32,
                        template.style,
                        title_ptr,
                        class_ptr,
                        template.ex_style,
                    ];
                    for (i, v) in fields.iter().enumerate() {
                        write(memory, storage + i as u32 * 4, *v)?;
                    }
                    write(memory, cbt, storage)?;
                    write(memory, cbt + 4, 0)?;
                    return self.hook(
                        chain,
                        0,
                        3,
                        window,
                        cbt,
                        CallbackFinish::DialogHook {
                            window,
                            procedure,
                            init_parameter,
                            storage,
                        },
                    );
                }
                let children = self
                    .window_data
                    .iter()
                    .filter_map(|(handle, data)| (data.parent == window).then_some(*handle))
                    .collect();
                return self.continue_dialog_children(
                    window,
                    procedure,
                    init_parameter,
                    children,
                    0,
                    memory,
                );
            }
            ("user32", "GetDlgItem") => self
                .window_data
                .iter()
                .find_map(|(hwnd, data)| (data.parent == a(0) && data.id == a(1)).then_some(*hwnd))
                .unwrap_or(0),
            ("user32", "GetTopWindow") => self.windows.top_child(a(0)),
            ("user32", "GetWindow") => self.windows.related(a(0), a(1)),
            ("user32", "MoveWindow") => {
                self.windows
                    .move_window(a(0), a(1) as i32, a(2) as i32, a(3), a(4))
                    .map_err(|e| format!("MoveWindow: {e:?}"))?;
                self.gdi
                    .resize_surface(a(0), a(3), a(4))
                    .map_err(|e| format!("MoveWindow surface: {e:?}"))?;
                if a(5) != 0 {
                    self.windows
                        .invalidate(a(0))
                        .map_err(|e| format!("MoveWindow invalidate: {e:?}"))?;
                }
                1
            }
            ("user32", "SetWindowPos") => {
                const SWP_NOSIZE: u32 = 0x0001;
                const SWP_NOMOVE: u32 = 0x0002;
                let current = self.windows.get(a(0)).map_err(|e| format!("SetWindowPos: {e:?}"))?;
                let (x, y) = if a(6) & SWP_NOMOVE != 0 { (current.x, current.y) } else { (a(2) as i32, a(3) as i32) };
                let (width, height) = if a(6) & SWP_NOSIZE != 0 { (current.width, current.height) } else { (a(4).max(1), a(5).max(1)) };
                let resized = (width, height) != (current.width, current.height);
                self.windows.move_window(a(0), x, y, width, height).map_err(|e| format!("SetWindowPos: {e:?}"))?;
                if resized {
                    self.gdi.resize_surface(a(0), width, height).map_err(|e| format!("SetWindowPos surface: {e:?}"))?;
                }
                1
            }
            ("user32", "BeginDeferWindowPos") => 0x7fff_0001,
            ("user32", "DeferWindowPos") => {
                if a(0) != 0x7fff_0001 {
                    return Ok(Effect::Return(0));
                }
                let args = [a(1), a(2), a(3), a(4), a(5), a(6), a(7)];
                match self.call("USER32.dll", "SetWindowPos", &args, memory)? {
                    Effect::Return(value) => value,
                    effect => return Ok(effect),
                }
            }
            ("user32", "EndDeferWindowPos") => (a(0) == 0x7fff_0001) as u32,
            ("user32", "ClientToScreen" | "ScreenToClient") => {
                let (x, y) = self
                    .windows
                    .screen_origin(a(0))
                    .ok_or("invalid window coordinate origin")?;
                let px = read(memory, a(1))? as i32;
                let py = read(memory, a(1) + 4)? as i32;
                let (nx, ny) = if name == "ClientToScreen" {
                    (px.saturating_add(x), py.saturating_add(y))
                } else {
                    (px.saturating_sub(x), py.saturating_sub(y))
                };
                write(memory, a(1), nx as u32)?;
                write(memory, a(1) + 4, ny as u32)?;
                1
            }
            ("user32", "SetTimer") => {
                if a(0) != 0 {
                    self.windows
                        .get(a(0))
                        .map_err(|e| format!("SetTimer window: {e:?}"))?;
                }
                let id = if a(1) == 0 {
                    let id = self.next_timer;
                    self.next_timer = self.next_timer.wrapping_add(1).max(1);
                    id
                } else {
                    a(1)
                };
                let interval = a(2).max(10);
                self.timers.insert(
                    (a(0), id),
                    (self.clock_ms + interval as u64, interval, a(3)),
                );
                id
            }
            ("user32", "KillTimer") => self.timers.remove(&(a(0), a(1))).is_some() as u32,
            ("user32", "SetDlgItemTextA") => {
                let child = self
                    .window_data
                    .iter()
                    .find_map(|(hwnd, data)| {
                        (data.parent == a(0) && data.id == a(1)).then_some(*hwnd)
                    })
                    .ok_or("SetDlgItemTextA control not found")?;
                let text = if a(2) == 0 {
                    Vec::new()
                } else {
                    string(memory, a(2))?
                };
                self.window_data.get_mut(&child).unwrap().text = text.clone();
                let data = self.window_data.get(&child).unwrap();
                self.gdi
                    .fill_surface(
                        child,
                        Rect {
                            left: 0,
                            top: 0,
                            right: self.windows.get(child).unwrap().width as i32,
                            bottom: self.windows.get(child).unwrap().height as i32,
                        },
                        0xc0c0c0,
                    )
                    .map_err(|e| format!("static text background: {e:?}"))?;
                let width = self.windows.get(child).unwrap().width;
                self.gdi
                    .draw_default_text(child, 0, 2, width, &text, 0)
                    .map_err(|e| format!("static text: {e:?}"))?;
                self.gdi
                    .composite(data.parent, child, data.x, data.y)
                    .map_err(|e| format!("static composition: {e:?}"))?;
                1
            }
            ("user32", "SetWindowTextA") => {
                let Some(data) = self.window_data.get(&a(0)) else {
                    self.thread.last_error = 1400;
                    return Ok(Effect::Return(0));
                };
                let parent = data.parent;
                let x = data.x;
                let y = data.y;
                let text = if a(1) == 0 { Vec::new() } else { string(memory, a(1))? };
                self.window_data.get_mut(&a(0)).unwrap().text = text.clone();
                if parent != 0 {
                    let width = self.windows.get(a(0)).map_err(|e| format!("SetWindowTextA: {e:?}"))?.width;
                    let height = self.windows.get(a(0)).map_err(|e| format!("SetWindowTextA: {e:?}"))?.height;
                    self.gdi.fill_surface(a(0), Rect { left: 0, top: 0, right: width as i32, bottom: height as i32 }, 0xc0c0c0)
                        .map_err(|e| format!("SetWindowTextA fill: {e:?}"))?;
                    self.gdi.draw_default_text(a(0), 0, 2, width, &text, 0)
                        .map_err(|e| format!("SetWindowTextA text: {e:?}"))?;
                    self.gdi.composite(parent, a(0), x, y)
                        .map_err(|e| format!("SetWindowTextA composite: {e:?}"))?;
                }
                1
            }
            ("user32", "CreateWindowExA") => {
                const WS_OVERLAPPEDWINDOW: u32 = 0x00cf_0000;
                const WS_CLIPCHILDREN: u32 = 0x0200_0000;
                const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
                const WS_VISIBLE: u32 = 0x1000_0000;
                const WS_POPUP: u32 = 0x8000_0000;
                const WS_BORDER: u32 = 0x0080_0000;
                const WS_CHILD: u32 = 0x4000_0000;
                let supported_style = WS_OVERLAPPEDWINDOW
                    | WS_CLIPCHILDREN
                    | WS_CLIPSIBLINGS
                    | WS_VISIBLE
                    | WS_POPUP
                    | WS_BORDER
                    | WS_CHILD
                    | 0x0000_ffff;
                const SUPPORTED_EX_STYLE: u32 = 0x0000_0200 | 0x0000_0100 | 0x0000_0001;
                if a(0) & !SUPPORTED_EX_STYLE != 0 || a(3) & !supported_style != 0 {
                    return Err(format!("CreateWindowExA unsupported ex_style={:#x} style={:#x} unknown_style={:#x}", a(0), a(3), a(3) & !supported_style));
                }
                let class = if a(1) <= 0xffff {
                    self.classes
                        .iter()
                        .find(|(_, (atom, _))| *atom as u32 == a(1))
                        .map(|(name, _)| name.clone())
                        .ok_or("invalid class atom")?
                } else {
                    string(memory, a(1))?
                };
                let parameter = self.allocate(memory, 48)?;
                for (index, value) in [
                    a(11),
                    a(10),
                    a(9),
                    a(8),
                    a(7),
                    a(6),
                    a(5),
                    a(4),
                    a(3),
                    a(2),
                    a(1),
                    a(0),
                ]
                .iter()
                .enumerate()
                {
                    write(memory, parameter + index as u32 * 4, *value)?;
                }
                let (mut left, mut top, mut right, mut bottom) = classic_window_margins(a(3), a(9) != 0);
                if a(0) & (0x0000_0200 | 0x0000_0100) != 0 { left+=2;top+=2;right+=2;bottom+=2; }
                else if a(0) & 1 != 0 { left+=3;top+=3;right+=3;bottom+=3; }
                // Windows permits zero-sized windows during creation; callers
                // commonly position or resize them after the create callback.
                // Keep a minimal backing surface until that later resize.
                let client_width = a(6).saturating_sub((left + right) as u32).max(1);
                let client_height = a(7).saturating_sub((top + bottom) as u32).max(1);
                let action = self
                    .windows
                    .create(&class, client_width, client_height, parameter)
                    .map_err(|e| format!("create window: {e:?}"))?;
                let window = match &action {
                    Action::Invoke { args, .. } => args[0],
                    _ => return Err("window creation did not yield a window".into()),
                };
                self.windows
                    .configure(window, a(8), a(9), a(4) as i32, a(5) as i32, a(3))
                    .map_err(|e| format!("configure window: {e:?}"))?;
                let extra = self.classes.get(&class).map_or(0, |(_, fields)| fields[3]);
                if extra > 4096 {
                    return Err("window extra storage exceeds supported size".into());
                }
                self.window_data.insert(
                    window,
                    WindowData {
                        class,
                        instance: a(10),
                        parent: a(8),
                        id: a(9),
                        style: a(3),
                        ex_style: a(0),
                        user_data: 0,
                        extra: vec![0; extra as usize],
                        x: a(4) as i32,
                        y: a(5) as i32,
                        text: Vec::new(),
                    },
                );
                if a(9)!=0 { self.menus.insert(window,a(9)); }
                let chain = self
                    .hook_order
                    .iter()
                    .rev()
                    .copied()
                    .filter(|handle| {
                        self.message_hooks
                            .get(*handle)
                            .is_some_and(|(kind, _, _)| *kind == 5)
                    })
                    .collect::<Vec<_>>();
                if !chain.is_empty() {
                    let cbt = self.allocate(memory, 8)?;
                    write(memory, cbt, parameter)?;
                    write(memory, cbt + 4, 0)?;
                    return self.hook(
                        chain,
                        0,
                        3,
                        window,
                        cbt,
                        CallbackFinish::Create {
                            action,
                            parameter: cbt,
                            create_parameter: parameter,
                            window,
                        },
                    );
                }
                return self.action(action, Finish::Create(parameter));
            }
            ("user32", "GetClientRect") => {
                let w = self
                    .windows
                    .get(a(0))
                    .map_err(|e| format!("GetClientRect: {e:?}"))?;
                let mut rect = [0u8; 16];
                rect[8..12].copy_from_slice(&w.width.to_le_bytes());
                rect[12..16].copy_from_slice(&w.height.to_le_bytes());
                memory
                    .write(a(1), &rect)
                    .map_err(|e| format!("RECT write: {e:?}"))?;
                1
            }
            ("user32", "GetWindowRect") => {
                let Ok(window) = self.windows.get(a(0)) else {
                    self.thread.last_error = 1400;
                    return Ok(Effect::Return(0));
                };
                let width = window.width;
                let height = window.height;
                let style = self.window_data.get(&a(0)).map_or(window.style, |data| data.style);
                let has_menu = self.menus.contains_key(&a(0));
                let (x, y) = self.windows.screen_origin(a(0)).ok_or("invalid window origin")?;
                let (left, top, right, bottom) = classic_window_margins(style, has_menu);
                let rect = [
                    x.saturating_sub(left),
                    y.saturating_sub(top),
                    x.saturating_add(width as i32).saturating_add(right),
                    y.saturating_add(height as i32).saturating_add(bottom),
                ];
                let mut raw = [0u8; 16];
                for (i, edge) in rect.iter().enumerate() {
                    raw[i * 4..i * 4 + 4].copy_from_slice(&edge.to_le_bytes());
                }
                memory.write(a(1), &raw).map_err(|e| format!("GetWindowRect RECT write: {e:?}"))?;
                1
            }
            ("user32", "AdjustWindowRect" | "AdjustWindowRectEx") => {
                // RECT is four signed 32-bit LONG values. Invalid geometry and
                // arithmetic overflow return FALSE without changing the input.
                if a(0) == 0 {
                    return Ok(Effect::Return(0));
                }
                let mut raw = [0u8; 16];
                memory
                    .read(a(0), &mut raw)
                    .map_err(|e| format!("AdjustWindowRect RECT read: {e:?}"))?;
                let mut edges = [0i32; 4];
                for (i, edge) in edges.iter_mut().enumerate() {
                    *edge = i32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
                }
                let [left, top, right, bottom] = edges;
                if left > right || top > bottom {
                    return Ok(Effect::Return(0));
                }

                const WS_BORDER: u32 = 0x0080_0000;
                const WS_DLGFRAME: u32 = 0x0040_0000;
                const WS_CAPTION: u32 = WS_BORDER | WS_DLGFRAME;
                const WS_THICKFRAME: u32 = 0x0004_0000;
                let style = a(1);
                // Use the same classic virtual desktop metrics exposed by
                // GetSystemMetrics: frame=4, dialog frame=3, border=1,
                // caption/menu=19 pixels. WS_OVERLAPPEDWINDOW has THICKFRAME.
                let mut frame = if style & WS_THICKFRAME != 0 {
                    4i32
                } else if style & WS_DLGFRAME != 0 {
                    3
                } else if style & WS_BORDER != 0 {
                    1
                } else {
                    0
                };
                if name == "AdjustWindowRectEx" {
                    let exstyle = a(3);
                    if exstyle & (0x0000_0200 | 0x0000_0100) != 0 {
                        frame += 2;
                    } else if exstyle & 0x0000_0001 != 0 {
                        frame += 3;
                    }
                }
                let caption = if style & WS_CAPTION == WS_CAPTION {
                    19i32
                } else {
                    0
                };
                let menu = if a(2) != 0 { 19i32 } else { 0 };
                let Some(new_left) = left.checked_sub(frame) else {
                    return Ok(Effect::Return(0));
                };
                let Some(new_top) = top.checked_sub(frame + caption + menu) else {
                    return Ok(Effect::Return(0));
                };
                let Some(new_right) = right.checked_add(frame) else {
                    return Ok(Effect::Return(0));
                };
                let Some(new_bottom) = bottom.checked_add(frame) else {
                    return Ok(Effect::Return(0));
                };
                let adjusted = [new_left, new_top, new_right, new_bottom];
                let mut output = [0u8; 16];
                for (i, edge) in adjusted.iter().enumerate() {
                    output[i * 4..i * 4 + 4].copy_from_slice(&edge.to_le_bytes());
                }
                memory
                    .write(a(0), &output)
                    .map_err(|e| format!("AdjustWindowRect RECT write: {e:?}"))?;
                1
            }
            ("user32", "PostMessageA") => {
                self.post(Message {
                    window: a(0),
                    message: a(1),
                    wparam: a(2),
                    lparam: a(3),
                })?;
                1
            }
            ("user32", "ShowWindow") => {
                match self.windows.show(a(0), a(1) != 0) {
                    Ok(was_visible) => was_visible as u32,
                    Err(crate::window::WindowError::InvalidHandle) => {
                        // ShowWindow reports prior visibility through its
                        // return value and does not define last-error state.
                        0
                    }
                    Err(e) => return Err(format!("show: {e:?}")),
                }
            }
            ("user32", "BringWindowToTop") => self.windows.bring_to_top(a(0)) as u32,
            ("user32", "GetLastActivePopup") => {
                if !self.window_data.contains_key(&a(0)) {
                    return Err("GetLastActivePopup: invalid window".into());
                }
                // Without tracked popup activation, Win32's no-active-popup
                // result is the window itself.
                a(0)
            }
            ("user32", "IsWindowVisible") => {
                self.windows.is_visible(a(0)) as u32
            }
            ("user32", "IsWindowEnabled") => {
                self.windows.get(a(0)).is_ok_and(|window| {
                    let style = self.window_data.get(&a(0)).map_or(window.style, |data| data.style);
                    style & 0x0800_0000 == 0 // WS_DISABLED
                }) as u32
            }
            ("user32", "DefWindowProcA") => {
                if a(1) == crate::window::WM_PAINT {
                    self.windows.validate_paint(a(0));
                }
                Windows::default_result(a(1))
            }
            ("user32", "PostQuitMessage") => {
                self.windows.post_quit(a(0));
                0
            }
            ("user32", "DestroyWindow") => {
                let action = self
                    .windows
                    .destroy(a(0))
                    .map_err(|e| format!("destroy: {e:?}"))?;
                return self.action(action, Finish::Destroy(a(0)));
            }
            ("user32", "InvalidateRect") => {
                if a(0) == 0 {
                    return Ok(Effect::Return(1));
                }
                if a(1) != 0 {
                    return Err("partial invalidation rectangles not implemented".into());
                }
                self.windows
                    .invalidate(a(0))
                    .map_err(|e| format!("invalidate: {e:?}"))?;
                1
            }
            ("user32", "RedrawWindow") => {
                if a(1) != 0 || a(2) != 0 || a(3) & !0x105 != 0
                    || (a(3) & 4 != 0 && a(3) & 0x101 != 0x101) {
                    return Err("RedrawWindow: only whole-window invalidation and immediate update supported".into());
                }
                if a(3) & 1 != 0 {
                    self.windows.invalidate(a(0)).map_err(|e| format!("redraw: {e:?}"))?;
                }
                if a(3) & 0x100 != 0 {
                    if a(3) & 5 == 5 {
                        let Effect::Return(dc) = self.call("user32", "GetDC", &[a(0)], memory)? else { unreachable!() };
                        let action = self.windows.send(Message { window: a(0), message: 20, wparam: dc, lparam: 0 })
                            .map_err(|e| format!("redraw erase: {e:?}"))?;
                        return self.action(action, Finish::Redraw { window: a(0), dc });
                    }
                    return self.call("user32", "UpdateWindow", &[a(0)], memory);
                }
                1
            }
            ("user32", "UpdateWindow") => {
                let w = self
                    .windows
                    .get(a(0))
                    .map_err(|e| format!("update: {e:?}"))?;
                if !w.invalid {
                    return Ok(Effect::Return(1));
                }
                let action = self
                    .windows
                    .send(Message {
                        window: a(0),
                        message: 15,
                        wparam: 0,
                        lparam: 0,
                    })
                    .map_err(|e| format!("paint: {e:?}"))?;
                return self.action(action, Finish::Update);
            }
            ("user32", "SendMessageA" | "DispatchMessageA") => {
                let message = if name == "SendMessageA" {
                    Message {
                        window: a(0),
                        message: a(1),
                        wparam: a(2),
                        lparam: a(3),
                    }
                } else {
                    Message {
                        window: read(memory, a(0))?,
                        message: read(memory, a(0) + 4)?,
                        wparam: read(memory, a(0) + 8)?,
                        lparam: read(memory, a(0) + 12)?,
                    }
                };
                if self.windows.get(message.window).is_err() {
                    return Ok(Effect::Return(0));
                }
                if let Ok(window) = self.windows.get(message.window) {
                    if window.procedure == 0xffff_ff02 {
                        return Ok(Effect::Return(self.static_window_proc(
                            message.window,
                            message.message,
                            message.wparam,
                            message.lparam,
                            memory,
                        )?));
                    }
                    if window.procedure == 0xffff_ff01 {
                        return Ok(Effect::Return(Windows::default_result(message.message)));
                    }
                }
                let action = self
                    .windows
                    .send(message)
                    .map_err(|e| format!("dispatch: {e:?}"))?;
                return self.action(action, Finish::Plain);
            }
            ("user32", "TranslateMessage") => {
                let mut raw = [0u8; 28];
                memory
                    .read(a(0), &mut raw)
                    .map_err(|e| format!("TranslateMessage MSG read: {e:?}"))?;
                let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
                let message = word(4);
                let virtual_key = word(8);
                let translated = match (message, virtual_key) {
                    (0x0100, 0x20) | (0x0104, 0x20) => Some(0x20),
                    (0x0100, 0x30..=0x39) | (0x0104, 0x30..=0x39) => Some(virtual_key),
                    (0x0100, 0x41..=0x5a) | (0x0104, 0x41..=0x5a) => {
                        let shifted = self.keys[0x10] & 0x80 != 0;
                        Some(if shifted { virtual_key } else { virtual_key + 0x20 })
                    }
                    _ => None,
                };
                if let Some(character) = translated {
                    self.windows
                        .post(crate::window::Message {
                            window: word(0),
                            message: if message == 0x0104 { 0x0106 } else { 0x0102 },
                            wparam: character,
                            lparam: word(12),
                        })
                        .map_err(|e| format!("TranslateMessage post: {e:?}"))?;
                    1
                } else {
                    0
                }
            }
            ("user32", "GetMessageA" | "PeekMessageA") => {
                let remove = name == "GetMessageA" || a(4) & 1 != 0;
                let Some(message) = self.windows.peek_filtered(a(1), a(2), a(3), remove) else {
                    return Ok(if name == "GetMessageA" {
                        Effect::Wait
                    } else {
                        Effect::Return(0)
                    });
                };
                if remove && matches!(message.message, 0x100 | 0x101 | 0x104 | 0x105) {
                    if let Some(state) = self.keys.get_mut(message.wparam as usize) {
                        let down = message.message == 0x100 || message.message == 0x104;
                        if down && *state & 0x80 == 0 {
                            *state ^= 1;
                        }
                        *state = (*state & 1) | if down { 0x80 } else { 0 };
                    }
                }
                let mut raw = [0u8; 28];
                for (i, v) in [
                    message.window,
                    message.message,
                    message.wparam,
                    message.lparam,
                ]
                .iter()
                .enumerate()
                {
                    raw[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                memory
                    .write(a(0), &raw)
                    .map_err(|e| format!("MSG write: {e:?}"))?;
                if name == "GetMessageA" && message.message == 18 {
                    0
                } else {
                    1
                }
            }
            ("user32", "BeginPaint") => {
                let (width, height) = self
                    .windows
                    .begin_paint(a(0))
                    .map_err(|e| format!("begin paint: {e:?}"))?;
                let dc = self
                    .gdi
                    .paint_dc(
                        a(0),
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: height as i32,
                        },
                    )
                    .map_err(|e| format!("paint DC: {e:?}"))?;
                let mut raw = [0u8; 64];
                raw[..4].copy_from_slice(&dc.to_le_bytes());
                raw[16..20].copy_from_slice(&width.to_le_bytes());
                raw[20..24].copy_from_slice(&height.to_le_bytes());
                memory
                    .write(a(1), &raw)
                    .map_err(|e| format!("PAINTSTRUCT write: {e:?}"))?;
                self.paints.insert(a(0), dc);
                dc
            }
            ("user32", "EndPaint") => {
                let dc = *self
                    .paints
                    .get(&a(0))
                    .ok_or("EndPaint without BeginPaint")?;
                if read(memory, a(1))? != dc {
                    return Err("paint DC mismatch".into());
                }
                self.windows
                    .end_paint(a(0))
                    .map_err(|e| format!("end paint: {e:?}"))?;
                self.gdi
                    .delete_dc(dc)
                    .map_err(|e| format!("release paint DC: {e:?}"))?;
                self.paints.remove(&a(0));
                1
            }
            ("gdi32", "CreateCompatibleDC") => {
                if a(0) != 0 && !self.gdi.is_dc(a(0)) {
                    return Err("invalid reference DC".into());
                }
                self.gdi
                    .memory_dc()
                    .map_err(|e| format!("create DC: {e:?}"))?
            }
            ("gdi32", "GetStockObject") if a(0) == 15 => {
                if let Some(handle) = self.default_palette { return Ok(Effect::Return(handle)); }
                let entries = DEFAULT_PALETTE_COLORS.into_iter().map(|rgb| [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0]).collect();
                let handle = self.palettes.insert(entries).ok_or("palette handle table full")?;
                self.default_palette = Some(handle);
                handle
            }
            ("gdi32", "GetStockObject") => self
                .gdi
                .stock_object(a(0))
                .map_err(|e| format!("stock GDI object: {e:?}"))?,
            ("gdi32", "CreateDIBSection") => {
                if a(2) != 0 || a(4) != 0 || a(5) != 0 {
                    return Err("only private DIB_RGB_COLORS bitmaps supported".into());
                }
                let width = read(memory, a(1) + 4)? as i32;
                let height = read(memory, a(1) + 8)? as i32;
                if width <= 0 || height == 0 {
                    return Err("invalid DIB dimensions".into());
                }
                let bit_count = (read(memory, a(1) + 14)? & 0xffff) as u64;
                if !matches!(bit_count, 8 | 32) {
                    return Err(format!("unsupported DIB bit depth {bit_count}"));
                }
                let stride = ((width as u64 * bit_count + 31) / 32) * 4;
                let size = stride
                    .checked_mul(height.unsigned_abs() as u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or("DIB size overflow")?;
                let bits = self.allocate(memory, size)?;
                let bitmap = self
                    .gdi
                    .bitmap(memory, a(1), bits)
                    .map_err(|e| format!("DIB: {e:?}"))?;
                self.bitmap_allocations.insert(bitmap, bits);
                write(memory, a(3), bits)?;
                bitmap
            }
            ("gdi32", "SelectObject") => {
                if a(1) == 0 {
                    return Ok(Effect::Return(0));
                }
                match self.gdi.select(a(0), a(1)) {
                    Ok(previous) => previous,
                    Err(crate::gdi::Error::InvalidHandle | crate::gdi::Error::WrongType | crate::gdi::Error::SelectedObject) => 0,
                    Err(e) => return Err(format!("select: {e:?}")),
                }
            }

            ("gdi32", "DeleteObject") => {
                if self.default_palette == Some(a(0)) { return Ok(Effect::Return(1)); }
                if self.palettes.get(a(0)).is_some() && self.gdi.references_object(a(0)) { return Ok(Effect::Return(0)); }
                if self.palettes.remove(a(0)).is_some() {
                    return Ok(Effect::Return(1));
                }
                match self.gdi.delete_object(a(0)) {
                    Ok(()) => {},
                    Err(crate::gdi::Error::InvalidHandle | crate::gdi::Error::WrongType | crate::gdi::Error::SelectedObject) => return Ok(Effect::Return(0)),
                    Err(e) => return Err(format!("delete object: {e:?}")),
                }
                if let Some(bits) = self.bitmap_allocations.remove(&a(0)) {
                    self.arena.release(bits)?;
                }
                1
            }
            ("gdi32", "DeleteDC") => {
                self.gdi
                    .delete_dc(a(0))
                    .map_err(|e| format!("delete DC: {e:?}"))?;
                1
            }
            ("gdi32", "PatBlt") => {
                self.gdi.pat_blt(memory, a(0), a(1) as i32, a(2) as i32, a(3) as i32, a(4) as i32, a(5))
                    .map_err(|e| format!("PatBlt: {e:?}"))?;
                1
            }
            ("gdi32", "BitBlt") => {
                self.gdi
                    .bit_blt(
                        memory,
                        a(0),
                        a(1) as i32,
                        a(2) as i32,
                        a(3) as i32,
                        a(4) as i32,
                        a(5),
                        a(6) as i32,
                        a(7) as i32,
                        a(8),
                    )
                    .map_err(|e| format!("BitBlt: {e:?}"))?;
                1
            }
            _ => return Err(format!("unsupported import {dll}!{name}")),
        };
        Ok(Effect::Return(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryError;
    #[derive(Default)]
    struct Memory(BTreeMap<u32, u8>);
    impl MemoryAccess for Memory {
        fn byte_len(&self) -> u64 {
            1u64 << 32
        }
        fn read(&self, address: u32, out: &mut [u8]) -> Result<(), MemoryError> {
            if address as u64 + out.len() as u64 > 1u64 << 32 {
                return Err(MemoryError {
                    address,
                    length: out.len(),
                });
            }
            for (i, b) in out.iter_mut().enumerate() {
                *b = *self.0.get(&(address + i as u32)).unwrap_or(&0);
            }
            Ok(())
        }
        fn write(&mut self, address: u32, input: &[u8]) -> Result<(), MemoryError> {
            if address as u64 + input.len() as u64 > 1u64 << 32 {
                return Err(MemoryError {
                    address,
                    length: input.len(),
                });
            }
            for (i, b) in input.iter().enumerate() {
                self.0.insert(address + i as u32, *b);
            }
            Ok(())
        }
    }
    fn value(effect: Effect) -> u32 {
        match effect {
            Effect::Return(value) => value,
            _ => panic!("expected return: {effect:?}"),
        }
    }
    #[test]
    fn accelerator_dispatches_command_and_returns_handled_after_callback() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        runtime.windows.register(b"keys".to_vec(), 0x401000).unwrap();
        let Action::Invoke { token, args, .. } = runtime.windows.create(b"keys", 10, 10, 0).unwrap() else { panic!("create") };
        let hwnd = args[0];
        let Action::Invoke { token, .. } = runtime.windows.resume(token, 1).unwrap() else { panic!("create") };
        runtime.windows.resume(token, 0).unwrap();
        // Two resource records: F2 and Shift+F2. The callback may return zero;
        // TranslateAccelerator must still return nonzero and consume the key.
        memory.write(0x2000, &[1, 0, 113, 0, 9, 128, 0, 0, 0x85, 0, 113, 0, 10, 128, 0, 0]).unwrap();
        runtime.resources.register(9, 128, 1033, 0x2000, 16).unwrap();
        assert_eq!(runtime.call("user32", "LoadAcceleratorsA", &[0, 128], &mut memory).unwrap(), Effect::Return(0x2000));
        for (offset, word) in [(0, hwnd), (4, 0x100), (8, 113), (12, 0)] { write(&mut memory, 0x3000 + offset, word).unwrap(); }
        for (shift, command) in [(0, 0x8009), (0x80, 0x800a)] {
            runtime.keys[16] = shift;
            let Effect::Invoke { token, procedure, args } = runtime.call("user32", "TranslateAcceleratorA", &[hwnd, 0x2000, 0x3000], &mut memory).unwrap() else { panic!("command callback") };
            assert_eq!(procedure, 0x401000);
            assert_eq!(args, vec![hwnd, 0x111, 0x10000 | command, 0]);
            assert_eq!(runtime.resume(token, 0).unwrap(), Effect::Return(1));
        }
        for (message, key) in [(0x101, 113), (0x100, 114)] {
            write(&mut memory, 0x3004, message).unwrap();
            write(&mut memory, 0x3008, key).unwrap();
            assert_eq!(runtime.call("user32", "TranslateAcceleratorA", &[hwnd, 0x2000, 0x3000], &mut memory).unwrap(), Effect::Return(0));
        }
        assert_eq!(runtime.call("winmm", "timeKillEvent", &[0], &mut memory).unwrap(), Effect::Return(97));
        assert!(runtime.call("winmm", "timeKillEvent", &[1], &mut memory).is_err());
    }
    #[test]
    fn redraw_erases_before_paint_and_stock_palette_stays_alive() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        runtime.windows.register(b"paint".to_vec(), 0x401000).unwrap();
        let Action::Invoke { token, args, .. } = runtime.windows.create(b"paint", 10, 10, 0).unwrap() else { panic!("create") };
        let hwnd = args[0];
        let Action::Invoke { token, .. } = runtime.windows.resume(token, 1).unwrap() else { panic!("create") };
        runtime.windows.resume(token, 0).unwrap();
        let Effect::Invoke { token, args, .. } = runtime.call("user32", "RedrawWindow", &[hwnd, 0, 0, 0x105], &mut memory).unwrap() else { panic!("erase callback") };
        assert_eq!(args[1], 20);
        let dc = args[2];
        assert!(runtime.gdi.is_dc(dc));
        let Effect::Invoke { token, args, .. } = runtime.resume(token, 1).unwrap() else { panic!("paint callback") };
        assert_eq!(args, vec![hwnd, 15, 0, 0]);
        assert!(!runtime.gdi.is_dc(dc));
        assert_eq!(runtime.resume(token, 0).unwrap(), Effect::Return(1));
        let palette = value(runtime.call("gdi32", "GetStockObject", &[15], &mut memory).unwrap());
        assert_eq!(runtime.call("gdi32", "GetStockObject", &[15], &mut memory).unwrap(), Effect::Return(palette));
        assert_eq!(runtime.palettes.get(palette).unwrap().len(), 20);
        assert_eq!(runtime.call("gdi32", "GetPaletteEntries", &[palette, 19, 1, 0x4000], &mut memory).unwrap(), Effect::Return(1));
        assert_eq!(read(&memory, 0x4000).unwrap(), 0x00ffffff);
        runtime.call("gdi32", "DeleteObject", &[palette], &mut memory).unwrap();
        assert!(runtime.palettes.get(palette).is_some());
    }
    #[test]
    fn indexed_display_caps_and_realized_system_palette_are_consistent() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let dc = value(runtime.call("gdi32", "CreateCompatibleDC", &[0], &mut memory).unwrap());
        assert_eq!(runtime.call("gdi32", "GetDeviceCaps", &[dc, 12], &mut memory).unwrap(), Effect::Return(32));
        assert_eq!(runtime.call("gdi32", "GetSystemPaletteEntries", &[dc, 0, 0, 0], &mut memory).unwrap(), Effect::Return(0));
        runtime.set_display_bits(8).unwrap();
        for (cap, expected) in [(12, 8), (14, 1), (24, 20), (38, 0x141), (104, 256), (106, 20)] {
            assert_eq!(runtime.call("gdi32", "GetDeviceCaps", &[dc, cap], &mut memory).unwrap(), Effect::Return(expected));
        }
        assert!(runtime.set_display_bits(16).is_err());
        assert_eq!(runtime.call("gdi32", "GetSystemPaletteEntries", &[dc, 0, 0, 0], &mut memory).unwrap(), Effect::Return(256));
        let reserved_low = runtime.system_palette[..10].to_vec();
        let reserved_high = runtime.system_palette[246..].to_vec();
        memory.write(0x2000, &[0, 3, 2, 0, 12, 34, 56, 1, 65, 43, 21, 4]).unwrap();
        let palette = value(runtime.call("gdi32", "CreatePalette", &[0x2000], &mut memory).unwrap());
        runtime.call("gdi32", "SelectPalette", &[dc, palette, 0], &mut memory).unwrap();
        assert_eq!(runtime.call("gdi32", "RealizePalette", &[dc], &mut memory).unwrap(), Effect::Return(2));
        assert_eq!(runtime.call("gdi32", "RealizePalette", &[dc], &mut memory).unwrap(), Effect::Return(0));
        assert_eq!(runtime.call("gdi32", "GetSystemPaletteEntries", &[dc, 10, 2, 0x3000], &mut memory).unwrap(), Effect::Return(2));
        assert_eq!(read(&memory, 0x3000).unwrap(), 0x0038220c);
        assert_eq!(read(&memory, 0x3004).unwrap(), 0x00152b41);
        assert_eq!(&runtime.system_palette[..10], reserved_low);
        assert_eq!(&runtime.system_palette[246..], reserved_high);
    }
    #[test]
    fn window_enabled_query_tracks_disabled_style_without_requiring_visibility() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        memory.write(0x11000, b"enabled-test\0").unwrap();
        for (address, value) in [(0x2000, 48), (0x2008, 0x401000), (0x2028, 0x11000)] {
            write(&mut memory, address, value).unwrap();
        }
        runtime.call("user32", "RegisterClassExA", &[0x2000], &mut memory).unwrap();
        let (pending, hwnd) = token(runtime.call("user32", "CreateWindowExA",
            &[0, 0x11000, 0, 0, 0, 0, 10, 10, 0, 0, 0x400000, 0], &mut memory).unwrap(), 129);
        let (pending, _) = token(runtime.resume(pending, 1).unwrap(), 1);
        assert_eq!(runtime.resume(pending, 0).unwrap(), Effect::Return(hwnd));
        for (style, enabled) in [(0x08000000, 0), (0, 1), (0x18000000, 0), (0x10000000, 1)] {
            runtime.call("user32", "SetWindowLongA", &[hwnd, (-16i32) as u32, style], &mut memory).unwrap();
            assert_eq!(runtime.call("user32", "IsWindowEnabled", &[hwnd], &mut memory).unwrap(), Effect::Return(enabled));
        }
        for hwnd in [0, 0xdeadbeef] {
            assert_eq!(runtime.call("user32", "IsWindowEnabled", &[hwnd], &mut memory).unwrap(), Effect::Return(0));
        }
    }
    #[test]
    fn machine_profile_and_memory_status_describe_virtual_guest() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        runtime
            .call("kernel32", "GetSystemInfo", &[0x2000], &mut memory)
            .unwrap();
        assert_eq!(memory.read_word(0x2018).unwrap(), 486);
        runtime
            .call("kernel32", "GlobalMemoryStatus", &[0x2100], &mut memory)
            .unwrap();
        let total = memory.read_word(0x2108).unwrap();
        assert_eq!(memory.read_word(0x2100).unwrap(), 32);
        let pointer = runtime.allocate(&memory, 8192).unwrap();
        runtime
            .call("kernel32", "GlobalMemoryStatus", &[0x2100], &mut memory)
            .unwrap();
        assert_eq!(memory.read_word(0x210c).unwrap(), total - 8192);
        runtime.arena.release(pointer).unwrap();
        runtime
            .call("kernel32", "GlobalMemoryStatus", &[0x2100], &mut memory)
            .unwrap();
        assert_eq!(memory.read_word(0x210c).unwrap(), total);
    }
    #[test]
    fn keyboard_state_advances_when_messages_are_removed() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        runtime
            .windows
            .register(b"keys".to_vec(), 0x401000)
            .unwrap();
        let Action::Invoke { args, .. } = runtime.windows.create(b"keys", 1, 1, 0).unwrap() else {
            panic!("window creation");
        };
        let window = args[0];
        runtime
            .post(Message {
                window,
                message: 0x100,
                wparam: 0x61,
                lparam: 0,
            })
            .unwrap();
        assert_eq!(
            value(
                runtime
                    .call("user32", "GetKeyState", &[0x61], &mut memory)
                    .unwrap()
            ),
            0
        );
        runtime
            .call("user32", "PeekMessageA", &[0x2000, 0, 0, 0, 0], &mut memory)
            .unwrap();
        assert_eq!(
            value(
                runtime
                    .call("user32", "GetKeyState", &[0x61], &mut memory)
                    .unwrap()
            ),
            0
        );
        runtime
            .call("user32", "GetMessageA", &[0x2000, 0, 0, 0], &mut memory)
            .unwrap();
        assert_eq!(
            value(
                runtime
                    .call("user32", "GetKeyState", &[0x61], &mut memory)
                    .unwrap()
            ),
            0xffff8001
        );
        runtime
            .post(Message {
                window,
                message: 0x101,
                wparam: 0x61,
                lparam: 0,
            })
            .unwrap();
        runtime
            .call("user32", "GetMessageA", &[0x2000, 0, 0, 0], &mut memory)
            .unwrap();
        assert_eq!(
            value(
                runtime
                    .call("user32", "GetKeyState", &[0x61], &mut memory)
                    .unwrap()
            ),
            1
        );
    }
    fn token(effect: Effect, expected: u32) -> (u64, u32) {
        match effect {
            Effect::Invoke { token, args, .. } => {
                assert_eq!(args[1], expected);
                (token, args[0])
            }
            _ => panic!("expected callback"),
        }
    }
    #[test]
    fn cbt_hooks_chain_before_creation_and_can_subclass_or_cancel() {
        let mut memory = Memory::default();
        let mut runtime = Runtime::default();
        runtime.register_code(0x401000, 0x3000);
        memory.write(0x11000, b"fixture\0").unwrap();
        for (address, value) in [(0x2000, 48), (0x2008, 0x401000), (0x2028, 0x11000)] {
            write(&mut memory, address, value).unwrap();
        }
        runtime
            .call("USER32.dll", "RegisterClassExA", &[0x2000], &mut memory)
            .unwrap();
        for procedure in [0x402000, 0x403000] {
            runtime
                .call(
                    "USER32.dll",
                    "SetWindowsHookExA",
                    &[5, procedure, 0, 1],
                    &mut memory,
                )
                .unwrap();
        }
        let create = [0, 0x11000, 0, 0, 0, 0, 32, 24, 0, 0, 0x400000, 0];
        let Effect::Invoke {
            token: outer,
            procedure,
            args,
        } = runtime
            .call("USER32.dll", "CreateWindowExA", &create, &mut memory)
            .unwrap()
        else {
            panic!("hook callback");
        };
        assert_eq!(procedure, 0x403000);
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], 3);
        let window = args[1];
        assert_ne!(read(&memory, args[2]).unwrap(), 0);
        let Effect::Invoke {
            token: inner,
            procedure,
            ..
        } = runtime
            .call(
                "USER32.dll",
                "CallNextHookEx",
                &[0, args[0], window, args[2]],
                &mut memory,
            )
            .unwrap()
        else {
            panic!("next hook");
        };
        assert_eq!(procedure, 0x402000);
        assert_eq!(runtime.resume(inner, 0).unwrap(), Effect::Return(0));
        assert_eq!(
            runtime
                .call(
                    "USER32.dll",
                    "SetWindowLongA",
                    &[window, (-4i32) as u32, 0x401100],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(0x401000)
        );
        let first = runtime.resume(outer, 0).unwrap();
        assert!(matches!(&first,Effect::Invoke {procedure:0x401100,args,..} if args[1]==129));
        let (t, _) = token(first, 129);
        let (t, _) = token(runtime.resume(t, 1).unwrap(), 1);
        assert_eq!(runtime.resume(t, 0).unwrap(), Effect::Return(window));
        let Effect::Invoke {
            token: cancel,
            args,
            ..
        } = runtime
            .call("USER32.dll", "CreateWindowExA", &create, &mut memory)
            .unwrap()
        else {
            panic!("hook callback");
        };
        assert_eq!(runtime.resume(cancel, 1).unwrap(), Effect::Return(0));
        assert!(runtime.windows.get(args[1]).is_err());
        assert!(!runtime.window_data.contains_key(&args[1]));
    }
    #[test]
    fn dispatcher_connects_window_creation_message_paint_and_dib_output() {
        let mut memory = Memory::default();
        let mut runtime = Runtime::default();
        memory.write(0x11000, b"fixture\0").unwrap();
        for (address, v) in [(0x2000, 48), (0x2008, 0x401000), (0x2028, 0x11000)] {
            write(&mut memory, address, v).unwrap();
        }
        assert_eq!(
            runtime
                .call("USER32.dll", "RegisterClassExA", &[0x2000], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        let (t, hwnd) = token(
            runtime
                .call(
                    "USER32.dll",
                    "CreateWindowExA",
                    &[0, 0x11000, 0, 0, 0, 0, 2, 1, 0, 0, 0x400000, 0],
                    &mut memory,
                )
                .unwrap(),
            129,
        );
        let (t, _) = token(runtime.resume(t, 1).unwrap(), 1);
        assert_eq!(runtime.resume(t, 0).unwrap(), Effect::Return(hwnd));
        runtime
            .call("USER32.dll", "ShowWindow", &[hwnd, 1], &mut memory)
            .unwrap();
        runtime
            .call("USER32.dll", "GetMessageA", &[0x3000, 0, 0, 0], &mut memory)
            .unwrap();
        assert_eq!(read(&memory, 0x3004).unwrap(), 15);
        let dst = value(
            runtime
                .call("USER32.dll", "BeginPaint", &[hwnd, 0x4000], &mut memory)
                .unwrap(),
        );
        let src = value(
            runtime
                .call("GDI32.dll", "CreateCompatibleDC", &[0], &mut memory)
                .unwrap(),
        );
        for (address, v) in [
            (0x5000, 40),
            (0x5004, 2),
            (0x5008, u32::MAX),
            (0x500c, 0x00200001),
        ] {
            write(&mut memory, address, v).unwrap();
        }
        let bitmap = value(
            runtime
                .call(
                    "GDI32.dll",
                    "CreateDIBSection",
                    &[src, 0x5000, 0, 0x6000, 0, 0],
                    &mut memory,
                )
                .unwrap(),
        );
        let bits = read(&memory, 0x6000).unwrap();
        memory.write(bits, &[1, 2, 3, 0, 4, 5, 6, 0]).unwrap();
        runtime
            .call("GDI32.dll", "SelectObject", &[src, bitmap], &mut memory)
            .unwrap();
        runtime
            .call(
                "GDI32.dll",
                "BitBlt",
                &[dst, 0, 0, 2, 1, src, 0, 0, 0x00cc0020],
                &mut memory,
            )
            .unwrap();
        runtime
            .call("USER32.dll", "EndPaint", &[hwnd, 0x4000], &mut memory)
            .unwrap();
        assert_eq!(
            runtime.gdi.surface(hwnd).unwrap().rgba,
            [3, 2, 1, 255, 6, 5, 4, 255]
        );
        assert_eq!(
            runtime
                .call("USER32.dll", "GetMessageA", &[0x3000, 0, 0, 0], &mut memory)
                .unwrap(),
            Effect::Wait
        );
        assert!(runtime
            .call("bogus", "BitBlt", &[], &mut memory)
            .unwrap_err()
            .contains("bogus!BitBlt"));
        assert!(runtime
            .call("USER32.dll", "GetMessageA", &[], &mut memory)
            .is_err());
    }
}
