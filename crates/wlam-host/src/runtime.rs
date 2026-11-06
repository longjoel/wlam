//! Engine-independent dispatcher for the first interactive Win32 slice.
use crate::{
    gdi::{Gdi, Rect},
    memory::MemoryAccess,
    window::{Action, Message, Windows},
};
use std::collections::BTreeMap;
pub const DESKTOP_WINDOW: u32 = 0x10000001;
/// Window procedure sentinel for `DefWindowProcA`: a class registered with the
/// host's import thunk as its procedure behaves like the default proc.
const DEFAULT_WINDOW_PROC: u32 = 0xffff_ff03;
/// Synthetic message target for multimedia timer callbacks. It is never a real
/// window; `DispatchMessageA` recognizes it and invokes the timer proc.
const MULTIMEDIA_TIMER_WINDOW: u32 = 0xffff_fffd;

#[derive(Debug)]
struct MultimediaTimer {
    due: u64,
    interval: u32,
    callback: u32,
    user: u32,
    periodic: bool,
}

const DEFAULT_PALETTE_COLORS: [u32; 20] = [
    0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0xc0dcc0,
    0xa6caf0, 0xfffbf0, 0xa0a0a4, 0x808080, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff,
    0x00ffff, 0xffffff,
];
fn initial_system_palette() -> [[u8; 4]; 256] {
    let mut colors = [[0; 4]; 256];
    let levels = [0, 51, 102, 153, 204, 255];
    let mut index = 10;
    for red in levels {
        for green in levels {
            for blue in levels {
                colors[index] = [red, green, blue, 0];
                index += 1;
            }
        }
    }
    for gray in 1..=20 {
        let value = (gray * 255 / 21) as u8;
        colors[index] = [value, value, value, 0];
        index += 1;
    }
    for (index, rgb) in DEFAULT_PALETTE_COLORS.into_iter().enumerate() {
        colors[if index < 10 { index } else { index + 236 }] =
            [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0];
    }
    colors
}

/// Convert days since 1970-01-01 to a proleptic Gregorian calendar date.
/// This is Howard Hinnant's civil_from_days algorithm; it keeps the host free
/// of a calendar dependency and works for the full FILETIME range.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + (month <= 2) as i64, month as u32, day as u32)
}

/// Convert a UTC FILETIME into packed MS-DOS date and time words.
/// The virtual time zone is UTC with zero bias, so no local adjustment is
/// needed. MS-DOS cannot represent dates outside 1980-01-01..2107-12-31.
fn filetime_to_dos(filetime: u64) -> Option<(u16, u16)> {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const SECONDS_PER_DAY: u64 = 86_400;
    const DAYS_1601_TO_1970: i64 = 134_774;
    let seconds = filetime / TICKS_PER_SECOND;
    let days = (seconds / SECONDS_PER_DAY) as i64 - DAYS_1601_TO_1970;
    let remainder = seconds % SECONDS_PER_DAY;
    let (year, month, day) = civil_from_days(days);
    if !(1980..=2107).contains(&year) {
        return None;
    }
    let hour = (remainder / 3_600) as u16;
    let minute = ((remainder / 60) % 60) as u16;
    let second = (remainder % 60) as u16;
    let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    let time = (hour << 11) | (minute << 5) | (second / 2);
    Some((date, time))
}

fn is_crt_library(stem: &str) -> bool {
    stem == "msvcrt"
        || stem == "ucrtbase"
        || stem == "api-ms-win-crt-runtime-l1-1-0"
        || stem.starts_with("msvcr")
        || stem.starts_with("vcruntime")
}

fn fill_guest(
    memory: &mut dyn MemoryAccess,
    address: u32,
    byte: u8,
    count: u32,
) -> Result<(), String> {
    let block = [byte; 4096];
    let mut offset = 0;
    while offset < count {
        let length = (count - offset).min(block.len() as u32) as usize;
        let at = address
            .checked_add(offset)
            .ok_or("memset address overflow")?;
        memory
            .write(at, &block[..length])
            .map_err(|e| format!("memset: {e:?}"))?;
        offset += length as u32;
    }
    Ok(())
}

fn copy_guest(
    memory: &mut dyn MemoryAccess,
    destination: u32,
    source: u32,
    count: u32,
) -> Result<(), String> {
    let mut block = [0u8; 4096];
    let mut offset = 0;
    while offset < count {
        let length = (count - offset).min(block.len() as u32) as usize;
        let from = source.checked_add(offset).ok_or("memcpy source overflow")?;
        let to = destination
            .checked_add(offset)
            .ok_or("memcpy destination overflow")?;
        memory
            .read(from, &mut block[..length])
            .map_err(|e| format!("memcpy source: {e:?}"))?;
        memory
            .write(to, &block[..length])
            .map_err(|e| format!("memcpy destination: {e:?}"))?;
        offset += length as u32;
    }
    Ok(())
}

fn move_guest(
    memory: &mut dyn MemoryAccess,
    destination: u32,
    source: u32,
    count: u32,
) -> Result<(), String> {
    if destination <= source || destination as u64 >= source as u64 + count as u64 {
        return copy_guest(memory, destination, source, count);
    }
    // Overlapping copy towards a higher address: walk backwards.
    let mut block = [0u8; 4096];
    let mut remaining = count;
    while remaining > 0 {
        let length = remaining.min(block.len() as u32);
        let at = remaining - length;
        let from = source.checked_add(at).ok_or("memmove source overflow")?;
        let to = destination
            .checked_add(at)
            .ok_or("memmove destination overflow")?;
        memory
            .read(from, &mut block[..length as usize])
            .map_err(|e| format!("memmove source: {e:?}"))?;
        memory
            .write(to, &block[..length as usize])
            .map_err(|e| format!("memmove destination: {e:?}"))?;
        remaining = at;
    }
    Ok(())
}

fn compare_guest(
    memory: &dyn MemoryAccess,
    left: u32,
    right: u32,
    count: u32,
) -> Result<u32, String> {
    let mut left_block = [0u8; 4096];
    let mut right_block = [0u8; 4096];
    let mut offset = 0;
    while offset < count {
        let length = (count - offset).min(left_block.len() as u32) as usize;
        let left_at = left.checked_add(offset).ok_or("memcmp left overflow")?;
        let right_at = right.checked_add(offset).ok_or("memcmp right overflow")?;
        memory
            .read(left_at, &mut left_block[..length])
            .map_err(|e| format!("memcmp left: {e:?}"))?;
        memory
            .read(right_at, &mut right_block[..length])
            .map_err(|e| format!("memcmp right: {e:?}"))?;
        for index in 0..length {
            if left_block[index] != right_block[index] {
                return Ok((left_block[index] as i32 - right_block[index] as i32) as u32);
            }
        }
        offset += length as u32;
    }
    Ok(0)
}

