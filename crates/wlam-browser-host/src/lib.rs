//! Thin wasm32 ABI around the engine-independent host. JavaScript owns guest
//! memory and executes callback actions only after each exported call returns.
#![cfg(target_arch = "wasm32")]
use std::sync::Mutex;
use wlam_host::{
    gl_renderer::GpuOp,
    memory::{MemoryAccess, MemoryError},
    runtime::{Effect, Runtime},
    window::Message,
};
#[link(wasm_import_module = "guest")]
extern "C" {
    fn read_guest(address: u32, destination: u32, length: u32) -> u32;
    fn write_guest(address: u32, source: u32, length: u32) -> u32;
    fn guest_length() -> u64;
}
struct Memory;
impl MemoryAccess for Memory {
    fn byte_len(&self) -> u64 {
        unsafe { guest_length() }
    }
    fn read(&self, address: u32, output: &mut [u8]) -> Result<(), MemoryError> {
        let length = u32::try_from(output.len()).map_err(|_| MemoryError {
            address,
            length: output.len(),
        })?;
        // The imported bridge writes only into this live, exclusive buffer.
        let success = unsafe { read_guest(address, output.as_mut_ptr() as u32, length) };
        if success == 1 {
            Ok(())
        } else {
            Err(MemoryError {
                address,
                length: output.len(),
            })
        }
    }
    fn write(&mut self, address: u32, input: &[u8]) -> Result<(), MemoryError> {
        let length = u32::try_from(input.len()).map_err(|_| MemoryError {
            address,
            length: input.len(),
        })?;
        // The bridge copies this live immutable buffer before returning.
        let success = unsafe { write_guest(address, input.as_ptr() as u32, length) };
        if success == 1 {
            Ok(())
        } else {
            Err(MemoryError {
                address,
                length: input.len(),
            })
        }
    }
}
const APIS: &[(&str, &str)] = &[
    ("USER32.dll", "LoadStringA"),
    ("KERNEL32.dll", "lstrlenA"),
    ("KERNEL32.dll", "lstrlenW"),
    ("KERNEL32.dll", "lstrcpyA"),
    ("KERNEL32.dll", "lstrcpyW"),
    ("KERNEL32.dll", "lstrcatW"),
    ("KERNEL32.dll", "lstrcatA"),
    ("USER32.dll", "GetWindowLongA"),
    ("USER32.dll", "SetWindowLongA"),
    ("USER32.dll", "GetParent"),
    ("USER32.dll", "GetDlgCtrlID"),
    ("USER32.dll", "GetWindowRect"),
    ("USER32.dll", "GetForegroundWindow"),
    ("USER32.dll", "SetFocus"),
    ("USER32.dll", "GetFocus"),
    ("USER32.dll", "GetClassNameA"),
    ("USER32.dll", "wsprintfA"),
    ("USER32.dll", "CallNextHookEx"),
    ("USER32.dll", "CallWindowProcA"),
    ("USER32.dll", "LoadIconA"),
    ("USER32.dll", "LoadMenuA"),
    ("USER32.dll", "LoadAcceleratorsA"),
    ("USER32.dll", "TranslateAcceleratorA"),
    ("USER32.dll", "GetMenu"),
    ("USER32.dll", "SetMenu"),
    ("KERNEL32.dll", "FindResourceA"),
    ("KERNEL32.dll", "LoadResource"),
    ("KERNEL32.dll", "LockResource"),
    ("KERNEL32.dll", "SizeofResource"),
    ("KERNEL32.dll", "FreeResource"),
    ("USER32.dll", "RegisterClassA"),
    ("USER32.dll", "GetClassInfoA"),
    ("USER32.dll", "SetWindowsHookExA"),
    ("USER32.dll", "UnhookWindowsHookEx"),
    ("KERNEL32.dll", "SetErrorMode"),
    ("USER32.dll", "SetMessageQueue"),
    ("COMCTL32.dll", "#17"),
    ("COMCTL32.dll", "InitCommonControls"),
    ("USER32.dll", "RegisterWindowMessageA"),
    ("USER32.dll", "LoadCursorA"),
    ("USER32.dll", "SetCursor"),
    ("USER32.dll", "ShowCursor"),
    ("WINMM.dll", "timeSetEvent"),
    ("GDI32.dll", "CreatePen"),
    ("GDI32.dll", "CreatePenIndirect"),
    ("GDI32.dll", "ExtCreatePen"),
    ("USER32.dll", "GetSysColor"),
    ("GDI32.dll", "CreateSolidBrush"),
    ("GDI32.dll", "CreateHatchBrush"),
    ("GDI32.dll", "CreatePatternBrush"),
    ("GDI32.dll", "CreateBrushIndirect"),
    ("GDI32.dll", "CreateDIBPatternBrushPt"),
    ("GDI32.dll", "FillRect"),
    ("GDI32.dll", "GetTextExtentPointA"),
    ("GDI32.dll", "GetTextExtentPoint32A"),
    ("GDI32.dll", "GetTextMetricsA"),
    ("GDI32.dll", "GdiFlush"),
    ("GDI32.dll", "UnrealizeObject"),
    ("GDI32.dll", "GetTextFaceA"),
    ("GDI32.dll", "GetCharWidthA"),
    ("GDI32.dll", "SetTextCharacterExtra"),
    ("GDI32.dll", "GetTextCharacterExtra"),
    ("GDI32.dll", "SetMapperFlags"),
    ("GDI32.dll", "GetMapperFlags"),
    ("GDI32.dll", "SetPolyFillMode"),
    ("GDI32.dll", "GetPolyFillMode"),
    ("GDI32.dll", "GetObjectType"),
    ("GDI32.dll", "GetCurrentObject"),
    ("GDI32.dll", "GetObjectA"),
    ("GDI32.dll", "GetObjectW"),
    ("GDI32.dll", "GetNearestColor"),
    ("GDI32.dll", "CreateCompatibleBitmap"),
    ("GDI32.dll", "CreateDiscardableBitmap"),
    ("GDI32.dll", "CreateBitmapIndirect"),
    ("GDI32.dll", "CreateBitmap"),
    ("GDI32.dll", "CreateDCA"),
    ("GDI32.dll", "CreateDCW"),
    ("USER32.dll", "EnumDisplayDevicesA"),
    ("USER32.dll", "EnumDisplayDevicesW"),
    ("USER32.dll", "EnumDisplaySettingsA"),
    ("USER32.dll", "EnumDisplayMonitors"),
    ("USER32.dll", "UnregisterClassA"),
    ("USER32.dll", "UnregisterClassW"),
    ("GDI32.dll", "GetBitmapBits"),
    ("GDI32.dll", "SetBitmapBits"),
    ("GDI32.dll", "GetDIBits"),
    ("GDI32.dll", "SetDIBits"),
    ("GDI32.dll", "GetDIBColorTable"),
    ("GDI32.dll", "SetDIBColorTable"),
    ("GDI32.dll", "SetDIBitsToDevice"),
    ("KERNEL32.dll", "VirtualQuery"),
    ("KERNEL32.dll", "GetTempFileNameA"),
    ("KERNEL32.dll", "DeleteFileA"),
    ("KERNEL32.dll", "DeleteFileW"),
    ("GDI32.dll", "CreateFontIndirectA"),
    ("USER32.dll", "GetDC"),
    ("USER32.dll", "GetWindowDC"),
    ("USER32.dll", "ReleaseDC"),
    ("USER32.dll", "GetDesktopWindow"),
    ("GDI32.dll", "GetDeviceCaps"),
    ("GDI32.dll", "ChoosePixelFormat"),
    ("GDI32.dll", "DescribePixelFormat"),
    ("GDI32.dll", "SetPixelFormat"),
    ("GDI32.dll", "GetPixelFormat"),
    ("GDI32.dll", "SwapBuffers"),
    ("OPENGL32.dll", "wglCreateContext"),
    ("OPENGL32.dll", "wglMakeCurrent"),
    ("OPENGL32.dll", "wglDeleteContext"),
    ("OPENGL32.dll", "glBegin"),
    ("OPENGL32.dll", "glBlendFunc"),
    ("OPENGL32.dll", "glCallList"),
    ("OPENGL32.dll", "glClear"),
    ("OPENGL32.dll", "glClearColor"),
    ("OPENGL32.dll", "glClearDepth"),
    ("OPENGL32.dll", "glClearIndex"),
    ("OPENGL32.dll", "glColor3ubv"),
    ("OPENGL32.dll", "glColorMaterial"),
    ("OPENGL32.dll", "glCullFace"),
    ("OPENGL32.dll", "glDepthFunc"),
    ("OPENGL32.dll", "glDisable"),
    ("OPENGL32.dll", "glDrawBuffer"),
    ("OPENGL32.dll", "glEnable"),
    ("OPENGL32.dll", "glEnd"),
    ("OPENGL32.dll", "glEndList"),
    ("OPENGL32.dll", "glFogf"),
    ("OPENGL32.dll", "glFogfv"),
    ("OPENGL32.dll", "glFogi"),
    ("OPENGL32.dll", "glFlush"),
    ("OPENGL32.dll", "glGenLists"),
    ("OPENGL32.dll", "glGetError"),
    ("OPENGL32.dll", "glIndexi"),
    ("OPENGL32.dll", "glLightModelfv"),
    ("OPENGL32.dll", "glLightfv"),
    ("OPENGL32.dll", "glLoadIdentity"),
    ("OPENGL32.dll", "glMaterialfv"),
    ("OPENGL32.dll", "glMatrixMode"),
    ("OPENGL32.dll", "glNewList"),
    ("OPENGL32.dll", "glNormal3f"),
    ("OPENGL32.dll", "glOrtho"),
    ("OPENGL32.dll", "glPopMatrix"),
    ("OPENGL32.dll", "glPushMatrix"),
    ("OPENGL32.dll", "glRotatef"),
    ("OPENGL32.dll", "glShadeModel"),
    ("OPENGL32.dll", "glTranslatef"),
    ("OPENGL32.dll", "glVertex3f"),
    ("OPENGL32.dll", "glViewport"),
    ("GLU32.dll", "gluLookAt"),
    ("GLU32.dll", "gluPerspective"),
    ("USER32.dll", "GetSystemMetrics"),
    ("KERNEL32.dll", "IsBadCodePtr"),
    ("KERNEL32.dll", "GlobalAddAtomA"),
    ("KERNEL32.dll", "GlobalDeleteAtom"),
    ("KERNEL32.dll", "GlobalGetAtomNameA"),
    ("KERNEL32.dll", "GlobalAlloc"),
    ("KERNEL32.dll", "GlobalFree"),
    ("KERNEL32.dll", "GlobalLock"),
    ("KERNEL32.dll", "GlobalUnlock"),
    ("KERNEL32.dll", "GlobalFlags"),
    ("KERNEL32.dll", "GlobalHandle"),
    ("KERNEL32.dll", "LocalAlloc"),
    ("KERNEL32.dll", "LocalFree"),
    ("KERNEL32.dll", "GetSystemInfo"),
    ("KERNEL32.dll", "GlobalMemoryStatus"),
    ("KERNEL32.dll", "CreateEventA"),
    ("KERNEL32.dll", "SetEvent"),
    ("KERNEL32.dll", "ResetEvent"),
    ("KERNEL32.dll", "CloseHandle"),
    ("KERNEL32.dll", "WaitForSingleObject"),
    ("USER32.dll", "GetKeyState"),
    ("USER32.dll", "MessageBoxA"),
    ("KERNEL32.dll", "GetCurrentThread"),
    ("KERNEL32.dll", "SetThreadPriority"),
    ("KERNEL32.dll", "GetCurrentProcess"),
    ("USER32.dll", "GetCursorPos"),
    ("KERNEL32.dll", "SetUnhandledExceptionFilter"),
    ("KERNEL32.dll", "MultiByteToWideChar"),
    ("KERNEL32.dll", "LoadLibraryA"),
    ("KERNEL32.dll", "FreeLibrary"),
    ("KERNEL32.dll", "GetProcAddress"),
    ("KERNEL32.dll", "GetStartupInfoA"),
    ("KERNEL32.dll", "GetCommandLineA"),
    ("KERNEL32.dll", "GetEnvironmentStrings"),
    ("KERNEL32.dll", "GetModuleFileNameA"),
    ("KERNEL32.dll", "GetACP"),
    ("KERNEL32.dll", "GetOEMCP"),
    ("KERNEL32.dll", "GetCPInfo"),
    ("KERNEL32.dll", "GetStdHandle"),
    ("KERNEL32.dll", "SetStdHandle"),
    ("KERNEL32.dll", "GetFileType"),
    ("KERNEL32.dll", "SetHandleCount"),
    ("KERNEL32.dll", "VirtualAlloc"),
    ("KERNEL32.dll", "VirtualFree"),
    ("KERNEL32.dll", "GetCurrentThreadId"),
    ("KERNEL32.dll", "TlsAlloc"),
    ("KERNEL32.dll", "TlsFree"),
    ("KERNEL32.dll", "TlsGetValue"),
    ("KERNEL32.dll", "TlsSetValue"),
    ("KERNEL32.dll", "InitializeCriticalSection"),
    ("KERNEL32.dll", "EnterCriticalSection"),
    ("KERNEL32.dll", "TryEnterCriticalSection"),
    ("KERNEL32.dll", "LeaveCriticalSection"),
    ("KERNEL32.dll", "DeleteCriticalSection"),
    ("KERNEL32.dll", "GetVersion"),
    ("KERNEL32.dll", "GetTickCount"),
    ("KERNEL32.dll", "MulDiv"),
    ("USER32.dll", "GetClientRect"),
    ("USER32.dll", "AdjustWindowRect"),
    ("USER32.dll", "AdjustWindowRectEx"),
    ("USER32.dll", "MoveWindow"),
    ("USER32.dll", "SetWindowPos"),
    ("USER32.dll", "BeginDeferWindowPos"),
    ("USER32.dll", "DeferWindowPos"),
    ("USER32.dll", "EndDeferWindowPos"),
    ("USER32.dll", "ClientToScreen"),
    ("USER32.dll", "ScreenToClient"),
    ("USER32.dll", "GetTopWindow"),
    ("USER32.dll", "GetWindow"),
    ("USER32.dll", "SetTimer"),
    ("USER32.dll", "KillTimer"),
    ("USER32.dll", "PostMessageA"),
    ("KERNEL32.dll", "GetModuleHandleA"),
    ("KERNEL32.dll", "GetPrivateProfileIntA"),
    ("KERNEL32.dll", "GetPrivateProfileStringA"),
    ("KERNEL32.dll", "GetCurrentDirectoryA"),
    ("KERNEL32.dll", "OpenFile"),
    ("KERNEL32.dll", "CreateFileA"),
    ("KERNEL32.dll", "lstrcmpA"),
    ("KERNEL32.dll", "SetFilePointer"),
    ("KERNEL32.dll", "ReadFile"),
    ("KERNEL32.dll", "WriteFile"),
    ("ADVAPI32.dll", "RegOpenKeyExA"),
    ("ADVAPI32.dll", "RegOpenKeyA"),
    ("ADVAPI32.dll", "RegCloseKey"),
    ("ADVAPI32.dll", "RegCreateKeyExA"),
    ("ADVAPI32.dll", "RegQueryValueExA"),
    ("ADVAPI32.dll", "RegSetValueExA"),
    ("ADVAPI32.dll", "RegDeleteKeyA"),
    ("ADVAPI32.dll", "RegDeleteValueA"),
    ("ADVAPI32.dll", "RegQueryValueA"),
    ("USER32.dll", "GetPropA"),
    ("USER32.dll", "SetPropA"),
    ("USER32.dll", "RemovePropA"),
    ("KERNEL32.dll", "IsWow64Process"),
    ("KERNEL32.dll", "GetLocalTime"),
    ("KERNEL32.dll", "GetTimeZoneInformation"),
    ("MSVCRT.dll", "_adjust_fdiv"),
    ("MSVCRT.dll", "__set_app_type"),
    ("MSVCRT.dll", "_controlfp"),
    ("MSVCRT.dll", "_control87"),
    ("MSVCRT.dll", "__p__acmdln"),
    ("MSVCRT.dll", "__p__commode"),
    ("MSVCRT.dll", "__p__fmode"),
    ("MSVCRT.dll", "__getmainargs"),
    ("MSVCRT.dll", "_initterm"),
    ("MSVCRT.dll", "_onexit"),
    ("MSVCRT.dll", "atexit"),
    ("MSVCRT.dll", "__dllonexit"),
    ("MSVCRT.dll", "exit"),
    ("MSVCRT.dll", "_exit"),
    ("MSVCRT.dll", "_XcptFilter"),
    ("MSVCRT.dll", "__CxxFrameHandler"),
    ("MSVCRT.dll", "_except_handler3"),
    ("MSVCRT.dll", "??2@YAPAXI@Z"),
    ("MSVCRT.dll", "??3@YAXPAX@Z"),
    ("MSVCRT.dll", "?set_terminate@@YAP6AXXZP6AXXZ@Z"),
    ("MSVCRT.dll", "?terminate@@YAXXZ"),
    ("MSVCRT.dll", "abort"),
    ("MSVCRT.dll", "malloc"),
    ("MSVCRT.dll", "free"),
    ("MSVCRT.dll", "calloc"),
    ("MSVCRT.dll", "realloc"),
    ("MSVCRT.dll", "memset"),
    ("MSVCRT.dll", "memcpy"),
    ("MSVCRT.dll", "memmove"),
    ("MSVCRT.dll", "memcmp"),
    ("MSVCRT.dll", "strlen"),
    ("MSVCRT.dll", "strcpy"),
    ("MSVCRT.dll", "strcmp"),
    ("MSVCRT.dll", "strncmp"),
    ("MSVCRT.dll", "strchr"),
    ("MSVCRT.dll", "strrchr"),
    ("MSVCRT.dll", "atoi"),
    ("MSVCRT.dll", "abs"),
    ("KERNEL32.dll", "FileTimeToLocalFileTime"),
    ("KERNEL32.dll", "FileTimeToDosDateTime"),
    ("KERNEL32.dll", "GetWindowsDirectoryA"),
    ("KERNEL32.dll", "GetLastError"),
    ("KERNEL32.dll", "SetLastError"),
    ("KERNEL32.dll", "ExitProcess"),
    ("USER32.dll", "RegisterClassExA"),
    ("USER32.dll", "CreateWindowExA"),
    ("USER32.dll", "CreateDialogParamA"),
    ("USER32.dll", "DialogBoxParamA"),
    ("USER32.dll", "DialogBoxIndirectParamA"),
    ("USER32.dll", "EndDialog"),
    ("USER32.dll", "SetDlgItemTextA"),
    ("USER32.dll", "SetWindowTextA"),
    ("USER32.dll", "GetDlgItem"),
    ("USER32.dll", "SetRect"),
    ("USER32.dll", "SetRectEmpty"),
    ("USER32.dll", "CopyRect"),
    ("USER32.dll", "EqualRect"),
    ("USER32.dll", "OffsetRect"),
    ("USER32.dll", "InflateRect"),
    ("USER32.dll", "IntersectRect"),
    ("USER32.dll", "UnionRect"),
    ("USER32.dll", "PtInRect"),
    ("USER32.dll", "ShowWindow"),
    ("USER32.dll", "BringWindowToTop"),
    ("USER32.dll", "GetLastActivePopup"),
    ("USER32.dll", "IsWindowVisible"),
    ("USER32.dll", "IsWindow"),
    ("USER32.dll", "IsIconic"),
    ("USER32.dll", "IsZoomed"),
    ("USER32.dll", "CheckDlgButton"),
    ("USER32.dll", "IsDlgButtonChecked"),
    ("USER32.dll", "CheckRadioButton"),
    ("USER32.dll", "EnableWindow"),
    ("USER32.dll", "IsWindowEnabled"),
    ("USER32.dll", "DefWindowProcA"),
    ("USER32.dll", "PostQuitMessage"),
    ("USER32.dll", "DestroyWindow"),
    ("USER32.dll", "InvalidateRect"),
    ("USER32.dll", "UpdateWindow"),
    ("USER32.dll", "RedrawWindow"),
    ("GDI32.dll", "SaveDC"),
    ("GDI32.dll", "RestoreDC"),
    ("GDI32.dll", "SetTextColor"),
    ("GDI32.dll", "SetBkColor"),
    ("GDI32.dll", "SetBkMode"),
    ("GDI32.dll", "SetTextAlign"),
    ("GDI32.dll", "SetStretchBltMode"),
    ("GDI32.dll", "SetROP2"),
    ("GDI32.dll", "GetTextColor"),
    ("GDI32.dll", "GetBkColor"),
    ("GDI32.dll", "GetBkMode"),
    ("GDI32.dll", "GetTextAlign"),
    ("GDI32.dll", "GetStretchBltMode"),
    ("GDI32.dll", "GetROP2"),
    ("GDI32.dll", "GetMapMode"),
    ("GDI32.dll", "GetMiterLimit"),
    ("GDI32.dll", "SetMiterLimit"),
    ("GDI32.dll", "CreateRectRgn"),
    ("GDI32.dll", "CreateRectRgnIndirect"),
    ("GDI32.dll", "SetRectRgn"),
    ("GDI32.dll", "GetRgnBox"),
    ("GDI32.dll", "GetRegionData"),
    ("GDI32.dll", "GetClipRgn"),
    ("GDI32.dll", "GetBoundsRect"),
    ("GDI32.dll", "GetDeviceGammaRamp"),
    ("GDI32.dll", "ResetDCA"),
    ("GDI32.dll", "CreateEnhMetaFileA"),
    ("GDI32.dll", "CreateEnhMetaFileW"),
    ("GDI32.dll", "CreateMetaFileA"),
    ("GDI32.dll", "CreateMetaFileW"),
    ("GDI32.dll", "CloseEnhMetaFile"),
    ("GDI32.dll", "CloseMetaFile"),
    ("GDI32.dll", "DeleteEnhMetaFile"),
    ("GDI32.dll", "DeleteMetaFile"),
    ("GDI32.dll", "ResetDCW"),
    ("GDI32.dll", "SetDeviceGammaRamp"),
    ("GDI32.dll", "SetBoundsRect"),
    ("GDI32.dll", "SelectClipRgn"),
    ("GDI32.dll", "SetMapMode"),
    ("GDI32.dll", "GetViewportExtEx"),
    ("GDI32.dll", "GetViewportOrgEx"),
    ("GDI32.dll", "GetWindowExtEx"),
    ("GDI32.dll", "GetWindowOrgEx"),
    ("GDI32.dll", "SetViewportExtEx"),
    ("GDI32.dll", "SetViewportOrgEx"),
    ("GDI32.dll", "SetWindowExtEx"),
    ("GDI32.dll", "SetWindowOrgEx"),
    ("GDI32.dll", "OffsetViewportOrgEx"),
    ("GDI32.dll", "OffsetWindowOrgEx"),
    ("GDI32.dll", "SetLayout"),
    ("GDI32.dll", "GetLayout"),
    ("GDI32.dll", "IntersectClipRect"),
    ("GDI32.dll", "GetClipBox"),
    ("GDI32.dll", "TextOutA"),
    ("GDI32.dll", "ExtTextOutA"),
    ("USER32.dll", "CharUpperA"),
    ("USER32.dll", "CharLowerA"),
    ("USER32.dll", "CharUpperBuffA"),
    ("USER32.dll", "CharLowerBuffA"),
    ("GDI32.dll", "MoveToEx"),
    ("GDI32.dll", "GetCurrentPositionEx"),
    ("GDI32.dll", "LineTo"),
    ("GDI32.dll", "Rectangle"),
    ("GDI32.dll", "GetPixel"),
    ("GDI32.dll", "SetPixel"),
    ("GDI32.dll", "SetPixelV"),
    ("GDI32.dll", "StretchBlt"),
    ("GDI32.dll", "StretchDIBits"),
    ("GDI32.dll", "SetBrushOrgEx"),
    ("GDI32.dll", "FixBrushOrgEx"),
    ("GDI32.dll", "GetBrushOrgEx"),
    ("GDI32.dll", "PatBlt"),
    ("USER32.dll", "SendMessageA"),
    ("USER32.dll", "DispatchMessageA"),
    ("USER32.dll", "TranslateMessage"),
    ("USER32.dll", "GetMessageA"),
    ("USER32.dll", "PeekMessageA"),
    ("USER32.dll", "BeginPaint"),
    ("USER32.dll", "EndPaint"),
    ("GDI32.dll", "CreateCompatibleDC"),
    ("GDI32.dll", "CreatePalette"),
    ("GDI32.dll", "SelectPalette"),
    ("GDI32.dll", "RealizePalette"),
    ("GDI32.dll", "GetPaletteEntries"),
    ("GDI32.dll", "SetPaletteEntries"),
    ("GDI32.dll", "GetNearestPaletteIndex"),
    ("GDI32.dll", "GetSystemPaletteEntries"),
    ("GDI32.dll", "CreateDIBSection"),
    ("GDI32.dll", "CreateDIBitmap"),
    ("GDI32.dll", "GetStockObject"),
    ("WINMM.dll", "waveOutGetNumDevs"),
    ("WINMM.dll", "midiOutGetNumDevs"),
    ("WINMM.dll", "timeGetTime"),
    ("WINMM.dll", "timeKillEvent"),
    ("WINMM.dll", "joyGetDevCapsA"),
    ("WINMM.dll", "joyGetPos"),
    ("wlam.x87", "sin"),
    ("wlam.x87", "cos"),
    ("wlam.x87", "tan"),
    ("wlam.x87", "atan2"),
    ("GDI32.dll", "SelectObject"),
    ("GDI32.dll", "DeleteObject"),
    ("GDI32.dll", "DeleteDC"),
    ("GDI32.dll", "BitBlt"),
];
struct Bridge {
    runtime: Runtime,
    effect: Effect,
    error: String,
    frame: Vec<u8>,
    gpu_packet: Vec<u32>,
    width: u32,
    height: u32,
}
static STATE: Mutex<Option<Bridge>> = Mutex::new(None);
fn kind(state: &Bridge) -> u32 {
    if !state.error.is_empty() {
        return 4;
    }
    match state.effect {
        Effect::Return(_) => 0,
        Effect::Invoke { .. } => 1,
        Effect::Wait => 2,
        Effect::Exit(_) => 3,
        Effect::MessageBox { .. } => 5,
        Effect::Retry => 6,
    }
}
#[no_mangle]
pub extern "C" fn initialize(image_base: u32) {
    let mut runtime = Runtime::default();
    runtime.image_base = image_base;
    *STATE.lock().unwrap() = Some(Bridge {
        runtime,
        effect: Effect::Return(0),
        error: String::new(),
        frame: Vec::new(),
        gpu_packet: Vec::new(),
        width: 0,
        height: 0,
    });
}
/// Select an opt-in guest display profile before program startup.
#[no_mangle]
pub extern "C" fn set_display_bits(bits: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let Some(state) = lock.as_mut() else {
        return 0;
    };
    match state.runtime.set_display_bits(bits) {
        Ok(()) => 1,
        Err(error) => {
            state.error = error;
            0
        }
    }
}
/// Register a read-only browser asset from guest memory. The runner stages
/// assets in newly grown guest pages before entering the program.
#[no_mangle]
pub extern "C" fn register_file(path: u32, data: u32, length: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let Some(state) = lock.as_mut() else {
        return 0;
    };
    let mut memory = Memory;
    let result = (|| {
        let path = wlam_host::runtime::read_c_string(&memory, path)?;
        let mut bytes = vec![0; length as usize];
        memory
            .read(data, &mut bytes)
            .map_err(|e| format!("asset read: {e:?}"))?;
        state.runtime.register_file(&path, bytes);
        Ok::<(), String>(())
    })();
    match result {
        Ok(()) => {
            state.error.clear();
            1
        }
        Err(error) => {
            state.error = error;
            0
        }
    }
}
#[no_mangle]
pub extern "C" fn api_count() -> u32 {
    APIS.len() as u32
}
#[no_mangle]
pub extern "C" fn register_import(index: u32, address: u32) {
    if let Some((dll, name)) = APIS.get(index as usize) {
        if let Some(state) = STATE.lock().unwrap().as_mut() {
            state.runtime.modules.register(dll, name, address);
        }
    }
}
#[no_mangle]
pub extern "C" fn register_code(address: u32, length: u32) {
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        state.runtime.register_code(address, length);
    }
}
#[no_mangle]
pub extern "C" fn register_resource(
    kind: u32,
    name: u32,
    language: u32,
    address: u32,
    size: u32,
) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let state = lock.as_mut().unwrap();
    match state
        .runtime
        .resources
        .register(kind, name, language, address, size)
    {
        Ok(()) => 1,
        Err(error) => {
            state.error = error;
            0
        }
    }
}
#[no_mangle]
pub extern "C" fn api_text(index: u32, field: u32, length: u32) -> u32 {
    let Some(api) = APIS.get(index as usize) else {
        return 0;
    };
    let text = if field == 0 { api.0 } else { api.1 };
    if length == 0 {
        text.as_ptr() as u32
    } else {
        text.len() as u32
    }
}
fn store_result(state: &mut Bridge, result: Result<Effect, String>) -> u32 {
    match result {
        Ok(effect) => {
            state.error.clear();
            state.effect = effect;
        }
        Err(error) => state.error = error,
    };
    kind(state)
}
#[no_mangle]
pub extern "C" fn dispatch(index: u32, arguments: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let state = lock.as_mut().unwrap();
    let result = (|| {
        let (dll, name) = APIS
            .get(index as usize)
            .ok_or("unknown browser API index")?;
        let signature = wlam_pcode::abi::signature(dll, name).ok_or("missing API signature")?;
        let mut args = Vec::new();
        let mut memory = Memory;
        for offset in (0..signature.argument_bytes).step_by(4) {
            let address = arguments
                .checked_add(offset)
                .ok_or("argument address overflow")?;
            args.push(
                memory
                    .read_word(address)
                    .map_err(|e| format!("arguments: {e:?}"))?,
            );
        }
        if *dll == "USER32.dll" && *name == "wsprintfA" {
            let format = wlam_host::runtime::read_c_string(&memory, args[1])
                .map_err(|e| format!("wsprintf format: {e}"))?;
            let count = format
                .iter()
                .enumerate()
                .filter(|(i, b)| {
                    if **b != b'%' {
                        return false;
                    }
                    let mut at = *i + 1;
                    while at < format.len() && !b"diuoxXcCsSp%".contains(&format[at]) {
                        at += 1;
                    }
                    at < format.len() && format[at] != b'%'
                })
                .count();
            for index in 0..count {
                let offset = signature
                    .argument_bytes
                    .checked_add(index as u32 * 4)
                    .ok_or("wsprintf arguments overflow")?;
                let address = arguments
                    .checked_add(offset)
                    .ok_or("wsprintf argument address overflow")?;
                args.push(
                    memory
                        .read_word(address)
                        .map_err(|e| format!("wsprintf arguments: {e:?}"))?,
                );
            }
        }
        state.runtime.call(dll, name, &args, &mut memory)
    })();
    store_result(state, result)
}
#[no_mangle]
pub extern "C" fn resume(token: u64, value: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let state = lock.as_mut().unwrap();
    let mut memory = Memory;
    let result = state.runtime.resume_with_memory(token, value, &mut memory);
    store_result(state, result)
}
#[no_mangle]
pub extern "C" fn action_value() -> u32 {
    let lock = STATE.lock().unwrap();
    match lock.as_ref().unwrap().effect {
        Effect::Return(v) | Effect::Exit(v) | Effect::MessageBox { flags: v, .. } => v,
        _ => 0,
    }
}
#[no_mangle]
pub extern "C" fn action_token() -> u64 {
    let lock = STATE.lock().unwrap();
    match lock.as_ref().unwrap().effect {
        Effect::Invoke { token, .. } => token,
        _ => 0,
    }
}
#[no_mangle]
pub extern "C" fn dialog_text(field: u32, length: u32) -> u32 {
    let lock = STATE.lock().unwrap();
    match &lock.as_ref().unwrap().effect {
        Effect::MessageBox { text, title, .. } => {
            let bytes = if field == 0 { text } else { title };
            if length == 0 {
                bytes.as_ptr() as u32
            } else {
                bytes.len() as u32
            }
        }
        _ => 0,
    }
}
#[no_mangle]
pub extern "C" fn action_procedure() -> u32 {
    let lock = STATE.lock().unwrap();
    match lock.as_ref().unwrap().effect {
        Effect::Invoke { procedure, .. } => procedure,
        _ => 0,
    }
}
#[no_mangle]
pub extern "C" fn action_argument(index: u32) -> u32 {
    let lock = STATE.lock().unwrap();
    match &lock.as_ref().unwrap().effect {
        Effect::Invoke { args, .. } => args.get(index as usize).copied().unwrap_or(0),
        _ => 0,
    }
}
#[no_mangle]
pub extern "C" fn action_argument_count() -> u32 {
    let lock = STATE.lock().unwrap();
    match &lock.as_ref().unwrap().effect {
        Effect::Invoke { args, .. } => args.len() as u32,
        _ => 0,
    }
}
#[no_mangle]
pub extern "C" fn error_text(length: u32) -> u32 {
    let lock = STATE.lock().unwrap();
    let error = &lock.as_ref().unwrap().error;
    if length == 0 {
        error.as_ptr() as u32
    } else {
        error.len() as u32
    }
}
#[no_mangle]
pub extern "C" fn post(window: u32, message: u32, wparam: u32, lparam: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let state = lock.as_mut().unwrap();
    let result = state
        .runtime
        .post(Message {
            window,
            message,
            wparam,
            lparam,
        })
        .map(|_| Effect::Return(1));
    store_result(state, result)
}
#[no_mangle]
pub extern "C" fn resize_window(window: u32, width: u32, height: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let state = lock.as_mut().unwrap();
    match state.runtime.resize_window(window, width, height) {
        Ok(()) => {
            state.error.clear();
            1
        }
        Err(error) => {
            state.error = error;
            0
        }
    }
}
#[no_mangle]
pub extern "C" fn wait_timeout() -> u32 {
    STATE
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .runtime
        .next_timer_delay()
}
#[no_mangle]
pub extern "C" fn advance_time(elapsed: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let state = lock.as_mut().unwrap();
    match state.runtime.advance_time(elapsed) {
        Ok(()) => 1,
        Err(error) => {
            state.error = error;
            0
        }
    }
}
#[no_mangle]
pub extern "C" fn presentation_window(window: u32) -> u32 {
    STATE
        .lock()
        .unwrap()
        .as_ref()
        .map_or(0, |state| state.runtime.presentation_window(window))
}
#[no_mangle]
pub extern "C" fn frame(window: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let state = lock.as_mut().unwrap();
    if state.runtime.gdi.surface(window).is_some() {
        let _ = state.runtime.compose_children(window);
    }
    if let Some(surface) = state.runtime.gdi.surface(window) {
        if !surface.dirty {
            return 0;
        }
        state.width = surface.width;
        state.height = surface.height;
        state.frame = state
            .runtime
            .presented_frame(window)
            .unwrap_or_else(|| surface.rgba.clone());
        state.runtime.gdi.acknowledge_present(window).unwrap();
        state.frame.as_ptr() as u32
    } else {
        0
    }
}
#[no_mangle]
pub extern "C" fn frame_width() -> u32 {
    STATE.lock().unwrap().as_ref().unwrap().width
}
#[no_mangle]
pub extern "C" fn frame_height() -> u32 {
    STATE.lock().unwrap().as_ref().unwrap().height
}