fn parse_atoi(text: &[u8]) -> i32 {
    let mut index = 0;
    while text.get(index).is_some_and(|c| c.is_ascii_whitespace()) {
        index += 1;
    }
    let mut negative = false;
    if let Some(sign) = text.get(index) {
        if *sign == b'-' || *sign == b'+' {
            negative = *sign == b'-';
            index += 1;
        }
    }
    let mut value: i64 = 0;
    while let Some(digit) = text.get(index).filter(|c| c.is_ascii_digit()) {
        value = (value * 10 + i64::from(*digit - b'0')).min(i32::MAX as i64 + 1);
        index += 1;
    }
    let value = if negative { -value } else { value };
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
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
    /// Resume the same host call to drain work already queued by callbacks.
    Retry,
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
    FixedReturn(u32),
    RadioChecks { remaining: Vec<u32>, checked: u32 },
    ModalControl(DialogKey),
    DisableAfterCancel { window: u32, previous_disabled: u32 },
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
    FixedReturn(u32),
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
    DialogChildHook(DialogChildContext),
    DialogChildNcCreate(DialogChildContext),
    DialogChildCreate(DialogChildContext),
    DialogBoxMessage {
        key: DialogKey,
        default_paint: Option<u32>,
    },
    /// Finish `EnumDisplayMonitors`: free the monitor rectangle and report
    /// whether the callback accepted it.
    EnumMonitors {
        rect: u32,
    },
    /// Continue the `_initterm` table after a constructor returns.
    InitTerm {
        remaining: Vec<u32>,
    },
    /// Run registered `atexit`/`_onexit` callbacks before terminating.
    ExitHandlers {
        remaining: Vec<u32>,
        code: u32,
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PropertyName {
    Atom(u16),
    String(Vec<u8>),
}
#[derive(Debug)]
struct CallbackPending {
    context: Option<HookContext>,
    finish: CallbackFinish,
}
#[derive(Debug)]
struct WindowData {
    button: crate::buttons::ButtonState,
    dialog_procedure: u32,
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
    /// Placement restored by `SW_RESTORE`/`SC_RESTORE` after a minimize or
    /// maximize. Win32 keeps this in WINDOWPLACEMENT; the host keeps the same
    /// rectangle so the window manager (the page) can mirror the transition.
    restore: Option<(i32, i32, u32, u32)>,
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
    /// MSVCRT state. The CRT is a host layer here, not a separately loaded DLL.
    crt_control_word: u32,
    crt_acmdln: Option<u32>,
    crt_commode: Option<u32>,
    crt_fmode: Option<u32>,
    exit_handlers: Vec<u32>,
    terminate_handler: u32,
    pub modules: crate::modules::Modules,
    pub registry: crate::registry::Registry,
    properties: BTreeMap<(u32, PropertyName), u32>,
    exception_filter: u32,
    cursor: (i32, i32),
    keys: [u8; 256],
    atoms: crate::atoms::Atoms,
    code_ranges: Vec<(u64, u64)>,
    common_dcs: BTreeMap<u32, u32>,
    cursors: BTreeMap<u32, u32>,
    resource_cursors: BTreeMap<(u32, u32), u32>,
    cursor_images: BTreeMap<u32, (u16, u16, crate::icon::Icon)>,
    active_cursor: u32,
    scrollbars: BTreeMap<(u32, u32), (i32, i32, i32)>, // minimum, maximum, position
    invalid_rects: BTreeMap<u32, Rect>,
    cursor_display_count: i32,
    focused_window: u32,
    pressed_button: Option<u32>,
    registered_messages: crate::atoms::Atoms,
    error_mode: u32,
    events: crate::events::Events,
    message_hooks: crate::objects::Handles<(u32, u32, u32)>,
    hook_order: Vec<u32>,
    classes: BTreeMap<Vec<u8>, (u16, [u32; 10])>,
    /// DC shared by every window of a `CS_CLASSDC` class.
    temp_file_counter: u32,
    class_dcs: BTreeMap<Vec<u8>, u32>,
    /// Private DC of a `CS_OWNDC` window.
    own_dcs: BTreeMap<u32, u32>,
    pub resources: crate::resources::Resources,
    icons: crate::objects::Handles<(u32, u32)>,
    shared_icons: BTreeMap<(u32, u32), u32>,
    static_icons: BTreeMap<u32, crate::icon::Icon>,
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
    multimedia_timers: BTreeMap<u32, MultimediaTimer>,
    next_multimedia_timer: u32,
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
            // x87/SSE defaults: round-to-nearest, 53-bit precision, all
            // exceptions masked. MSVCRT also tracks SSE status in bit 24.
            crt_control_word: 0x0008_027F,
            crt_acmdln: None,
            crt_commode: None,
            crt_fmode: None,
            exit_handlers: Vec::new(),
            terminate_handler: 0,
            modules: crate::modules::Modules::default(),
            registry: crate::registry::Registry::default(),
            properties: BTreeMap::new(),
            exception_filter: 0,
            cursor: (0, 0),
            keys: [0; 256],
            atoms: crate::atoms::Atoms::default(),
            code_ranges: Vec::new(),
            common_dcs: BTreeMap::new(),
            cursors: BTreeMap::new(),
            resource_cursors: BTreeMap::new(),
            cursor_images: BTreeMap::new(),
            active_cursor: 0,
            scrollbars: BTreeMap::new(),
            invalid_rects: BTreeMap::new(),
            cursor_display_count: 0,
            focused_window: 0,
            pressed_button: None,
            registered_messages: crate::atoms::Atoms::default(),
            error_mode: 0,
            events: crate::events::Events::default(),
            message_hooks: crate::objects::Handles::with_namespace(7),
            hook_order: Vec::new(),
            classes: BTreeMap::new(),
            temp_file_counter: 0,
            class_dcs: BTreeMap::new(),
            own_dcs: BTreeMap::new(),
            resources: crate::resources::Resources::default(),
            icons: crate::objects::Handles::with_namespace(9),
            shared_icons: BTreeMap::new(),
            static_icons: BTreeMap::new(),
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
            multimedia_timers: BTreeMap::new(),
            next_multimedia_timer: 1,
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
fn write_u16(memory: &mut dyn MemoryAccess, address: u32, value: u16) -> Result<(), String> {
    memory
        .write(address, &value.to_le_bytes())
        .map_err(|e| format!("guest u16 write: {e:?}"))
}
fn read_byte(memory: &dyn MemoryAccess, address: u32) -> Result<u8, String> {
    let mut byte = [0; 1];
    memory
        .read(address, &mut byte)
        .map_err(|e| format!("guest byte: {e:?}"))?;
    Ok(byte[0])
}
fn read_rect(memory: &dyn MemoryAccess, address: u32) -> Result<[i32; 4], String> {
    let mut raw = [0u8; 16];
    memory
        .read(address, &mut raw)
        .map_err(|e| format!("RECT read: {e:?}"))?;
    Ok(std::array::from_fn(|i| {
        i32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap())
    }))
}
fn write_rect(memory: &mut dyn MemoryAccess, address: u32, rect: [i32; 4]) -> Result<(), String> {
    let mut raw = [0u8; 16];
    for (i, edge) in rect.iter().enumerate() {
        raw[i * 4..i * 4 + 4].copy_from_slice(&edge.to_le_bytes());
    }
    memory
        .write(address, &raw)
        .map_err(|e| format!("RECT write: {e:?}"))
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
/// Read NUL-terminated UTF-16 code units from guest memory.
fn read_wide_units(memory: &dyn MemoryAccess, address: u32) -> Result<Vec<u16>, String> {
    let mut units = Vec::new();
    for index in 0..4096u32 {
        let mut bytes = [0; 2];
        memory
            .read(address + index * 2, &mut bytes)
            .map_err(|e| format!("wide string: {e:?}"))?;
        let unit = u16::from_le_bytes(bytes);
        if unit == 0 {
            return Ok(units);
        }
        units.push(unit);
    }
    Err("guest wide string exceeds 4096 units".into())
}
/// Read a NUL-terminated UTF-16 string from guest memory.
fn read_wide_string(memory: &dyn MemoryAccess, address: u32) -> Result<String, String> {
    Ok(String::from_utf16_lossy(&read_wide_units(memory, address)?))
}
fn format_wsprintf(
    format: &[u8],
    values: &[u32],
    memory: &dyn MemoryAccess,
) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut arg = 0usize;
    let mut at = 0usize;
    while at < format.len() {
        if format[at] != b'%' {
            output.push(format[at]);
            at += 1;
            continue;
        }
        at += 1;
        if at == format.len() {
            break;
        }
        if format[at] == b'%' {
            output.push(b'%');
            at += 1;
            continue;
        }
        let mut left = false;
        let mut zero = false;
        while at < format.len() && matches!(format[at], b'-' | b'0' | b'#' | b' ' | b'+') {
            left |= format[at] == b'-';
            zero |= format[at] == b'0';
            at += 1;
        }
        let mut width = 0usize;
        while at < format.len() && format[at].is_ascii_digit() {
            width = width
                .saturating_mul(10)
                .saturating_add((format[at] - b'0') as usize)
                .min(1024);
            at += 1;
        }
        let mut precision = None;
        if at < format.len() && format[at] == b'.' {
            at += 1;
            let mut p = 0usize;
            while at < format.len() && format[at].is_ascii_digit() {
                p = p
                    .saturating_mul(10)
                    .saturating_add((format[at] - b'0') as usize)
                    .min(1024);
                at += 1;
            }
            precision = Some(p);
        }
        while at < format.len() && matches!(format[at], b'l' | b'h' | b'I' | b'z' | b'w') {
            at += 1;
        }
        let kind = *format.get(at).ok_or("incomplete wsprintf format")?;
        at += 1;
        let value = *values.get(arg).ok_or("not enough wsprintf arguments")?;
        arg += 1;
        let mut piece = match kind {
            b'x' => format!("{value:x}").into_bytes(),
            b'X' => format!("{value:X}").into_bytes(),
            b'd' | b'i' => format!("{}", value as i32).into_bytes(),
            b'u' => value.to_string().into_bytes(),
            b'p' => format!("{value:08x}").into_bytes(),
            b'c' => vec![value as u8],
            b's' | b'S' => {
                if value == 0 {
                    Vec::new()
                } else {
                    string(memory, value)?
                }
            }
            other => vec![other],
        };
        if let Some(p) = precision {
            if matches!(kind, b's' | b'S') && piece.len() > p {
                piece.truncate(p);
            }
        }
        let padding = width.saturating_sub(piece.len()).min(1024);
        if !left && padding > 0 {
            let pad = if zero { b'0' } else { b' ' };
            output.extend(std::iter::repeat(pad).take(padding));
        }
        output.extend(piece);
        if left && padding > 0 {
            output.extend(std::iter::repeat(b' ').take(padding));
        }
        if output.len() > 1023 {
            output.truncate(1023);
            break;
        }
    }
    Ok(output)
}
impl Runtime {
    pub fn with_allocation_base(address: u32) -> Self {
        Self { arena: crate::arena::Arena::starting_at(address), ..Self::default() }
    }
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
        if !matches!(bits, 8 | 32) {
            return Err("display depth must be 8 or 32".into());
        }
        self.display_bits = bits;
        Ok(())
    }
    pub fn next_timer_delay(&self) -> u32 {
        self.timers
            .values()
            .map(|(due, _, _)| due.saturating_sub(self.clock_ms).min(u32::MAX as u64) as u32)
            .chain(
                self.multimedia_timers
                    .values()
                    .filter(|timer| timer.due != u64::MAX)
                    .map(|timer| {
                        timer
                            .due
                            .saturating_sub(self.clock_ms)
                            .min(u32::MAX as u64) as u32
                    }),
            )
            .min()
            .unwrap_or(u32::MAX)
    }

    /// Return the composited window surface as display pixels. Indexed display
    /// profiles translate RGB GDI output through the currently realized system
    /// palette before the browser presents it.
    pub fn presented_frame(&self, window: u32) -> Option<Vec<u8>> {
        let surface = self.gdi.surface(window)?;
        let mut pixels = surface.rgba.clone();
        if self.display_bits != 8 {
            return Some(pixels);
        }

        // Cache mappings for repeated colors in this frame. Most classic game
        // frames contain a small palette, so this avoids a 256-color search for
        // every pixel while still mapping arbitrary RGB GDI output correctly.
        let mut mapped = BTreeMap::<u32, [u8; 3]>::new();
        for pixel in pixels.chunks_exact_mut(4) {
            let key = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], 0]);
            let color = *mapped.entry(key).or_insert_with(|| {
                let r = pixel[0] as i32;
                let g = pixel[1] as i32;
                let b = pixel[2] as i32;
                let nearest = self
                    .system_palette
                    .iter()
                    .min_by_key(|candidate| {
                        let dr = candidate[0] as i32 - r;
                        let dg = candidate[1] as i32 - g;
                        let db = candidate[2] as i32 - b;
                        dr * dr + dg * dg + db * db
                    })
                    .unwrap();
                [nearest[0], nearest[1], nearest[2]]
            });
            pixel[..3].copy_from_slice(&color);
        }
        Some(pixels)
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
        let mut fired = Vec::new();
        for (&id, timer) in &mut self.multimedia_timers {
            if timer.due == u64::MAX || timer.due > self.clock_ms {
                continue;
            }
            fired.push((id, timer.callback));
            if timer.periodic {
                while timer.due <= self.clock_ms {
                    timer.due = timer.due.saturating_add(timer.interval.max(1) as u64);
                }
            } else {
                // Keep the entry until DispatchMessageA invokes the callback so
                // its user data is still available; due == MAX retires it.
                timer.due = u64::MAX;
            }
        }
        for (id, callback) in fired {
            self.windows.post_system(Message {
                window: MULTIMEDIA_TIMER_WINDOW,
                message: 0,
                wparam: id,
                lparam: callback,
            });
        }
        Ok(())
    }
    fn button_check_action(&mut self, window: u32, value: u32) -> Result<Action, String> {
        let procedure = self
            .windows
            .get(window)
            .map_err(|e| format!("button: {e:?}"))?
            .procedure;
        if procedure == 0xffff_ff02 {
            if let Some(data) = self
                .window_data
                .get_mut(&window)
                .filter(|data| data.class == b"BUTTON")
            {
                data.button.set_check(data.style, value);
                self.render_static(window)?;
            }
            Ok(Action::Return(0))
        } else {
            self.windows
                .send(Message {
                    window,
                    message: 0xf1,
                    wparam: value,
                    lparam: 0,
                })
                .map_err(|e| format!("button check: {e:?}"))
        }
    }
    fn radio_checks(&mut self, mut remaining: Vec<u32>, checked: u32) -> Result<Effect, String> {
        while let Some(window) = remaining.pop() {
            let action = self.button_check_action(window, (window == checked) as u32)?;
            if matches!(action, Action::Invoke { .. }) {
                return self.action(action, Finish::RadioChecks { remaining, checked });
            }
        }
        Ok(Effect::Return(1))
    }
    fn activate_button(&mut self, window: u32) -> Result<(), String> {
        let Some(data) = self.window_data.get(&window) else {
            return Ok(());
        };
        let (parent, style) = (data.parent, data.style);
        if style & 15 == 9 {
            let children = self.windows.children(parent);
            if let Some(index) = children.iter().position(|handle| *handle == window) {
                let start = (0..=index)
                    .rev()
                    .find(|at| {
                        self.window_data
                            .get(&children[*at])
                            .is_some_and(|data| data.style & 0x20000 != 0)
                    })
                    .unwrap_or(0);
                let end = (index + 1..children.len())
                    .find(|at| {
                        self.window_data
                            .get(&children[*at])
                            .is_some_and(|data| data.style & 0x20000 != 0)
                    })
                    .unwrap_or(children.len());
                for child in &children[start..end] {
                    if let Some(data) = self
                        .window_data
                        .get_mut(child)
                        .filter(|data| data.class == b"BUTTON" && matches!(data.style & 15, 4 | 9))
                    {
                        data.button.set_check(data.style, (*child == window) as u32);
                        self.render_static(*child)?;
                    }
                }
            }
        }
        if let Some(data) = self.window_data.get_mut(&window) {
            data.button.activate(data.style);
        }
        self.render_static(window)
    }
    fn control_effect(
        &mut self,
        window: u32,
        message: u32,
        wparam: u32,
        lparam: u32,
        memory: &mut dyn MemoryAccess,
    ) -> Result<Effect, String> {
        let Some(data) = self.window_data.get(&window) else {
            return Ok(Effect::Return(0));
        };
        if data.class != b"BUTTON" || !matches!(message, 0xf5 | 0x201 | 0x202 | 0x100 | 0x101 | 31)
        {
            return Ok(Effect::Return(
                self.static_window_proc(window, message, wparam, lparam, memory)?,
            ));
        }
        let (parent, id, enabled, was_pressed) = (
            data.parent,
            data.id,
            data.style & 0x08000000 == 0,
            data.button.pressed,
        );
        if !enabled {
            return Ok(Effect::Return(0));
        }
        if matches!(message, 0x100 | 0x101) && wparam != 32 {
            return Ok(Effect::Return(0));
        }
        if matches!(message, 0x201 | 0x100) {
            self.window_data.get_mut(&window).unwrap().button.pressed = true;
            self.pressed_button = Some(window);
            self.focused_window = window;
            self.render_static(window)?;
            return Ok(Effect::Return(0));
        }
        self.window_data.get_mut(&window).unwrap().button.pressed = false;
        self.pressed_button = None;
        self.render_static(window)?;
        if message != 0xf5 && (message == 31 || !was_pressed) {
            return Ok(Effect::Return(0));
        }
        if message == 0x202 {
            let x = lparam as i16 as i32;
            let y = (lparam >> 16) as i16 as i32;
            let bounds = self
                .windows
                .get(window)
                .map_err(|e| format!("button bounds: {e:?}"))?;
            if x < 0 || y < 0 || x >= bounds.width as i32 || y >= bounds.height as i32 {
                return Ok(Effect::Return(0));
            }
        }
        self.activate_button(window)?;
        if parent == 0 {
            return Ok(Effect::Return(0));
        }
        let parent_window = self
            .windows
            .get(parent)
            .map_err(|e| format!("button parent: {e:?}"))?;
        if parent_window.procedure == 0xffff_ff01 {
            let procedure = self
                .window_data
                .get(&parent)
                .map_or(0, |data| data.dialog_procedure);
            if procedure == 0 {
                return Ok(Effect::Return(0));
            }
            return self.callback(
                procedure,
                vec![parent, 0x111, id & 0xffff, window],
                None,
                CallbackFinish::FixedReturn(0),
            );
        }
        let action = self
            .windows
            .send(Message {
                window: parent,
                message: 0x111,
                wparam: id & 0xffff,
                lparam: window,
            })
            .map_err(|e| format!("button notification: {e:?}"))?;
        self.action(action, Finish::FixedReturn(0))
    }
    fn modal_control_effect(&mut self, effect: Effect, key: DialogKey) -> Result<Effect, String> {
        match effect {
            Effect::Return(_) => self.finish_modal(key),
            effect @ Effect::Invoke { token, .. } => {
                if let Some(finish) = self.pending.get_mut(&token) {
                    *finish = Finish::ModalControl(key);
                } else if let Some(callback) = self.callbacks.get_mut(&token) {
                    callback.finish = CallbackFinish::DialogBoxMessage {
                        key,
                        default_paint: None,
                    };
                } else {
                    return Err("unknown dialog control continuation".into());
                }
                Ok(effect)
            }
            _ => Err("unexpected dialog control effect".into()),
        }
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
        if self
            .window_data
            .get(&window)
            .is_some_and(|data| data.class == b"BUTTON")
        {
            match message {
                0xf0 => {
                    return Ok(self.window_data[&window]
                        .button
                        .check(self.window_data[&window].style))
                }
                0xf1 => {
                    let data = self.window_data.get_mut(&window).unwrap();
                    data.button.set_check(data.style, wparam);
                    self.render_static(window)?;
                    return Ok(0);
                }
                0xf2 => {
                    return Ok(self.window_data[&window].button.state(
                        self.window_data[&window].style,
                        self.focused_window == window,
                    ))
                }
                0xf3 => {
                    self.window_data.get_mut(&window).unwrap().button.pressed = wparam != 0;
                    self.render_static(window)?;
                    return Ok(0);
                }
                0xf4 => {
                    let data = self.window_data.get_mut(&window).unwrap();
                    data.style = (data.style & !0xffff) | (wparam & 0xffff);
                    if lparam != 0 {
                        self.render_static(window)?;
                    }
                    return Ok(0);
                }
                _ => {}
            }
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
                if self.window_data.get(&window).is_some_and(|data| matches!(data.class.as_slice(), b"BUTTON" | b"STATIC" | b"EDIT" | b"LISTBOX" | b"SCROLLBAR")) { self.render_static(window)?; }
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
        self.invalid_rects.remove(&window);
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
    fn render_check_button(
        &mut self,
        window: u32,
        width: u32,
        height: u32,
        style: u32,
        text: &[u8],
        state: crate::buttons::ButtonState,
    ) -> Result<(), String> {
        let size = width.min(height).min(13) as i32;
        let top = (height as i32 - size) / 2;
        let enabled = style & 0x08000000 == 0;
        let ink = if enabled { 0 } else { 0x808080 };
        let checked = state.check(style);
        let paint = |gdi: &mut Gdi, x: i32, y: i32, color: u32| -> Result<(), String> {
            gdi.fill_surface(
                window,
                Rect {
                    left: x,
                    top: y,
                    right: x + 1,
                    bottom: y + 1,
                },
                color,
            )
            .map_err(|e| format!("button glyph: {e:?}"))
        };
        if matches!(style & 15, 4 | 9) {
            let diameter = size - 1;
            for y in 0..size {
                for x in 0..size {
                    let dx = 2 * x - diameter;
                    let dy = 2 * y - diameter;
                    let squared = dx * dx + dy * dy;
                    if squared <= diameter * diameter {
                        let color = if squared >= (diameter - 2).max(0).pow(2) {
                            0x808080
                        } else if checked != 0 && squared <= (diameter / 2).pow(2) {
                            ink
                        } else {
                            0xffffff
                        };
                        paint(&mut self.gdi, x + 1, top + y, color)?;
                    }
                }
            }
        } else {
            self.gdi
                .fill_surface(
                    window,
                    Rect {
                        left: 1,
                        top,
                        right: size + 1,
                        bottom: top + size,
                    },
                    0xffffff,
                )
                .map_err(|e| format!("checkbox fill: {e:?}"))?;
            self.gdi
                .frame_surface(
                    window,
                    Rect {
                        left: 1,
                        top,
                        right: size + 1,
                        bottom: top + size,
                    },
                    0x808080,
                )
                .map_err(|e| format!("checkbox border: {e:?}"))?;
            if checked == 2 {
                self.gdi
                    .fill_surface(
                        window,
                        Rect {
                            left: 3,
                            top: top + 2,
                            right: size - 1,
                            bottom: top + size - 2,
                        },
                        0x808080,
                    )
                    .map_err(|e| format!("checkbox mixed mark: {e:?}"))?;
            } else if checked == 1 && size >= 11 {
                for (x, y) in [(3, 6), (4, 7), (5, 8), (6, 7), (7, 6), (8, 5), (9, 4)] {
                    paint(&mut self.gdi, x + 1, top + y, ink)?;
                    paint(&mut self.gdi, x + 1, top + y + 1, ink)?;
                }
            }
        }
        let x = size + 5;
        self.gdi
            .draw_default_text(
                window,
                x,
                ((height as i32 - 14) / 2).max(0),
                crate::font::extent(text).0,
                text,
                ink,
            )
            .map_err(|e| format!("check-button text: {e:?}"))?;
        Ok(())
    }
    fn render_static(&mut self, window: u32) -> Result<(), String> {
        let data = self
            .window_data
            .get(&window)
            .ok_or("unknown static control")?;
        let class = data.class.clone();
        let style = data.style;
        let button = data.button;
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
            b"BUTTON" if matches!(style & 15, 2..=6 | 9) && style & 0x1000 == 0 => {
                self.render_check_button(window, width, height, style, &text, button)?;
            }
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
            b"SCROLLBAR" => {
                let vertical = style & 1 != 0;
                let length = if vertical { height } else { width } as i32;
                let thickness = if vertical { width } else { height } as i32;
                let (minimum, maximum, position) = self.scrollbars.get(&(window, 2)).copied().unwrap_or((0, 100, 0));
                let travel = (length - thickness * 3).max(0);
                let offset = if maximum <= minimum { 0 } else { ((i64::from(position) - i64::from(minimum)) * i64::from(travel) / (i64::from(maximum) - i64::from(minimum))) as i32 };
                let thumb = if vertical { Rect { left: 1, top: thickness + offset, right: thickness - 1, bottom: thickness * 2 + offset } }
                    else { Rect { left: thickness + offset, top: 1, right: thickness * 2 + offset, bottom: thickness - 1 } };
                self.gdi.frame_surface(window, thumb, 0x808080).map_err(|e| format!("scrollbar thumb: {e:?}"))?;
            }
            b"STATIC" => match style & 15 {
                3 => {
                    let icon = self.static_icons.get(&window).ok_or("static icon image missing")?;
                    let mut rgba = self.gdi.surface(window).ok_or("static icon surface missing")?.rgba.clone();
                    for y in 0..height.min(icon.height) {
                        for x in 0..width.min(icon.width) {
                            let pixel = icon.pixels[(y * icon.width + x) as usize];
                            let at = ((y * width + x) * 4) as usize;
                            for channel in 0..3 { rgba[at + channel] = (rgba[at + channel] & pixel[3]) ^ pixel[channel]; }
                        }
                    }
                    self.gdi.present_rgba(window, &rgba).map_err(|e| format!("static icon drawing: {e:?}"))?;
                }
                4..=6 => self.gdi.fill_surface(
                    window,
                    Rect { left: 0, top: 0, right: width as i32, bottom: height as i32 },
                    match style & 15 { 4 => 0, 5 => 0x808080, _ => 0xffffff },
                ).map_err(|e| format!("static rectangle: {e:?}"))?,
                7..=9 => self
                    .gdi
                    .frame_surface(
                        window,
                        Rect {
                            left: 0,
                            top: 0,
                            right: width as i32,
                            bottom: height as i32,
                        },
                        match style & 15 { 7 => 0, 8 => 0x808080, _ => 0xffffff },
                    )
                    .map_err(|e| format!("static frame: {e:?}"))?,
                0..=2 | 11 | 12 => {
                    let lines =
                        crate::font::wrap_lines(&text, width, !matches!(style & 15, 11 | 12));
                    let color = if style & 0x08000000 == 0 { 0 } else { 0x808080 };
                    for (index, line) in lines.iter().enumerate() {
                        let text_width = crate::font::extent(line).0;
                        let x = match style & 15 {
                            1 => (width as i32 - text_width as i32) / 2,
                            2 => width as i32 - text_width as i32,
                            _ => 0,
                        };
                        let y = if style & 0x200 != 0 && lines.len() == 1 {
                            (height as i32 - 14) / 2
                        } else {
                            index as i32 * 14
                        };
                        if y >= height as i32 {
                            break;
                        }
                        self.gdi
                            .draw_default_text(window, x.max(0), y, text_width, line, color)
                            .map_err(|e| format!("static text: {e:?}"))?;
                    }
                }
                style => return Err(format!("unsupported STATIC style {style}")),
            },
            _ => {
                return Err(format!(
                    "unsupported dialog child class {}",
                    String::from_utf8_lossy(&class)
                ))
            }
        }
        if parent != 0 && self.gdi.surface(parent).is_some() {
            self.gdi
                .composite(parent, window, x, y)
                .map_err(|e| format!("static composition: {e:?}"))?;
        }
        Ok(())
    }
    /// Resolve child control paint callbacks to their containing application
    /// surface. Owned top-level dialogs remain separate presentation targets.
    pub fn presentation_window(&self, mut window: u32) -> u32 {
        for _ in 0..64 {
            let Ok(w) = self.windows.get(window) else {
                // A dialog can be destroyed while the guest is blocked inside
                // its modal pump. The runner's last presented HWND is then
                // stale; resume presentation at the visible application root.
                return self.windows.foreground_window();
            };
            let style = self
                .window_data
                .get(&window)
                .map_or(w.style, |data| data.style);
            if style & 0x40000000 == 0 || w.parent == 0 {
                return window;
            }
            window = w.parent;
        }
        0
    }
    pub fn compose_children(&mut self, parent: u32) -> Result<(), String> {
        let children = self
            .windows
            .children(parent)
            .into_iter()
            .filter_map(|handle| {
                let data = self.window_data.get(&handle)?;
                (data.style & 0x40000000 != 0
                    && self.gdi.surface(handle).is_some()
                    && self.windows.get(handle).is_ok_and(|window| window.visible))
                .then_some((handle, data.x, data.y))
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
        let owner = self.window_data.get(&window).map_or(0, |data| data.parent);
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
        if owner != 0 && self.gdi.surface(owner).is_some() {
            self.gdi
                .mark_surface_dirty(owner)
                .map_err(|e| format!("invalidate dialog owner surface: {e:?}"))?;
        }
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
        } else if self.windows.peek(false).is_some() {
            Ok(Effect::Retry)
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

    /// `CreateDIBitmap`. The created DDB takes the depth of the DC's selected
    /// bitmap (or the display depth for a device DC); `CBM_INIT` converts the
    /// source DIB through the raster backend.
    fn create_dib_bitmap(
        &mut self,
        memory: &mut dyn MemoryAccess,
        hdc: u32,
        header: u32,
        init_flags: u32,
        source_bits: u32,
        source_info: u32,
        usage: u32,
    ) -> Result<u32, String> {
        const CBM_INIT: u32 = 4;
        if header == 0 {
            return Ok(0);
        }
        let mut raw = [0u8; 40];
        memory
            .read(header, &mut raw)
            .map_err(|e| format!("CreateDIBitmap header: {e:?}"))?;
        let word = |offset: usize| u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
        let width = word(4) as i32;
        let height = word(8) as i32;
        let planes = u16::from_le_bytes([raw[12], raw[13]]);
        let bit_count = u16::from_le_bytes([raw[14], raw[15]]);
        let compression = word(16);
        if word(0) != 40
            || height == i32::MIN
            || compression == 4
            || compression == 5
            || usage > 2
        {
            return Ok(0);
        }
        let _ = planes;
        if width < 0 || height < 0 {
            return Ok(0);
        }
        if width == 0 || height == 0 {
            // Windows still hands out a handle for a zero-sized header; the
            // result is a placeholder that can be deleted.
            let bits = self.allocate(memory, 4)?;
            fill_guest(memory, bits, 0, 4)?;
            let bitmap = self
                .gdi
                .bitmap_pixels(memory, 1, 1, bits, 1)
                .map_err(|e| format!("CreateDIBitmap placeholder: {e:?}"))?;
            self.bitmap_allocations.insert(bitmap, bits);
            return Ok(bitmap);
        }
        let rows = height.unsigned_abs();
        let depth = if hdc == 0 {
            1
        } else {
            self.gdi
                .dc_bitmap_depth(hdc)
                .unwrap_or(self.display_bits as u16)
        };
        let stride = ((width as u64 * depth as u64 + 31) / 32 * 4) as u32;
        let size = stride
            .checked_mul(rows)
            .ok_or("CreateDIBitmap size overflow")?;
        let bits = self.allocate(memory, size.max(1))?;
        fill_guest(memory, bits, 0, size)?;
        if init_flags & CBM_INIT != 0 {
            if source_bits == 0 || source_info == 0 {
                return Ok(0);
            }
            if usage != 0 {
                return Err("CreateDIBitmap DIB_PAL_COLORS is not implemented".into());
            }
            if matches!(bit_count, 1 | 4 | 8 | 16 | 24 | 32) {
                if let Ok(source) = crate::raster::Dib::from_header(memory, source_info, source_bits) {
                    let target = crate::raster::Dib::new(memory, width as u32, rows, bits, depth)
                        .map_err(|e| format!("CreateDIBitmap target: {e:?}"))?;
                    let mut row = vec![0u8; width as usize * 4];
                    for y in 0..rows {
                        if source.rgba_row(memory, 0, y, &mut row).is_err() {
                            break;
                        }
                        target
                            .write_rgba_row(memory, 0, y, &row)
                            .map_err(|e| format!("CreateDIBitmap target row: {e:?}"))?;
                    }
                }
            }
        }
        let bitmap = self
            .gdi
            .bitmap_pixels(memory, width as u32, rows, bits, depth)
            .map_err(|e| format!("CreateDIBitmap object: {e:?}"))?;
        self.bitmap_allocations.insert(bitmap, bits);
        Ok(bitmap)
    }
    fn set_window_enabled(&mut self, window: u32, enabled: bool) -> Result<(), String> {
        self.windows
            .set_enabled(window, enabled)
            .map_err(|e| format!("EnableWindow: {e:?}"))?;
        if let Some(data) = self.window_data.get_mut(&window) {
            if enabled {
                data.style &= !0x08000000;
            } else {
                data.style |= 0x08000000;
            }
        }
        if !enabled {
            let mut focused = self.focused_window;
            for _ in 0..64 {
                if focused == 0 {
                    break;
                }
                if focused == window {
                    self.focused_window = 0;
                    break;
                }
                focused = self.windows.get(focused).map_or(0, |w| w.parent);
            }
        }
        Ok(())
    }
    /// Resolve palette-indexed and DIB-indexed COLORREF forms for pixel APIs.
    fn resolve_color(&self, dc: u32, color: u32) -> u32 {
        match color >> 24 {
            0 => color,
            // DIBINDEX(n): index into the selected DIB's color table.
            0x10 => {
                let index = (color & 0xffff) as usize;
                self.gdi.dib_index_color(dc, index).unwrap_or(0)
            }
            // PALETTEINDEX(n): logical-palette entry, then nearest DIB colour.
            0x01 => {
                let index = (color & 0xffff) as usize;
                let palette = self
                    .gdi
                    .dc_state(dc)
                    .ok()
                    .and_then(|state| state.palette)
                    .and_then(|palette| self.palettes.get(palette))
                    .or_else(|| {
                        self.default_palette
                            .and_then(|palette| self.palettes.get(palette))
                    });
                let fallback = {
                    let rgb = DEFAULT_PALETTE_COLORS[0];
                    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255]
                };
                let entry = palette
                    .filter(|entries| !entries.is_empty())
                    .map(|entries| entries.get(index).copied().unwrap_or(entries[0]))
                    .unwrap_or(fallback);
                let rgb =
                    u32::from(entry[0]) | (u32::from(entry[1]) << 8) | (u32::from(entry[2]) << 16);
                self.gdi.nearest_color(dc, rgb).unwrap_or(rgb)
            }
            // PALETTERGB(r,g,b): explicit colour, snapped to the DIB palette.
            0x02 => {
                let rgb = color & 0xffffff;
                self.gdi.nearest_color(dc, rgb).unwrap_or(rgb)
            }
            _ => color & 0xffffff,
        }
    }
    fn action(&mut self, action: Action, finish: Finish) -> Result<Effect, String> {
        if let Action::Invoke { args, .. } = &action {
            if self
                .windows
                .get(args[0])
                .is_ok_and(|window| window.procedure == DEFAULT_WINDOW_PROC)
            {
                return self.action(
                    Action::Return(crate::window::Windows::default_result(args[1])),
                    finish,
                );
            }
        }
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
                        self.properties.retain(|(handle, _), _| *handle != window);
                        self.window_data.remove(&window);
                        self.gdi
                            .remove_surface(window)
                            .map_err(|e| format!("{e:?}"))?;
                    }
                    Finish::Redraw { window, dc } => {
                        self.gdi
                            .delete_dc(dc)
                            .map_err(|e| format!("redraw DC: {e:?}"))?;
                        self.common_dcs.remove(&dc);
                        let action = self
                            .windows
                            .send(Message {
                                window,
                                message: 15,
                                wparam: 0,
                                lparam: 0,
                            })
                            .map_err(|e| format!("redraw paint: {e:?}"))?;
                        return self.action(action, Finish::Update);
                    }
                    Finish::RadioChecks { remaining, checked } => {
                        return self.radio_checks(remaining, checked)
                    }
                    Finish::ModalControl(key) => return self.finish_modal(key),
                    Finish::FixedReturn(value) => return Ok(Effect::Return(value)),
                    Finish::DisableAfterCancel {
                        window,
                        previous_disabled,
                    } => {
                        self.set_window_enabled(window, false)?;
                        let action = self
                            .windows
                            .send(Message {
                                window,
                                message: 10,
                                wparam: 0,
                                lparam: 0,
                            })
                            .map_err(|e| format!("EnableWindow notification: {e:?}"))?;
                        return self.action(action, Finish::FixedReturn(previous_disabled));
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
        let storage_size = 56usize.checked_add(class.len()).and_then(|n| n.checked_add(title.len())).and_then(|n| n.checked_add(2)).ok_or("dialog child strings overflow")?;
        let storage = self.allocate(memory, u32::try_from(storage_size).map_err(|_| "dialog child strings too large")?)?;
        let class_ptr = storage + 56;
        let title_ptr = class_ptr + class.len() as u32 + 1;
        let cbt = storage + 48;
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
        if procedure == 0xffff_ff02
            || procedure == 0xffff_ff01
            || procedure == DEFAULT_WINDOW_PROC
        {
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
                CallbackFinish::EnumMonitors { rect } => {
                    self.arena.release(rect)?;
                    Ok(Effect::Return((value != 0) as u32))
                }
                CallbackFinish::FixedReturn(value) => Ok(Effect::Return(value)),
                CallbackFinish::InitTerm { mut remaining } => {
                    if remaining.is_empty() {
                        Ok(Effect::Return(0))
                    } else {
                        let procedure = remaining.remove(0);
                        self.callback(
                            procedure,
                            Vec::new(),
                            None,
                            CallbackFinish::InitTerm { remaining },
                        )
                    }
                }
                CallbackFinish::ExitHandlers {
                    mut remaining,
                    code,
                } => {
                    if remaining.is_empty() {
                        Ok(Effect::Exit(code))
                    } else {
                        let procedure = remaining.remove(0);
                        self.callback(
                            procedure,
                            Vec::new(),
                            None,
                            CallbackFinish::ExitHandlers { remaining, code },
                        )
                    }
                }
                CallbackFinish::DialogInit(window) => {
                    if let Some(key) = self.modal_windows.get(&window).copied() {
                        self.windows
                            .show(window, true)
                            .map_err(|e| format!("show modal dialog: {e:?}"))?;
                        if let Some(data) = self.window_data.get_mut(&window) {
                            data.style |= 0x10000000;
                        }
                        self.finish_modal(key)
                    } else {
                        Ok(Effect::Return(window))
                    }
                }
                CallbackFinish::DialogBoxMessage { key, default_paint } => {
                    if value == 0 {
                        if let Some(window) = default_paint {
                            self.windows.validate_paint(window);
                            self.compose_children(window)?;
                        }
                    }
                    self.finish_modal(key)
                }
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
                            self.gdi
                                .remove_surface(window)
                                .map_err(|e| format!("cancel window surface: {e:?}"))?;
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
    /// Access a word in class-defined window storage without a DWORD overread.
    pub fn window_extra_word(&mut self, window: u32, offset: u16, value: Option<u16>) -> Result<u16, String> {
        let data = self.window_data.get_mut(&window).ok_or("invalid extra-word window")?;
        let start = usize::from(offset);
        let bytes = data.extra.get_mut(start..start + 2).ok_or("window extra word out of bounds")?;
        let old = u16::from_le_bytes([bytes[0], bytes[1]]);
        if let Some(value) = value { bytes.copy_from_slice(&value.to_le_bytes()); }
        Ok(old)
    }
    pub fn window_class(&self, window: u32) -> Option<&[u8]> {
        self.window_data.get(&window).map(|data| data.class.as_slice())
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
        let library = if is_crt_library(library) {
            "msvcrt"
        } else {
            library
        };
        let signature = wlam_pcode::abi::signature(library, name)
            .ok_or_else(|| format!("unsupported import {dll}!{name}"))?;
        let variadic_wsprintf = library == "user32" && name == "wsprintfA";
        if (!variadic_wsprintf && args.len() != signature.argument_bytes as usize / 4)
            || (variadic_wsprintf && args.len() < 2)
        {
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
            let result = self
                .opengl
                .call(context, window, name, args, memory, &self.gdi)?;
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
                let output = format_wsprintf(&string(memory, a(1))?, &args[2..], memory)?;
                let end = a(0)
                    .checked_add(output.len() as u32 + 1)
                    .ok_or("wsprintf buffer overflow")?;
                if end as u64 > memory.byte_len() {
                    return Err("wsprintf output outside guest memory".into());
                }
                let mut bytes = output.clone();
                bytes.push(0);
                memory
                    .write(a(0), &bytes)
                    .map_err(|e| format!("wsprintf output: {e:?}"))?;
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
            ("kernel32", "lstrlenW") => {
                if a(0) == 0 {
                    0
                } else {
                    read_wide_units(memory, a(0))?.len() as u32
                }
            }
            ("kernel32", "lstrcpyW" | "lstrcatW") => {
                let mut units = if name == "lstrcatW" {
                    read_wide_units(memory, a(0))?
                } else {
                    Vec::new()
                };
                units.extend(read_wide_units(memory, a(1))?);
                units.push(0);
                let bytes: Vec<u8> = units
                    .iter()
                    .flat_map(|unit| unit.to_le_bytes())
                    .collect();
                memory
                    .write(a(0), &bytes)
                    .map_err(|e| format!("wide string copy: {e:?}"))?;
                a(0)
            }
            ("user32", "GetPropA" | "SetPropA" | "RemovePropA") => {
                if a(1) == 0 || self.windows.get(a(0)).is_err() {
                    0
                } else {
                    let key = if a(1) <= 0xffff {
                        PropertyName::Atom(a(1) as u16)
                    } else {
                        PropertyName::String(string(memory, a(1))?)
                    };
                    match name {
                        "GetPropA" => self.properties.get(&(a(0), key)).copied().unwrap_or(0),
                        "SetPropA" => {
                            self.properties.insert((a(0), key), a(2));
                            1
                        }
                        _ => self.properties.remove(&(a(0), key)).unwrap_or(0),
                    }
                }
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
            ("user32", "GetDlgCtrlID") => self.windows.get(a(0)).map_or(0, |window| window.id),
            ("user32", "GetForegroundWindow") => self.windows.foreground_window(),
            ("user32", "GetFocus") => self.focused_window,
            ("user32", "SetFocus") => {
                if a(0) != 0 && !self.window_data.contains_key(&a(0)) {
                    self.thread.last_error = 1400;
                    0
                } else {
                    std::mem::replace(&mut self.focused_window, a(0))
                }
            }
            ("user32", "GetMenu") => {
                if !self.window_data.contains_key(&a(0)) {
                    return Err("GetMenu: invalid window".into());
                }
                self.menus.get(&a(0)).copied().unwrap_or(0)
            }
            ("user32", "SetMenu") => {
                if !self.window_data.contains_key(&a(0)) {
                    return Err("SetMenu: invalid window".into());
                }
                if a(1) == 0 {
                    self.menus.remove(&a(0));
                } else {
                    self.menus.insert(a(0), a(1));
                }
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
                    return self.control_effect(a(1), a(2), a(3), a(4), memory);
                }
                if a(0) == 0xffff_ff01 || a(0) == DEFAULT_WINDOW_PROC {
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
            ("kernel32", "FindResourceA") => {
                if a(1) > 0xffff || a(2) > 0xffff {
                    return Err("named resource lookup is not implemented".into());
                }
                self.resources.find(a(2), a(1)).unwrap_or_else(|| {
                    self.thread.last_error = 1814;
                    0
                })
            }
            ("kernel32", "LoadResource") => {
                if self.resources.get(a(1)).is_some() {
                    a(1)
                } else {
                    self.thread.last_error = 6;
                    0
                }
            }
            ("kernel32", "LockResource") => self.resources.get(a(0)).map_or(0, |r| r.address),
            ("kernel32", "SizeofResource") => self.resources.get(a(1)).map_or(0, |r| r.size),
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
                if a(0) != 0 && a(0) != self.image_base {
                    return Err("LoadMenuA: unsupported module".into());
                }
                if a(1) > 0xffff {
                    return Err("LoadMenuA: named menu resources are unsupported".into());
                }
                self.resources.named(4, a(1)).map_or_else(
                    || {
                        self.thread.last_error = 1813;
                        0
                    },
                    |resource| resource.address,
                )
            }
            ("user32", "LoadAcceleratorsA") => {
                if a(0) != 0 && a(0) != self.image_base {
                    return Err("LoadAcceleratorsA: unsupported module".into());
                }
                if a(1) > 0xffff {
                    return Err(
                        "LoadAcceleratorsA: named accelerator resources are unsupported".into(),
                    );
                }
                self.resources.named(9, a(1)).map_or_else(
                    || {
                        self.thread.last_error = 1813;
                        0
                    },
                    |resource| resource.address,
                )
            }
            ("user32", "TranslateAcceleratorA") => {
                let message = read(memory, a(2) + 4)?;
                if !matches!(message, 0x100 | 0x104 | 0x102 | 0x106) {
                    return Ok(Effect::Return(0));
                }
                let key = read(memory, a(2) + 8)?;
                let table = self
                    .resources
                    .at_address(9, a(1))
                    .ok_or("TranslateAcceleratorA: invalid accelerator table")?;
                if table.size % 8 != 0 {
                    return Err("malformed accelerator table".into());
                }
                let modifiers = [(16, 4), (17, 8), (18, 16)]
                    .into_iter()
                    .fold(0, |flags, (key, flag)| {
                        flags | if self.keys[key] & 0x80 != 0 { flag } else { 0 }
                    });
                for offset in (0..table.size).step_by(8) {
                    let entry = table
                        .address
                        .checked_add(offset)
                        .ok_or("accelerator address overflow")?;
                    let flags_key = read(memory, entry)?;
                    let flags = flags_key & 0xffff;
                    let virtual_key = flags & 1 != 0;
                    if flags_key >> 16 == key
                        && flags & 0x1c == modifiers
                        && virtual_key == matches!(message, 0x100 | 0x104)
                    {
                        let command = read(memory, entry + 4)? & 0xffff;
                        let action = self
                            .windows
                            .send(Message {
                                window: a(0),
                                message: 0x111,
                                wparam: command | 0x10000,
                                lparam: 0,
                            })
                            .map_err(|e| format!("accelerator dispatch: {e:?}"))?;
                        return self.action(action, Finish::Accelerator);
                    }
                    if flags & 0x80 != 0 {
                        break;
                    }
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
            ("user32", "LoadCursorA") if a(0) == self.image_base && a(1) <= 0xffff => {
                if let Some(handle) = self.resource_cursors.get(&(a(0), a(1))) { *handle }
                else {
                    let group = self.resources.named(12, a(1)).ok_or("cursor group resource missing")?;
                    let mut header = [0; 6];
                    memory.read(group.address, &mut header).map_err(|e| format!("cursor group header: {e:?}"))?;
                    let count = u16::from_le_bytes([header[4], header[5]]) as u32;
                    if count == 0 || 6 + count * 14 > group.size { return Err("invalid cursor group".into()); }
                    let mut selected = None;
                    for i in 0..count {
                        let mut entry = [0; 14];
                        memory.read(group.address + 6 + i * 14, &mut entry).map_err(|e| format!("cursor group entry: {e:?}"))?;
                        let id = u16::from_le_bytes([entry[12], entry[13]]) as u32;
                        if selected.is_none() { selected = self.resources.named(1, id); }
                    }
                    let source = selected.ok_or("cursor image resource missing")?;
                    let mut bytes = vec![0; source.size as usize];
                    memory.read(source.address, &mut bytes).map_err(|e| format!("cursor image: {e:?}"))?;
                    if bytes.len() < 4 { return Err("truncated cursor hotspot".into()); }
                    let x = u16::from_le_bytes([bytes[0], bytes[1]]);
                    let y = u16::from_le_bytes([bytes[2], bytes[3]]);
                    let icon = crate::icon::decode(&bytes[4..])?;
                    if u32::from(x) >= icon.width || u32::from(y) >= icon.height { return Err("cursor hotspot outside image".into()); }
                    let index = u16::try_from(self.resource_cursors.len() + 1).map_err(|_| "cursor handles exhausted")?;
                    let handle = 0x60020000 | u32::from(index);
                    self.cursors.insert(handle, a(1));
                    self.cursor_images.insert(handle, (x, y, icon));
                    self.resource_cursors.insert((a(0), a(1)), handle);
                    handle
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
            ("user32", "SetScrollRange" | "GetScrollRange" | "SetScrollPos" | "GetScrollPos") => {
                self.windows.get(a(0)).map_err(|e| format!("scrollbar window: {e:?}"))?;
                if a(1) > 2 { return Err("invalid scrollbar selector".into()); }
                let state = self.scrollbars.entry((a(0), a(1))).or_insert((0, 100, 0));
                match name {
                    "SetScrollRange" => {
                        let (minimum, maximum) = (a(2) as i32, a(3) as i32);
                        if minimum > maximum { self.thread.last_error = 87; 0 }
                        else { state.0 = minimum; state.1 = maximum; state.2 = state.2.clamp(minimum, maximum); 1 }
                    }
                    "GetScrollRange" => {
                        write(memory, a(2), state.0 as u32)?;
                        write(memory, a(3), state.1 as u32)?;
                        1
                    }
                    "SetScrollPos" => {
                        let previous = state.2;
                        state.2 = (a(2) as i32).clamp(state.0, state.1);
                        previous as u32
                    }
                    _ => state.2 as u32,
                }
            }
            ("user32", "ShowCursor") => {
                self.cursor_display_count = self.cursor_display_count.saturating_add(if a(0) != 0 { 1 } else { -1 });
                self.cursor_display_count as u32
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
            ("gdi32", "CreatePenIndirect") => {
                let style = read(memory, a(0))?;
                let width = read(memory, a(0).checked_add(4).ok_or("LOGPEN overflow")?)?;
                let color = read(memory, a(0).checked_add(12).ok_or("LOGPEN overflow")?)?;
                self.gdi
                    .pen(style, width, color)
                    .map_err(|e| format!("pen: {e:?}"))?
            }
            ("gdi32", "ExtCreatePen") => {
                let style = a(0);
                let width = a(1);
                let brush = a(2);
                let count = a(3);
                let user_style_pointer = a(4);
                if (count as i32) < 0 {
                    // Wine's suite expects a negative count to fail without
                    // changing the caller's last-error value.
                    return Ok(Effect::Return(0));
                }
                if count > 16 || (count > 0 && user_style_pointer == 0) {
                    self.thread.last_error = 87;
                    return Ok(Effect::Return(0));
                }
                let brush_style = read(memory, brush)?;
                let color = read(memory, brush.checked_add(4).ok_or("LOGBRUSH overflow")?)?;
                let hatch = read(memory, brush.checked_add(8).ok_or("LOGBRUSH overflow")?)?;
                let mut user_style = Vec::with_capacity(count as usize);
                for index in 0..count {
                    let at = user_style_pointer
                        .checked_add(index * 4)
                        .ok_or("user style overflow")?;
                    user_style.push(read(memory, at)?);
                }
                match self.gdi.ext_pen(style, width, brush_style, color, hatch, &user_style) {
                    Ok(pen) => pen,
                    Err(_) => {
                        self.thread.last_error = 87;
                        0
                    }
                }
            }
            ("gdi32", "CreateFontIndirectA") => {
                let mut logfont = [0; 60];
                memory
                    .read(a(0), &mut logfont)
                    .map_err(|e| format!("LOGFONTA: {e:?}"))?;
                let height = i32::from_le_bytes(logfont[0..4].try_into().unwrap());
                let width = i32::from_le_bytes(logfont[4..8].try_into().unwrap());
                let weight = u32::from_le_bytes(logfont[16..20].try_into().unwrap());
                let face_end = logfont[28..60].iter().position(|byte| *byte == 0).unwrap_or(32);
                self.gdi
                    .font(height, width, weight, logfont[20] != 0, &logfont[28..28 + face_end])
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
                let entries = match self.palettes.get(a(1)) {
                    Some(entries) => entries.clone(),
                    None => return Err("invalid DC or palette handle".into()),
                };
                self.gdi.set_palette_colors(a(1), entries);
                let Effect::Return(default) =
                    self.call("gdi32", "GetStockObject", &[15], memory)?
                else {
                    unreachable!()
                };
                self.gdi
                    .dc_state_mut(a(0))
                    .map_err(|e| format!("palette DC: {e:?}"))?
                    .palette
                    .replace(a(1))
                    .unwrap_or(default)
            }
            ("gdi32", "RealizePalette") => {
                if !self.gdi.is_dc(a(0)) {
                    return Err("invalid palette DC".into());
                }
                if self.display_bits != 8 {
                    return Ok(Effect::Return(0));
                }
                let Some(handle) = self
                    .gdi
                    .dc_state(a(0))
                    .map_err(|e| format!("palette DC: {e:?}"))?
                    .palette
                else {
                    return Ok(Effect::Return(0));
                };
                let entries = self
                    .palettes
                    .get(handle)
                    .ok_or("invalid selected palette")?;
                let mut used = [false; 256];
                let mut changed = 0;
                for entry in entries {
                    // PC_EXPLICIT entries name a physical palette index rather
                    // than an RGB color. Static system colors stay reserved.
                    if entry[3] & 2 != 0 {
                        continue;
                    }
                    if entry[3] & 5 == 0 {
                        if let Some(index) = self
                            .system_palette
                            .iter()
                            .position(|color| color[..3] == entry[..3])
                        {
                            used[index] = true;
                            continue;
                        }
                    }
                    let Some(next) = (10..246).find(|index| !used[*index]) else {
                        break;
                    };
                    let color = [entry[0], entry[1], entry[2], 0];
                    if self.system_palette[next] != color {
                        changed += 1;
                    }
                    self.system_palette[next] = color;
                    used[next] = true;
                }
                if changed != 0 {
                    self.gdi.mark_all_surfaces_dirty();
                }
                changed
            }
            ("gdi32", "GetPaletteEntries") => {
                let entries = self.palettes.get(a(0)).ok_or("invalid palette handle")?;
                let start = a(1) as usize;
                if start >= entries.len() {
                    return Ok(Effect::Return(0));
                }
                let count = (a(2) as usize).min(entries.len() - start);
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
                if start >= entries.len() {
                    return Ok(Effect::Return(0));
                }
                let count = (a(2) as usize).min(entries.len() - start);
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
                if self.display_bits != 8 {
                    return Ok(Effect::Return(0));
                }
                if a(3) == 0 {
                    return Ok(Effect::Return(256));
                }
                let start = (a(1) as usize).min(256);
                let count = (a(2) as usize).min(256 - start);
                let bytes: Vec<u8> = self.system_palette[start..start + count]
                    .iter()
                    .flatten()
                    .copied()
                    .collect();
                memory
                    .write(a(3), &bytes)
                    .map_err(|e| format!("system palette output: {e:?}"))?;
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
            ("gdi32", "CreateHatchBrush") => self
                .gdi
                .hatch_brush(a(0), a(1))
                .unwrap_or(0),
            ("gdi32", "CreatePatternBrush") => self
                .gdi
                .pattern_brush(memory, a(0))
                .unwrap_or(0),
            ("gdi32", "CreateDIBPatternBrushPt") => {
                let usage = a(1);
                if usage > 2 {
                    return Ok(Effect::Return(0));
                }
                let size = read(memory, a(0))?;
                let planes = (read(memory, a(0).checked_add(12).ok_or("BITMAPINFO overflow")?)? & 0xffff) as u16;
                let bits = (read(memory, a(0).checked_add(14).ok_or("BITMAPINFO overflow")?)? & 0xffff) as u16;
                let compression = read(memory, a(0).checked_add(16).ok_or("BITMAPINFO overflow")?)?;
                if size != 40 || planes != 1 || !matches!(bits, 1 | 8 | 32) || compression != 0 {
                    self.thread.last_error = 87;
                    return Ok(Effect::Return(0));
                }
                self.gdi
                    .dib_pattern_brush(memory, a(0), usage)
                    .unwrap_or(0)
            }
            ("gdi32" | "user32", "FillRect") => {
                let [left, top, right, bottom] = read_rect(memory, a(1))?;
                self.gdi
                    .fill_rect(memory, a(0), crate::gdi::Rect { left, top, right, bottom }, a(2))
                    .map_err(|e| format!("FillRect: {e:?}"))?;
                1
            }
            ("gdi32", "CreateBrushIndirect") => {
                let style = read(memory, a(0))?;
                let color = read(memory, a(0).checked_add(4).ok_or("LOGBRUSH overflow")?)?;
                let hatch = read(memory, a(0).checked_add(8).ok_or("LOGBRUSH overflow")?)?;
                self.gdi
                    .brush_indirect(memory, style, color, hatch)
                    .unwrap_or(0)
            }
            ("gdi32", "CreateSolidBrush") => self
                .gdi
                .solid_brush(a(0))
                .map_err(|e| format!("solid brush: {e:?}"))?,
            ("gdi32", "SaveDC") => {
                if !self.gdi.is_dc(a(0)) {
                    self.thread.last_error = 6;
                    0
                } else {
                    self.gdi
                        .save_dc(a(0))
                        .map_err(|e| format!("SaveDC: {e:?}"))?
                }
            }
            ("gdi32", "RestoreDC") => {
                if !self.gdi.is_dc(a(0)) {
                    self.thread.last_error = 6;
                    0
                } else {
                    self.gdi
                        .restore_dc(a(0), a(1) as i32)
                        .map_err(|e| format!("RestoreDC: {e:?}"))? as u32
                }
            }
            ("gdi32", "SetMapMode") => {
                if !self.gdi.is_dc(a(0)) {
                    self.thread.last_error = 6;
                    return Ok(Effect::Return(0));
                }
                let mode = a(1);
                if !(1..=8).contains(&mode) {
                    return Ok(Effect::Return(0));
                }
                let previous = self
                    .gdi
                    .dc_state(a(0))
                    .map_err(|e| format!("SetMapMode: {e:?}"))?
                    .map_mode;
                self.gdi
                    .set_map_mode(a(0), mode)
                    .map_err(|e| format!("SetMapMode: {e:?}"))?;
                previous
            }
            (
                "gdi32",
                "GetViewportExtEx" | "GetViewportOrgEx" | "GetWindowExtEx" | "GetWindowOrgEx",
            ) => {
                let state = self
                    .gdi
                    .dc_state(a(0))
                    .map_err(|e| format!("{name}: {e:?}"))?;
                let (x, y) = match name {
                    "GetViewportExtEx" => state.viewport_ext,
                    "GetViewportOrgEx" => state.viewport_org,
                    "GetWindowExtEx" => state.window_ext,
                    _ => state.window_org,
                };
                if a(1) != 0 {
                    memory
                        .write(a(1), &(x as u32).to_le_bytes())
                        .map_err(|e| format!("{name} x: {e:?}"))?;
                    memory
                        .write(a(1) + 4, &(y as u32).to_le_bytes())
                        .map_err(|e| format!("{name} y: {e:?}"))?;
                }
                1
            }
            (
                "gdi32",
                "SetViewportExtEx" | "SetViewportOrgEx" | "SetWindowExtEx" | "SetWindowOrgEx",
            ) => {
                let state = self
                    .gdi
                    .dc_state(a(0))
                    .map_err(|e| format!("{name}: {e:?}"))?;
                let (old_x, old_y) = match name {
                    "SetViewportExtEx" => state.viewport_ext,
                    "SetViewportOrgEx" => state.viewport_org,
                    "SetWindowExtEx" => state.window_ext,
                    _ => state.window_org,
                };
                if a(3) != 0 {
                    memory
                        .write(a(3), &(old_x as u32).to_le_bytes())
                        .map_err(|e| format!("{name} old x: {e:?}"))?;
                    memory
                        .write(a(3) + 4, &(old_y as u32).to_le_bytes())
                        .map_err(|e| format!("{name} old y: {e:?}"))?;
                }
                let state = self
                    .gdi
                    .dc_state_mut(a(0))
                    .map_err(|e| format!("{name}: {e:?}"))?;
                let value = (a(1) as i32, a(2) as i32);
                match name {
                    "SetViewportExtEx" => state.viewport_ext = value,
                    "SetViewportOrgEx" => state.viewport_org = value,
                    "SetWindowExtEx" => state.window_ext = value,
                    _ => state.window_org = value,
                }
                1
            }
            ("gdi32", "OffsetViewportOrgEx" | "OffsetWindowOrgEx") => {
                let viewport = name.starts_with("OffsetViewport");
                let state = self
                    .gdi
                    .dc_state_mut(a(0))
                    .map_err(|e| format!("{name}: {e:?}"))?;
                let slot = if viewport {
                    &mut state.viewport_org
                } else {
                    &mut state.window_org
                };
                let previous = *slot;
                if a(3) != 0 {
                    memory
                        .write(a(3), &(previous.0 as u32).to_le_bytes())
                        .map_err(|e| format!("{name} old x: {e:?}"))?;
                    memory
                        .write(a(3) + 4, &(previous.1 as u32).to_le_bytes())
                        .map_err(|e| format!("{name} old y: {e:?}"))?;
                }
                slot.0 = slot.0.wrapping_add(a(1) as i32);
                slot.1 = slot.1.wrapping_add(a(2) as i32);
                1
            }
            ("gdi32", "SetLayout") => {
                let state = self
                    .gdi
                    .dc_state_mut(a(0))
                    .map_err(|e| format!("SetLayout: {e:?}"))?;
                let previous = state.layout;
                state.layout = a(1) & 0x0f;
                previous
            }
            ("gdi32", "GetLayout") => self
                .gdi
                .dc_state(a(0))
                .map_err(|e| format!("GetLayout: {e:?}"))?
                .layout,
            (
                "gdi32",
                "GetTextColor" | "GetBkColor" | "GetBkMode" | "GetTextAlign" | "GetStretchBltMode"
                | "GetROP2" | "SetTextColor" | "SetBkColor" | "SetBkMode" | "SetTextAlign"
                | "SetStretchBltMode" | "SetROP2" | "GetMapMode",
            ) => {
                if self.gdi.dc_state(a(0)).is_err() {
                    self.thread.last_error = 6; // ERROR_INVALID_HANDLE
                    return Ok(Effect::Return(if matches!(name, "GetBkColor" | "GetTextColor") {
                        0xffff_ffff // CLR_INVALID
                    } else {
                        0
                    }));
                }
                let state = self
                    .gdi
                    .dc_state_mut(a(0))
                    .map_err(|e| format!("DC attribute: {e:?}"))?;
                let value = if name.starts_with("Set") { a(1) } else { 0 };
                let slot = match name {
                    "GetTextColor" | "SetTextColor" => &mut state.text_color,
                    "GetBkColor" | "SetBkColor" => &mut state.background_color,
                    "GetBkMode" | "SetBkMode" => {
                        if name.starts_with("Set") && !matches!(value, 1 | 2) {
                            return Ok(Effect::Return(0));
                        }
                        &mut state.background_mode
                    }
                    "GetTextAlign" | "SetTextAlign" => {
                        if name.starts_with("Set")
                            && (value & !31 != 0
                                || !matches!(value & 6, 0 | 2 | 6)
                                || !matches!(value & 24, 0 | 8 | 24))
                        {
                            return Err("unsupported text alignment".into());
                        }
                        &mut state.text_align
                    }
                    "GetMapMode" => &mut state.map_mode,
                    "GetStretchBltMode" | "SetStretchBltMode" => {
                        if name.starts_with("Set") && !(1..=4).contains(&value) {
                            return Ok(Effect::Return(0));
                        }
                        &mut state.stretch_mode
                    }
                    _ => {
                        if name.starts_with("Set") && !(1..=16).contains(&value) {
                            return Ok(Effect::Return(0));
                        }
                        &mut state.rop2
                    }
                };
                if name.starts_with("Set") {
                    // COLORREF values round-trip verbatim, including
                    // palette-indexed forms and 0xffffffff.
                    std::mem::replace(slot, value)
                } else {
                    *slot
                }
            }
            ("gdi32", "GetMiterLimit") => {
                if self.gdi.dc_state(a(0)).is_err() {
                    self.thread.last_error = 6;
                    return Ok(Effect::Return(0));
                }
                let limit = self.gdi.dc_state(a(0)).map_err(|e| format!("GetMiterLimit: {e:?}"))?.miter_limit;
                memory
                    .write(a(1), &limit.to_bits().to_le_bytes())
                    .map_err(|e| format!("GetMiterLimit output: {e:?}"))?;
                1
            }
            ("gdi32", "SetMiterLimit") => {
                let limit = f32::from_bits(a(1));
                if self.gdi.dc_state(a(0)).is_err() {
                    self.thread.last_error = 6;
                    return Ok(Effect::Return(0));
                }
                if !(limit >= 1.0) || !limit.is_finite() {
                    return Ok(Effect::Return(0));
                }
                let previous = self
                    .gdi
                    .dc_state(a(0))
                    .map_err(|e| format!("SetMiterLimit: {e:?}"))?
                    .miter_limit;
                if a(2) != 0 {
                    memory
                        .write(a(2), &previous.to_bits().to_le_bytes())
                        .map_err(|e| format!("SetMiterLimit output: {e:?}"))?;
                }
                self.gdi
                    .dc_state_mut(a(0))
                    .map_err(|e| format!("SetMiterLimit: {e:?}"))?
                    .miter_limit = limit;
                1
            }
            ("gdi32", "CreateRectRgn") => {
                let rect = Rect {
                    left: a(0) as i32,
                    top: a(1) as i32,
                    right: a(2) as i32,
                    bottom: a(3) as i32,
                };
                self.gdi
                    .region(rect)
                    .map_err(|e| format!("CreateRectRgn: {e:?}"))?
            }
            ("gdi32", "CreateRectRgnIndirect") => {
                let [left, top, right, bottom] = read_rect(memory, a(0))?;
                self.gdi
                    .region(Rect { left, top, right, bottom })
                    .map_err(|e| format!("CreateRectRgnIndirect: {e:?}"))?
            }
            ("gdi32", "SetRectRgn") => {
                let rect = Rect {
                    left: a(1) as i32,
                    top: a(2) as i32,
                    right: a(3) as i32,
                    bottom: a(4) as i32,
                };
                self.gdi
                    .set_region(a(0), rect)
                    .map_err(|e| format!("SetRectRgn: {e:?}"))?;
                1
            }
            ("gdi32", "GetRgnBox") => {
                let (kind, rect) = self
                    .gdi
                    .region_box(a(0))
                    .map_err(|e| format!("GetRgnBox: {e:?}"))?;
                write_rect(memory, a(1), [rect.left, rect.top, rect.right, rect.bottom])?;
                kind
            }
            ("gdi32", "GetRegionData") => {
                let bytes = self
                    .gdi
                    .region_data(a(0))
                    .map_err(|e| format!("GetRegionData: {e:?}"))?;
                let needed = bytes.len() as u32;
                if a(2) != 0 && a(1) >= needed {
                    memory
                        .write(a(2), &bytes)
                        .map_err(|e| format!("GetRegionData output: {e:?}"))?;
                }
                needed
            }
            ("gdi32", "GetDeviceGammaRamp" | "SetDeviceGammaRamp") => 0,
            ("gdi32", "CreateEnhMetaFileA" | "CreateEnhMetaFileW") => self
                .gdi
                .meta_dc(true)
                .map_err(|e| format!("CreateEnhMetaFile: {e:?}"))?,
            ("gdi32", "CreateMetaFileA" | "CreateMetaFileW") => self
                .gdi
                .meta_dc(false)
                .map_err(|e| format!("CreateMetaFile: {e:?}"))?,
            ("gdi32", "CloseEnhMetaFile") => self
                .gdi
                .close_metafile(a(0), true)
                .map_err(|e| format!("CloseEnhMetaFile: {e:?} (type {:?})", self.gdi.object_type(a(0))))?,
            ("gdi32", "CloseMetaFile") => self
                .gdi
                .close_metafile(a(0), false)
                .map_err(|e| format!("CloseMetaFile: {e:?}"))?,
            ("gdi32", "DeleteEnhMetaFile" | "DeleteMetaFile") => {
                self.gdi
                    .delete_metafile(a(0))
                    .map_err(|e| format!("DeleteMetaFile: {e:?}"))?;
                1
            }
            ("gdi32", "ResetDCA" | "ResetDCW") => {
                if !self.gdi.is_dc(a(0)) || a(1) == 0 {
                    return Ok(Effect::Return(0));
                }
                // The only supported device is the display; resetting to the
                // current display mode leaves every capability unchanged.
                a(0)
            }
            ("gdi32", "SetBoundsRect") => {
                if self.gdi.is_meta_dc(a(0)) == Some(false) || !self.gdi.is_dc(a(0)) {
                    return Ok(Effect::Return(0));
                }
                if a(2) & 0x0004 != 0 && a(2) & 0x0008 != 0 {
                    return Ok(Effect::Return(0));
                }
                let rect = if a(1) == 0 {
                    None
                } else {
                    let [left, top, right, bottom] = read_rect(memory, a(1))?;
                    Some(Rect { left, top, right, bottom })
                };
                self.gdi
                    .set_bounds_rect(a(0), rect, a(2))
                    .map_err(|e| format!("SetBoundsRect: {e:?}"))?
            }
            ("gdi32", "GetBoundsRect") => {
                if self.gdi.is_meta_dc(a(0)) == Some(false) || !self.gdi.is_dc(a(0)) {
                    return Ok(Effect::Return(0));
                }
                if a(1) == 0 {
                    // A NULL rectangle still honours DCB_RESET.
                    let _ = self.gdi.get_bounds_rect(a(0), a(2));
                    return Ok(Effect::Return(0));
                }
                let (result, rect) = self
                    .gdi
                    .get_bounds_rect(a(0), a(2))
                    .map_err(|e| format!("GetBoundsRect: {e:?}"))?;
                if a(1) != 0 {
                    write_rect(memory, a(1), [rect.left, rect.top, rect.right, rect.bottom])?;
                }
                result
            }
            ("gdi32", "GetClipRgn") => self
                .gdi
                .get_clip_region(a(0), a(1))
                .map_err(|e| format!("GetClipRgn: {e:?}"))? as u32,
            ("gdi32", "SelectClipRgn") => self
                .gdi
                .select_clip_region(a(0), a(1))
                .map_err(|e| format!("SelectClipRgn: {e:?}"))?,
            ("gdi32", "IntersectClipRect") => self
                .gdi
                .intersect_clip(
                    a(0),
                    Rect {
                        left: a(1) as i32,
                        top: a(2) as i32,
                        right: a(3) as i32,
                        bottom: a(4) as i32,
                    },
                )
                .map_err(|e| format!("clip: {e:?}"))?,
            ("gdi32", "GetClipBox") => {
                if self.gdi.is_meta_dc(a(0)) == Some(false) {
                    return Ok(Effect::Return(0)); // ERROR
                }
                let clip = self
                    .gdi
                    .clip_bounds(a(0))
                    .map_err(|e| format!("clip: {e:?}"))?;
                write_rect(memory, a(1), [clip.left, clip.top, clip.right, clip.bottom])?;
                if clip.left >= clip.right || clip.top >= clip.bottom {
                    1
                } else {
                    2
                }
            }
            ("gdi32", "TextOutA" | "ExtTextOutA") => {
                let extended = name == "ExtTextOutA";
                let (pointer, count) = if extended { (a(5), a(6)) } else { (a(3), a(4)) };
                if count > 8192 {
                    return Err("text output exceeds 8192 bytes".into());
                }
                let mut text = vec![0; count as usize];
                memory
                    .read(pointer, &mut text)
                    .map_err(|e| format!("text input: {e:?}"))?;
                let options = if extended { a(3) } else { 0 };
                let rect = if extended && a(4) != 0 {
                    let [left, top, right, bottom] = read_rect(memory, a(4))?;
                    Some(Rect {
                        left,
                        top,
                        right,
                        bottom,
                    })
                } else {
                    None
                };
                let advances = if extended && a(7) != 0 {
                    Some(
                        (0..count)
                            .map(|i| {
                                read(
                                    memory,
                                    a(7).checked_add(i * 4).ok_or("text advance overflow")?,
                                )
                                .map(|v| v as i32)
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                    )
                } else {
                    None
                };
                self.gdi
                    .text_out(
                        memory,
                        a(0),
                        a(1) as i32,
                        a(2) as i32,
                        &text,
                        options,
                        rect,
                        advances.as_deref(),
                    )
                    .map_err(|e| format!("text output: {e:?}"))?;
                1
            }
            ("gdi32", "GetTextExtentPointA" | "GetTextExtentPoint32A") => {
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
                let extra = self.gdi.dc_state(a(0)).map_err(|e| format!("text extent state: {e:?}"))?.text_extra as i64 * text.len() as i64;
                let width = (width as i64 + extra).max(0) as u32;
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
            ("gdi32", "SetTextCharacterExtra") => {
                let state = self.gdi.dc_state_mut(a(0)).map_err(|e| format!("text extra state: {e:?}"))?;
                let previous = state.text_extra;
                state.text_extra = a(1) as i32;
                previous as u32
            }
            ("gdi32", "GetTextCharacterExtra") => self.gdi.dc_state(a(0)).map_err(|e| format!("text extra state: {e:?}"))?.text_extra as u32,
            ("gdi32", "SetPolyFillMode") => {
                if !matches!(a(1), 1 | 2) {
                    return Ok(Effect::Return(0));
                }
                let state = self.gdi.dc_state_mut(a(0)).map_err(|e| format!("poly fill state: {e:?}"))?;
                std::mem::replace(&mut state.poly_fill_mode, a(1))
            }
            ("gdi32", "GetPolyFillMode") => self.gdi.dc_state(a(0)).map_err(|e| format!("poly fill state: {e:?}"))?.poly_fill_mode,
            ("gdi32", "SetMapperFlags") => {
                let state = self.gdi.dc_state_mut(a(0)).map_err(|e| format!("mapper state: {e:?}"))?;
                std::mem::replace(&mut state.mapper_flags, a(1))
            }
            ("gdi32", "GetMapperFlags") => self.gdi.dc_state(a(0)).map_err(|e| format!("mapper state: {e:?}"))?.mapper_flags,
            ("gdi32", "GetTextFaceA") => {
                if !self.gdi.is_dc(a(0)) || a(1) == 0 || a(2) == 0 {
                    return Ok(Effect::Return(0));
                }
                let face = self.gdi.font_face(a(0)).unwrap_or_default();
                let count = face.len().min(a(1) as usize - 1);
                let mut bytes = face[..count].to_vec();
                bytes.push(0);
                memory
                    .write(a(2), &bytes)
                    .map_err(|e| format!("GetTextFaceA: {e:?}"))?;
                count as u32
            }
            ("gdi32", "GetCharWidthA") => {
                if a(1) > a(2) || a(2) - a(1) >= 0x10000 {
                    return Ok(Effect::Return(0));
                }
                let mut widths = Vec::with_capacity(((a(2) - a(1) + 1) as usize) * 4);
                for value in a(1)..=a(2) {
                    widths.extend_from_slice(
                        &self.gdi.char_width(a(0), value as u8).map_err(|e| format!("GetCharWidthA: {e:?}"))?.to_le_bytes(),
                    );
                }
                memory
                    .write(a(3), &widths)
                    .map_err(|e| format!("GetCharWidthA output: {e:?}"))?;
                1
            }
            ("gdi32", "GetNearestColor") => {
                if a(1) >> 24 == 0x10 {
                    // DIBINDEX names a colour-table entry directly.
                    self.gdi
                        .dib_index_color(a(0), (a(1) & 0xffff) as usize)
                        .unwrap_or(0)
                } else {
                    let color = self.resolve_color(a(0), a(1));
                    self.gdi.nearest_color(a(0), color).unwrap_or(u32::MAX)
                }
            }
            ("kernel32", "GlobalFlags") => {
                if a(0) != 0 && self.arena.size(a(0)).is_some() {
                    2
                } else {
                    0
                }
            }
            ("gdi32", "GetObjectType") => match self
                .gdi
                .object_type(a(0))
                .or_else(|| self.palettes.get(a(0)).map(|_| 5))
            {
                Some(kind) => kind,
                None => {
                    self.thread.last_error = 6;
                    0
                }
            },
            ("gdi32", "GetCurrentObject") => {
                let current = self.gdi.current_object(a(0), a(1)).unwrap_or(0);
                if current == 0 && a(1) == 5 {
                    let Effect::Return(palette) =
                        self.call("gdi32", "GetStockObject", &[15], memory)?
                    else {
                        unreachable!()
                    };
                    palette
                } else {
                    current
                }
            }
            ("gdi32", "GdiFlush") => 1,
            ("gdi32", "UnrealizeObject") => self.palettes.get(a(0)).is_some() as u32,
            ("gdi32", "GetObjectA" | "GetObjectW") => {
                if let Some(info) = self.gdi.object_info(a(0)) {
                    let mut bytes = Vec::new();
                    match info {
                        crate::gdi::ObjectInfo::Pen {
                            style,
                            width,
                            color,
                            extended,
                        } => {
                            if let Some(crate::gdi::ExtPenInfo::Fields {
                                style: ext_style,
                                width: ext_width,
                                brush_style,
                                color: ext_color,
                                hatch,
                                user_style,
                            }) = extended
                            {
                                let base = ext_style & 0xf;
                                if base == 5 {
                                    // PS_NULL historically reports LOGPEN for a
                                    // 16-byte buffer and EXTLOGPEN for exactly
                                    // sizeof(EXTLOGPEN).
                                    if a(1) == 0 {
                                        return Ok(Effect::Return(16));
                                    }
                                    if a(1) < 16 || a(2) < 0x1000 {
                                        return Ok(Effect::Return(0));
                                    }
                                    if a(1) == 28 {
                                        bytes.resize(28, 0);
                                        bytes[0..4].copy_from_slice(&ext_style.to_le_bytes());
                                        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
                                        bytes[8..12].copy_from_slice(&0u32.to_le_bytes());
                                        bytes[12..16].copy_from_slice(&0u32.to_le_bytes());
                                        bytes[16..20].copy_from_slice(&0u32.to_le_bytes());
                                        bytes[20..24].copy_from_slice(&0u32.to_le_bytes());
                                        memory
                                            .write(a(2), &bytes)
                                            .map_err(|e| format!("GetObjectA ext null pen: {e:?}"))?;
                                        return Ok(Effect::Return(28));
                                    }
                                    bytes.resize(16, 0);
                                    bytes[0..4].copy_from_slice(&5u32.to_le_bytes());
                                    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
                                    bytes[12..16].copy_from_slice(&0u32.to_le_bytes());
                                    memory
                                        .write(a(2), &bytes)
                                        .map_err(|e| format!("GetObjectA null pen: {e:?}"))?;
                                    return Ok(Effect::Return(16));
                                }
                                let size = 24 + user_style.len() * 4;
                                if a(1) == 0 {
                                    return Ok(Effect::Return(size as u32));
                                }
                                if a(1) < size as u32 || a(2) < 0x1000 {
                                    return Ok(Effect::Return(0));
                                }
                                bytes.resize(size, 0);
                                bytes[0..4].copy_from_slice(&ext_style.to_le_bytes());
                                bytes[4..8].copy_from_slice(&ext_width.to_le_bytes());
                                bytes[8..12].copy_from_slice(&brush_style.to_le_bytes());
                                bytes[12..16].copy_from_slice(&ext_color.to_le_bytes());
                                bytes[16..20].copy_from_slice(&hatch.to_le_bytes());
                                bytes[20..24].copy_from_slice(&(user_style.len() as u32).to_le_bytes());
                                for (index, entry) in user_style.iter().enumerate() {
                                    let at = 24 + index * 4;
                                    bytes[at..at + 4].copy_from_slice(&entry.to_le_bytes());
                                }
                                memory
                                    .write(a(2), &bytes)
                                    .map_err(|e| format!("GetObjectA ext pen: {e:?}"))?;
                                return Ok(Effect::Return(size as u32));
                            }
                            if style == 5 && a(1) == 28 {
                                if a(2) < 0x1000 {
                                    return Ok(Effect::Return(0));
                                }
                                let mut header = [0u8; 28];
                                header[0..4].copy_from_slice(&5u32.to_le_bytes());
                                memory
                                    .write(a(2), &header)
                                    .map_err(|e| format!("GetObjectA null ext pen: {e:?}"))?;
                                return Ok(Effect::Return(28));
                            }
                            bytes.resize(16, 0);
                            bytes[0..4].copy_from_slice(&style.to_le_bytes());
                            bytes[4..8].copy_from_slice(&width.to_le_bytes());
                            bytes[12..16].copy_from_slice(&color.to_le_bytes());
                        }
                        crate::gdi::ObjectInfo::Brush { style, color, hatch } => {
                            bytes.resize(12, 0);
                            bytes[0..4].copy_from_slice(&style.to_le_bytes());
                            bytes[4..8].copy_from_slice(&color.to_le_bytes());
                            bytes[8..12].copy_from_slice(&hatch.to_le_bytes());
                        }
                        crate::gdi::ObjectInfo::Font {
                            height,
                            width,
                            weight,
                            italic,
                            face,
                        } => {
                            bytes.resize(60, 0);
                            bytes[0..4].copy_from_slice(&height.to_le_bytes());
                            bytes[4..8].copy_from_slice(&width.to_le_bytes());
                            bytes[8..12].copy_from_slice(&0i32.to_le_bytes());
                            bytes[12..16].copy_from_slice(&0i32.to_le_bytes());
                            bytes[16..20].copy_from_slice(&weight.to_le_bytes());
                            bytes[20] = u8::from(italic);
                            let face = &face[..face.len().min(31)];
                            bytes[28..28 + face.len()].copy_from_slice(face);
                        }
                        crate::gdi::ObjectInfo::Bitmap {
                            width,
                            height,
                            stride,
                            bits,
                            bit_count,
                            section,
                        } => {
                            if section {
                                if a(1) == 0 {
                                    // A NULL buffer reports only the BITMAP
                                    // part; a non-NULL buffer with count 0 fails.
                                    return Ok(Effect::Return(if a(2) == 0 { 24 } else { 0 }));
                                }
                                if a(1) < 24 || a(2) < 0x1000 {
                                    return Ok(Effect::Return(0));
                                }
                                let size = if a(1) >= 84 { 84 } else { 24 };
                                let word_stride = ((width * bit_count as u32 + 15) >> 3) & !1;
                                let mut header = vec![0u8; size];
                                header[4..8].copy_from_slice(&width.to_le_bytes());
                                header[8..12].copy_from_slice(&height.to_le_bytes());
                                header[12..16].copy_from_slice(&word_stride.to_le_bytes());
                                header[16..18].copy_from_slice(&1u16.to_le_bytes());
                                header[18..20].copy_from_slice(&bit_count.to_le_bytes());
                                header[20..24].copy_from_slice(&bits.to_le_bytes());
                                if size == 84 {
                                    let info = self
                                        .gdi
                                        .bitmap_dib(a(0))
                                        .ok_or("GetObjectA DIBSECTION: bitmap gone")?;
                                    header[24..28].copy_from_slice(&40u32.to_le_bytes());
                                    header[28..32].copy_from_slice(&width.to_le_bytes());
                                    header[32..36].copy_from_slice(&height.to_le_bytes());
                                    header[36..38].copy_from_slice(&info.planes().to_le_bytes());
                                    header[38..40].copy_from_slice(&bit_count.to_le_bytes());
                                    header[40..44].copy_from_slice(&info.compression().to_le_bytes());
                                    header[44..48].copy_from_slice(&0u32.to_le_bytes()); // biSizeImage
                                    header[56..60].copy_from_slice(&info.clr_used().to_le_bytes());
                                }
                                memory
                                    .write(a(2), &header)
                                    .map_err(|e| format!("GetObjectA DIBSECTION: {e:?}"))?;
                                return Ok(Effect::Return(size as u32));
                            }
                            let word_stride = ((width * bit_count as u32 + 15) >> 3) & !1;
                            bytes.resize(24, 0);
                            bytes[4..8].copy_from_slice(&width.to_le_bytes());
                            bytes[8..12].copy_from_slice(&height.to_le_bytes());
                            bytes[12..16].copy_from_slice(&word_stride.to_le_bytes());
                            bytes[16..18].copy_from_slice(&1u16.to_le_bytes());
                            bytes[18..20].copy_from_slice(&bit_count.to_le_bytes());
                            // A DDB reports no bits pointer; a DIB section does.
                            bytes[20..24].copy_from_slice(&0u32.to_le_bytes());
                            if section {
                                // DIBSECTION: BITMAP followed by BITMAPINFOHEADER,
                                // three color masks, section handle and offset.
                                bytes[24..28].copy_from_slice(&40u32.to_le_bytes());
                                bytes[28..32].copy_from_slice(&width.to_le_bytes());
                                bytes[32..36].copy_from_slice(&height.to_le_bytes());
                                bytes[36..38].copy_from_slice(&1u16.to_le_bytes());
                                bytes[38..40].copy_from_slice(&bit_count.to_le_bytes());
                            }
                        }
                    }
                    if a(1) == 0 {
                        return Ok(Effect::Return(if a(2) == 0 {
                            bytes.len() as u32
                        } else {
                            0
                        }));
                    }
                    if a(1) < bytes.len() as u32 || a(2) < 0x1000 {
                        return Ok(Effect::Return(0));
                    }
                    memory
                        .write(a(2), &bytes)
                        .map_err(|e| format!("GetObjectA: {e:?}"))?;
                    return Ok(Effect::Return(bytes.len() as u32));
                }
                if let Some(palette) = self.palettes.get(a(0)) {
                    if a(1) < 2 || a(2) == 0 {
                        return Ok(Effect::Return(0));
                    }
                    let count = palette.len() as u16;
                    memory
                        .write(a(2), &count.to_le_bytes())
                        .map_err(|e| format!("GetObjectA palette: {e:?}"))?;
                    return Ok(Effect::Return(2));
                }
                0
            }
            ("gdi32", "CreateBitmap") => {
                let (width, height, planes, bits_per_pixel, data) =
                    (a(0), a(1), a(2), a(3), a(4));
                if width == 0
                    || height == 0
                    || width > 0x07ff_ffff
                    || height > 0x07ff_ffff
                    || planes != 1
                    || !matches!(bits_per_pixel, 1 | 2 | 4 | 8 | 16 | 24 | 32)
                {
                    self.thread.last_error = 87;
                    return Ok(Effect::Return(0));
                }
                let stride = (((width as u64 * bits_per_pixel as u64 + 31) / 32) * 4) as u32;
                let size = match stride.checked_mul(height) {
                    Some(size) => size,
                    None => return Ok(Effect::Return(0)),
                };
                let owned = data == 0;
                let pixels = if owned {
                    let pixels = match self.allocate(memory, size.max(1)) {
                        Ok(pixels) => pixels,
                        Err(_) => return Ok(Effect::Return(0)),
                    };
                    fill_guest(memory, pixels, 0, size)?;
                    pixels
                } else {
                    data
                };
                let bitmap = match self.gdi.bitmap_pixels(
                    memory,
                    width,
                    height,
                    pixels,
                    bits_per_pixel as u16,
                ) {
                    Ok(bitmap) => bitmap,
                    Err(_) => {
                        if owned {
                            let _ = self.arena.release(pixels);
                        }
                        self.thread.last_error = 87;
                        return Ok(Effect::Return(0));
                    }
                };
                if owned {
                    self.bitmap_allocations.insert(bitmap, pixels);
                }
                bitmap
            }
            ("gdi32", "GetBitmapBits" | "SetBitmapBits") => {
                if name == "SetBitmapBits" {
                    if a(1) > 64 * 1024 * 1024 {
                        return Err("bitmap bits exceed supported size".into());
                    }
                    let mut bytes = vec![0; a(1) as usize];
                    memory
                        .read(a(2), &mut bytes)
                        .map_err(|e| format!("SetBitmapBits source: {e:?}"))?;
                    let length = self
                        .gdi
                        .bitmap_bits_write(memory, a(0), &bytes)
                        .map_err(|e| format!("SetBitmapBits: {e:?}"))?;
                    return Ok(Effect::Return(length as u32));
                }
                let packed = self
                    .gdi
                    .bitmap_bits_packed(memory, a(0))
                    .map_err(|e| format!("GetBitmapBits: {e:?}"))?;
                let full = packed.len() as u32;
                if a(2) == 0 {
                    return Ok(Effect::Return(full));
                }
                if a(1) == 0 {
                    return Ok(Effect::Return(0));
                }
                let copy = (a(1) as u64).min(full as u64) as usize;
                memory
                    .write(a(2), &packed[..copy])
                    .map_err(|e| format!("GetBitmapBits destination: {e:?}"))?;
                full
            }
            ("gdi32", "CreateBitmapIndirect") => {
                let width = read(memory, a(0) + 4)?;
                let height = read(memory, a(0) + 8)?;
                let width_bytes = read(memory, a(0) + 12)?;
                let planes = u16::from_le_bytes([
                    read_byte(memory, a(0) + 16)?,
                    read_byte(memory, a(0) + 17)?,
                ]);
                let bit_count = u16::from_le_bytes([
                    read_byte(memory, a(0) + 18)?,
                    read_byte(memory, a(0) + 19)?,
                ]);
                let source = read(memory, a(0) + 20)?;
                if width == 0
                    || height == 0
                    || width > 0x07ff_ffff
                    || height > 0x07ff_ffff
                    || planes != 1
                    || !matches!(bit_count, 1 | 2 | 4 | 8 | 16 | 24 | 32)
                {
                    self.thread.last_error = 87;
                    return Ok(Effect::Return(0));
                }
                let stride = (((width as u64 * bit_count as u64 + 31) / 32) * 4) as u32;
                let size = match stride.checked_mul(height) {
                    Some(size) => size,
                    None => return Ok(Effect::Return(0)),
                };
                let bits = match self.allocate(memory, size.max(1)) {
                    Ok(bits) => bits,
                    Err(_) => return Ok(Effect::Return(0)),
                };
                fill_guest(memory, bits, 0, size)?;
                if source != 0 {
                    let row_bytes = width_bytes.min(stride);
                    for row in 0..height {
                        let mut row_data = vec![0u8; row_bytes as usize];
                        memory
                            .read(source + row * width_bytes, &mut row_data)
                            .map_err(|e| format!("CreateBitmapIndirect row: {e:?}"))?;
                        memory
                            .write(bits + row * stride, &row_data)
                            .map_err(|e| format!("CreateBitmapIndirect row: {e:?}"))?;
                    }
                }
                let bitmap = match self
                    .gdi
                    .bitmap_pixels(memory, width, height, bits, bit_count)
                {
                    Ok(bitmap) => bitmap,
                    Err(_) => {
                        let _ = self.arena.release(bits);
                        return Ok(Effect::Return(0));
                    }
                };
                self.bitmap_allocations.insert(bitmap, bits);
                bitmap
            }
            ("gdi32", "CreateDiscardableBitmap") => {
                if !self.gdi.is_dc(a(0)) || a(1) == 0 || a(2) == 0 {
                    return Ok(Effect::Return(0));
                }
                let (width, height) = (a(1), a(2));
                let stride = (((width as u64 * 32 + 31) / 32) * 4) as u32;
                let size = match stride.checked_mul(height) {
                    Some(size) => size,
                    None => return Ok(Effect::Return(0)),
                };
                let bits = match self.allocate(memory, size.max(1)) {
                    Ok(bits) => bits,
                    Err(_) => return Ok(Effect::Return(0)),
                };
                fill_guest(memory, bits, 0, size)?;
                let bitmap = match self.gdi.bitmap_pixels(memory, width, height, bits, 32) {
                    Ok(bitmap) => bitmap,
                    Err(_) => {
                        let _ = self.arena.release(bits);
                        return Ok(Effect::Return(0));
                    }
                };
                self.bitmap_allocations.insert(bitmap, bits);
                bitmap
            }
            ("gdi32", "CreateCompatibleBitmap") => {
                if !self.gdi.is_dc(a(0)) {
                    self.thread.last_error = 6;
                    return Ok(Effect::Return(0));
                }
                let (width, height) = (a(1), a(2));
                if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
                    self.thread.last_error = 87;
                    return Ok(Effect::Return(0));
                }
                let stride = (((width as u64 * 32 + 31) / 32) * 4) as u32;
                let size = match stride.checked_mul(height) {
                    Some(size) => size,
                    None => {
                        self.thread.last_error = 8;
                        return Ok(Effect::Return(0));
                    }
                };
                let bits = match self.allocate(memory, size.max(1)) {
                    Ok(bits) => bits,
                    Err(_) => {
                        self.thread.last_error = 8;
                        return Ok(Effect::Return(0));
                    }
                };
                fill_guest(memory, bits, 0, size)?;
                let bitmap = match self.gdi.bitmap_pixels(memory, width, height, bits, 32) {
                    Ok(bitmap) => bitmap,
                    Err(_) => {
                        let _ = self.arena.release(bits);
                        self.thread.last_error = 8;
                        return Ok(Effect::Return(0));
                    }
                };
                self.bitmap_allocations.insert(bitmap, bits);
                bitmap
            }
            ("user32", "GetDesktopWindow") => DESKTOP_WINDOW,
            ("user32", "GetDC" | "GetWindowDC") => {
                let target = if a(0) == 0 { DESKTOP_WINDOW } else { a(0) };
                if name == "GetDC" {
                    let class = self.window_data.get(&target).map(|data| data.class.clone());
                    let style = class
                        .as_ref()
                        .and_then(|class| self.classes.get(class))
                        .map_or(0, |(_, fields)| fields[0]);
                    // CS_CLASSDC = 0x40, CS_OWNDC = 0x20.
                    if style & 0x40 != 0 {
                        if let Some(ref class) = class {
                            if let Some(&dc) = self.class_dcs.get(class) {
                                return Ok(Effect::Return(dc));
                            }
                        }
                    }
                    if style & 0x20 != 0 {
                        if let Some(&dc) = self.own_dcs.get(&target) {
                            return Ok(Effect::Return(dc));
                        }
                    }
                }
                if self.gdi.surface(target).is_none() {
                    let window = self
                        .windows
                        .get(target)
                        .map_err(|e| format!("GetDC: invalid window: {e:?}"))?;
                    self.gdi
                        .create_surface(target, window.width, window.height)
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
                if name == "GetDC" {
                    let class = self.window_data.get(&target).map(|data| data.class.clone());
                    let style = class
                        .as_ref()
                        .and_then(|class| self.classes.get(class))
                        .map_or(0, |(_, fields)| fields[0]);
                    if style & 0x40 != 0 {
                        if let Some(class) = class {
                            self.class_dcs.insert(class, dc);
                        }
                    } else if style & 0x20 != 0 {
                        self.own_dcs.insert(target, dc);
                    }
                }
                dc
            }
            ("user32", "ReleaseDC") => {
                let is_class_dc = self.class_dcs.values().any(|dc| *dc == a(1));
                let is_own_dc = self.own_dcs.values().any(|dc| *dc == a(1));
                let owns = self.common_dcs.get(&a(1)) == Some(&a(0)) || is_class_dc || is_own_dc;
                if !owns {
                    return Ok(Effect::Return(0));
                }
                self.common_dcs.remove(&a(1));
                if !is_class_dc && !is_own_dc && self.gdi.is_dc(a(1)) {
                    self.gdi
                        .delete_dc(a(1))
                        .map_err(|e| format!("ReleaseDC: {e:?}"))?;
                }
                1
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
                    // A DC without a pixel format still reports one format;
                    // only format 1 can be described, and format 0/out-of-range
                    // describe calls return 0 without touching the buffer.
                    if a(3) == 0 {
                        return Ok(Effect::Return(1));
                    }
                    if a(1) != 1 || a(2) < 40 {
                        return Ok(Effect::Return(0));
                    }
                    let mut bytes = [0u8; 40];
                    bytes[0..2].copy_from_slice(&40u16.to_le_bytes());
                    bytes[2..4].copy_from_slice(&1u16.to_le_bytes());
                    memory
                        .write(a(3), &bytes)
                        .map_err(|e| format!("DescribePixelFormat PFD write: {e:?}"))?;
                    return Ok(Effect::Return(1));
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
                if deleted {
                    self.opengl.context_destroyed(a(0));
                }
                deleted as u32
            }
            ("gdi32", "CreateDCA" | "CreateDCW") => {
                let wide = name.ends_with('W');
                // CreateDC(driver, device): a NULL driver means "use the
                // default driver for this device", so the device name is the
                // second argument in that case.
                let device_pointer = if a(0) != 0 { a(0) } else { a(1) };
                if device_pointer == 0 {
                    return Ok(Effect::Return(0));
                }
                let device = if wide {
                    read_wide_string(memory, device_pointer)?
                } else {
                    String::from_utf8_lossy(&read_c_string(memory, device_pointer)?).into_owned()
                };
                let normalized = device.to_ascii_lowercase();
                let is_display = normalized == "display"
                    || normalized.strip_prefix("\\\\.\\").is_some_and(|rest| rest.starts_with("display"));
                if !is_display {
                    // Only the display device exists in this host; printer and
                    // other port drivers are not emulated.
                    return Ok(Effect::Return(0));
                }
                let surface = self
                    .gdi
                    .surface(DESKTOP_WINDOW)
                    .ok_or("CreateDC: no desktop surface")?;
                let clip = Rect {
                    left: 0,
                    top: 0,
                    right: surface.width as i32,
                    bottom: surface.height as i32,
                };
                self.gdi
                    .paint_dc(DESKTOP_WINDOW, clip)
                    .map_err(|e| format!("CreateDC: {e:?}"))?
            }
            ("user32", "EnumDisplaySettingsA") => {
                if a(2) == 0 {
                    return Ok(Effect::Return(0));
                }
                let mut bytes = [0u8; 156];
                bytes[..7].copy_from_slice(b"DISPLAY");
                bytes[32..34].copy_from_slice(&0x401u16.to_le_bytes());
                bytes[36..38].copy_from_slice(&156u16.to_le_bytes());
                // DM_BITSPERPEL | DM_PELSWIDTH | DM_PELSHEIGHT |
                // DM_DISPLAYFREQUENCY | DM_POSITION
                bytes[40..44].copy_from_slice(&(0x0004_0000u32 | 0x0008_0000 | 0x0010_0000 | 0x0040_0000 | 0x20).to_le_bytes());
                bytes[104..108].copy_from_slice(&32u32.to_le_bytes());
                bytes[108..112].copy_from_slice(&640u32.to_le_bytes());
                bytes[112..116].copy_from_slice(&480u32.to_le_bytes());
                bytes[120..124].copy_from_slice(&60u32.to_le_bytes());
                // dmSize may already be a caller-supplied prefix length; keep
                // it so the conversion test sees the caller's value.
                let mut size = [0u8; 2];
                memory
                    .read(a(2) + 36, &mut size)
                    .map_err(|e| format!("EnumDisplaySettingsA size: {e:?}"))?;
                let limit = (u16::from_le_bytes(size) as usize).clamp(36, bytes.len());
                memory
                    .write(a(2), &bytes[..limit])
                    .map_err(|e| format!("EnumDisplaySettingsA output: {e:?}"))?;
                1
            }
            ("kernel32", "DeleteFileA" | "DeleteFileW") => {
                let path = if name.ends_with('W') {
                    read_wide_string(memory, a(0))?.into_bytes()
                } else {
                    string(memory, a(0))?
                };
                if self.files.delete(&path) {
                    1
                } else {
                    self.thread.last_error = 2;
                    0
                }
            }
            ("kernel32", "GetTempFileNameA") => {
                if a(0) == 0 || a(1) == 0 || a(3) == 0 {
                    self.thread.last_error = 87;
                    return Ok(Effect::Return(0));
                }
                let directory = String::from_utf8_lossy(&read_c_string(memory, a(0))?).into_owned();
                let prefix = String::from_utf8_lossy(&read_c_string(memory, a(1))?).into_owned();
                self.temp_file_counter = self.temp_file_counter.wrapping_add(1);
                let separator = if directory.ends_with('\\') || directory.ends_with('/') {
                    ""
                } else {
                    "\\"
                };
                let name = format!(
                    "{directory}{separator}{prefix}{:04X}.tmp",
                    self.temp_file_counter & 0xffff
                );
                let mut bytes = name.into_bytes();
                bytes.push(0);
                memory
                    .write(a(3), &bytes)
                    .map_err(|e| format!("GetTempFileNameA output: {e:?}"))?;
                self.temp_file_counter
            }
            ("kernel32", "VirtualQuery") => {
                // VirtualQuery(address, info, length)
                let Some((start, size)) = self.arena.allocation_at(a(0)) else {
                    return Ok(Effect::Return(0));
                };
                if a(1) == 0 || a(2) < 28 {
                    return Ok(Effect::Return(0));
                }
                let region = (size + 4095) & !4095;
                let mut bytes = [0u8; 28];
                bytes[0..4].copy_from_slice(&start.to_le_bytes());
                bytes[4..8].copy_from_slice(&start.to_le_bytes());
                bytes[8..12].copy_from_slice(&4u32.to_le_bytes()); // PAGE_READWRITE
                bytes[12..16].copy_from_slice(&(region as u32).to_le_bytes());
                bytes[16..20].copy_from_slice(&0x1000u32.to_le_bytes()); // MEM_COMMIT
                bytes[20..24].copy_from_slice(&4u32.to_le_bytes()); // PAGE_READWRITE
                bytes[24..28].copy_from_slice(&0x20000u32.to_le_bytes()); // MEM_PRIVATE
                memory
                    .write(a(1), &bytes)
                    .map_err(|e| format!("VirtualQuery output: {e:?}"))?;
                28
            }
            ("gdi32", "GetDIBColorTable" | "SetDIBColorTable") => {
                let Some(bitmap) = self.gdi.selected_dib_bitmap(a(0)) else {
                    return Ok(Effect::Return(0));
                };
                let start = a(1) as usize;
                if name == "GetDIBColorTable" {
                    let table = self
                        .gdi
                        .dib_color_table(bitmap)
                        .ok_or("GetDIBColorTable: bitmap gone")?;
                    let bit_count = self
                        .gdi
                        .bitmap_dib(bitmap)
                        .map_or(8, |dib| dib.bit_count() as u32);
                    if bit_count > 8 {
                        return Ok(Effect::Return(0));
                    }
                    let available = 1usize << bit_count;
                    let last = start.saturating_add(a(2) as usize).min(available);
                    let mut count = 0u32;
                    for index in start..last {
                        let color = table.get(index).copied().unwrap_or([0, 0, 0, 255]);
                        let bytes = [color[2], color[1], color[0], 0];
                        memory
                            .write(a(3) + count * 4, &bytes)
                            .map_err(|e| format!("GetDIBColorTable output: {e:?}"))?;
                        count += 1;
                    }
                    count
                } else {
                    let mut colors = Vec::new();
                    for index in 0..a(2) {
                        let mut bytes = [0u8; 4];
                        memory
                            .read(a(3) + index * 4, &mut bytes)
                            .map_err(|e| format!("SetDIBColorTable input: {e:?}"))?;
                        colors.push([bytes[2], bytes[1], bytes[0], bytes[3]]);
                    }
                    self.gdi.set_dib_color_table(bitmap, start, &colors) as u32
                }
            }
            ("gdi32", "SetDIBitsToDevice") => {
                // SetDIBitsToDevice(hdc, x, y, cx, cy, xSrc, ySrc, start, lines,
                //                   bits, bmi, usage)
                if a(10) == 0 || a(9) == 0 || a(11) > 2 {
                    return Ok(Effect::Return(0));
                }
                let size = read(memory, a(10))?;
                let core = size == 12;
                if !core && size < 40 {
                    return Ok(Effect::Return(0));
                }
                let (width, height, planes, bit_count, compression, size_image) = if core {
                    (
                        u16::from_le_bytes([read_byte(memory, a(10) + 4)?, read_byte(memory, a(10) + 5)?]) as i32,
                        u16::from_le_bytes([read_byte(memory, a(10) + 6)?, read_byte(memory, a(10) + 7)?]) as i32,
                        u16::from_le_bytes([read_byte(memory, a(10) + 8)?, read_byte(memory, a(10) + 9)?]),
                        u16::from_le_bytes([read_byte(memory, a(10) + 10)?, read_byte(memory, a(10) + 11)?]),
                        0u32,
                        0u32,
                    )
                } else {
                    (
                        read(memory, a(10) + 4)? as i32,
                        read(memory, a(10) + 8)? as i32,
                        u16::from_le_bytes([read_byte(memory, a(10) + 12)?, read_byte(memory, a(10) + 13)?]),
                        u16::from_le_bytes([read_byte(memory, a(10) + 14)?, read_byte(memory, a(10) + 15)?]),
                        read(memory, a(10) + 16)?,
                        read(memory, a(10) + 20)?,
                    )
                };
                let rle = (bit_count == 4 && compression == 2) || (bit_count == 8 && compression == 1);
                if width <= 0 || height == 0 || (planes == 0 && !rle) {
                    return Ok(Effect::Return(0));
                }
                let valid = match bit_count {
                    1 | 24 => compression == 0,
                    4 => compression == 0 || compression == 2,
                    8 => compression == 0 || compression == 1,
                    16 | 32 => compression == 0 || compression == 3,
                    _ => false,
                };
                if !valid || (rle && size_image == 0) {
                    return Ok(Effect::Return(0));
                }
                if rle {
                    // RLE decoding is not implemented; the call is accepted so
                    // the non-RLE paths in the sweep stay exercised.
                    return Ok(Effect::Return(a(8).max(1)));
                }
                let source = match crate::raster::Dib::from_header(memory, a(10), a(9)) {
                    Ok(source) => source,
                    Err(_) => return Ok(Effect::Return(0)),
                };
                let start = a(7);
                if start >= source.height {
                    return Ok(Effect::Return(0));
                }
                let lines = a(8).min(source.height - start);
                let cx = a(3).min(source.width);
                let cy = a(4).min(lines);
                for row in 0..cy {
                    let source_y = if source.top_down {
                        start + row
                    } else {
                        source.height - 1 - (start + row)
                    };
                    for column in 0..cx {
                        let mut pixel = [0u8; 4];
                        if source
                            .rgba_row(memory, column, source_y, &mut pixel)
                            .is_err()
                        {
                            continue;
                        }
                        let color = u32::from(pixel[0])
                            | (u32::from(pixel[1]) << 8)
                            | (u32::from(pixel[2]) << 16);
                        // Scan line 0 is the bottom of the DIB, so it lands on
                        // the bottom row of the destination rectangle.
                        let _ = self.gdi.set_pixel(
                            memory,
                            a(0),
                            a(1) as i32 + column as i32,
                            a(2) as i32 + (cy - 1 - row) as i32,
                            color,
                        );
                    }
                }
                cy.max(1)
            }
            ("gdi32", "SetDIBits") => {
                let Some(target) = self.gdi.bitmap_dib(a(1)) else {
                    return Ok(Effect::Return(0));
                };
                if a(5) == 0 || a(4) == 0 || a(6) > 2 {
                    return Ok(Effect::Return(0));
                }
                if a(6) == 2 {
                    let monochrome = self
                        .gdi
                        .bitmap_dib(a(1))
                        .is_some_and(|dib| dib.bit_count() == 1);
                    if !monochrome {
                        return Ok(Effect::Return(0));
                    }
                }
                let bit_count = u16::from_le_bytes([
                    read_byte(memory, a(5) + 14)?,
                    read_byte(memory, a(5) + 15)?,
                ]);
                let planes = u16::from_le_bytes([
                    read_byte(memory, a(5) + 12)?,
                    read_byte(memory, a(5) + 13)?,
                ]);
                let compression = read(memory, a(5) + 16)?;
                let size_image = read(memory, a(5) + 20)?;
                let valid = match bit_count {
                    1 | 24 => compression == 0,
                    4 => compression == 0 || compression == 2,
                    8 => compression == 0 || compression == 1,
                    16 | 32 => compression == 0 || compression == 3,
                    _ => false,
                };
                let rle = (bit_count == 4 && compression == 2) || (bit_count == 8 && compression == 1);
                if !valid || (planes == 0 && !rle) || (rle && size_image == 0) {
                    return Ok(Effect::Return(0));
                }
                if compression == 3 {
                    let masks = [
                        read(memory, a(5) + 40)?,
                        read(memory, a(5) + 44)?,
                        read(memory, a(5) + 48)?,
                    ];
                    if masks.contains(&0) {
                        return Ok(Effect::Return(0));
                    }
                }
                if rle {
                    // RLE decoding is not implemented; accepted streams report
                    // the requested number of lines.
                    return Ok(Effect::Return(a(3).max(1)));
                }
                let source = match crate::raster::Dib::from_header(memory, a(5), a(4)) {
                    Ok(source) => source,
                    Err(_) => return Ok(Effect::Return(0)),
                };
                let start = a(2);
                if start >= target.height {
                    return Ok(Effect::Return(0));
                }
                let lines = a(3).min(target.height - start);
                let width = target.width.min(source.width);
                let mut row = vec![0u8; width as usize * 4];
                for line in 0..lines {
                    let source_y = if source.top_down {
                        start + line
                    } else {
                        source.height - 1 - (start + line)
                    };
                    if source_y >= source.height {
                        break;
                    }
                    source
                        .rgba_row(memory, 0, source_y, &mut row)
                        .map_err(|e| format!("SetDIBits source row: {e:?}"))?;
                    let target_y = if target.top_down {
                        start + line
                    } else {
                        target.height - 1 - (start + line)
                    };
                    target
                        .write_rgba_row(memory, 0, target_y, &row)
                        .map_err(|e| format!("SetDIBits target row: {e:?}"))?;
                }
                lines
            }
            ("user32", "EnumDisplayMonitors") => {
                if a(2) == 0 {
                    return Ok(Effect::Return(1));
                }
                let rect = self.allocate(memory, 16)?;
                for (index, value) in [0i32, 0, 640, 480].iter().enumerate() {
                    write(memory, rect + index as u32 * 4, *value as u32)?;
                }
                // One monitor covers the virtual desktop.
                return self.callback(
                    a(2),
                    vec![0x0001_0001, 0, rect, a(3)],
                    None,
                    CallbackFinish::EnumMonitors { rect },
                );
            }
            ("user32", "EnumDisplayDevicesA" | "EnumDisplayDevicesW") => {
                if a(0) != 0 || a(1) != 0 || a(2) == 0 {
                    // No monitor children are reported for an adapter.
                    return Ok(Effect::Return(0));
                }
                let wide = name.ends_with('W');
                let mut bytes = vec![0u8; if wide { 840 } else { 424 }];
                if wide {
                    for (index, unit) in "\\\\.\\DISPLAY1".encode_utf16().enumerate() {
                        bytes[4 + index * 2..6 + index * 2]
                            .copy_from_slice(&unit.to_le_bytes());
                    }
                    let text = "Wine GDI display";
                    for (index, unit) in text.encode_utf16().enumerate() {
                        bytes[68 + index * 2..70 + index * 2]
                            .copy_from_slice(&unit.to_le_bytes());
                    }
                    bytes[0..4].copy_from_slice(&840u32.to_le_bytes());
                    bytes[324..328].copy_from_slice(&5u32.to_le_bytes());
                } else {
                    for (index, byte) in b"\\\\.\\DISPLAY1".iter().enumerate() {
                        bytes[4 + index] = *byte;
                    }
                    for (index, byte) in b"Wine GDI display".iter().enumerate() {
                        bytes[36 + index] = *byte;
                    }
                    bytes[0..4].copy_from_slice(&424u32.to_le_bytes());
                    bytes[164..168].copy_from_slice(&5u32.to_le_bytes());
                }
                memory
                    .write(a(2), &bytes)
                    .map_err(|e| format!("EnumDisplayDevices: {e:?}"))?;
                1
            }
            ("gdi32", "GetDeviceCaps") => {
                if !self.gdi.is_dc(a(0)) {
                    self.thread.last_error = 6; // ERROR_INVALID_HANDLE
                    return Ok(Effect::Return(0));
                }
                if self.gdi.is_meta_dc(a(0)) == Some(false) {
                    // Old-style metafile DC: only TECHNOLOGY is defined.
                    // DT_METAFILE is 5.
                    return Ok(Effect::Return(if a(1) == 2 { 5 } else { 0 }));
                }
                match a(1) {
                    0 => 0x300,   // DRIVERVERSION
                    2 => 1,       // TECHNOLOGY: DT_RASDISPLAY
                    4 => 169,     // HORZSIZE (mm)
                    6 => 127,     // VERTSIZE (mm)
                    8 | 118 => 640, // HORZRES, DESKTOPHORZRES
                    10 | 117 => 480, // VERTRES, DESKTOPVERTRES
                    12 => self.display_bits, // BITSPIXEL
                    14 => 1,      // PLANES
                    16 | 18 => u32::MAX, // NUMBRUSHES, NUMPENS
                    20 | 22 => 0, // NUMMARKERS, NUMFONTS
                    24 => {
                        if self.display_bits == 8 {
                            // The default 8-bpp system palette reserves 20
                            // entries, which is what Windows reports here.
                            20
                        } else {
                            u32::MAX // NUMCOLORS: "more than 256"
                        }
                    }
                    26 => 0,      // PDEVICESIZE
                    28 | 30 | 32 | 34 => 0, // CURVE/LINE/POLYGONAL/TEXTCAPS
                    36 => 1,      // CLIPCAPS: CP_RECTANGLE
                    38 => 0x2841 | if self.display_bits == 8 { 0x100 } else { 0 },
                    40 | 42 => 36, // ASPECTX, ASPECTY
                    44 => 51,      // ASPECTXY
                    88 | 90 => 96, // LOGPIXELSX, LOGPIXELSY
                    104 => {
                        if self.display_bits == 8 {
                            256
                        } else {
                            0
                        }
                    }
                    106 => {
                        if self.display_bits == 8 {
                            20
                        } else {
                            0
                        }
                    }
                    108 => match self.display_bits {
                        8 => 18,
                        16 => 16,
                        _ => 24,
                    }, // COLORRES
                    110..=115 => 0, // PHYSICAL*, SCALINGFACTOR*
                    116 => 60,     // VREFRESH
                    119 | 120 => 0, // BLTALIGNMENT, SHADEBLENDCAPS
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
                const CREATE_NEW: u32 = 1;
                const CREATE_ALWAYS: u32 = 2;
                const OPEN_EXISTING: u32 = 3;
                const OPEN_ALWAYS: u32 = 4;
                const TRUNCATE_EXISTING: u32 = 5;
                let path = string(memory, a(0))?;
                let writable = a(1) & GENERIC_WRITE != 0;
                if writable {
                    match a(4) {
                        CREATE_ALWAYS | CREATE_NEW | TRUNCATE_EXISTING => {
                            match self.files.create(&path) {
                                Some(handle) => handle,
                                None => {
                                    self.thread.last_error = 5;
                                    u32::MAX
                                }
                            }
                        }
                        OPEN_ALWAYS | OPEN_EXISTING => {
                            if self.files.open(&path).is_some() {
                                self.files.open(&path).unwrap()
                            } else {
                                self.thread.last_error = 2;
                                u32::MAX
                            }
                        }
                        _ => {
                            self.thread.last_error = 5;
                            u32::MAX
                        }
                    }
                } else if a(4) != OPEN_EXISTING {
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
                let written = if self.stdio.write(a(0), &bytes) {
                    Some(bytes.len() as u32)
                } else {
                    self.files.write(a(0), &bytes)
                };
                if let Some(written) = written {
                    if a(3) != 0 {
                        write(memory, a(3), written)?;
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
            ("kernel32", "FileTimeToLocalFileTime") => {
                if a(0) == 0 || a(1) == 0 {
                    self.thread.last_error = 87; // ERROR_INVALID_PARAMETER
                    return Ok(Effect::Return(0));
                }
                let mut file_time = [0u8; 8];
                memory
                    .read(a(0), &mut file_time)
                    .map_err(|e| format!("FileTimeToLocalFileTime source: {e:?}"))?;
                // The virtual guest has a zero UTC bias (GetTimeZoneInformation),
                // so a local file time is the same 100 ns interval as UTC.
                memory
                    .write(a(1), &file_time)
                    .map_err(|e| format!("FileTimeToLocalFileTime result: {e:?}"))?;
                1
            }
            ("kernel32", "FileTimeToDosDateTime") => {
                if a(0) == 0 || a(1) == 0 || a(2) == 0 {
                    self.thread.last_error = 87; // ERROR_INVALID_PARAMETER
                    return Ok(Effect::Return(0));
                }
                let mut raw = [0u8; 8];
                memory
                    .read(a(0), &mut raw)
                    .map_err(|e| format!("FileTimeToDosDateTime source: {e:?}"))?;
                let Some((date, time)) = filetime_to_dos(u64::from_le_bytes(raw)) else {
                    self.thread.last_error = 87; // outside MS-DOS 1980..2107
                    return Ok(Effect::Return(0));
                };
                memory
                    .write(a(1), &date.to_le_bytes())
                    .map_err(|e| format!("FileTimeToDosDateTime date: {e:?}"))?;
                memory
                    .write(a(2), &time.to_le_bytes())
                    .map_err(|e| format!("FileTimeToDosDateTime time: {e:?}"))?;
                1
            }
            ("kernel32", "GetLocalTime") => {
                // Stable local epoch for deterministic demos: 2000-01-01 00:00:00,
                // advanced by the VM's virtual clock.
                let seconds = self.clock_ms / 1000;
                let day_seconds = seconds % 86_400;
                let mut time = [0u8; 16];
                // 2000-01-01 was Saturday (SYSTEMTIME weekday 6).
                for (offset, value) in [
                    2000u16,
                    1,
                    6,
                    1,
                    (day_seconds / 3600) as u16,
                    ((day_seconds / 60) % 60) as u16,
                    (day_seconds % 60) as u16,
                    (self.clock_ms % 1000) as u16,
                ]
                .into_iter()
                .enumerate()
                {
                    time[offset * 2..offset * 2 + 2].copy_from_slice(&value.to_le_bytes());
                }
                memory
                    .write(a(0), &time)
                    .map_err(|e| format!("local time: {e:?}"))?;
                0
            }
            ("kernel32", "GetTimeZoneInformation") => {
                // TIME_ZONE_ID_UNKNOWN with a zero-bias UTC virtual clock.
                memory
                    .write(a(0), &[0; 172])
                    .map_err(|e| format!("time zone: {e:?}"))?;
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
            // Browser guests have no writable host INI store; return the
            // caller's default exactly as Win32 does for a missing key. MFC
            // probes PROGRAM.INI for its MRU list during startup.
            ("kernel32", "GetPrivateProfileStringA") => {
                let mut value = string(memory, a(2))?;
                let capacity = a(4) as usize;
                if capacity == 0 {
                    0
                } else {
                    value.truncate(capacity - 1);
                    value.push(0);
                    memory
                        .write(a(3), &value)
                        .map_err(|e| format!("GetPrivateProfileStringA buffer: {e:?}"))?;
                    (value.len() - 1) as u32
                }
            }
            ("kernel32", "GetWindowsDirectoryA") => {
                // A stable virtual installation root. The argument order differs
                // from GetCurrentDirectoryA: buffer first, then capacity.
                let windows = b"C:\\WINDOWS";
                let required = (windows.len() + 1) as u32;
                let capacity = a(1);
                if a(0) == 0 {
                    if capacity == 0 {
                        return Ok(Effect::Return(required));
                    }
                    self.thread.last_error = 87; // ERROR_INVALID_PARAMETER
                    return Ok(Effect::Return(0));
                }
                if capacity == 0 || capacity < required {
                    return Ok(Effect::Return(required));
                }
                if (a(0) as u64).saturating_add(capacity as u64) > memory.byte_len() {
                    self.thread.last_error = 998; // ERROR_NOACCESS
                    return Ok(Effect::Return(0));
                }
                memory
                    .write(a(0), windows)
                    .map_err(|e| format!("windows directory: {e:?}"))?;
                memory
                    .write(a(0) + windows.len() as u32, &[0])
                    .map_err(|e| format!("windows directory NUL: {e:?}"))?;
                windows.len() as u32
            }
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
            (
                "advapi32",
                "RegOpenKeyA" | "RegOpenKeyExA" | "RegCreateKeyExA" | "RegCloseKey"
                | "RegQueryValueExA" | "RegSetValueExA" | "RegDeleteKeyA" | "RegDeleteValueA"
                | "RegQueryValueA",
            ) => {
                return self.registry.call(name, args, memory).map(Effect::Return);
            }
            ("winmm", "waveOutGetNumDevs" | "midiOutGetNumDevs") => 0,
            ("winmm", "joyGetDevCapsA" | "joyGetPos") => 6,
            ("winmm", "timeGetTime") => self.clock_ms as u32,
            ("winmm", "timeSetEvent") => {
                if a(2) == 0 {
                    return Ok(Effect::Return(0));
                }
                let id = self.next_multimedia_timer.max(1);
                self.next_multimedia_timer = id.wrapping_add(1).max(1);
                self.multimedia_timers.insert(
                    id,
                    MultimediaTimer {
                        due: self.clock_ms.saturating_add(a(0) as u64),
                        interval: a(0),
                        callback: a(2),
                        user: a(3),
                        periodic: a(4) & 1 != 0,
                    },
                );
                id
            }
            // No multimedia timer can have ID zero. Applications commonly
            // cancel their zero-initialized timer during game reset.
            ("winmm", "timeKillEvent") => {
                if a(0) == 0 {
                    97 // TIMERR_NOCANDO
                } else if self.multimedia_timers.remove(&a(0)).is_some() {
                    0
                } else {
                    11 // MMSYSERR_INVALPARAM
                }
            }
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
                if a(0) == 0 || a(1) == 0 {
                    return Ok(Effect::Return(0));
                }
                let rect = read_rect(memory, a(1))?;
                write_rect(memory, a(0), rect)?;
                1
            }
            ("user32", "EqualRect") => {
                if a(0) == 0 || a(1) == 0 {
                    return Ok(Effect::Return(0));
                }
                (read_rect(memory, a(0))? == read_rect(memory, a(1))?) as u32
            }
            ("user32", "OffsetRect") => {
                if a(0) == 0 {
                    return Ok(Effect::Return(0));
                }
                let mut rect = read_rect(memory, a(0))?;
                rect[0] = rect[0].wrapping_add(a(1) as i32);
                rect[2] = rect[2].wrapping_add(a(1) as i32);
                rect[1] = rect[1].wrapping_add(a(2) as i32);
                rect[3] = rect[3].wrapping_add(a(2) as i32);
                write_rect(memory, a(0), rect)?;
                1
            }
            ("user32", "InflateRect") => {
                if a(0) == 0 {
                    return Ok(Effect::Return(0));
                }
                let mut rect = read_rect(memory, a(0))?;
                rect[0] = rect[0].wrapping_sub(a(1) as i32);
                rect[1] = rect[1].wrapping_sub(a(2) as i32);
                rect[2] = rect[2].wrapping_add(a(1) as i32);
                rect[3] = rect[3].wrapping_add(a(2) as i32);
                write_rect(memory, a(0), rect)?;
                1
            }
            ("user32", "IntersectRect" | "UnionRect") => {
                if a(0) == 0 || a(1) == 0 || a(2) == 0 {
                    return Ok(Effect::Return(0));
                }
                let lhs = read_rect(memory, a(1))?;
                let rhs = read_rect(memory, a(2))?;
                let out = if name == "IntersectRect" {
                    [
                        lhs[0].max(rhs[0]),
                        lhs[1].max(rhs[1]),
                        lhs[2].min(rhs[2]),
                        lhs[3].min(rhs[3]),
                    ]
                } else if lhs[0] >= lhs[2] || lhs[1] >= lhs[3] {
                    rhs
                } else if rhs[0] >= rhs[2] || rhs[1] >= rhs[3] {
                    lhs
                } else {
                    [
                        lhs[0].min(rhs[0]),
                        lhs[1].min(rhs[1]),
                        lhs[2].max(rhs[2]),
                        lhs[3].max(rhs[3]),
                    ]
                };
                write_rect(memory, a(0), out)?;
                if name == "IntersectRect" {
                    (out[0] < out[2] && out[1] < out[3]) as u32
                } else {
                    1
                }
            }
            ("user32", "PtInRect") => {
                if a(0) == 0 {
                    return Ok(Effect::Return(0));
                }
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
            // MFC 3.0 and other 16-bit-era libraries still use the local/global
            // allocator APIs during DllMain and window setup. Fixed memory is
            // directly addressable, so the process arena is the backing store.
            ("kernel32", "LocalAlloc" | "GlobalAlloc") => {
                let bytes = a(1);
                match self.arena.allocate(bytes, memory.byte_len()) {
                    Ok(address) => {
                        if a(0) & 0x40 != 0 && bytes > 0 {
                            if let Err(error) = memory.write(address, &vec![0; bytes as usize]) {
                                self.thread.last_error = 8; // ERROR_NOT_ENOUGH_MEMORY
                                let _ = self.arena.release(address);
                                let _ = error;
                                0
                            } else {
                                address
                            }
                        } else {
                            address
                        }
                    }
                    Err(_) => {
                        self.thread.last_error = 8;
                        0
                    }
                }
            }
            ("kernel32", "LocalFree" | "GlobalFree") => {
                let pointer = a(0);
                if pointer == 0 {
                    0
                } else if self.arena.release(pointer).is_ok() {
                    0
                } else {
                    self.thread.last_error = 87; // ERROR_INVALID_PARAMETER
                    pointer
                }
            }
            ("kernel32", "GlobalLock" | "GlobalHandle") => a(0),
            ("kernel32", "GlobalUnlock") => 1,
            ("kernel32", "IsWow64Process") => {
                if a(1) != 0 {
                    write(memory, a(1), 0)?;
                }
                1
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
                        quotient += if (numerator < 0) ^ (denominator < 0) {
                            -1
                        } else {
                            1
                        };
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
                | "GetCurrentThreadId" | "SetThreadPriority" | "TlsAlloc" | "TlsFree"
                | "TlsGetValue" | "TlsSetValue",
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
                    let procedure = if self
                        .modules
                        .handle("user32.dll")
                        .and_then(|handle| self.modules.procedure(handle, "DefWindowProcA"))
                        == Some(fields[1])
                    {
                        DEFAULT_WINDOW_PROC
                    } else {
                        fields[1]
                    };
                    self.windows
                        .register(class.clone(), procedure)
                        .map_err(|e| format!("register class: {e:?}"))?;
                    let atom = u16::try_from(self.classes.len() + 1)
                        .map_err(|_| "class atoms exhausted")?;
                    self.classes.insert(class, (atom, fields));
                    atom as u32
                }
            }
            ("user32", "UnregisterClassA" | "UnregisterClassW") => {
                let class = if a(0) <= 0xffff {
                    self.classes
                        .iter()
                        .find(|(_, (atom, _))| *atom as u32 == a(0))
                        .map(|(name, _)| name.clone())
                        .unwrap_or_default()
                } else {
                    string(memory, a(0))?
                };
                let removed = self.classes.remove(&class).is_some();
                if removed {
                    self.windows.unregister(&class);
                    if let Some(dc) = self.class_dcs.remove(&class) {
                        self.common_dcs.remove(&dc);
                        if self.gdi.is_dc(dc) {
                            self.gdi
                                .delete_dc(dc)
                                .map_err(|e| format!("UnregisterClass DC: {e:?}"))?;
                        }
                    }
                    1
                } else {
                    self.thread.last_error = 1411;
                    0
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
                    if message.window != state.window
                        && !matches!(message.message,0x100..=0x109 | 0x200..=0x209)
                    {
                        let effect = self.call(
                            "user32",
                            "SendMessageA",
                            &[
                                message.window,
                                message.message,
                                message.wparam,
                                message.lparam,
                            ],
                            memory,
                        )?;
                        return self.modal_control_effect(effect, key);
                    }
                    let mut msg = message.message;
                    let mut wp = message.wparam;
                    let lp = message.lparam;
                    if msg == 0x100 && matches!(wp, 13 | 27) {
                        msg = 0x111;
                        wp = if wp == 13 { 1 } else { 2 };
                    } else if matches!(msg, 0x201 | 0x202)
                        || (matches!(msg, 0x100 | 0x101) && wp == 32)
                    {
                        let px = lp as i16 as i32;
                        let py = (lp >> 16) as i16 as i32;
                        let child = if matches!(msg, 0x100 | 0x101) {
                            Some(self.focused_window)
                        } else if msg == 0x202 {
                            self.pressed_button
                        } else {
                            self.window_data.iter().find_map(|(child, data)| {
                                if data.parent != state.window
                                    || data.class != b"BUTTON"
                                    || data.style & 0x08000000 != 0
                                {
                                    return None;
                                }
                                let window = self.windows.get(*child).ok()?;
                                (px >= data.x
                                    && py >= data.y
                                    && px < data.x + window.width as i32
                                    && py < data.y + window.height as i32)
                                    .then_some(*child)
                            })
                        };
                        if let Some(child) = child.filter(|child| {
                            self.window_data.get(child).is_some_and(|data| {
                                data.parent == state.window && data.class == b"BUTTON"
                            })
                        }) {
                            let data = &self.window_data[&child];
                            let local = if matches!(msg, 0x201 | 0x202) {
                                ((px - data.x) as u32 & 0xffff)
                                    | (((py - data.y) as u32 & 0xffff) << 16)
                            } else {
                                lp
                            };
                            let effect = self.call(
                                "user32",
                                "SendMessageA",
                                &[child, msg, wp, local],
                                memory,
                            )?;
                            return self.modal_control_effect(effect, key);
                        }
                    }
                    if self
                        .windows
                        .get(state.window)
                        .is_ok_and(|window| window.procedure != 0xffff_ff01)
                    {
                        let effect = self.call(
                            "user32",
                            "SendMessageA",
                            &[state.window, msg, wp, lp],
                            memory,
                        )?;
                        return self.modal_control_effect(effect, key);
                    }
                    return self.callback(
                        state.procedure,
                        vec![state.window, msg, wp, lp],
                        None,
                        CallbackFinish::DialogBoxMessage {
                            key,
                            default_paint: if msg == 15 { Some(state.window) } else { None },
                        },
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
                    template.x as i32 * 7 / 4
                };
                let y = if template.y == -1 {
                    (480 - height as i32) / 2
                } else {
                    template.y as i32 * 14 / 8
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
                        button: crate::buttons::ButtonState::default(),
                        dialog_procedure: a(3),
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
                        restore: None,
                    },
                );
                for control in template.controls {
                    let class = match control.class {
                        crate::dialog::OrdinalOrString::Ordinal(128) => b"BUTTON".to_vec(),
                        crate::dialog::OrdinalOrString::Ordinal(129) => b"EDIT".to_vec(),
                        crate::dialog::OrdinalOrString::Ordinal(130) => b"STATIC".to_vec(),
                        crate::dialog::OrdinalOrString::Ordinal(131) => b"LISTBOX".to_vec(),
                        crate::dialog::OrdinalOrString::Ordinal(132) => b"SCROLLBAR".to_vec(),
                        crate::dialog::OrdinalOrString::String(v) => String::from_utf16(&v)
                            .map_err(|_| "invalid control class")?
                            .into_bytes(),
                        _ => return Err("unsupported dialog control class".into()),
                    };
                    let class = self.classes.keys().find(|name| name.eq_ignore_ascii_case(&class)).cloned().unwrap_or_else(|| class.to_ascii_uppercase());
                    let custom = self.classes.get(&class).map(|(_, fields)| (fields[1], fields[3]));
                    if custom.is_some_and(|fields| fields.1 > 4096) { return Err("dialog control extra storage exceeds supported size".into()); }
                    if !matches!(
                        class.as_slice(),
                        b"BUTTON" | b"EDIT" | b"STATIC" | b"LISTBOX" | b"SCROLLBAR"
                    ) && custom.is_none() {
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
                            custom.map_or(0xffff_ff02, |fields| fields.0),
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
                    if class == b"STATIC" && control.style & 15 == 3 {
                        let id = match &control.title {
                            crate::dialog::OrdinalOrString::Ordinal(id) => u32::from(*id),
                            crate::dialog::OrdinalOrString::String(name) => String::from_utf16(name).map_err(|_| "invalid static icon name")?.strip_prefix('#').and_then(|id| id.parse::<u16>().ok()).map(u32::from).ok_or("named static icon resources are unsupported")?,
                            _ => return Err("named static icon resources are unsupported".into()),
                        };
                        let Effect::Return(icon) = self.call("user32", "LoadIconA", &[a(0), id], memory)? else { return Err("icon loading yielded a callback".into()); };
                        let (address, length) = self.icons.get(icon).copied().ok_or("static icon resource not found")?;
                        let mut bytes = vec![0; length as usize];
                        memory.read(address, &mut bytes).map_err(|e| format!("static icon resource: {e:?}"))?;
                        self.static_icons.insert(child, crate::icon::decode(&bytes)?);
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
                            button: crate::buttons::ButtonState::default(),
                            dialog_procedure: 0,
                            class,
                            instance: a(0),
                            parent: window,
                            id: control.id,
                            style: control.style,
                            ex_style: control.ex_style,
                            user_data: 0,
                            extra: vec![0; custom.map_or(0, |fields| fields.1) as usize],
                            x: px,
                            y: py,
                            text,
                            restore: None,
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
                    if custom.is_none() { self.render_static(child)?; }
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
            ("user32", "CheckDlgButton" | "IsDlgButtonChecked") => {
                let child = self.window_data.iter().find_map(|(window, data)| {
                    (data.parent == a(0) && data.id == a(1)).then_some(*window)
                });
                let Some(child) = child else {
                    self.thread.last_error = 1421;
                    return Ok(Effect::Return(0));
                };
                if name == "CheckDlgButton" {
                    let action = self.button_check_action(child, a(2))?;
                    return self.action(action, Finish::FixedReturn(1));
                }
                return self.call("user32", "SendMessageA", &[child, 0xf0, 0, 0], memory);
            }
            ("user32", "CheckRadioButton") => {
                if self.windows.get(a(0)).is_err() || (a(1) as i32) > a(2) as i32 {
                    self.thread.last_error = 87;
                    return Ok(Effect::Return(0));
                }
                let mut checked = 0;
                let mut children = self
                    .window_data
                    .iter()
                    .filter_map(|(window, data)| {
                        let id = data.id as i32;
                        if data.parent == a(0) && id >= a(1) as i32 && id <= a(2) as i32 {
                            if data.id == a(3) {
                                checked = *window;
                            }
                            Some(*window)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                children.reverse();
                return self.radio_checks(children, checked);
            }
            ("user32", "GetDlgItem") => self
                .window_data
                .iter()
                .find_map(|(hwnd, data)| (data.parent == a(0) && data.id == a(1)).then_some(*hwnd))
                .unwrap_or(0),
            ("user32", "GetTopWindow") => self.windows.top_child(a(0)),
            ("user32", "GetWindow") => self.windows.related(a(0), a(1)),
            ("user32", "MoveWindow") => {
                if let Some(data) = self.window_data.get_mut(&a(0)) {
                    data.x = a(1) as i32;
                    data.y = a(2) as i32;
                }
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
                let current = self
                    .windows
                    .get(a(0))
                    .map_err(|e| format!("SetWindowPos: {e:?}"))?;
                let (x, y) = if a(6) & SWP_NOMOVE != 0 {
                    (current.x, current.y)
                } else {
                    (a(2) as i32, a(3) as i32)
                };
                let (width, height) = if a(6) & SWP_NOSIZE != 0 {
                    (current.width, current.height)
                } else {
                    (a(4).max(1), a(5).max(1))
                };
                let resized = (width, height) != (current.width, current.height);
                self.windows
                    .move_window(a(0), x, y, width, height)
                    .map_err(|e| format!("SetWindowPos: {e:?}"))?;
                if resized {
                    self.gdi
                        .resize_surface(a(0), width, height)
                        .map_err(|e| format!("SetWindowPos surface: {e:?}"))?;
                }
                if let Some(data) = self.window_data.get_mut(&a(0)) { data.x = x; data.y = y; }
                if a(6) & 4 == 0 { self.windows.position_after(a(0), a(1)).map_err(|e| format!("SetWindowPos order: {e:?}"))?; }
                if a(6) & 0xc0 != 0 {
                    let visible = a(6) & 0x80 == 0;
                    self.windows.show(a(0), visible).map_err(|e| format!("SetWindowPos visibility: {e:?}"))?;
                    if let Some(data) = self.window_data.get_mut(&a(0)) { if visible { data.style |= 0x10000000; } else { data.style &= !0x10000000; } }
                }
                if resized {
                    self.windows.invalidate(a(0)).map_err(|e| format!("SetWindowPos repaint: {e:?}"))?;
                    self.invalid_rects.remove(&a(0));
                    let procedure = self.windows.get(a(0)).map_err(|e| format!("SetWindowPos procedure: {e:?}"))?.procedure;
                    if procedure != 0xffff_ff01 && procedure != 0xffff_ff02 {
                        let action = self.windows.send(Message { window: a(0), message: 5, wparam: 0, lparam: (width & 0xffff) | ((height & 0xffff) << 16) }).map_err(|e| format!("SetWindowPos size notification: {e:?}"))?;
                        return self.action(action, Finish::FixedReturn(1));
                    }
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
                if !self.window_data.contains_key(&a(0)) { self.thread.last_error = 1400; return Ok(Effect::Return(0)); }
                return self.call("user32", "SendMessageA", &[a(0), 0x0c, 0, a(1)], memory);
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
                    | 0x0030_0000 // WS_HSCROLL | WS_VSCROLL
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
                let class = self.classes.keys().find(|name| name.eq_ignore_ascii_case(&class)).cloned().unwrap_or(class);
                let builtin = class.to_ascii_uppercase();
                if !self.classes.contains_key(&class) && matches!(builtin.as_slice(), b"BUTTON" | b"EDIT" | b"STATIC" | b"LISTBOX") {
                    // Controls can be created by their parent's WM_CREATE before
                    // the parent creation continuation allocates its surface.
                    if a(8) != 0 && self.gdi.surface(a(8)).is_none() {
                        let parent = self.windows.get(a(8)).map_err(|e| format!("control parent: {e:?}"))?;
                        self.gdi.create_surface(a(8), parent.width, parent.height)
                            .map_err(|e| format!("control parent surface: {e:?}"))?;
                    }
                    let window = self.windows.create_builtin(0xffff_ff02, a(8), a(9), a(4) as i32, a(5) as i32, a(6).max(1), a(7).max(1), a(3))
                        .map_err(|e| format!("create built-in control: {e:?}"))?;
                    self.gdi.create_surface(window, a(6).max(1), a(7).max(1))
                        .map_err(|e| format!("control surface: {e:?}"))?;
                    self.window_data.insert(window, WindowData {
                        button: crate::buttons::ButtonState::default(), dialog_procedure: 0,
                        class: builtin, instance: a(10), parent: a(8), id: a(9), style: a(3), ex_style: a(0),
                        user_data: 0, extra: Vec::new(), x: a(4) as i32, y: a(5) as i32,
                        text: if a(2) == 0 { Vec::new() } else { string(memory, a(2))? },
                        restore: None,
                    });
                    self.render_static(window)?;
                    return Ok(Effect::Return(window));
                }
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
                let (mut left, mut top, mut right, mut bottom) =
                    classic_window_margins(a(3), a(9) != 0);
                if a(0) & (0x0000_0200 | 0x0000_0100) != 0 {
                    left += 2;
                    top += 2;
                    right += 2;
                    bottom += 2;
                } else if a(0) & 1 != 0 {
                    left += 3;
                    top += 3;
                    right += 3;
                    bottom += 3;
                }
                // Windows permits zero-sized windows during creation; callers
                // commonly position or resize them after the create callback.
                // Keep a minimal backing surface until that later resize.
                let client_width = a(6).saturating_sub((left + right) as u32).max(1);
                let client_height = a(7).saturating_sub((top + bottom) as u32).max(1);
                let action = self
                    .windows
                    .create(&class, client_width, client_height, parameter)
                    .map_err(|e| format!("create window class {}: {e:?}", String::from_utf8_lossy(&class)))?;
                let window = match &action {
                    Action::Invoke { args, .. } => args[0],
                    Action::Return(window) => *window,
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
                        button: crate::buttons::ButtonState::default(),
                        dialog_procedure: 0,
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
                        restore: None,
                    },
                );
                if a(9) != 0 {
                    self.menus.insert(window, a(9));
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
                let style = self
                    .window_data
                    .get(&a(0))
                    .map_or(window.style, |data| data.style);
                let has_menu = self.menus.contains_key(&a(0));
                let (x, y) = self
                    .windows
                    .screen_origin(a(0))
                    .ok_or("invalid window origin")?;
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
                memory
                    .write(a(1), &raw)
                    .map_err(|e| format!("GetWindowRect RECT write: {e:?}"))?;
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
                // Save the normal geometry before maximizing so SW_RESTORE can
                // return to it. Win32 keeps this in WINDOWPLACEMENT.
                if matches!(
                    a(1),
                    crate::window::SW_MAXIMIZE
                ) {
                    if let Ok(window) = self.windows.get(a(0)) {
                        if let Some(data) = self.window_data.get_mut(&a(0)) {
                            if data.restore.is_none() {
                                data.restore = Some((window.x, window.y, window.width, window.height));
                            }
                        }
                    }
                }
                let previous = match self.windows.show_window(a(0), a(1)) {
                    Ok(was_visible) => was_visible as u32,
                    Err(crate::window::WindowError::InvalidHandle) => {
                        // ShowWindow reports prior visibility through its
                        // return value and does not define last-error state.
                        0
                    }
                    Err(e) => return Err(format!("show: {e:?}")),
                };
                // Restore the saved geometry when leaving a minimized or
                // maximized state.
                if matches!(a(1), crate::window::SW_RESTORE | crate::window::SW_SHOWNORMAL) {
                    if let Some((x, y, width, height)) =
                        self.window_data.get(&a(0)).and_then(|data| data.restore)
                    {
                        self.windows
                            .move_window(a(0), x, y, width, height)
                            .map_err(|e| format!("restore geometry: {e:?}"))?;
                        self.gdi
                            .resize_surface(a(0), width, height)
                            .map_err(|e| format!("restore surface: {e:?}"))?;
                        if let Some(data) = self.window_data.get_mut(&a(0)) {
                            data.x = x;
                            data.y = y;
                            data.restore = None;
                        }
                    }
                }
                // Notify the guest after the state transition. The page's CSS
                // mirror and the guest's window procedure both consume these.
                let state = self.windows.get(a(0)).map(|window| {
                    (
                        if window.minimized {
                            crate::window::SIZE_MINIMIZED
                        } else if window.maximized {
                            crate::window::SIZE_MAXIMIZED
                        } else {
                            crate::window::SIZE_RESTORED
                        },
                        if window.minimized {
                            (0, 0)
                        } else {
                            (window.width, window.height)
                        },
                        window.visible,
                    )
                });
                if let Ok((size_param, (width, height), visible)) = state {
                    self.windows
                        .post(Message {
                            window: a(0),
                            message: crate::window::WM_SIZE,
                            wparam: size_param,
                            lparam: (width.min(u16::MAX as u32)) | (height.min(u16::MAX as u32) << 16),
                        })
                        .map_err(|e| format!("post WM_SIZE after ShowWindow: {e:?}"))?;
                    self.windows
                        .post(Message {
                            window: a(0),
                            message: crate::window::WM_SHOWWINDOW,
                            wparam: visible as u32,
                            lparam: 0,
                        })
                        .map_err(|e| format!("post WM_SHOWWINDOW after ShowWindow: {e:?}"))?;
                }
                previous
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
            ("user32", "IsWindowVisible") => self.windows.is_visible(a(0)) as u32,
            ("user32", "CharUpperA" | "CharLowerA" | "CharUpperBuffA" | "CharLowerBuffA") => {
                let uppercase = name.starts_with("CharUpper");
                let buffer = name.contains("Buff");
                if !buffer && a(0) >> 16 == 0 {
                    return Ok(Effect::Return(
                        (a(0) & 0xff00) | crate::locale::ansi_case(a(0) as u8, uppercase) as u32,
                    ));
                }
                let mut bytes = if buffer {
                    if a(0) as u64 + a(1) as u64 > memory.byte_len() {
                        return Err("case-conversion buffer outside memory".into());
                    }
                    let mut bytes = Vec::new();
                    bytes
                        .try_reserve_exact(a(1) as usize)
                        .map_err(|_| "case-conversion allocation failed")?;
                    bytes.resize(a(1) as usize, 0);
                    memory
                        .read(a(0), &mut bytes)
                        .map_err(|e| format!("case-conversion input: {e:?}"))?;
                    bytes
                } else {
                    string(memory, a(0))?
                };
                for byte in &mut bytes {
                    *byte = crate::locale::ansi_case(*byte, uppercase);
                }
                memory
                    .write(a(0), &bytes)
                    .map_err(|e| format!("case-conversion output: {e:?}"))?;
                if buffer {
                    a(1)
                } else {
                    a(0)
                }
            }
            ("user32", "IsWindow") => self.windows.get(a(0)).is_ok() as u32,
            ("user32", "IsIconic" | "IsZoomed") => self.windows.get(a(0)).is_ok_and(|window| {
                let style = self
                    .window_data
                    .get(&a(0))
                    .map_or(window.style, |data| data.style);
                let flag = if name == "IsIconic" {
                    0x20000000
                } else {
                    0x01000000
                };
                style & flag != 0
            }) as u32,
            ("user32", "EnableWindow") => {
                let Ok(window) = self.windows.get(a(0)) else {
                    self.thread.last_error = 1400;
                    return Ok(Effect::Return(0));
                };
                let style = self
                    .window_data
                    .get(&a(0))
                    .map_or(window.style, |data| data.style);
                let previous_disabled = (style & 0x08000000 != 0) as u32;
                let enabled = a(1) != 0;
                if enabled == (previous_disabled == 0) {
                    return Ok(Effect::Return(previous_disabled));
                }
                if window.procedure >= 0xffff_ff00 {
                    self.set_window_enabled(a(0), enabled)?;
                    return Ok(Effect::Return(previous_disabled));
                }
                if enabled {
                    self.set_window_enabled(a(0), true)?;
                    let action = self
                        .windows
                        .send(Message {
                            window: a(0),
                            message: 10,
                            wparam: 1,
                            lparam: 0,
                        })
                        .map_err(|e| format!("EnableWindow notification: {e:?}"))?;
                    return self.action(action, Finish::FixedReturn(previous_disabled));
                }
                let action = self
                    .windows
                    .send(Message {
                        window: a(0),
                        message: 31,
                        wparam: 0,
                        lparam: 0,
                    })
                    .map_err(|e| format!("EnableWindow cancel: {e:?}"))?;
                return self.action(
                    action,
                    Finish::DisableAfterCancel {
                        window: a(0),
                        previous_disabled,
                    },
                );
            }
            ("user32", "IsWindowEnabled") => {
                self.windows.get(a(0)).is_ok_and(|window| {
                    let style = self
                        .window_data
                        .get(&a(0))
                        .map_or(window.style, |data| data.style);
                    style & 0x0800_0000 == 0 // WS_DISABLED
                }) as u32
            }
            ("user32", "DefWindowProcA") => {
                if matches!(a(1), 0x0c..=0x0e) { return self.static_window_proc(a(0), a(1), a(2), a(3), memory).map(Effect::Return); }
                if a(1) == crate::window::WM_PAINT {
                    self.windows.validate_paint(a(0));
                }
                // WM_CLOSE: the default procedure destroys the window. The
                // guest's window procedure may handle WM_CLOSE first and show a
                // save-changes prompt; only unhandled close reaches here.
                if a(1) == crate::window::WM_CLOSE {
                    return self.call("user32", "DestroyWindow", &[a(0)], memory);
                }
                // WM_SYSCOMMAND: translate the system-menu command into the same
                // ShowWindow path the guest would call directly.
                if a(1) == crate::window::WM_SYSCOMMAND {
                    match a(2) & 0xfff0 {
                        crate::window::SC_CLOSE => {
                            return self.call("user32", "DestroyWindow", &[a(0)], memory);
                        }
                        crate::window::SC_MINIMIZE => {
                            return self.call("user32", "ShowWindow", &[a(0), crate::window::SW_MINIMIZE], memory);
                        }
                        crate::window::SC_MAXIMIZE => {
                            return self.call("user32", "ShowWindow", &[a(0), crate::window::SW_MAXIMIZE], memory);
                        }
                        crate::window::SC_RESTORE => {
                            return self.call("user32", "ShowWindow", &[a(0), crate::window::SW_RESTORE], memory);
                        }
                        _ => {}
                    }
                }
                Windows::default_result(a(1))
            }
            ("user32", "PostQuitMessage") => {
                self.windows.post_quit(a(0));
                0
            }
            ("user32", "DestroyWindow") => {
                if let Ok(window) = self.windows.get(a(0)) {
                    if matches!(window.procedure, 0xffff_ff02 | 0xffff_ff01 | DEFAULT_WINDOW_PROC) {
                        // Built-in controls have no guest procedure, so there
                        // is no WM_DESTROY callback to make.
                        self.window_data.remove(&a(0));
                        if let Some(dc) = self.own_dcs.remove(&a(0)) {
                            self.common_dcs.remove(&dc);
                            let _ = self.gdi.delete_dc(dc);
                        }
                        self.gl_context.window_destroyed(a(0));
                        self.properties.retain(|(handle, _), _| *handle != a(0));
                        let _ = self.gdi.remove_surface(a(0));
                        self.windows.remove(a(0));
                        return Ok(Effect::Return(1));
                    }
                }
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
                    let [left, top, right, bottom] = read_rect(memory, a(1))?;
                    let w = self.windows.get(a(0)).map_err(|e| format!("invalidation window: {e:?}"))?;
                    let rect = Rect { left: left.max(0), top: top.max(0), right: right.min(w.width as i32), bottom: bottom.min(w.height as i32) };
                    if rect.left >= rect.right || rect.top >= rect.bottom { return Ok(Effect::Return(1)); }
                    if !w.invalid || self.invalid_rects.contains_key(&a(0)) {
                        self.invalid_rects.entry(a(0)).and_modify(|old| {
                            old.left = old.left.min(rect.left); old.top = old.top.min(rect.top);
                            old.right = old.right.max(rect.right); old.bottom = old.bottom.max(rect.bottom);
                        }).or_insert(rect);
                    }
                } else {
                    self.invalid_rects.remove(&a(0));
                }
                self.windows
                    .invalidate(a(0))
                    .map_err(|e| format!("invalidate: {e:?}"))?;
                1
            }
            ("user32", "RedrawWindow") => {
                if a(1) != 0
                    || a(2) != 0
                    || a(3) & !0x105 != 0
                    || (a(3) & 4 != 0 && a(3) & 0x101 != 0x101)
                {
                    return Err("RedrawWindow: only whole-window invalidation and immediate update supported".into());
                }
                // A NULL handle means the desktop. This host draws a single
                // interactive guest, so the visible foreground window is the
                // window the desktop redraw can actually reach.
                let window = if a(0) == 0 {
                    self.windows.foreground_window()
                } else {
                    a(0)
                };
                if window == 0 {
                    return Ok(Effect::Return(1));
                }
                if a(3) & 1 != 0 {
                    self.windows
                        .invalidate(window)
                        .map_err(|e| format!("redraw: {e:?}"))?;
                }
                if a(3) & 0x100 != 0 {
                    if a(3) & 5 == 5 {
                        let Effect::Return(dc) = self.call("user32", "GetDC", &[window], memory)?
                        else {
                            unreachable!()
                        };
                        let action = self
                            .windows
                            .send(Message {
                                window,
                                message: 20,
                                wparam: dc,
                                lparam: 0,
                            })
                            .map_err(|e| format!("redraw erase: {e:?}"))?;
                        return self.action(action, Finish::Redraw { window, dc });
                    }
                    return self.call("user32", "UpdateWindow", &[window], memory);
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
                // Built-in controls have no guest window procedure: their
                // sentinel must be handled in the host, exactly as
                // SendMessageA/DispatchMessageA already do. Routing WM_PAINT
                // to the sentinel called into guest data and trapped.
                if w.procedure == 0xffff_ff02 {
                    return self.control_effect(a(0), 15, 0, 0, memory);
                }
                if w.procedure == 0xffff_ff01 || w.procedure == DEFAULT_WINDOW_PROC {
                    self.windows.validate_paint(a(0));
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
                if name == "DispatchMessageA" && message.window == MULTIMEDIA_TIMER_WINDOW {
                    let Some((callback, user, periodic)) =
                        self.multimedia_timers.get(&message.wparam).map(|timer| {
                            (timer.callback, timer.user, timer.periodic)
                        })
                    else {
                        return Ok(Effect::Return(0));
                    };
                    if !periodic {
                        self.multimedia_timers.remove(&message.wparam);
                    }
                    if callback == 0 {
                        return Ok(Effect::Return(0));
                    }
                    return self.callback(
                        callback,
                        vec![message.wparam, 0, user, 0, 0],
                        None,
                        CallbackFinish::Return,
                    );
                }
                if self.windows.get(message.window).is_err() {
                    return Ok(Effect::Return(0));
                }
                if let Ok(window) = self.windows.get(message.window) {
                    if window.procedure == 0xffff_ff02 {
                        return self.control_effect(
                            message.window,
                            message.message,
                            message.wparam,
                            message.lparam,
                            memory,
                        );
                    }
                    if window.procedure == 0xffff_ff01 {
                        if message.message == 15 {
                            self.windows.validate_paint(message.window);
                        }
                        return Ok(Effect::Return(Windows::default_result(message.message)));
                    }
                    if window.procedure == DEFAULT_WINDOW_PROC {
                        if message.message == 15 {
                            self.windows.validate_paint(message.window);
                        }
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
                        Some(if shifted {
                            virtual_key
                        } else {
                            virtual_key + 0x20
                        })
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
                let rect = self.invalid_rects.remove(&a(0)).unwrap_or(Rect { left: 0, top: 0, right: width as i32, bottom: height as i32 });
                let dc = self
                    .gdi
                    .paint_dc(
                        a(0),
                        rect,
                    )
                    .map_err(|e| format!("paint DC: {e:?}"))?;
                let mut raw = [0u8; 64];
                raw[..4].copy_from_slice(&dc.to_le_bytes());
                for (index, value) in [rect.left, rect.top, rect.right, rect.bottom].iter().enumerate() {
                    raw[8 + index * 4..12 + index * 4].copy_from_slice(&value.to_le_bytes());
                }
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
                if self.gdi.is_meta_dc(a(0)) == Some(false) {
                    return Ok(Effect::Return(0));
                }
                if a(0) != 0 && !self.gdi.is_dc(a(0)) {
                    // A deleted or bogus reference DC makes the call fail.
                    return Ok(Effect::Return(0));
                }
                let stock_bits = self.allocate(memory, 4)?;
                fill_guest(memory, stock_bits, 0, 4)?;
                self.gdi
                    .memory_dc_with_bits(stock_bits)
                    .map_err(|e| format!("create DC: {e:?}"))?
            }
            ("gdi32", "GetStockObject") if a(0) == 15 => {
                if let Some(handle) = self.default_palette {
                    return Ok(Effect::Return(handle));
                }
                let entries = DEFAULT_PALETTE_COLORS
                    .into_iter()
                    .map(|rgb| [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0])
                    .collect();
                let handle = self
                    .palettes
                    .insert(entries)
                    .ok_or("palette handle table full")?;
                self.default_palette = Some(handle);
                handle
            }
            ("gdi32", "GetStockObject") => self
                .gdi
                .stock_object(a(0))
                .map_err(|e| format!("stock GDI object: {e:?}"))?,
            ("gdi32", "CreateDIBitmap") => {
                // CreateDIBitmap(hdc, pbmih, fdwInit, pjInit, pbmi, fuUsage)
                self.create_dib_bitmap(memory, a(0), a(1), a(2), a(3), a(4), a(5))?
            }
            ("gdi32", "GetDIBits") => {
                // GetDIBits(hdc, hbm, start, lines, bits, bmi, usage)
                let Some(source) = self.gdi.bitmap_dib(a(1)) else {
                    return Ok(Effect::Return(0));
                };
                if a(5) == 0 || a(6) > 1 {
                    return Ok(Effect::Return(0));
                }
                let header_size = read(memory, a(5))?;
                if header_size != 12 && header_size < 40 {
                    return Ok(Effect::Return(0));
                }
                let core = header_size == 12;
                let (width, height, bpp) = if core {
                    (
                        u16::from_le_bytes([read_byte(memory, a(5) + 4)?, read_byte(memory, a(5) + 5)?]) as u32,
                        u16::from_le_bytes([read_byte(memory, a(5) + 6)?, read_byte(memory, a(5) + 7)?]) as i32,
                        u16::from_le_bytes([read_byte(memory, a(5) + 10)?, read_byte(memory, a(5) + 11)?]),
                    )
                } else {
                    (
                        read(memory, a(5) + 4)?,
                        read(memory, a(5) + 8)? as i32,
                        read(memory, a(5) + 14)? as u16,
                    )
                };
                let compression = if core { 0 } else { read(memory, a(5) + 16)? };
                if bpp == 0 {
                    // A real copy with no target depth fails without touching
                    // the header; otherwise this is a query.
                    if a(4) != 0 && a(3) != 0 {
                        return Ok(Effect::Return(0));
                    }
                    if a(4) != 0 && matches!(compression, 4 | 5) {
                        return Ok(Effect::Return(0));
                    }
                    if core {
                        write_u16(memory, a(5) + 4, source.width as u16)?;
                        write_u16(memory, a(5) + 6, source.height as u16)?;
                        write_u16(memory, a(5) + 8, 1)?;
                        write_u16(memory, a(5) + 10, source.bit_count())?;
                    } else {
                        write(memory, a(5) + 4, source.width)?;
                        write(memory, a(5) + 8, source.height)?;
                        write_u16(memory, a(5) + 12, 1)?;
                        write_u16(memory, a(5) + 14, source.bit_count())?;
                        write(memory, a(5) + 16, 0)?; // BI_RGB
                        write(memory, a(5) + 32, 0)?; // biClrUsed
                    }
                    return Ok(Effect::Return(source.height));
                }
                let valid = match bpp {
                    1 | 24 => compression == 0,
                    4 => compression == 0 || compression == 2,
                    8 => compression == 0 || compression == 1,
                    16 | 32 => compression == 0 || compression == 3,
                    _ => false,
                };
                if !valid {
                    return Ok(Effect::Return(0));
                }
                let rle = (bpp == 4 && compression == 2) || (bpp == 8 && compression == 1);
                if a(3) != 0 && !core {
                    let planes = u16::from_le_bytes([read_byte(memory, a(5) + 12)?, read_byte(memory, a(5) + 13)?]);
                    let rle = (bpp == 4 && compression == 2) || (bpp == 8 && compression == 1);
                    if planes == 0 && !rle {
                        return Ok(Effect::Return(0));
                    }
                }
                if a(3) == 0 {
                    // A zero line count validates the format (planes are
                    // ignored) and reports success.
                    return Ok(Effect::Return(source.height));
                }
                if rle && a(4) != 0 {
                    // Compressed source data cannot be produced by this path.
                    return Ok(Effect::Return(0));
                }
                if a(4) == 0 {
                    // A NULL bits pointer still refreshes the colour table when
                    // the caller supplied a bit depth; biClrUsed resets to 0.
                    if bpp <= 8 {
                        let table = a(5) + if core { 12 } else { 40 };
                        let colors = 1usize << bpp;
                        for index in 0..colors {
                            let color = source
                                .palette()
                                .get(index)
                                .copied()
                                .unwrap_or([0, 0, 0, 255]);
                            if core {
                                memory
                                    .write(table + index as u32 * 3, &[color[2], color[1], color[0]])
                                    .map_err(|e| format!("GetDIBits core color table: {e:?}"))?;
                            } else {
                                memory
                                    .write(
                                        table + index as u32 * 4,
                                        &[color[2], color[1], color[0], 0],
                                    )
                                    .map_err(|e| format!("GetDIBits color table: {e:?}"))?;
                            }
                        }
                        if !core {
                            write(memory, a(5) + 32, 0)?;
                        }
                    }
                    return Ok(Effect::Return(source.height));
                }
                if width == 0 || height == 0 {
                    return Ok(Effect::Return(0));
                }
                let start = a(2);
                if start >= source.height {
                    return Ok(Effect::Return(0));
                }
                let lines = a(3).min(source.height - start);
                let target = match crate::raster::Dib::new(memory, width, lines, a(4), bpp) {
                    Ok(target) => target.with_top_down(height < 0),
                    Err(_) => return Ok(Effect::Return(0)),
                };
                let mut row = vec![0u8; width as usize * 4];
                for line in 0..lines {
                    let y = source.height - 1 - (start + line);
                    source
                        .rgba_row(memory, 0, y, &mut row)
                        .map_err(|e| format!("GetDIBits source row: {e:?}"))?;
                    target
                        .write_rgba_row(memory, 0, line, &row)
                        .map_err(|e| format!("GetDIBits target row: {e:?}"))?;
                }
                if bpp <= 8 {
                    let colors = 1usize << bpp;
                    let table = a(5) + if core { 12 } else { 40 };
                    for index in 0..colors {
                        let color = source
                            .palette()
                            .get(index)
                            .copied()
                            .unwrap_or([0, 0, 0, 255]);
                        if core {
                            memory
                                .write(table + index as u32 * 3, &[color[2], color[1], color[0]])
                                .map_err(|e| format!("GetDIBits core color table: {e:?}"))?;
                        } else {
                            memory
                                .write(
                                    table + index as u32 * 4,
                                    &[color[2], color[1], color[0], 0],
                                )
                                .map_err(|e| format!("GetDIBits color table: {e:?}"))?;
                        }
                    }
                }
                lines
            }
            ("gdi32", "CreateDIBSection") => {
                if a(1) == 0 {
                    if a(3) != 0 {
                        write(memory, a(3), 0)?;
                    }
                    return Ok(Effect::Return(0));
                }
                if a(2) > 1 {
                    return Ok(Effect::Return(0));
                }
                let header_size = read(memory, a(1))?;
                let core = header_size == 12;
                if !core && header_size < 40 {
                    return Ok(Effect::Return(0));
                }
                let (width, height, planes, bit_count, compression, table, entry) = if core {
                    (
                        u16::from_le_bytes([read_byte(memory, a(1) + 4)?, read_byte(memory, a(1) + 5)?]) as u32,
                        u16::from_le_bytes([read_byte(memory, a(1) + 6)?, read_byte(memory, a(1) + 7)?]) as i32,
                        u16::from_le_bytes([read_byte(memory, a(1) + 8)?, read_byte(memory, a(1) + 9)?]),
                        u16::from_le_bytes([read_byte(memory, a(1) + 10)?, read_byte(memory, a(1) + 11)?]),
                        0u32,
                        a(1) + 12,
                        3u32,
                    )
                } else {
                    (
                        read(memory, a(1) + 4)?,
                        read(memory, a(1) + 8)? as i32,
                        u16::from_le_bytes([read_byte(memory, a(1) + 12)?, read_byte(memory, a(1) + 13)?]),
                        u16::from_le_bytes([read_byte(memory, a(1) + 14)?, read_byte(memory, a(1) + 15)?]),
                        read(memory, a(1) + 16)?,
                        a(1) + 40,
                        4u32,
                    )
                };
                if width == 0
                    || height == 0
                    || height == i32::MIN
                    || planes == 0
                    || (planes != 1 && planes as u32 * bit_count as u32 > 16)
                {
                    return Ok(Effect::Return(0));
                }
                let valid = match bit_count {
                    1 | 4 | 8 | 24 => compression == 0,
                    16 | 32 => compression == 0 || compression == 3,
                    _ => false,
                };
                if !valid || (compression == 3 && a(2) == 1) {
                    return Ok(Effect::Return(0));
                }
                if compression == 3 {
                    let masks = [
                        read(memory, a(1) + 40)?,
                        read(memory, a(1) + 44)?,
                        read(memory, a(1) + 48)?,
                    ];
                    if masks.contains(&0) {
                        return Ok(Effect::Return(0));
                    }
                }
                let rows = height.unsigned_abs();
                let stride_u64 = (width as u64 * bit_count as u64 + 31) / 32 * 4;
                let size_u64 = match stride_u64.checked_mul(rows as u64) {
                    Some(size) => size,
                    None => return Ok(Effect::Return(0)),
                };
                let (Ok(stride), Ok(size)) = (u32::try_from(stride_u64), u32::try_from(size_u64)) else {
                    return Ok(Effect::Return(0));
                };
                if size_u64 > 0x8000_0000 {
                    return Ok(Effect::Return(0));
                }
                let bits = match self.allocate(memory, size.max(1)) {
                    Ok(bits) => bits,
                    Err(_) => return Ok(Effect::Return(0)),
                };
                fill_guest(memory, bits, 0, size)?;
                let mut palette = Vec::new();
                if bit_count <= 8 {
                    let colors = if core {
                        1usize << bit_count
                    } else {
                        let used = read(memory, a(1) + 32)? as usize;
                        if used == 0 {
                            1usize << bit_count
                        } else {
                            used.min(1usize << bit_count)
                        }
                    };
                    if a(2) == 1 {
                        // DIB_PAL_COLORS: the table holds WORD indexes into the
                        // DC's logical palette as it was when the section was
                        // created (or the default system palette).
                        let entries = self
                            .gdi
                            .dc_state(a(0))
                            .ok()
                            .and_then(|state| state.palette)
                            .and_then(|palette| self.palettes.get(palette))
                            .cloned()
                            .or_else(|| {
                                self.default_palette
                                    .and_then(|palette| self.palettes.get(palette))
                                    .cloned()
                            })
                            .unwrap_or_default();
                        let full = 1usize << bit_count;
                        for index in 0..full {
                            if index >= colors {
                                // Entries beyond biClrUsed stay black; Windows
                                // still reports the full palette size.
                                palette.push([0, 0, 0, 255]);
                                continue;
                            }
                            let mut value = [0u8; 2];
                            memory
                                .read(table + index as u32 * 2, &mut value)
                                .map_err(|e| format!("CreateDIBSection palette index: {e:?}"))?;
                            let palette_index = u16::from_le_bytes(value) as usize;
                            let entry = if entries.is_empty() {
                                [0, 0, 0, 255]
                            } else {
                                entries[palette_index % entries.len()]
                            };
                            palette.push([entry[0], entry[1], entry[2], 255]);
                        }
                    } else {
                        for index in 0..colors {
                            let mut entry_bytes = vec![0u8; entry as usize];
                            memory
                                .read(table + index as u32 * entry, &mut entry_bytes)
                                .map_err(|e| format!("CreateDIBSection palette: {e:?}"))?;
                            // Both RGBQUAD and RGBTRIPLE store blue, green, red.
                            palette.push([entry_bytes[2], entry_bytes[1], entry_bytes[0], 255]);
                        }
                    }
                }
                let dib = crate::raster::Dib::section(
                    memory,
                    width,
                    rows,
                    bits,
                    bit_count,
                    compression,
                    height < 0,
                    palette,
                )
                .map_err(|e| format!("CreateDIBSection: {e:?}"))?;
                let bitmap = self
                    .gdi
                    .insert_bitmap_dib(dib)
                    .map_err(|e| format!("CreateDIBSection object: {e:?}"))?;
                self.bitmap_allocations.insert(bitmap, bits);
                if a(3) != 0 {
                    write(memory, a(3), bits)?;
                }
                bitmap
            }
            ("gdi32", "SelectObject") => {
                if a(1) == 0 {
                    return Ok(Effect::Return(0));
                }
                match self.gdi.select(a(0), a(1)) {
                    Ok(previous) => previous,
                    Err(
                        crate::gdi::Error::InvalidHandle
                        | crate::gdi::Error::WrongType
                        | crate::gdi::Error::SelectedObject,
                    ) => 0,
                    Err(e) => return Err(format!("select: {e:?}")),
                }
            }

            ("gdi32", "DeleteObject") if a(0) == 0 => 0,
            ("gdi32", "DeleteObject") => {
                if self.default_palette == Some(a(0)) {
                    return Ok(Effect::Return(1));
                }
                if self.palettes.get(a(0)).is_some() && self.gdi.references_object(a(0)) {
                    return Ok(Effect::Return(0));
                }
                if self.palettes.remove(a(0)).is_some() {
                    return Ok(Effect::Return(1));
                }
                match self.gdi.delete_object(a(0)) {
                    Ok(()) => {}
                    Err(
                        crate::gdi::Error::InvalidHandle
                        | crate::gdi::Error::WrongType
                        | crate::gdi::Error::SelectedObject,
                    ) => return Ok(Effect::Return(0)),
                    Err(e) => return Err(format!("delete object: {e:?}")),
                }
                if let Some(bits) = self.bitmap_allocations.remove(&a(0)) {
                    self.arena.release(bits)?;
                }
                1
            }
            ("gdi32", "DeleteDC") => {
                if self.class_dcs.values().any(|dc| *dc == a(0))
                    || self.own_dcs.values().any(|dc| *dc == a(0))
                {
                    // A class/private DC cannot be deleted before its owner.
                    return Ok(Effect::Return(1));
                }
                if !self.gdi.is_dc(a(0)) {
                    self.thread.last_error = 6;
                    0
                } else {
                    self.gdi
                        .delete_dc(a(0))
                        .map_err(|e| format!("delete DC: {e:?}"))?;
                    1
                }
            }
            ("gdi32", "FixBrushOrgEx") => 0,
            ("gdi32", "SetBrushOrgEx" | "GetBrushOrgEx") => {
                let state = self
                    .gdi
                    .dc_state_mut(a(0))
                    .map_err(|e| format!("brush origin: {e:?}"))?;
                let previous = state.brush_origin;
                let pointer = if name == "SetBrushOrgEx" { a(3) } else { a(1) };
                if pointer == 0 && name == "GetBrushOrgEx" {
                    return Ok(Effect::Return(0));
                }
                if pointer != 0 {
                    write(memory, pointer, previous.0 as u32)?;
                    write(
                        memory,
                        pointer
                            .checked_add(4)
                            .ok_or("brush origin output overflow")?,
                        previous.1 as u32,
                    )?;
                }
                if name == "SetBrushOrgEx" {
                    state.brush_origin = (a(1) as i32, a(2) as i32);
                }
                1
            }
            ("gdi32", "StretchBlt") => {
                match self.gdi.stretch_blt(
                    memory,
                    a(0),
                    a(1) as i32,
                    a(2) as i32,
                    a(3) as i32,
                    a(4) as i32,
                    a(5),
                    a(6) as i32,
                    a(7) as i32,
                    a(8) as i32,
                    a(9) as i32,
                    a(10),
                ) {
                    Ok(()) => 1,
                    Err(crate::gdi::Error::InvalidDimensions) => 0,
                    Err(error) => return Err(format!("StretchBlt: {error:?}")),
                }
            }
            ("gdi32", "StretchDIBits") => {
                if a(10) == 0 || a(9) == 0 || a(11) > 2 {
                    return Ok(Effect::Return(0));
                }
                let size = read(memory, a(10))?;
                if size < 40 {
                    return Ok(Effect::Return(0));
                }
                let width = read(memory, a(10) + 4)? as i32;
                let height = read(memory, a(10) + 8)? as i32;
                let planes = u16::from_le_bytes([
                    read_byte(memory, a(10) + 12)?,
                    read_byte(memory, a(10) + 13)?,
                ]);
                let bit_count = u16::from_le_bytes([
                    read_byte(memory, a(10) + 14)?,
                    read_byte(memory, a(10) + 15)?,
                ]);
                let compression = read(memory, a(10) + 16)?;
                let size_image = read(memory, a(10) + 20)?;
                let rle = (bit_count == 4 && compression == 2) || (bit_count == 8 && compression == 1);
                if width <= 0 || height == 0 || (planes == 0 && !rle) {
                    return Ok(Effect::Return(0));
                }
                let valid = match bit_count {
                    1 | 24 => compression == 0,
                    4 => compression == 0 || compression == 2,
                    8 => compression == 0 || compression == 1,
                    16 | 32 => compression == 0 || compression == 3,
                    _ => false,
                };
                if !valid || (rle && size_image == 0) {
                    return Ok(Effect::Return(0));
                }
                if rle {
                    return Ok(Effect::Return((a(8) as i32).unsigned_abs().max(1)));
                }
                match self.gdi.stretch_dib(
                    memory,
                    a(0),
                    a(1) as i32,
                    a(2) as i32,
                    a(3) as i32,
                    a(4) as i32,
                    a(5) as i32,
                    a(6) as i32,
                    a(7) as i32,
                    a(8) as i32,
                    a(9),
                    a(10),
                    a(12),
                ) {
                    Ok(()) => (a(8) as i32).unsigned_abs(),
                    Err(crate::gdi::Error::InvalidDimensions)
                    | Err(crate::gdi::Error::UnsupportedOperation) => 0,
                    Err(crate::gdi::Error::Raster(_)) => 0,
                    Err(error) => return Err(format!("StretchDIBits: {error:?}")),
                }
            }
            ("gdi32", "MoveToEx" | "GetCurrentPositionEx") => {
                let previous = self
                    .gdi
                    .dc_state(a(0))
                    .map_err(|e| format!("current position: {e:?}"))?
                    .current_position;
                let pointer = if name == "MoveToEx" { a(3) } else { a(1) };
                if pointer != 0 {
                    write(memory, pointer, previous.0 as u32)?;
                    write(
                        memory,
                        pointer.checked_add(4).ok_or("position output overflow")?,
                        previous.1 as u32,
                    )?;
                } else if name == "GetCurrentPositionEx" {
                    return Ok(Effect::Return(0));
                }
                if name == "MoveToEx" {
                    self.gdi.dc_state_mut(a(0)).unwrap().current_position =
                        (a(1) as i32, a(2) as i32);
                }
                1
            }
            ("gdi32", "GetPixel" | "SetPixel" | "SetPixelV") => {
                let result = if name == "GetPixel" {
                    self.gdi.get_pixel(memory, a(0), a(1) as i32, a(2) as i32)
                } else {
                    let color = a(3);
                    let index = match color >> 24 {
                        0x10 => Some((color & 0xffff) as usize),
                        0x01 => {
                            let palette = self
                                .gdi
                                .dc_state(a(0))
                                .ok()
                                .and_then(|state| state.palette)
                                .and_then(|palette| self.palettes.get(palette))
                                .or_else(|| {
                                    self.default_palette
                                        .and_then(|palette| self.palettes.get(palette))
                                });
                            let entry = palette
                                .filter(|entries| !entries.is_empty())
                                .map(|entries| {
                                    entries
                                        .get((color & 0xffff) as usize)
                                        .copied()
                                        .unwrap_or(entries[0])
                                })
                                .unwrap_or([0, 0, 0, 255]);
                            let rgb = u32::from(entry[0])
                                | (u32::from(entry[1]) << 8)
                                | (u32::from(entry[2]) << 16);
                            self.gdi
                                .dib_nearest_index(a(0), rgb)
                                .map(|index| index as usize)
                        }
                        0x02 => self
                            .gdi
                            .dib_nearest_index(a(0), color & 0xffffff)
                            .map(|index| index as usize),
                        _ => None,
                    };
                    if let Some(index) = index {
                        self.gdi
                            .set_pixel_index(memory, a(0), a(1) as i32, a(2) as i32, index)
                    } else {
                        self.gdi.set_pixel(
                            memory,
                            a(0),
                            a(1) as i32,
                            a(2) as i32,
                            color & 0xffffff,
                        )
                    }
                };
                match result {
                    Ok(value) => {
                        if name == "SetPixelV" {
                            1
                        } else {
                            value
                        }
                    }
                    Err(
                        crate::gdi::Error::InvalidHandle
                        | crate::gdi::Error::WrongType
                        | crate::gdi::Error::InvalidDimensions,
                    ) => {
                        if name == "SetPixelV" {
                            0
                        } else {
                            u32::MAX
                        }
                    }
                    Err(error) => return Err(format!("pixel operation: {error:?}")),
                }
            }
            ("gdi32", "LineTo") => {
                self.gdi
                    .line_to(memory, a(0), a(1) as i32, a(2) as i32)
                    .map_err(|e| format!("LineTo: {e:?}"))?;
                1
            }
            ("gdi32", "Rectangle") => {
                self.gdi
                    .rectangle(
                        memory,
                        a(0),
                        Rect {
                            left: a(1) as i32,
                            top: a(2) as i32,
                            right: a(3) as i32,
                            bottom: a(4) as i32,
                        },
                    )
                    .map_err(|e| format!("Rectangle: {e:?}"))?;
                1
            }
            ("gdi32", "PatBlt") => {
                self.gdi
                    .pat_blt(
                        memory,
                        a(0),
                        a(1) as i32,
                        a(2) as i32,
                        a(3) as i32,
                        a(4) as i32,
                        a(5),
                    )
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
            // MSVCRT is a host layer, like the Win32 APIs. The common C
            // runtime entry points are replaced here instead of requiring a
            // separately recompiled CRT DLL. The C ABI is cdecl; the signature
            // table leaves the guest's argument cleanup to the guest.
            ("msvcrt", "malloc") => match self.arena.allocate(a(0).max(1), memory.byte_len()) {
                Ok(address) => address,
                Err(_) => 0,
            },
            ("msvcrt", "free") => {
                if a(0) != 0 {
                    let _ = self.arena.release(a(0));
                }
                0
            }
            ("msvcrt", "calloc") => {
                let Some(total) = a(0).checked_mul(a(1)) else {
                    return Ok(Effect::Return(0));
                };
                match self.arena.allocate(total.max(1), memory.byte_len()) {
                    Ok(address) => {
                        let zero = [0u8; 4096];
                        let mut offset = 0;
                        while offset < total {
                            let length = (total - offset).min(zero.len() as u32) as usize;
                            memory
                                .write(address + offset, &zero[..length])
                                .map_err(|e| format!("calloc: {e:?}"))?;
                            offset += length as u32;
                        }
                        address
                    }
                    Err(_) => 0,
                }
            }
            ("msvcrt", "realloc") => {
                let pointer = a(0);
                let size = a(1);
                if pointer == 0 {
                    return Ok(Effect::Return(
                        match self.arena.allocate(size.max(1), memory.byte_len()) {
                            Ok(address) => address,
                            Err(_) => 0,
                        },
                    ));
                }
                if size == 0 {
                    let _ = self.arena.release(pointer);
                    return Ok(Effect::Return(0));
                }
                let old_size = self.arena.size(pointer).unwrap_or(0) as u32;
                let Some(address) = self.arena.allocate(size, memory.byte_len()).ok() else {
                    return Ok(Effect::Return(0));
                };
                let mut buffer = [0u8; 4096];
                let copy = old_size.min(size);
                let mut offset = 0;
                while offset < copy {
                    let length = (copy - offset).min(buffer.len() as u32) as usize;
                    memory
                        .read(pointer + offset, &mut buffer[..length])
                        .map_err(|e| format!("realloc read: {e:?}"))?;
                    memory
                        .write(address + offset, &buffer[..length])
                        .map_err(|e| format!("realloc write: {e:?}"))?;
                    offset += length as u32;
                }
                let _ = self.arena.release(pointer);
                address
            }
            ("msvcrt", "memset") => {
                fill_guest(memory, a(0), (a(1) & 0xff) as u8, a(2))?;
                a(0)
            }
            ("msvcrt", "memcpy") => {
                copy_guest(memory, a(0), a(1), a(2))?;
                a(0)
            }
            ("msvcrt", "memmove") => {
                move_guest(memory, a(0), a(1), a(2))?;
                a(0)
            }
            ("msvcrt", "memcmp") => compare_guest(memory, a(0), a(1), a(2))?,
            ("msvcrt", "strlen") => read_c_string(memory, a(0))?.len() as u32,
            ("msvcrt", "strcpy") => {
                let mut text = read_c_string(memory, a(1))?;
                text.push(0);
                memory
                    .write(a(0), &text)
                    .map_err(|e| format!("strcpy: {e:?}"))?;
                a(0)
            }
            ("msvcrt", "strcmp" | "strncmp") => {
                let limit = if name == "strncmp" { a(2) } else { u32::MAX };
                let left = read_c_string(memory, a(0))?;
                let right = read_c_string(memory, a(1))?;
                let mut result = 0i32;
                for index in 0..limit as usize {
                    let left_byte = left.get(index).copied().unwrap_or(0);
                    let right_byte = right.get(index).copied().unwrap_or(0);
                    result = left_byte as i32 - right_byte as i32;
                    if result != 0 || (left_byte == 0 && right_byte == 0) {
                        break;
                    }
                }
                result as u32
            }
            ("msvcrt", "strchr" | "strrchr") => {
                let text = read_c_string(memory, a(0))?;
                let wanted = (a(1) & 0xff) as u8;
                let found = if name == "strchr" {
                    text.iter().position(|byte| *byte == wanted)
                } else {
                    text.iter().rposition(|byte| *byte == wanted)
                };
                let found = found.or_else(|| (wanted == 0).then_some(text.len()));
                found.map_or(0, |index| a(0) + index as u32)
            }
            ("msvcrt", "atoi") => parse_atoi(&read_c_string(memory, a(0))?) as u32,
            ("msvcrt", "abs") => (a(0) as i32).unsigned_abs(),
            ("msvcrt", "??2@YAPAXI@Z") => {
                // operator new(unsigned int): the guest heap doubles as malloc.
                match self.arena.allocate(a(0).max(1), memory.byte_len()) {
                    Ok(address) => address,
                    Err(_) => 0,
                }
            }
            ("msvcrt", "??3@YAXPAX@Z") => {
                // operator delete(void*): both null and live pointers are valid.
                if a(0) != 0 {
                    let _ = self.arena.release(a(0));
                }
                0
            }
            ("msvcrt", "_adjust_fdiv" | "__set_app_type") => 0,
            ("msvcrt", "_controlfp" | "_control87") => {
                let previous = self.crt_control_word;
                if a(0) != 0 || a(1) != 0 {
                    self.crt_control_word = (previous & !a(1)) | (a(0) & a(1));
                }
                self.crt_control_word
            }
            ("msvcrt", "__p__acmdln") => {
                if let Some(address) = self.crt_acmdln {
                    address
                } else {
                    let command_line = self.process.command_line(memory).unwrap_or(0);
                    let address = self
                        .arena
                        .allocate(4, memory.byte_len())
                        .map_err(|e| format!("__p__acmdln: {e}"))?;
                    write(memory, address, command_line)?;
                    self.crt_acmdln = Some(address);
                    address
                }
            }
            ("msvcrt", "__p__commode" | "__p__fmode") => {
                let cached = if name == "__p__commode" {
                    self.crt_commode
                } else {
                    self.crt_fmode
                };
                if let Some(address) = cached {
                    address
                } else {
                    let address = self
                        .arena
                        .allocate(4, memory.byte_len())
                        .map_err(|e| format!("{name}: {e}"))?;
                    // _commode and _fmode both default to text/console defaults
                    // in a GUI process that never called _setmode.
                    write(memory, address, 0)?;
                    if name == "__p__commode" {
                        self.crt_commode = Some(address);
                    } else {
                        self.crt_fmode = Some(address);
                    }
                    address
                }
            }
            ("msvcrt", "__getmainargs") => {
                if a(0) == 0 || a(1) == 0 || a(2) == 0 {
                    return Ok(Effect::Return(u32::MAX));
                }
                let path = crate::startup::MODULE_FILE_NAME;
                let path_address = match self
                    .arena
                    .allocate(path.len() as u32 + 1, memory.byte_len())
                {
                    Ok(address) => address,
                    Err(_) => return Ok(Effect::Return(u32::MAX)),
                };
                memory
                    .write(path_address, path)
                    .map_err(|e| format!("__getmainargs path: {e:?}"))?;
                memory
                    .write(path_address + path.len() as u32, &[0])
                    .map_err(|e| format!("__getmainargs terminator: {e:?}"))?;
                let argv = match self.arena.allocate(8, memory.byte_len()) {
                    Ok(address) => address,
                    Err(_) => return Ok(Effect::Return(u32::MAX)),
                };
                write(memory, argv, path_address)?;
                write(memory, argv + 4, 0)?;
                let envp = match self.arena.allocate(4, memory.byte_len()) {
                    Ok(address) => address,
                    Err(_) => return Ok(Effect::Return(u32::MAX)),
                };
                write(memory, envp, 0)?;
                write(memory, a(0), 1)?;
                write(memory, a(1), argv)?;
                write(memory, a(2), envp)?;
                if a(4) != 0 {
                    write(memory, a(4), 0)?;
                }
                0
            }
            ("msvcrt", "_initterm") => {
                if a(1) < a(0) {
                    return Ok(Effect::Return(0));
                }
                let count = (a(1) - a(0)) / 4;
                if count > 4096 {
                    return Err("_initterm table exceeds 4096 entries".into());
                }
                let mut functions = Vec::new();
                for index in 0..count {
                    let slot = a(0)
                        .checked_add(index * 4)
                        .ok_or("_initterm table address overflow")?;
                    let function = read(memory, slot)?;
                    if function != 0 {
                        functions.push(function);
                    }
                }
                if functions.is_empty() {
                    return Ok(Effect::Return(0));
                }
                let first = functions.remove(0);
                return self.callback(
                    first,
                    Vec::new(),
                    None,
                    CallbackFinish::InitTerm {
                        remaining: functions,
                    },
                );
            }
            ("msvcrt", "_onexit" | "atexit") => {
                if a(0) == 0 {
                    return Ok(Effect::Return(0));
                }
                self.exit_handlers.push(a(0));
                a(0)
            }
            ("msvcrt", "__dllonexit") => {
                if a(0) != 0 {
                    self.exit_handlers.push(a(0));
                }
                a(0)
            }
            ("msvcrt", "exit") => {
                let mut remaining = std::mem::take(&mut self.exit_handlers);
                remaining.reverse();
                if remaining.is_empty() {
                    return Ok(Effect::Exit(a(0)));
                }
                let procedure = remaining.remove(0);
                return self.callback(
                    procedure,
                    Vec::new(),
                    None,
                    CallbackFinish::ExitHandlers {
                        remaining,
                        code: a(0),
                    },
                );
            }
            ("msvcrt", "_exit") => {
                self.exit_handlers.clear();
                return Ok(Effect::Exit(a(0)));
            }
            ("msvcrt", "?set_terminate@@YAP6AXXZP6AXXZ@Z") => {
                std::mem::replace(&mut self.terminate_handler, a(0))
            }
            ("msvcrt", "?terminate@@YAXXZ" | "abort") => {
                let handler = std::mem::take(&mut self.terminate_handler);
                if handler == 0 {
                    return Ok(Effect::Exit(3));
                }
                return self.callback(
                    handler,
                    Vec::new(),
                    None,
                    CallbackFinish::ExitHandlers {
                        remaining: Vec::new(),
                        code: 3,
                    },
                );
            }
            // No signal or C++ exception translation is implemented. Returning
            // CONTINUE_SEARCH is a real answer: the exception was not handled
            // here, and fabricating a handler result would hide the gap.
            ("msvcrt", "_XcptFilter") => 0,
            ("msvcrt", "__CxxFrameHandler" | "_except_handler3") => 1,
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
    fn read_u16(memory: &Memory, address: u32) -> u16 {
        let mut bytes = [0; 2];
        memory.read(address, &mut bytes).unwrap();
        u16::from_le_bytes(bytes)
    }
    #[test]
    fn local_alloc_is_zeroed_and_freeable() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let address = value(
            runtime
                .call("kernel32", "LocalAlloc", &[0x40, 16], &mut memory)
                .unwrap(),
        );
        assert_ne!(address, 0);
        let mut bytes = [0xff; 16];
        memory.read(address, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 16]);
        memory.write(address, &[1, 2, 3, 4]).unwrap();
        assert_eq!(
            value(
                runtime
                    .call("kernel32", "LocalFree", &[address], &mut memory)
                    .unwrap()
            ),
            0
        );
    }

    #[test]
    fn file_time_conversions_and_windows_directory_follow_win32_contracts() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        // 2024-01-02 03:04:05 UTC.
        let file_time = 133_486_382_450_000_000u64;
        memory.write(0x2000, &file_time.to_le_bytes()).unwrap();
        assert_eq!(
            runtime
                .call(
                    "kernel32",
                    "FileTimeToLocalFileTime",
                    &[0x2000, 0x2100],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(1)
        );
        let mut copied = [0; 8];
        memory.read(0x2100, &mut copied).unwrap();
        assert_eq!(u64::from_le_bytes(copied), file_time);
        assert_eq!(
            runtime
                .call(
                    "kernel32",
                    "FileTimeToDosDateTime",
                    &[0x2000, 0x3000, 0x3002],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            read_u16(&memory, 0x3000),
            ((2024 - 1980) << 9) | (1 << 5) | 2
        );
        assert_eq!(read_u16(&memory, 0x3002), (3 << 11) | (4 << 5) | 2);
        // MS-DOS dates begin at 1980-01-01 and end at 2107-12-31.
        memory
            .write(0x2000, &119_600_063_990_000_000u64.to_le_bytes())
            .unwrap();
        assert_eq!(
            runtime
                .call(
                    "kernel32",
                    "FileTimeToDosDateTime",
                    &[0x2000, 0x3000, 0x3002],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(0)
        );
        memory
            .write(0x2000, &119_600_064_000_000_000u64.to_le_bytes())
            .unwrap();
        assert_eq!(
            runtime
                .call(
                    "kernel32",
                    "FileTimeToDosDateTime",
                    &[0x2000, 0x3000, 0x3002],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(read_u16(&memory, 0x3000), (1 << 5) | 1);
        assert_eq!(read_u16(&memory, 0x3002), 0);
        // Leap day, and the final representable MS-DOS day.
        for (file_time, expected_date, expected_time) in [
            (125_963_423_980_000_000u64, 10_333u16, 49_021u16),
            (159_992_927_980_000_000u64, 65_439u16, 49_021u16),
        ] {
            memory.write(0x2000, &file_time.to_le_bytes()).unwrap();
            assert_eq!(
                runtime
                    .call(
                        "kernel32",
                        "FileTimeToDosDateTime",
                        &[0x2000, 0x3000, 0x3002],
                        &mut memory
                    )
                    .unwrap(),
                Effect::Return(1)
            );
            assert_eq!(read_u16(&memory, 0x3000), expected_date);
            assert_eq!(read_u16(&memory, 0x3002), expected_time);
        }
        memory
            .write(0x2000, &159_992_928_000_000_000u64.to_le_bytes())
            .unwrap();
        assert_eq!(
            runtime
                .call(
                    "kernel32",
                    "FileTimeToDosDateTime",
                    &[0x2000, 0x3000, 0x3002],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(
            runtime
                .call("kernel32", "GetWindowsDirectoryA", &[0, 0], &mut memory)
                .unwrap(),
            Effect::Return(11)
        );
        assert_eq!(
            runtime
                .call("kernel32", "GetWindowsDirectoryA", &[0, 20], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(runtime.thread.last_error, 87);
        assert_eq!(
            runtime
                .call(
                    "kernel32",
                    "GetWindowsDirectoryA",
                    &[0x4000, 5],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(11)
        );
        assert_eq!(
            runtime
                .call(
                    "kernel32",
                    "GetWindowsDirectoryA",
                    &[0x4000, 11],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(10)
        );
        assert_eq!(read_c_string(&memory, 0x4000).unwrap(), b"C:\\WINDOWS");
    }
    #[test]
    fn crt_startup_globals_and_operator_new_work() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        assert_eq!(
            runtime
                .call(
                    "MSVCRT20.dll",
                    "__getmainargs",
                    &[0x2000, 0x2004, 0x2008, 0, 0x200c],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(read(&memory, 0x2000).unwrap(), 1);
        let argv = read(&memory, 0x2004).unwrap();
        let program = read(&memory, argv).unwrap();
        assert_eq!(read_c_string(&memory, program).unwrap(), b"C:\\PROGRAM.EXE");
        assert_eq!(read(&memory, argv + 4).unwrap(), 0);
        let envp = read(&memory, 0x2008).unwrap();
        assert_eq!(read(&memory, envp).unwrap(), 0);
        let acmdln = value(
            runtime
                .call("msvcrt", "__p__acmdln", &[], &mut memory)
                .unwrap(),
        );
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "__p__acmdln", &[], &mut memory)
                    .unwrap()
            ),
            acmdln
        );
        assert_eq!(
            read_c_string(&memory, read(&memory, acmdln).unwrap()).unwrap(),
            b""
        );
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "_controlfp", &[0, 0], &mut memory)
                    .unwrap()
            ),
            0x0008_027F
        );
        assert_eq!(
            value(
                runtime
                    .call(
                        "msvcrt",
                        "_controlfp",
                        &[0x0000_027F, 0x0000_FFFF],
                        &mut memory
                    )
                    .unwrap()
            ) & 0xFFFF,
            0x027F
        );
        let first = value(
            runtime
                .call("msvcrt", "??2@YAPAXI@Z", &[16], &mut memory)
                .unwrap(),
        );
        let second = value(
            runtime
                .call("msvcrt", "??2@YAPAXI@Z", &[16], &mut memory)
                .unwrap(),
        );
        assert_ne!(first, 0);
        assert_ne!(second, 0);
        assert_ne!(first, second);
        assert_eq!(
            runtime
                .call("msvcrt", "??3@YAXPAX@Z", &[first], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
    }
    #[test]
    fn crt_memory_and_string_helpers_match_c() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        memory.write(0x2000, &[0xAA; 16]).unwrap();
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "memset", &[0x2000, 0x55, 8], &mut memory)
                    .unwrap()
            ),
            0x2000
        );
        let mut bytes = [0u8; 16];
        memory.read(0x2000, &mut bytes).unwrap();
        assert_eq!(
            &bytes[..9],
            &[0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0xAA]
        );
        memory.write(0x2100, b"hello\0").unwrap();
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "strlen", &[0x2100], &mut memory)
                    .unwrap()
            ),
            5
        );
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "strcpy", &[0x2200, 0x2100], &mut memory)
                    .unwrap()
            ),
            0x2200
        );
        assert_eq!(read_c_string(&memory, 0x2200).unwrap(), b"hello");
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "strcmp", &[0x2200, 0x2100], &mut memory)
                    .unwrap()
            ),
            0
        );
        memory.write(0x2300, b"hellp\0").unwrap();
        assert_ne!(
            value(
                runtime
                    .call("msvcrt", "strcmp", &[0x2200, 0x2300], &mut memory)
                    .unwrap()
            ),
            0
        );
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "strncmp", &[0x2200, 0x2300, 4], &mut memory)
                    .unwrap()
            ),
            0
        );
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "strchr", &[0x2200, b'l' as u32], &mut memory)
                    .unwrap()
            ),
            0x2202
        );
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "strrchr", &[0x2200, b'l' as u32], &mut memory)
                    .unwrap()
            ),
            0x2203
        );
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "strchr", &[0x2200, 0], &mut memory)
                    .unwrap()
            ),
            0x2205
        );
        memory.write(0x2400, b" -123xyz\0").unwrap();
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "atoi", &[0x2400], &mut memory)
                    .unwrap()
            ) as i32,
            -123
        );
        memory.write(0x2500, b"abcdef\0").unwrap();
        memory.write(0x2600, b"abcxyz\0").unwrap();
        assert!(
            value(
                runtime
                    .call("msvcrt", "memcmp", &[0x2500, 0x2600, 6], &mut memory)
                    .unwrap()
            ) != 0
        );
        memory.write(0x2700, b"abcd\0").unwrap();
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "memcpy", &[0x2800, 0x2700, 5], &mut memory)
                    .unwrap()
            ),
            0x2800
        );
        assert_eq!(read_c_string(&memory, 0x2800).unwrap(), b"abcd");
        memory.write(0x2900, b"abcdef\0").unwrap();
        assert_eq!(
            value(
                runtime
                    .call("msvcrt", "memmove", &[0x2901, 0x2900, 6], &mut memory)
                    .unwrap()
            ),
            0x2901
        );
        assert_eq!(read_c_string(&memory, 0x2901).unwrap(), b"abcdef");
    }
    #[test]
    fn crt_malloc_calloc_and_realloc_use_guest_memory() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let block = value(
            runtime
                .call("msvcrt", "calloc", &[4, 8], &mut memory)
                .unwrap(),
        );
        assert_ne!(block, 0);
        for offset in (0..32).step_by(4) {
            assert_eq!(read(&memory, block + offset).unwrap(), 0);
        }
        write(&mut memory, block, 0x1234_5678).unwrap();
        let grown = value(
            runtime
                .call("msvcrt", "realloc", &[block, 64], &mut memory)
                .unwrap(),
        );
        assert_ne!(grown, 0);
        assert_eq!(read(&memory, grown).unwrap(), 0x1234_5678);
        assert_eq!(
            runtime
                .call("msvcrt", "free", &[grown], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(
            runtime.call("msvcrt", "free", &[0], &mut memory).unwrap(),
            Effect::Return(0)
        );
        let direct = value(runtime.call("msvcrt", "malloc", &[8], &mut memory).unwrap());
        assert_ne!(direct, 0);
        assert_eq!(
            runtime
                .call("msvcrt", "realloc", &[direct, 0], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
    }
    #[test]
    fn initterm_and_exit_handlers_chain_guest_callbacks() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        for (offset, address) in [(0, 0x401000), (4, 0), (8, 0x401010)] {
            write(&mut memory, 0x5000 + offset, address).unwrap();
        }
        let Effect::Invoke {
            token,
            procedure,
            args,
        } = runtime
            .call("msvcrt", "_initterm", &[0x5000, 0x500c], &mut memory)
            .unwrap()
        else {
            panic!("first initializer");
        };
        assert_eq!(procedure, 0x401000);
        assert!(args.is_empty());
        let Effect::Invoke {
            token, procedure, ..
        } = runtime.resume(token, 0).unwrap()
        else {
            panic!("second initializer");
        };
        assert_eq!(procedure, 0x401010);
        assert_eq!(runtime.resume(token, 0).unwrap(), Effect::Return(0));
        assert_eq!(
            runtime
                .call("msvcrt", "_onexit", &[0x401020], &mut memory)
                .unwrap(),
            Effect::Return(0x401020)
        );
        let Effect::Invoke {
            token, procedure, ..
        } = runtime.call("msvcrt", "exit", &[7], &mut memory).unwrap()
        else {
            panic!("exit handler");
        };
        assert_eq!(procedure, 0x401020);
        assert_eq!(runtime.resume(token, 0).unwrap(), Effect::Exit(7));
        assert_eq!(
            runtime
                .call("msvcrt", "_onexit", &[0x401030], &mut memory)
                .unwrap(),
            Effect::Return(0x401030)
        );
        assert_eq!(
            runtime.call("msvcrt", "_exit", &[9], &mut memory).unwrap(),
            Effect::Exit(9)
        );
    }
    #[test]
    fn accelerator_dispatches_command_and_returns_handled_after_callback() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        runtime
            .windows
            .register(b"keys".to_vec(), 0x401000)
            .unwrap();
        let Action::Invoke { token, args, .. } =
            runtime.windows.create(b"keys", 10, 10, 0).unwrap()
        else {
            panic!("create")
        };
        let hwnd = args[0];
        let Action::Invoke { token, .. } = runtime.windows.resume(token, 1).unwrap() else {
            panic!("create")
        };
        runtime.windows.resume(token, 0).unwrap();
        // Two resource records: F2 and Shift+F2. The callback may return zero;
        // TranslateAccelerator must still return nonzero and consume the key.
        memory
            .write(
                0x2000,
                &[1, 0, 113, 0, 9, 128, 0, 0, 0x85, 0, 113, 0, 10, 128, 0, 0],
            )
            .unwrap();
        runtime
            .resources
            .register(9, 128, 1033, 0x2000, 16)
            .unwrap();
        assert_eq!(
            runtime
                .call("user32", "LoadAcceleratorsA", &[0, 128], &mut memory)
                .unwrap(),
            Effect::Return(0x2000)
        );
        for (offset, word) in [(0, hwnd), (4, 0x100), (8, 113), (12, 0)] {
            write(&mut memory, 0x3000 + offset, word).unwrap();
        }
        for (shift, command) in [(0, 0x8009), (0x80, 0x800a)] {
            runtime.keys[16] = shift;
            let Effect::Invoke {
                token,
                procedure,
                args,
            } = runtime
                .call(
                    "user32",
                    "TranslateAcceleratorA",
                    &[hwnd, 0x2000, 0x3000],
                    &mut memory,
                )
                .unwrap()
            else {
                panic!("command callback")
            };
            assert_eq!(procedure, 0x401000);
            assert_eq!(args, vec![hwnd, 0x111, 0x10000 | command, 0]);
            assert_eq!(runtime.resume(token, 0).unwrap(), Effect::Return(1));
        }
        for (message, key) in [(0x101, 113), (0x100, 114)] {
            write(&mut memory, 0x3004, message).unwrap();
            write(&mut memory, 0x3008, key).unwrap();
            assert_eq!(
                runtime
                    .call(
                        "user32",
                        "TranslateAcceleratorA",
                        &[hwnd, 0x2000, 0x3000],
                        &mut memory
                    )
                    .unwrap(),
                Effect::Return(0)
            );
        }
        assert_eq!(
            runtime
                .call("winmm", "timeKillEvent", &[0], &mut memory)
                .unwrap(),
            Effect::Return(97)
        );
        assert_eq!(
            runtime
                .call("winmm", "timeKillEvent", &[1], &mut memory)
                .unwrap(),
            Effect::Return(11)
        );
    }
    #[test]
    fn multimedia_timers_fire_a_guest_callback_and_can_be_cancelled() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let id = value(
            runtime
                .call(
                    "winmm",
                    "timeSetEvent",
                    &[25, 1, 0x401000, 0xbeef, 0],
                    &mut memory,
                )
                .unwrap(),
        );
        assert_ne!(id, 0);
        assert_eq!(runtime.next_timer_delay(), 25);
        runtime.advance_time(24).unwrap();
        assert_eq!(runtime.next_timer_delay(), 1);
        runtime.advance_time(1).unwrap();
        assert_eq!(
            value(
                runtime
                    .call("user32", "GetMessageA", &[0x3000, 0, 0, 0], &mut memory)
                    .unwrap()
            ),
            1
        );
        assert_eq!(
            read(&memory, 0x3000).unwrap(),
            MULTIMEDIA_TIMER_WINDOW
        );
        assert_eq!(read(&memory, 0x3008).unwrap(), id);
        let Effect::Invoke {
            procedure, args, ..
        } = runtime
            .call("user32", "DispatchMessageA", &[0x3000], &mut memory)
            .unwrap()
        else {
            panic!("multimedia timer callback")
        };
        assert_eq!(procedure, 0x401000);
        assert_eq!(args, vec![id, 0, 0xbeef, 0, 0]);
        // A one-shot timer retires after its callback.
        assert_eq!(
            runtime
                .call("winmm", "timeKillEvent", &[id], &mut memory)
                .unwrap(),
            Effect::Return(11)
        );

        let periodic = value(
            runtime
                .call(
                    "winmm",
                    "timeSetEvent",
                    &[10, 1, 0x402000, 0, 1],
                    &mut memory,
                )
                .unwrap(),
        );
        assert_ne!(periodic, 0);
        assert_eq!(
            runtime
                .call("winmm", "timeKillEvent", &[periodic], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        runtime.advance_time(10).unwrap();
        assert_eq!(runtime.next_timer_delay(), u32::MAX);
    }
    #[test]
    fn redraw_erases_before_paint_and_stock_palette_stays_alive() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        runtime
            .windows
            .register(b"paint".to_vec(), 0x401000)
            .unwrap();
        let Action::Invoke { token, args, .. } =
            runtime.windows.create(b"paint", 10, 10, 0).unwrap()
        else {
            panic!("create")
        };
        let hwnd = args[0];
        let Action::Invoke { token, .. } = runtime.windows.resume(token, 1).unwrap() else {
            panic!("create")
        };
        runtime.windows.resume(token, 0).unwrap();
        let Effect::Invoke { token, args, .. } = runtime
            .call("user32", "RedrawWindow", &[hwnd, 0, 0, 0x105], &mut memory)
            .unwrap()
        else {
            panic!("erase callback")
        };
        assert_eq!(args[1], 20);
        let dc = args[2];
        assert!(runtime.gdi.is_dc(dc));
        let Effect::Invoke { token, args, .. } = runtime.resume(token, 1).unwrap() else {
            panic!("paint callback")
        };
        assert_eq!(args, vec![hwnd, 15, 0, 0]);
        assert!(!runtime.gdi.is_dc(dc));
        assert_eq!(runtime.resume(token, 0).unwrap(), Effect::Return(1));
        let palette = value(
            runtime
                .call("gdi32", "GetStockObject", &[15], &mut memory)
                .unwrap(),
        );
        assert_eq!(
            runtime
                .call("gdi32", "GetStockObject", &[15], &mut memory)
                .unwrap(),
            Effect::Return(palette)
        );
        assert_eq!(runtime.palettes.get(palette).unwrap().len(), 20);
        assert_eq!(
            runtime
                .call(
                    "gdi32",
                    "GetPaletteEntries",
                    &[palette, 19, 1, 0x4000],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(read(&memory, 0x4000).unwrap(), 0x00ffffff);
        runtime
            .call("gdi32", "DeleteObject", &[palette], &mut memory)
            .unwrap();
        assert!(runtime.palettes.get(palette).is_some());
    }
    #[test]
    fn null_redraw_window_targets_the_visible_foreground_window() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        runtime
            .windows
            .register(b"paint".to_vec(), 0x401000)
            .unwrap();
        let Action::Invoke { token, args, .. } = runtime.windows.create(b"paint", 10, 10, 0).unwrap()
        else {
            panic!("create")
        };
        let hwnd = args[0];
        let Action::Invoke { token, .. } = runtime.windows.resume(token, 1).unwrap() else {
            panic!("create")
        };
        runtime.windows.resume(token, 0).unwrap();
        runtime.windows.show(hwnd, true).unwrap();
        let Effect::Invoke { args, .. } = runtime
            .call("user32", "RedrawWindow", &[0, 0, 0, 0x105], &mut memory)
            .unwrap()
        else {
            panic!("desktop redraw must reach the foreground window")
        };
        assert_eq!(args[0], hwnd);
        assert_eq!(args[1], 20);
        // No windows are visible: the desktop redraw is a successful no-op.
        runtime.windows.show(hwnd, false).unwrap();
        assert_eq!(
            runtime
                .call("user32", "RedrawWindow", &[0, 0, 0, 0x105], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
    }
    #[test]
    fn indexed_display_caps_and_realized_system_palette_are_consistent() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let dc = value(
            runtime
                .call("gdi32", "CreateCompatibleDC", &[0], &mut memory)
                .unwrap(),
        );
        assert_eq!(
            runtime
                .call("gdi32", "GetDeviceCaps", &[dc, 12], &mut memory)
                .unwrap(),
            Effect::Return(32)
        );
        assert_eq!(
            runtime
                .call(
                    "gdi32",
                    "GetSystemPaletteEntries",
                    &[dc, 0, 0, 0],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(0)
        );
        runtime.set_display_bits(8).unwrap();
        for (cap, expected) in [
            (12, 8),
            (14, 1),
            (24, 20),
            (38, 0x2941),
            (104, 256),
            (106, 20),
        ] {
            assert_eq!(
                runtime
                    .call("gdi32", "GetDeviceCaps", &[dc, cap], &mut memory)
                    .unwrap(),
                Effect::Return(expected)
            );
        }
        assert!(runtime.set_display_bits(16).is_err());
        assert_eq!(
            runtime
                .call(
                    "gdi32",
                    "GetSystemPaletteEntries",
                    &[dc, 0, 0, 0],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(256)
        );
        let reserved_low = runtime.system_palette[..10].to_vec();
        let reserved_high = runtime.system_palette[246..].to_vec();
        memory
            .write(0x2000, &[0, 3, 2, 0, 12, 34, 56, 1, 65, 43, 21, 4])
            .unwrap();
        let palette = value(
            runtime
                .call("gdi32", "CreatePalette", &[0x2000], &mut memory)
                .unwrap(),
        );
        runtime
            .call("gdi32", "SelectPalette", &[dc, palette, 0], &mut memory)
            .unwrap();
        assert_eq!(
            runtime
                .call("gdi32", "RealizePalette", &[dc], &mut memory)
                .unwrap(),
            Effect::Return(2)
        );
        assert_eq!(
            runtime
                .call("gdi32", "RealizePalette", &[dc], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(
            runtime
                .call(
                    "gdi32",
                    "GetSystemPaletteEntries",
                    &[dc, 10, 2, 0x3000],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(2)
        );
        assert_eq!(read(&memory, 0x3000).unwrap(), 0x0038220c);
        assert_eq!(read(&memory, 0x3004).unwrap(), 0x00152b41);
        assert_eq!(&runtime.system_palette[..10], reserved_low);
        assert_eq!(&runtime.system_palette[246..], reserved_high);
    }
    #[test]
    fn indexed_display_presents_window_pixels_through_realized_palette() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        runtime.set_display_bits(8).unwrap();
        let dc = value(
            runtime
                .call("gdi32", "CreateCompatibleDC", &[0], &mut memory)
                .unwrap(),
        );
        // A one-entry logical palette with a color absent from the initial
        // system palette. Realization should make that color displayable.
        memory.write(0x2000, &[0, 3, 1, 0, 17, 99, 203, 0]).unwrap();
        let palette = value(
            runtime
                .call("gdi32", "CreatePalette", &[0x2000], &mut memory)
                .unwrap(),
        );
        runtime
            .call("gdi32", "SelectPalette", &[dc, palette, 0], &mut memory)
            .unwrap();
        runtime.gdi.create_surface(0x1234, 1, 1).unwrap();
        runtime.gdi.acknowledge_present(0x1234).unwrap();
        assert_eq!(
            runtime
                .call("gdi32", "RealizePalette", &[dc], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert!(runtime.gdi.surface(0x1234).unwrap().dirty);
        runtime
            .gdi
            .present_rgba(0x1234, &[17, 99, 203, 255])
            .unwrap();
        assert_eq!(runtime.presented_frame(0x1234).unwrap(), [17, 99, 203, 255]);
    }
    #[test]
    fn initial_indexed_display_palette_contains_a_color_cube_and_gray_ramp() {
        let palette = initial_system_palette();
        assert_eq!(palette[10], [0, 0, 0, 0]);
        assert_eq!(palette[10 + 1 * 36 + 2 * 6 + 3], [51, 102, 153, 0]);
        assert_eq!(palette[10 + 216], [12, 12, 12, 0]);
        assert_eq!(palette[10 + 235], [242, 242, 242, 0]);
        assert_eq!(palette[0][..3], [0, 0, 0]);
        assert_eq!(palette[255][..3], [255, 255, 255]);
    }
    #[test]
    fn window_enabled_query_tracks_disabled_style_without_requiring_visibility() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        memory.write(0x11000, b"enabled-test\0").unwrap();
        for (address, value) in [(0x2000, 48), (0x2008, 0x401000), (0x2028, 0x11000)] {
            write(&mut memory, address, value).unwrap();
        }
        runtime
            .call("user32", "RegisterClassExA", &[0x2000], &mut memory)
            .unwrap();
        let (pending, hwnd) = token(
            runtime
                .call(
                    "user32",
                    "CreateWindowExA",
                    &[0, 0x11000, 0, 0, 0, 0, 10, 10, 0, 0, 0x400000, 0],
                    &mut memory,
                )
                .unwrap(),
            129,
        );
        let (pending, _) = token(runtime.resume(pending, 1).unwrap(), 1);
        assert_eq!(runtime.resume(pending, 0).unwrap(), Effect::Return(hwnd));
        for (style, enabled) in [(0x08000000, 0), (0, 1), (0x18000000, 0), (0x10000000, 1)] {
            runtime
                .call(
                    "user32",
                    "SetWindowLongA",
                    &[hwnd, (-16i32) as u32, style],
                    &mut memory,
                )
                .unwrap();
            assert_eq!(
                runtime
                    .call("user32", "IsWindowEnabled", &[hwnd], &mut memory)
                    .unwrap(),
                Effect::Return(enabled)
            );
        }
        for (style, iconic, zoomed) in [(0, 0, 0), (0x20000000, 1, 0), (0x01000000, 0, 1)] {
            runtime
                .call(
                    "user32",
                    "SetWindowLongA",
                    &[hwnd, (-16i32) as u32, style],
                    &mut memory,
                )
                .unwrap();
            assert_eq!(
                runtime
                    .call("user32", "IsWindow", &[hwnd], &mut memory)
                    .unwrap(),
                Effect::Return(1)
            );
            assert_eq!(
                runtime
                    .call("user32", "IsIconic", &[hwnd], &mut memory)
                    .unwrap(),
                Effect::Return(iconic)
            );
            assert_eq!(
                runtime
                    .call("user32", "IsZoomed", &[hwnd], &mut memory)
                    .unwrap(),
                Effect::Return(zoomed)
            );
        }
        runtime.focused_window = hwnd;
        assert_eq!(
            runtime
                .call("user32", "GetFocus", &[], &mut memory)
                .unwrap(),
            Effect::Return(hwnd)
        );
        let Effect::Invoke {
            token: pending,
            args,
            ..
        } = runtime
            .call("user32", "EnableWindow", &[hwnd, 0], &mut memory)
            .unwrap()
        else {
            panic!("WM_CANCELMODE");
        };
        assert_eq!(args, vec![hwnd, 31, 0, 0]);
        assert_eq!(
            runtime
                .call("user32", "IsWindowEnabled", &[hwnd], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        let Effect::Invoke {
            token: pending,
            args,
            ..
        } = runtime.resume(pending, 123).unwrap()
        else {
            panic!("WM_ENABLE");
        };
        assert_eq!(args, vec![hwnd, 10, 0, 0]);
        assert_eq!(runtime.focused_window, 0);
        assert_eq!(
            runtime
                .call("user32", "GetFocus", &[], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(
            runtime
                .call("user32", "IsWindowEnabled", &[hwnd], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(runtime.resume(pending, 456).unwrap(), Effect::Return(0));
        assert_eq!(
            runtime
                .call("user32", "EnableWindow", &[hwnd, 0], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        let Effect::Invoke {
            token: pending,
            args,
            ..
        } = runtime
            .call("user32", "EnableWindow", &[hwnd, 1], &mut memory)
            .unwrap()
        else {
            panic!("WM_ENABLE");
        };
        assert_eq!(args, vec![hwnd, 10, 1, 0]);
        assert_eq!(
            runtime
                .call("user32", "IsWindowEnabled", &[hwnd], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(runtime.resume(pending, 789).unwrap(), Effect::Return(1));
        assert_eq!(
            runtime
                .call("user32", "EnableWindow", &[hwnd, 1], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        for hwnd in [0, 0xdeadbeef] {
            assert_eq!(
                runtime
                    .call("user32", "IsWindowEnabled", &[hwnd], &mut memory)
                    .unwrap(),
                Effect::Return(0)
            );
        }
    }
    #[test]
    fn ansi_case_apis_distinguish_character_string_and_counted_buffer() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        assert_eq!(
            runtime
                .call("user32", "CharUpperA", &[b'a' as u32], &mut memory)
                .unwrap(),
            Effect::Return(b'A' as u32)
        );
        assert_eq!(
            runtime
                .call("user32", "CharLowerA", &[0x9f], &mut memory)
                .unwrap(),
            Effect::Return(0xff)
        );
        memory.write(0x11000, b"beacon_17\0z").unwrap();
        assert_eq!(
            runtime
                .call("user32", "CharUpperA", &[0x11000], &mut memory)
                .unwrap(),
            Effect::Return(0x11000)
        );
        let mut bytes = [0; 11];
        memory.read(0x11000, &mut bytes).unwrap();
        assert_eq!(&bytes, b"BEACON_17\0z");
        assert_eq!(
            runtime
                .call("user32", "CharUpperBuffA", &[0x11000, 11], &mut memory)
                .unwrap(),
            Effect::Return(11)
        );
        memory.read(0x11000, &mut bytes).unwrap();
        assert_eq!(&bytes, b"BEACON_17\0Z");
        assert_eq!(
            runtime
                .call("user32", "CharLowerBuffA", &[0x11000, 11], &mut memory)
                .unwrap(),
            Effect::Return(11)
        );
        memory.read(0x11000, &mut bytes).unwrap();
        assert_eq!(&bytes, b"beacon_17\0z");
        assert_eq!(crate::locale::ansi_case(0xe9, true), 0xc9);
        assert_eq!(crate::locale::ansi_case(0x9a, true), 0x8a);
        assert_eq!(crate::locale::ansi_case(0xdf, true), 0xdf);
    }
    fn test_control(runtime: &mut Runtime, parent: u32, class: &[u8], id: u32, style: u32) -> u32 {
        let window = runtime
            .windows
            .create_builtin(
                if class == b"#32770" {
                    0xffff_ff01
                } else {
                    0xffff_ff02
                },
                parent,
                id,
                0,
                0,
                100,
                24,
                style,
            )
            .unwrap();
        runtime.gdi.create_surface(window, 100, 24).unwrap();
        runtime.window_data.insert(
            window,
            WindowData {
                button: crate::buttons::ButtonState::default(),
                dialog_procedure: if class == b"#32770" { 0x401000 } else { 0 },
                class: class.to_vec(),
                instance: 0x400000,
                parent,
                id,
                style,
                ex_style: 0,
                user_data: 0,
                extra: vec![0; 32],
                x: 0,
                y: 0,
                text: Vec::new(),
                restore: None,
            },
        );
        window
    }
    #[test]
    fn user_window_properties_support_names_atoms_replacement_and_removal() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let window = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        memory.write(0x1000, b"StayDisabled\0").unwrap();
        assert_eq!(
            runtime
                .call("user32", "GetPropA", &[window, 0x1000], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(
            runtime
                .call("user32", "SetPropA", &[window, 0x1000, 0x1234], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime
                .call("user32", "GetPropA", &[window, 0x1000], &mut memory)
                .unwrap(),
            Effect::Return(0x1234)
        );
        assert_eq!(
            runtime
                .call("user32", "SetPropA", &[window, 0x1000, 0x5678], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime
                .call("user32", "RemovePropA", &[window, 0x1000], &mut memory)
                .unwrap(),
            Effect::Return(0x5678)
        );
        assert_eq!(
            runtime
                .call("user32", "GetPropA", &[window, 0x1000], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(
            runtime
                .call("user32", "SetPropA", &[window, 0x123, 0x9876], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime
                .call("user32", "GetPropA", &[window, 0x123], &mut memory)
                .unwrap(),
            Effect::Return(0x9876)
        );
        assert_eq!(
            runtime
                .call("user32", "GetPropA", &[0xdeadbeef, 0x1000], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
    }
    #[test]
    fn dialog_children_compose_in_creation_order_even_when_handle_sort_differs() {
        let mut runtime = Runtime::default();
        let parent = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        let retired = runtime
            .windows
            .create_builtin(0xffff_ff02, parent, 9, 0, 0, 1, 1, 0x50000000)
            .unwrap();
        runtime.windows.cancel_create(retired);
        let first = test_control(&mut runtime, parent, b"STATIC", 10, 0x50000000);
        let second = test_control(&mut runtime, parent, b"STATIC", 11, 0x50000000);
        assert!(
            first > second,
            "fixture requires handle order to differ from child creation order"
        );
        runtime
            .gdi
            .fill_surface(
                first,
                Rect {
                    left: 0,
                    top: 0,
                    right: 1,
                    bottom: 1,
                },
                0x000000ff,
            )
            .unwrap();
        runtime
            .gdi
            .fill_surface(
                second,
                Rect {
                    left: 0,
                    top: 0,
                    right: 1,
                    bottom: 1,
                },
                0x0000ff00,
            )
            .unwrap();
        runtime.compose_children(parent).unwrap();
        assert_eq!(
            &runtime.gdi.surface(parent).unwrap().rgba[..3],
            &[0, 255, 0]
        );
    }
    #[test]
    fn dialog_custom_class_runs_creation_callbacks_before_dialog_initialization() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let class = b"StatBarLongName".to_vec();
        runtime.windows.register(class.clone(), 0x401000).unwrap();
        runtime.classes.insert(class, (1, [0, 0x401000, 0, 4, 0x400000, 0, 0, 0, 0, 0]));
        let mut template = 0x10000000u32.to_le_bytes().to_vec();
        template.extend_from_slice(&0u32.to_le_bytes());
        for word in [1u16, 0, 0, 100, 60, 0, 0, 0] { template.extend_from_slice(&word.to_le_bytes()); }
        while template.len() % 4 != 0 { template.push(0); }
        template.extend_from_slice(&0x50000000u32.to_le_bytes());
        template.extend_from_slice(&0u32.to_le_bytes());
        for word in [3u16, 4, 50, 10, 42] { template.extend_from_slice(&word.to_le_bytes()); }
        for text in ["statbarlongname\0", "A long custom control title\0"] {
            for unit in text.encode_utf16() { template.extend_from_slice(&unit.to_le_bytes()); }
        }
        template.extend_from_slice(&0u16.to_le_bytes());
        memory.write(0x10000, &template).unwrap();
        runtime.resources.register(5, 128, 0, 0x10000, template.len() as u32).unwrap();
        let Effect::Invoke { token, procedure, args } = runtime.call("user32", "CreateDialogParamA", &[0x400000, 128, 0, 0x402000, 0], &mut memory).unwrap() else { panic!("custom NCCREATE callback missing") };
        assert_eq!(procedure, 0x401000);
        assert_eq!(args[1], 0x81);
        assert_eq!(read_c_string(&memory, read(&memory, args[3] + 40).unwrap()).unwrap(), b"StatBarLongName");
        assert_eq!(read_c_string(&memory, read(&memory, args[3] + 36).unwrap()).unwrap(), b"A long custom control title");
        assert_eq!(runtime.window_data[&args[0]].extra.len(), 4);
        assert_eq!(runtime.window_extra_word(args[0], 2, Some(0xbeef)).unwrap(), 0);
        assert_eq!(runtime.window_extra_word(args[0], 2, None).unwrap(), 0xbeef);
        assert_eq!(runtime.window_extra_word(args[0], 0, None).unwrap(), 0);
        assert!(runtime.window_extra_word(args[0], 3, Some(1)).is_err());
        let Effect::Invoke { token, procedure, args } = runtime.resume_with_memory(token, 1, &mut memory).unwrap() else { panic!("custom CREATE callback missing") };
        assert_eq!((procedure, args[1]), (0x401000, 1));
        let Effect::Invoke { procedure, args, .. } = runtime.resume_with_memory(token, 0, &mut memory).unwrap() else { panic!("dialog INIT callback missing") };
        assert_eq!((procedure, args[1]), (0x402000, 0x110));
    }
    #[test]
    fn partial_invalidation_unions_bounds_and_clips_the_paint_dc() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let window = test_control(&mut runtime, 0, b"STATIC", 0, 0x10000000);
        runtime.windows.validate_paint(window);
        for rect in [[1u32, 2, 5, 6], [3, 4, 8, 9]] {
            for (i, value) in rect.iter().enumerate() { write(&mut memory, 0x1000 + i as u32 * 4, *value).unwrap(); }
            runtime.call("user32", "InvalidateRect", &[window, 0x1000, 0], &mut memory).unwrap();
        }
        let Effect::Return(dc) = runtime.call("user32", "BeginPaint", &[window, 0x2000], &mut memory).unwrap() else { panic!("BeginPaint callback") };
        assert_eq!((read(&memory, 0x2008).unwrap(), read(&memory, 0x200c).unwrap(), read(&memory, 0x2010).unwrap(), read(&memory, 0x2014).unwrap()), (1, 2, 8, 9));
        let brush = runtime.gdi.solid_brush(0xff).unwrap();
        for (i, value) in [0u32, 0, 100, 24].iter().enumerate() { write(&mut memory, 0x1000 + i as u32 * 4, *value).unwrap(); }
        runtime.call("user32", "FillRect", &[dc, 0x1000, brush], &mut memory).unwrap();
        let surface = runtime.gdi.surface(window).unwrap();
        assert_eq!(&surface.rgba[..3], &[0, 0, 0]);
        assert_eq!(&surface.rgba[(2 * 100 + 1) * 4..(2 * 100 + 1) * 4 + 3], &[255, 0, 0]);
        runtime.call("user32", "EndPaint", &[window, 0x2000], &mut memory).unwrap();
        runtime.call("user32", "InvalidateRect", &[window, 0, 0], &mut memory).unwrap();
        runtime.call("user32", "BeginPaint", &[window, 0x2000], &mut memory).unwrap();
        assert_eq!((read(&memory, 0x2010).unwrap(), read(&memory, 0x2014).unwrap()), (100, 24));
    }
    #[test]
    fn set_window_text_dispatches_custom_proc_and_default_proc_updates_caption_only() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let parent = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        let child = test_control(&mut runtime, parent, b"Custom", 42, 0x50000000);
        runtime.gdi.remove_surface(parent).unwrap();
        runtime.call("user32", "SetWindowLongA", &[child, (-4i32) as u32, 0x401000], &mut memory).unwrap();
        memory.write(0x10000, b"New caption\0").unwrap();
        let before = runtime.gdi.surface(child).unwrap().rgba.clone();
        let Effect::Invoke { token, procedure, args } = runtime.call("user32", "SetWindowTextA", &[child, 0x10000], &mut memory).unwrap() else { panic!("WM_SETTEXT missing") };
        assert_eq!(procedure, 0x401000);
        assert_eq!(args, [child, 0x0c, 0, 0x10000]);
        assert_eq!(runtime.call("user32", "DefWindowProcA", &args, &mut memory).unwrap(), Effect::Return(1));
        assert_eq!(runtime.resume_with_memory(token, 1, &mut memory).unwrap(), Effect::Return(1));
        assert_eq!(runtime.window_data[&child].text, b"New caption");
        assert_eq!(runtime.gdi.surface(child).unwrap().rgba, before);
    }
    #[test]
    fn set_window_pos_updates_geometry_visibility_and_preserves_return_across_size_callback() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let window = test_control(&mut runtime, 0, b"Custom", 0, 0);
        runtime.call("user32", "SetWindowLongA", &[window, (-4i32) as u32, 0x401000], &mut memory).unwrap();
        let Effect::Invoke { token, procedure, args } = runtime.call("user32", "SetWindowPos", &[window, 0, 4, 5, 80, 40, 0x40], &mut memory).unwrap() else { panic!("WM_SIZE missing") };
        assert_eq!(procedure, 0x401000);
        assert_eq!(args, [window, 5, 0, 80 | (40 << 16)]);
        let w = runtime.windows.get(window).unwrap();
        assert_eq!((w.x, w.y, w.width, w.height, w.visible), (4, 5, 80, 40, true));
        assert_eq!((runtime.window_data[&window].x, runtime.window_data[&window].y), (4, 5));
        assert_eq!(runtime.resume_with_memory(token, 0, &mut memory).unwrap(), Effect::Return(1));
        let other = test_control(&mut runtime, 0, b"STATIC", 0, 0);
        runtime.call("user32", "SetWindowPos", &[other, window, 0, 0, 0, 0, 3], &mut memory).unwrap();
        assert_eq!(runtime.windows.children(0), [other, window]);
    }
    #[test]
    fn user32_fill_rect_routes_to_gdi_surface() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let window = test_control(&mut runtime, 0, b"STATIC", 0, 0x10000000);
        let Effect::Return(dc) = runtime.call("user32", "GetDC", &[window], &mut memory).unwrap() else { panic!("GetDC callback") };
        let brush = runtime.gdi.solid_brush(0x00332211).unwrap();
        for (i, field) in [0u32, 0, 2, 2].iter().enumerate() { write(&mut memory, 0x1000 + i as u32 * 4, *field).unwrap(); }
        assert_eq!(runtime.call("user32", "FillRect", &[dc, 0x1000, brush], &mut memory).unwrap(), Effect::Return(1));
        assert_eq!(&runtime.gdi.surface(window).unwrap().rgba[..3], &[0x11, 0x22, 0x33]);
    }
    #[test]
    fn scrollbar_range_changes_clamp_position_and_keep_windows_independent() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let window = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        assert_eq!(runtime.call("user32", "SetScrollRange", &[window, 0, 10, 20, 0], &mut memory).unwrap(), Effect::Return(1));
        assert_eq!(runtime.call("user32", "SetScrollPos", &[window, 0, 90, 0], &mut memory).unwrap(), Effect::Return(10));
        assert_eq!(runtime.call("user32", "GetScrollPos", &[window, 0], &mut memory).unwrap(), Effect::Return(20));
        assert_eq!(runtime.call("user32", "GetScrollPos", &[window, 1], &mut memory).unwrap(), Effect::Return(0));
        let other = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        assert_eq!(runtime.call("user32", "GetScrollPos", &[other, 0], &mut memory).unwrap(), Effect::Return(0));
        runtime.call("user32", "SetScrollRange", &[window, 0, 0, 5, 0], &mut memory).unwrap();
        assert_eq!(runtime.call("user32", "GetScrollPos", &[window, 0], &mut memory).unwrap(), Effect::Return(5));
        runtime.call("user32", "GetScrollRange", &[window, 0, 0x1000, 0x1004], &mut memory).unwrap();
        assert_eq!(read(&memory, 0x1000).unwrap(), 0);
        assert_eq!(read(&memory, 0x1004).unwrap(), 5);
    }
    #[test]
    fn static_rectangles_and_frames_use_their_requested_colors() {
        let mut runtime = Runtime::default();
        for kind in 4..=9 {
            let control = test_control(&mut runtime, 0, b"STATIC", kind, 0x50000000 | kind);
            runtime.render_static(control).unwrap();
            let surface = runtime.gdi.surface(control).unwrap();
            let expected = match kind { 4 | 7 => 0, 5 | 8 => 128, _ => 255 };
            assert_eq!(&surface.rgba[..3], &[expected; 3]);
            let center = ((surface.height / 2 * surface.width + surface.width / 2) * 4) as usize;
            let interior = if kind < 7 { expected } else { 192 };
            assert_eq!(&surface.rgba[center..center + 3], &[interior; 3]);
        }
    }
    #[test]
    fn default_window_proc_thunk_handles_paint_without_calling_guest_code() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let class = b"defaultproc".to_vec();
        runtime
            .windows
            .register(class.clone(), DEFAULT_WINDOW_PROC)
            .unwrap();
        let window = match runtime.windows.create(&class, 20, 10, 0).unwrap() {
            Action::Return(window) => window,
            action => panic!("default window proc must not dispatch a guest callback: {action:?}"),
        };
        runtime.windows.invalidate(window).unwrap();
        assert_eq!(
            runtime
                .call(
                    "user32",
                    "CallWindowProcA",
                    &[DEFAULT_WINDOW_PROC, window, 15, 0, 0],
                    &mut memory,
                )
                .unwrap(),
            Effect::Return(0)
        );
        assert!(!runtime.windows.get(window).unwrap().invalid);
    }

    #[test]
    fn create_window_accepts_lowercase_builtin_button_and_child_identifier() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let parent = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        runtime.gdi.remove_surface(parent).unwrap(); // parent WM_CREATE is still running
        memory.write(0x10100, b"button\0").unwrap();
        memory.write(0x10120, b"Start\0").unwrap();
        let effect = runtime.call("user32", "CreateWindowExA", &[0, 0x10100, 0x10120, 0x50000001, 2, 3, 60, 24, parent, 1, 0x400000, 0], &mut memory).unwrap();
        let Effect::Return(window) = effect else { panic!("built-in control must not invoke guest code") };
        assert_ne!(window, 0);
        assert_eq!(runtime.windows.get(window).unwrap().id, 1);
        assert_eq!(runtime.window_data[&window].class, b"BUTTON");
        assert_eq!(runtime.window_data[&window].text, b"Start");
        assert!(runtime.gdi.surface(window).is_some());
    }
    #[test]
    fn dialog_button_helpers_and_messages_share_state_rendering_and_subclass_callbacks() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let parent = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        let check = test_control(&mut runtime, parent, b"BUTTON", 101, 0x50000003);
        runtime.render_static(check).unwrap();
        let pixel = (11 * 100 + 4) * 4;
        assert_eq!(
            &runtime.gdi.surface(check).unwrap().rgba[pixel..pixel + 3],
            &[255, 255, 255]
        );
        assert_eq!(
            runtime
                .call("user32", "CheckDlgButton", &[parent, 101, 1], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime
                .call("user32", "IsDlgButtonChecked", &[parent, 101], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            &runtime.gdi.surface(check).unwrap().rgba[pixel..pixel + 3],
            &[0, 0, 0]
        );
        assert_eq!(
            runtime
                .call("user32", "SendMessageA", &[check, 0xf0, 0, 0], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        let Effect::Invoke {
            token,
            procedure,
            args,
        } = runtime
            .call("user32", "SendMessageA", &[check, 0xf5, 0, 0], &mut memory)
            .unwrap()
        else {
            panic!("BN_CLICKED")
        };
        assert_eq!(procedure, 0x401000);
        assert_eq!(args, vec![parent, 0x111, 101, check]);
        assert_eq!(
            runtime
                .call("user32", "IsDlgButtonChecked", &[parent, 101], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(runtime.resume(token, 123).unwrap(), Effect::Return(0));
        runtime.windows.set_procedure(check, 0x402000).unwrap();
        let Effect::Invoke {
            token,
            procedure,
            args,
        } = runtime
            .call("user32", "CheckDlgButton", &[parent, 101, 1], &mut memory)
            .unwrap()
        else {
            panic!("subclass BM_SETCHECK")
        };
        assert_eq!(procedure, 0x402000);
        assert_eq!(args, vec![check, 0xf1, 1, 0]);
        assert_eq!(
            runtime
                .call(
                    "user32",
                    "CallWindowProcA",
                    &[0xffff_ff02, check, 0xf1, 1, 0],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(runtime.resume(token, 999).unwrap(), Effect::Return(1));
        assert_eq!(runtime.window_data[&check].button.checked, 1);
        assert_eq!(runtime.presentation_window(check), parent);
        let owned = test_control(&mut runtime, parent, b"#32770", 0, 0x10000000);
        assert_eq!(runtime.presentation_window(owned), owned);
        assert_eq!(
            runtime
                .call("user32", "CheckDlgButton", &[parent, 999, 1], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
    }
    #[test]
    fn auto_radio_groups_and_explicit_id_ranges_are_distinct() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let parent = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        let a = test_control(&mut runtime, parent, b"BUTTON", 201, 0x50020009);
        let b = test_control(&mut runtime, parent, b"BUTTON", 202, 0x50000009);
        let c = test_control(&mut runtime, parent, b"BUTTON", 203, 0x50020009);
        let d = test_control(&mut runtime, parent, b"BUTTON", 204, 0x50000009);
        for id in [201, 203] {
            runtime
                .call("user32", "CheckDlgButton", &[parent, id, 1], &mut memory)
                .unwrap();
        }
        let Effect::Invoke { token, .. } = runtime
            .call("user32", "SendMessageA", &[b, 0xf5, 0, 0], &mut memory)
            .unwrap()
        else {
            panic!("radio click")
        };
        runtime.resume(token, 0).unwrap();
        for (window, expected) in [(a, 0), (b, 1), (c, 1), (d, 0)] {
            assert_eq!(runtime.window_data[&window].button.checked, expected);
        }
        assert_eq!(
            runtime
                .call(
                    "user32",
                    "CheckRadioButton",
                    &[parent, 201, 204, 204],
                    &mut memory
                )
                .unwrap(),
            Effect::Return(1)
        );
        for (window, expected) in [(a, 0), (b, 0), (c, 0), (d, 1)] {
            assert_eq!(runtime.window_data[&window].button.checked, expected);
        }
        runtime
            .call("user32", "SendMessageA", &[b, 0x201, 1, 0], &mut memory)
            .unwrap();
        assert!(runtime.window_data[&b].button.pressed);
        runtime
            .call("user32", "SendMessageA", &[b, 31, 0, 0], &mut memory)
            .unwrap();
        assert!(!runtime.window_data[&b].button.pressed);
        assert_eq!(
            runtime
                .call("user32", "SendMessageA", &[b, 0x202, 0, 0], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        assert_eq!(runtime.window_data[&d].button.checked, 1);
    }
    #[test]
    fn modal_pump_drains_child_paints_and_dispatches_subclassed_dialog_messages() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let parent = test_control(&mut runtime, 0, b"#32770", 0, 0x10000000);
        let child = test_control(&mut runtime, parent, b"BUTTON", 1, 0x50000001);
        let text = test_control(&mut runtime, parent, b"STATIC", 2, 0x50000000);
        let key = (0x400000, 115, 0, 0x401000, 0);
        runtime.modal_dialogs.insert(
            key,
            ModalDialog {
                window: parent,
                procedure: 0x401000,
                result: None,
            },
        );
        runtime.modal_windows.insert(parent, key);
        runtime.windows.validate_paint(parent);
        let args = [key.0, key.1, key.2, key.3, key.4];
        assert_eq!(
            runtime
                .call("user32", "DialogBoxParamA", &args, &mut memory)
                .unwrap(),
            Effect::Retry
        );
        assert_eq!(
            runtime
                .call("user32", "DialogBoxParamA", &args, &mut memory)
                .unwrap(),
            Effect::Wait
        );
        assert!(!runtime.windows.get(child).unwrap().invalid);
        assert!(!runtime.windows.get(text).unwrap().invalid);
        runtime.windows.set_procedure(parent, 0x402000).unwrap();
        runtime
            .post(Message {
                window: parent,
                message: 0x111,
                wparam: 1,
                lparam: child,
            })
            .unwrap();
        let Effect::Invoke {
            token,
            procedure,
            args: callback_args,
        } = runtime
            .call("user32", "DialogBoxParamA", &args, &mut memory)
            .unwrap()
        else {
            panic!("subclass dialog dispatch")
        };
        assert_eq!(procedure, 0x402000);
        assert_eq!(callback_args, vec![parent, 0x111, 1, child]);
        assert_eq!(
            runtime
                .call("user32", "EndDialog", &[parent, 7], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(runtime.resume(token, 0).unwrap(), Effect::Return(7));
    }
    #[test]
    fn destroyed_owned_dialog_returns_presentation_to_and_invalidates_its_owner() {
        let mut runtime = Runtime::default();
        let owner = test_control(&mut runtime, 0, b"#32770", 0, 0x90000000);
        let dialog = test_control(&mut runtime, owner, b"#32770", 0, 0x90000000);
        runtime.gdi.acknowledge_present(owner).unwrap();
        assert!(!runtime.gdi.surface(owner).unwrap().dirty);
        runtime.cancel_dialog(dialog).unwrap();
        assert_eq!(runtime.presentation_window(dialog), owner);
        assert!(runtime.gdi.surface(owner).unwrap().dirty);
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
        // ShowWindow queues WM_SIZE/WM_SHOWWINDOW first; drain until the
        // paint request that the rest of this test exercises.
        let mut message = 0;
        for _ in 0..8 {
            runtime
                .call("USER32.dll", "GetMessageA", &[0x3000, 0, 0, 0], &mut memory)
                .unwrap();
            message = read(&memory, 0x3004).unwrap();
            if message == 15 {
                break;
            }
        }
        assert_eq!(message, 15);
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
    #[test]
    fn gdi_object_queries_and_text_state_are_consistent() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let dc = value(runtime.call("gdi32", "CreateCompatibleDC", &[0], &mut memory).unwrap());
        let bitmap = value(
            runtime
                .call("gdi32", "CreateCompatibleBitmap", &[dc, 4, 3], &mut memory)
                .unwrap(),
        );
        assert_ne!(bitmap, 0);
        assert_eq!(
            runtime.call("gdi32", "GetObjectType", &[dc], &mut memory).unwrap(),
            Effect::Return(10)
        );
        assert_eq!(
            runtime.call("gdi32", "GetObjectType", &[bitmap], &mut memory).unwrap(),
            Effect::Return(7)
        );
        assert_ne!(
            value(runtime.call("gdi32", "SelectObject", &[dc, bitmap], &mut memory).unwrap()),
            0
        );
        let pen = value(runtime.call("gdi32", "CreatePen", &[0, 3, 0x00123456], &mut memory).unwrap());
        let brush = value(runtime.call("gdi32", "CreateSolidBrush", &[0x00abcdef], &mut memory).unwrap());
        let font = value(runtime.call("gdi32", "GetStockObject", &[13], &mut memory).unwrap());
        assert_eq!(
            runtime.call("gdi32", "GetObjectType", &[pen], &mut memory).unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime.call("gdi32", "GetObjectType", &[brush], &mut memory).unwrap(),
            Effect::Return(2)
        );
        assert_eq!(
            runtime.call("gdi32", "GetObjectType", &[font], &mut memory).unwrap(),
            Effect::Return(6)
        );
        assert_eq!(
            runtime.call("gdi32", "GetObjectA", &[pen, 16, 0x7000], &mut memory).unwrap(),
            Effect::Return(16)
        );
        assert_eq!(read(&memory, 0x7000).unwrap(), 0);
        assert_eq!(read(&memory, 0x7004).unwrap(), 3);
        assert_eq!(read(&memory, 0x700c).unwrap(), 0x00123456);
        assert_eq!(
            runtime.call("gdi32", "GetObjectA", &[brush, 12, 0x7000], &mut memory).unwrap(),
            Effect::Return(12)
        );
        assert_eq!(read(&memory, 0x7000).unwrap(), 0);
        assert_eq!(read(&memory, 0x7004).unwrap(), 0x00abcdef);
        assert_eq!(
            runtime.call("gdi32", "GetObjectA", &[bitmap, 24, 0x7000], &mut memory).unwrap(),
            Effect::Return(24)
        );
        assert_eq!(read(&memory, 0x7004).unwrap(), 4);
        assert_eq!(read(&memory, 0x7008).unwrap(), 3);
        assert_eq!(read(&memory, 0x700c).unwrap(), 16);
        assert_eq!(read(&memory, 0x7010).unwrap() >> 16, 32);
        // A device-dependent bitmap has no bits pointer.
        assert_eq!(read(&memory, 0x7014).unwrap(), 0);
        assert_eq!(
            runtime.call("gdi32", "GetObjectA", &[font, 60, 0x7000], &mut memory).unwrap(),
            Effect::Return(60)
        );
        assert_eq!(
            runtime.call("gdi32", "GetTextFaceA", &[dc, 16, 0x7000], &mut memory).unwrap(),
            Effect::Return(6)
        );
        assert_eq!(read_c_string(&memory, 0x7000).unwrap(), b"System");
        memory.write(0x7100, b"AA").unwrap();
        assert_eq!(
            runtime.call("gdi32", "GetTextExtentPointA", &[dc, 0x7100, 2, 0x7200], &mut memory).unwrap(),
            Effect::Return(1)
        );
        let base_width = read(&memory, 0x7200).unwrap();
        assert_eq!(
            runtime.call("gdi32", "SetTextCharacterExtra", &[dc, 3], &mut memory).unwrap(),
            Effect::Return(0)
        );
        assert_eq!(
            runtime.call("gdi32", "GetTextCharacterExtra", &[dc], &mut memory).unwrap(),
            Effect::Return(3)
        );
        runtime.call("gdi32", "GetTextExtentPointA", &[dc, 0x7100, 2, 0x7200], &mut memory).unwrap();
        assert_eq!(read(&memory, 0x7200).unwrap(), base_width + 6);
        memory.write(0x7300, &[0; 12]).unwrap();
        assert_eq!(
            runtime.call("gdi32", "GetCharWidthA", &[dc, b'A' as u32, b'C' as u32, 0x7300], &mut memory).unwrap(),
            Effect::Return(1)
        );
        assert_eq!(read(&memory, 0x7300).unwrap(), crate::font::extent(b"A").0);
        assert_eq!(read(&memory, 0x7308).unwrap(), crate::font::extent(b"C").0);
        assert_eq!(
            runtime.call("gdi32", "GetNearestColor", &[dc, 0x00123456], &mut memory).unwrap(),
            Effect::Return(0x00123456)
        );
        assert_eq!(
            runtime.call("gdi32", "SetPolyFillMode", &[dc, 2], &mut memory).unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime.call("gdi32", "GetPolyFillMode", &[dc], &mut memory).unwrap(),
            Effect::Return(2)
        );
        assert_eq!(
            runtime.call("gdi32", "SetMapperFlags", &[dc, 0x80000001], &mut memory).unwrap(),
            Effect::Return(0)
        );
        assert_eq!(
            runtime.call("gdi32", "GetMapperFlags", &[dc], &mut memory).unwrap(),
            Effect::Return(0x80000001)
        );
        memory.write(0x2000, &[0, 3, 2, 0, 1, 2, 3, 0, 4, 5, 6, 0]).unwrap();
        let palette = value(runtime.call("gdi32", "CreatePalette", &[0x2000], &mut memory).unwrap());
        assert_eq!(
            runtime.call("gdi32", "GetObjectType", &[palette], &mut memory).unwrap(),
            Effect::Return(5)
        );
        assert_eq!(
            runtime.call("gdi32", "GetObjectA", &[palette, 2, 0x7000], &mut memory).unwrap(),
            Effect::Return(2)
        );
        assert_eq!(read(&memory, 0x7000).unwrap() & 0xffff, 2);
    }

    #[test]
    fn create_dc_accepts_the_display_device_names() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let mut write_wide = |memory: &mut Memory, address: u32, text: &str| {
            let mut at = address;
            for unit in text.encode_utf16() {
                memory.write(at, &unit.to_le_bytes()).unwrap();
                at += 2;
            }
            memory.write(at, &[0, 0]).unwrap();
        };
        write_wide(&mut memory, 0x2000, "\\\\.\\DISPLAY1");
        let dc = value(
            runtime
                .call("gdi32", "CreateDCW", &[0x2000, 0, 0, 0], &mut memory)
                .unwrap(),
        );
        assert_ne!(dc, 0, "CreateDCW(\\.\\DISPLAY1)");
        assert_eq!(
            runtime
                .call("gdi32", "GetDeviceCaps", &[dc, 2], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        write_wide(&mut memory, 0x3000, "display");
        let dc = value(
            runtime
                .call("gdi32", "CreateDCW", &[0x3000, 0, 0, 0], &mut memory)
                .unwrap(),
        );
        assert_ne!(dc, 0, "CreateDCW(display)");
        memory.write(0x4000, b"display\0").unwrap();
        let dc = value(
            runtime
                .call("gdi32", "CreateDCA", &[0x4000, 0, 0, 0], &mut memory)
                .unwrap(),
        );
        assert_ne!(dc, 0, "CreateDCA(display)");

        // The display-device enumeration result must round-trip through
        // CreateDCW, which is how dc.c obtains the adapter name.
        assert_eq!(
            runtime
                .call("user32", "EnumDisplayDevicesW", &[0, 0, 0x5000, 0], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        let mut name = Vec::new();
        for index in 0..32u32 {
            let unit = read_u16(&memory, 0x5004 + index * 2);
            if unit == 0 {
                break;
            }
            name.push(unit);
        }
        let name = String::from_utf16(&name).unwrap();
        assert_eq!(name, "\\\\.\\DISPLAY1");
        let dc = value(
            runtime
                .call("gdi32", "CreateDCW", &[0x5004, 0, 0, 0], &mut memory)
                .unwrap(),
        );
        assert_ne!(dc, 0, "CreateDCW(enumerated adapter name)");
        assert_eq!(
            runtime
                .call("gdi32", "GetDeviceCaps", &[dc, 2], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
    }

    #[test]
    fn a_class_dc_survives_delete_dc_until_unregister_class() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        // WNDCLASSEXA: cbSize, style = CS_CLASSDC, lpfnWndProc, ..., class name.
        let mut class = vec![0u8; 48];
        class[0..4].copy_from_slice(&48u32.to_le_bytes());
        class[4..8].copy_from_slice(&0x40u32.to_le_bytes()); // CS_CLASSDC
        class[8..12].copy_from_slice(&0x1234u32.to_le_bytes());
        class[40..44].copy_from_slice(&0x12000u32.to_le_bytes());
        memory.write(0x2000, &class).unwrap();
        memory.write(0x12000, b"class dc\0").unwrap();
        // Match the class procedure to the host's DefWindowProcA thunk so
        // window creation does not dispatch a guest callback.
        runtime.modules.register("user32", "DefWindowProcA", 0x1234);
        let atom = value(
            runtime
                .call("user32", "RegisterClassExA", &[0x2000], &mut memory)
                .unwrap(),
        );
        assert_ne!(atom, 0);
        memory.write(0x12100, b"class dc\0").unwrap();
        let window = value(
            runtime
                .call(
                    "user32",
                    "CreateWindowExA",
                    &[0, 0x12100, 0, 0x9000_0000, 0, 0, 100, 100, 0, 0, 0, 0],
                    &mut memory,
                )
                .unwrap(),
        );
        assert_ne!(window, 0);
        let dc = value(
            runtime
                .call("user32", "GetDC", &[window], &mut memory)
                .unwrap(),
        );
        assert_ne!(dc, 0);
        assert_eq!(
            runtime
                .call("gdi32", "GetObjectType", &[dc], &mut memory)
                .unwrap(),
            Effect::Return(3)
        );
        assert_eq!(
            runtime.call("gdi32", "DeleteDC", &[dc], &mut memory).unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime
                .call("gdi32", "GetDeviceCaps", &[dc, 2], &mut memory)
                .unwrap(),
            Effect::Return(1),
            "class DC must survive DeleteDC"
        );
        assert_eq!(
            runtime.call("user32", "ReleaseDC", &[window, dc], &mut memory).unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime
                .call("gdi32", "GetDeviceCaps", &[dc, 2], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        let window_dc = value(
            runtime
                .call("user32", "GetWindowDC", &[window], &mut memory)
                .unwrap(),
        );
        assert_ne!(window_dc, 0);
        assert_ne!(window_dc, dc);
        assert_eq!(
            runtime
                .call("gdi32", "DeleteDC", &[window_dc], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime
                .call("gdi32", "GetDeviceCaps", &[window_dc, 2], &mut memory)
                .unwrap(),
            Effect::Return(0)
        );
        runtime
            .call("user32", "DestroyWindow", &[window], &mut memory)
            .unwrap();
        assert_eq!(
            runtime
                .call("gdi32", "GetDeviceCaps", &[dc, 2], &mut memory)
                .unwrap(),
            Effect::Return(1),
            "class DC must survive DestroyWindow"
        );
        memory.write(0x12200, b"class dc\0").unwrap();
        assert_eq!(
            runtime
                .call("user32", "UnregisterClassA", &[0x12200, 0], &mut memory)
                .unwrap(),
            Effect::Return(1)
        );
        assert_eq!(
            runtime
                .call("gdi32", "GetDeviceCaps", &[dc, 2], &mut memory)
                .unwrap(),
            Effect::Return(0),
            "class DC dies with the class"
        );
    }

    #[test]
    fn get_dibits_writes_a_core_color_table_in_rgb_order() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        // 1-bpp BITMAPINFO with red and blue entries.
        let mut info = [0u8; 48];
        info[0..4].copy_from_slice(&40u32.to_le_bytes());
        info[4..8].copy_from_slice(&16i32.to_le_bytes());
        info[8..12].copy_from_slice(&16i32.to_le_bytes());
        info[12..14].copy_from_slice(&1u16.to_le_bytes());
        info[14..16].copy_from_slice(&1u16.to_le_bytes());
        info[40..44].copy_from_slice(&[0, 0, 0xff, 0]); // blue field = 0xff? RGBQUAD is B,G,R
        info[44..48].copy_from_slice(&[0xff, 0, 0, 0]);
        memory.write(0x2000, &info).unwrap();
        let dib = value(
            runtime
                .call(
                    "gdi32",
                    "CreateDIBSection",
                    &[0, 0x2000, 0, 0x2100, 0, 0],
                    &mut memory,
                )
                .unwrap(),
        );
        assert_ne!(dib, 0);
        // BITMAPCOREINFO at 0x3000, output bits at 0x4000.
        let mut core = [0u8; 12];
        core[0..4].copy_from_slice(&12u32.to_le_bytes());
        memory.write(0x3000, &core).unwrap();
        let hdc = value(
            runtime
                .call("user32", "GetDC", &[0], &mut memory)
                .unwrap(),
        );
        let lines = value(
            runtime
                .call(
                    "gdi32",
                    "GetDIBits",
                    &[hdc, dib, 0, 16, 0, 0x3000, 0],
                    &mut memory,
                )
                .unwrap(),
        );
        assert_eq!(lines, 16);
        let lines = value(
            runtime
                .call(
                    "gdi32",
                    "GetDIBits",
                    &[hdc, dib, 0, 16, 0x4000, 0x3000, 0],
                    &mut memory,
                )
                .unwrap(),
        );
        assert_eq!(lines, 16);
        assert_eq!(read_u16(&memory, 0x3004), 16);
        assert_eq!(read_u16(&memory, 0x3006), 16);
        assert_eq!(read_u16(&memory, 0x3008), 1);
        assert_eq!(read_u16(&memory, 0x300a), 1);
        let mut table = [0u8; 6];
        memory.read(0x300c, &mut table).unwrap();
        // RGBTRIPLE stores blue, green, red.
        assert_eq!(&table[0..3], &[0, 0, 0xff]);
        assert_eq!(&table[3..6], &[0xff, 0, 0]);
    }

    #[test]
    fn stretch_dibits_accepts_a_1bpp_section() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let mut info = [0u8; 48];
        info[0..4].copy_from_slice(&40u32.to_le_bytes());
        info[4..8].copy_from_slice(&2i32.to_le_bytes());
        info[8..12].copy_from_slice(&2i32.to_le_bytes());
        info[12..14].copy_from_slice(&1u16.to_le_bytes());
        info[14..16].copy_from_slice(&1u16.to_le_bytes());
        info[40..44].copy_from_slice(&[0, 0, 0, 0]);
        info[44..48].copy_from_slice(&[0xff, 0xff, 0xff, 0]);
        memory.write(0x2000, &info).unwrap();
        memory.write(0x2100, &[0xff, 0, 0, 0]).unwrap();
        let dc = value(
            runtime
                .call("gdi32", "CreateCompatibleDC", &[0], &mut memory)
                .unwrap(),
        );
        assert_ne!(dc, 0);
        let direct = runtime.gdi.stretch_dib(
            &mut memory,
            dc,
            0,
            0,
            1,
            1,
            0,
            0,
            1,
            1,
            0x2100,
            0x2000,
            0x00cc0020,
        );
        assert!(direct.is_ok(), "{direct:?}");
    }

    #[test]
    fn create_dibitmap_initialises_from_a_24bpp_dib() {
        let mut runtime = Runtime::default();
        let mut memory = Memory::default();
        let hdc = value(
            runtime
                .call("user32", "GetDC", &[0], &mut memory)
                .unwrap(),
        );
        // 1x1 1-bpp destination header.
        let mut header = [0u8; 40];
        header[0..4].copy_from_slice(&40u32.to_le_bytes());
        header[4..8].copy_from_slice(&1i32.to_le_bytes());
        header[8..12].copy_from_slice(&1i32.to_le_bytes());
        header[12..14].copy_from_slice(&1u16.to_le_bytes());
        header[14..16].copy_from_slice(&1u16.to_le_bytes());
        memory.write(0x2000, &header).unwrap();
        // 24-bpp white source.
        let mut info = [0u8; 40];
        info[0..4].copy_from_slice(&40u32.to_le_bytes());
        info[4..8].copy_from_slice(&1i32.to_le_bytes());
        info[8..12].copy_from_slice(&1i32.to_le_bytes());
        info[12..14].copy_from_slice(&1u16.to_le_bytes());
        info[14..16].copy_from_slice(&24u16.to_le_bytes());
        memory.write(0x2100, &info).unwrap();
        memory.write(0x2200, &[0xff, 0xff, 0xff, 0]).unwrap();
        let bitmap = value(
            runtime
                .call(
                    "gdi32",
                    "CreateDIBitmap",
                    &[hdc, 0x2000, 4, 0x2200, 0x2100, 0],
                    &mut memory,
                )
                .unwrap(),
        );
        assert_ne!(bitmap, 0);
        // Read back as 32 bpp.
        header[14..16].copy_from_slice(&32u16.to_le_bytes());
        memory.write(0x3000, &header).unwrap();
        memory.write(0x3100, &0xdeadbeefu32.to_le_bytes()).unwrap();
        let info_dib = runtime.gdi.bitmap_dib(bitmap);
        let result = runtime.call(
            "gdi32",
            "GetDIBits",
            &[hdc, bitmap, 0, 1, 0x3100, 0x3000, 0],
            &mut memory,
        );
        let lines = value(result.unwrap());
        assert_eq!((lines, info_dib.map(|d| (d.width, d.height, d.bit_count()))), (1, Some((1, 1, 32))));
        assert_eq!(read(&memory, 0x3100).unwrap(), 0x00ffffff);
    }
}