/// Opt into the WebGL draw-packet sidecar while retaining CPU framebuffer
/// output for comparison and fallback.
#[no_mangle]
pub extern "C" fn set_gpu_capture(enabled: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let Some(state) = lock.as_mut() else {
        return 0;
    };
    state.runtime.set_gl_gpu_capture(enabled != 0);
    1
}

#[no_mangle]
pub extern "C" fn set_gpu_mode(enabled: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let Some(state) = lock.as_mut() else {
        return 0;
    };
    state.runtime.set_gl_gpu_mode(enabled != 0);
    1
}

/// Drain one ordered GL packet for the current window. The returned pointer
/// addresses bridge-owned memory and remains valid until the next call.
#[no_mangle]
pub extern "C" fn gpu_frame(window: u32) -> u32 {
    let mut lock = STATE.lock().unwrap();
    let Some(state) = lock.as_mut() else {
        return 0;
    };
    let window = if window == 0 {
        state.runtime.current_gl_window()
    } else {
        window
    };
    let Some((width, height, operations)) = state.runtime.take_gl_gpu_ops(window) else {
        state.gpu_packet.clear();
        return 0;
    };
    state.gpu_packet = encode_gpu_packet(width, height, operations);
    state.gpu_packet.as_ptr() as u32
}

#[no_mangle]
pub extern "C" fn gpu_window() -> u32 {
    STATE
        .lock()
        .unwrap()
        .as_ref()
        .map_or(0, |state| state.runtime.current_gl_window())
}

#[no_mangle]
pub extern "C" fn gpu_frame_length() -> u32 {
    STATE.lock().unwrap().as_ref().map_or(0, |state| {
        (state.gpu_packet.len() * std::mem::size_of::<u32>()) as u32
    })
}

fn encode_gpu_packet(width: u32, height: u32, operations: Vec<GpuOp>) -> Vec<u32> {
    let mut words = vec![0x574c_474c, 1, width, height, operations.len() as u32];
    let push_float = |words: &mut Vec<u32>, value: f32| words.push(value.to_bits());
    for operation in operations {
        match operation {
            GpuOp::Clear { mask, color, depth } => {
                words.extend([0, mask]);
                for value in color {
                    push_float(&mut words, value);
                }
                push_float(&mut words, depth);
            }
            GpuOp::Draw(draw) => {
                let state = draw.state;
                let flags = u32::from(state.depth_test)
                    | (u32::from(state.cull_face) << 1)
                    | (u32::from(state.front_ccw) << 2)
                    | (u32::from(state.two_sided_lighting) << 3)
                    | (u32::from(state.blend) << 4)
                    | (u32::from(state.dither) << 5)
                    | (u32::from(state.fog) << 6);
                words.extend([1, draw.vertices.len() as u32, draw.primitive]);
                words.extend(state.viewport.map(|value| value as u32));
                words.extend([
                    flags,
                    state.depth_func,
                    state.cull_mode,
                    state.blend_src,
                    state.blend_dst,
                    state.fog_mode,
                ]);
                for value in state.fog_color {
                    push_float(&mut words, value);
                }
                for value in [state.fog_density, state.fog_start, state.fog_end] {
                    push_float(&mut words, value);
                }
                for vertex in draw.vertices {
                    for value in vertex.clip_position {
                        push_float(&mut words, value);
                    }
                    push_float(&mut words, vertex.eye_distance);
                    for value in vertex.front_color {
                        push_float(&mut words, value);
                    }
                    for value in vertex.back_color {
                        push_float(&mut words, value);
                    }
                }
            }
        }
    }
    words
}
