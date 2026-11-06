//! Calling conventions shared by emitters and runtime adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Convention {
    Stdcall,
    Cdecl,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature {
    pub convention: Convention,
    pub argument_bytes: u32,
}
impl Signature {
    /// Bytes removed after a host import, including the pushed return address.
    pub const fn return_pop(self) -> u32 {
        4 + match self.convention {
            Convention::Stdcall => self.argument_bytes,
            Convention::Cdecl => 0,
        }
    }
}
/// The CRT ships under many versioned names. They share calling conventions.
fn is_crt(stem: &str) -> bool {
    stem == "msvcrt"
        || stem == "ucrtbase"
        || stem == "api-ms-win-crt-runtime-l1-1-0"
        || stem.starts_with("msvcr")
        || stem.starts_with("vcruntime")
}

/// Only explicitly specified signatures are returned. Never infer stack cleanup.
pub fn signature(dll: &str, symbol: &str) -> Option<Signature> {
    let dll = dll.to_ascii_lowercase();
    let stem = dll.strip_suffix(".dll").unwrap_or(&dll);
    // Wine's test binaries import every Win32/CRT symbol from one synthetic
    // test module. Resolve their signatures by name so a single import library
    // can drive the conformance harness without duplicating per-DLL .def files.
    if stem == "wlamtest" {
        for candidate in [
            "kernel32",
            "user32",
            "gdi32",
            "advapi32",
            "winmm",
            "msvcrt",
            "comctl32",
            "shell32",
        ] {
            if let Some(signature) = signature(candidate, symbol) {
                return Some(signature);
            }
        }
        return None;
    }
    let library = if is_crt(stem) { "msvcrt" } else { stem };
    let count = match (library, symbol) {
        ("kernel32", "SetEvent" | "ResetEvent") => 1,
        ("user32", "GetKeyState") => 1,
        ("user32", "CreateDialogParamA") => 5,
        ("user32", "DialogBoxParamA" | "DialogBoxIndirectParamA") => 5,
        ("user32", "EndDialog") => 2,
        ("user32", "SetDlgItemTextA") => 3,
        ("user32", "GetDlgItem") => 2,
        ("user32", "KillTimer") => 2,
        ("user32", "CallWindowProcA") => 5,
        ("user32", "SetWindowsHookExA" | "CallNextHookEx") => 4,
        ("user32", "UnhookWindowsHookEx") => 1,
        ("user32", "SetMessageQueue") => 1,
        ("comctl32", "#17" | "InitCommonControls") => 0,
        (
            "kernel32",
            "GetSystemInfo"
            | "GetLocalTime"
            | "GetTimeZoneInformation"
            | "GlobalMemoryStatus"
            | "OutputDebugStringA"
            | "SetErrorMode"
            | "GlobalFree"
            | "GlobalLock"
            | "GlobalUnlock"
            | "GlobalHandle"
            | "GlobalFlags"
            | "LocalFree"
            | "FreeResource"
            | "LockResource"
            | "GlobalAddAtomA"
            | "GlobalDeleteAtom"
            | "lstrlenA"
            | "lstrlenW"
            | "IsBadCodePtr",
        ) => 1,
        (
            "kernel32",
            "GlobalAlloc"
            | "LocalAlloc"
            | "LoadResource"
            | "SizeofResource"
            | "SetThreadPriority"
            | "WaitForSingleObject"
            | "SetEnvironmentVariableA"
            | "lstrcatA"
            | "lstrcpyA"
            | "lstrcpyW"
            | "lstrcatW"
            | "lstrcmpA"
            | "lstrcmpiA"
            | "GetCurrentDirectoryA"
            | "IsWow64Process"
            | "FileTimeToLocalFileTime"
            | "GetWindowsDirectoryA",
        ) => 2,
        (
            "kernel32",
            "GlobalReAlloc"
            | "GlobalGetAtomNameA"
            | "FindResourceA"
            | "FileTimeToDosDateTime"
            | "MulDiv"
            | "OpenFile",
        ) => 3,
        (
            "kernel32",
            "GetFullPathNameA"
            | "GetPrivateProfileIntA"
            | "WritePrivateProfileStringA"
            | "CreateEventA"
            | "RaiseException",
        ) => 4,
        ("kernel32", "SetFilePointer") => 4,
        ("kernel32", "CreateFileA") => 7,
        ("kernel32", "CreateThread" | "GetPrivateProfileStringA") => 6,
        ("gdi32", "CreateDIBitmap") => 6,
        ("advapi32", "RegOpenKeyA") => 3,
        ("advapi32", "RegOpenKeyExA") => 5,
        ("advapi32", "RegCloseKey") => 1,
        ("advapi32", "RegCreateKeyExA") => 9,
        ("advapi32", "RegQueryValueExA") => 6,
        ("advapi32", "RegSetValueExA") => 6,
        ("advapi32", "RegDeleteKeyA" | "RegDeleteValueA") => 2,
        ("advapi32", "RegQueryValueA") => 4,
        ("user32", "GetPropA" | "RemovePropA") => 2,
        ("user32", "SetPropA") => 3,
        ("wlam.x87", "sin" | "cos" | "tan" | "atan2") => 2,
        ("winmm", "joyGetDevCapsA") => 3,
        ("winmm", "joyGetPos") => 2,
        ("winmm", "timeKillEvent") => 1,
        ("winmm", "timeSetEvent") => 5,
        ("kernel32", "DuplicateHandle" | "FormatMessageA") => 7,
        ("kernel32", "WideCharToMultiByte" | "GetVolumeInformationA") => 8,
        (
            "user32",
            "RegisterClassA"
            | "RegisterWindowMessageA"
            | "GetSystemMetrics"
            | "GetSysColor"
            | "SetCursor"
            | "ShowCursor"
            | "IsWindow"
            | "IsWindowVisible"
            | "IsWindowEnabled"
            | "GetDC"
            | "GetWindowDC"
            | "GetParent"
            | "GetDlgCtrlID"
            | "GetTopWindow"
            | "BringWindowToTop"
            | "GetMenu"
            | "GetMenuItemCount"
            | "GetWindowTextLengthA"
            | "SetFocus"
            | "SetCapture"
            | "SetActiveWindow"
            | "SetForegroundWindow"
            | "LockWindowUpdate"
            | "MessageBeep"
            | "BeginDeferWindowPos"
            | "SetRectEmpty",
        ) => 1,
        (
            "user32",
            "GetDesktopWindow"
            | "GetForegroundWindow"
            | "GetFocus"
            | "GetCapture"
            | "GetMessagePos"
            | "GetMessageTime"
            | "GetMenuCheckMarkDimensions"
            | "CreatePopupMenu"
            | "ReleaseCapture",
        ) => 0,
        (
            "user32",
            "LoadCursorA" | "LoadIconA" | "LoadMenuA" | "LoadBitmapA" | "LoadAcceleratorsA"
            | "GetWindowLongA" | "GetWindowRect" | "SetWindowTextA" | "GetWindow" | "SetMenu"
            | "GetSubMenu" | "GetMenuItemID" | "ReleaseDC" | "SetParent" | "ClientToScreen"
            | "ScreenToClient" | "CopyRect" | "EqualRect" | "EnableWindow" | "ValidateRect",
        ) => 2,
        (
            "user32",
            "PtInRect"
            | "GetWindowTextA"
            | "GetClassNameA"
            | "GetClassInfoA"
            | "SetWindowLongA"
            | "FillRect"
            | "OffsetRect"
            | "InflateRect"
            | "IntersectRect"
            | "UnionRect"
            | "TranslateAcceleratorA",
        ) => 3,
        ("user32", "AdjustWindowRect") => 3,
        ("user32", "GetScrollPos") => 2,
        ("user32", "SetScrollPos" | "GetScrollRange") => 4,
        ("user32", "SetScrollRange") => 5,
        ("gdi32", "ChoosePixelFormat") => 2,
        ("gdi32", "DescribePixelFormat") => 4,
        ("gdi32", "SetPixelFormat") => 3,
        ("gdi32", "GetPixelFormat" | "SwapBuffers") => 1,
        ("opengl32", "wglCreateContext" | "wglDeleteContext") => 1,
        ("opengl32", "wglMakeCurrent") => 2,
        (
            "opengl32",
            "glBegin" | "glCallList" | "glClear" | "glClearIndex" | "glCullFace" | "glDisable"
            | "glEnable" | "glFogi" | "glFogf" | "glGenLists" | "glIndexi" | "glMatrixMode"
            | "glShadeModel" | "glDepthFunc" | "glDrawBuffer",
        ) => 1,
        ("opengl32", "glGetError") => 0,
        ("opengl32", "glBlendFunc" | "glColorMaterial" | "glFogfv" | "glNewList") => 2,
        ("opengl32", "glLightModelfv") => 2,
        ("opengl32", "glLightfv" | "glMaterialfv") => 3,
        ("opengl32", "glViewport" | "glClearColor") => 4,
        ("opengl32", "glNormal3f" | "glVertex3f" | "glTranslatef") => 3,
        ("opengl32", "glRotatef") => 4,
        ("opengl32", "glColor3ubv") => 1,
        ("opengl32", "glClearDepth") => 2,
        ("opengl32", "glOrtho") => 12,
        (
            "opengl32",
            "glEnd" | "glEndList" | "glFlush" | "glLoadIdentity" | "glPopMatrix" | "glPushMatrix",
        ) => 0,
        ("glu32", "gluPerspective") => 8,
        ("glu32", "gluLookAt") => 18,
        (
            "user32",
            "LoadStringA" | "MessageBoxA" | "AdjustWindowRectEx" | "RedrawWindow" | "SetTimer"
            | "AppendMenuA",
        ) => 4,
        ("user32", "SetRect") => 5,
        ("user32", "wsprintfA") => 2,
        ("gdi32", "CreateDCA" | "CreateDCW") => 4,
        ("user32", "EnumDisplayDevicesA" | "EnumDisplayDevicesW") => 4,
        ("user32", "EnumDisplaySettingsA") => 3,
        ("user32", "EnumDisplayMonitors") => 4,
        ("user32", "UnregisterClassA" | "UnregisterClassW") => 2,
        ("user32", "MoveWindow") => 6,
        ("user32", "SetWindowPos") => 7,
        ("user32", "DeferWindowPos") => 8,
        ("user32", "EndDeferWindowPos") => 1,
        (
            "gdi32",
            "GetStockObject"
            | "CreateSolidBrush"
            | "CreatePalette"
            | "CreatePatternBrush"
            | "CreateFontIndirectA"
            | "CreateRectRgnIndirect"
            | "SaveDC"
            | "RealizePalette"
            | "GdiSetBatchLimit",
        ) => 1,
        (
            "gdi32",
            "GetDeviceCaps"
            | "GetTextMetricsA"
            | "GetClipBox"
            | "SetBkColor"
            | "SetBkMode"
            | "SetTextColor"
            | "SetTextAlign"
            | "SetMapMode"
            | "RestoreDC"
            | "GetNearestPaletteIndex"
            | "RectVisible"
            | "SelectClipRgn"
            | "GetClipRgn"
            | "GetRgnBox"
            | "GetViewportExtEx"
            | "GetViewportOrgEx"
            | "GetWindowExtEx"
            | "GetWindowOrgEx"
            | "SetLayout",
        ) => 2,
        (
            "gdi32",
            "CreatePen" | "GetObjectA" | "SelectPalette" | "LineTo" | "GetPixel" | "PtVisible"
            | "CreateDiscardableBitmap",
        ) => 3,
        (
            "gdi32",
            "CreateRectRgn"
            | "CombineRgn"
            | "GetPaletteEntries"
            | "SetPaletteEntries"
            | "GetSystemPaletteEntries"
            | "MoveToEx"
            | "SetViewportExtEx"
            | "SetViewportOrgEx"
            | "SetWindowExtEx"
            | "SetWindowOrgEx"
            | "OffsetViewportOrgEx"
            | "OffsetWindowOrgEx"
            | "GetTextExtentPointA"
            | "GetTextExtentPoint32A",
        ) => 4,
        ("user32", "CheckDlgButton") => 3,
        ("user32", "IsDlgButtonChecked") => 2,
        ("user32", "CheckRadioButton") => 4,
        ("user32", "CharUpperA" | "CharLowerA" | "IsIconic" | "IsZoomed") => 1,
        ("user32", "CharUpperBuffA" | "CharLowerBuffA") => 2,
        ("gdi32", "SetPixel" | "SetPixelV") => 4,
        ("gdi32", "SetRectRgn") => 5,
        ("gdi32", "CreateBitmapIndirect") => 1,
        ("gdi32", "CreateEnhMetaFileA" | "CreateEnhMetaFileW") => 4,
        (
            "gdi32",
            "CreateMetaFileA" | "CreateMetaFileW" | "CloseEnhMetaFile" | "CloseMetaFile"
            | "DeleteEnhMetaFile" | "DeleteMetaFile",
        ) => 1,
        ("gdi32", "GetCurrentPositionEx") => 2,
        ("gdi32", "StretchBlt") => 11,
        ("gdi32", "GetDIBits" | "SetDIBits") => 7,
        ("gdi32", "GetDIBColorTable" | "SetDIBColorTable") => 4,
        ("gdi32", "SetDIBitsToDevice") => 12,
        ("kernel32", "VirtualQuery") => 3,
        ("kernel32", "GetTempFileNameA") => 4,
        ("kernel32", "DeleteFileA" | "DeleteFileW") => 1,
        ("gdi32", "StretchDIBits") => 13,
        ("gdi32", "SetBrushOrgEx" | "FixBrushOrgEx") => 4,
        ("gdi32", "GetBrushOrgEx" | "GetMiterLimit") => 2,
        ("gdi32", "SetMiterLimit") => 3,
        ("gdi32", "ExtTextOutA") => 8,
        ("gdi32", "IntersectClipRect") => 5,
        (
            "gdi32",
            "GetTextColor" | "GetBkColor" | "GetBkMode" | "GetTextAlign" | "GetStretchBltMode"
            | "GetROP2" | "GetMapMode",
        ) => 1,
        ("gdi32", "SetStretchBltMode" | "SetROP2" | "CreateHatchBrush") => 2,
        ("gdi32", "CreateBitmap" | "Rectangle" | "TextOutA" | "ExtCreatePen") => 5,
        ("gdi32", "PatBlt") => 6,
        ("gdi32", "GdiFlush") => 0,
        ("winmm", "timeGetTime" | "waveOutGetNumDevs" | "midiOutGetNumDevs") => 0,
        ("kernel32", "GetCurrentThread" | "GetCurrentProcess") => 0,
        ("user32", "GetCursorPos") => 1,
        ("kernel32", "SetUnhandledExceptionFilter") => 1,
        ("kernel32", "MultiByteToWideChar") => 6,
        ("kernel32", "LoadLibraryA" | "FreeLibrary") => 1,
        ("kernel32", "GetProcAddress") => 2,
        ("kernel32", "VirtualAlloc") => 4,
        ("kernel32", "VirtualFree") => 3,
        ("kernel32", "TlsAlloc" | "GetCurrentThreadId") => 0,
        ("kernel32", "TlsFree" | "TlsGetValue") => 1,
        ("kernel32", "TlsSetValue") => 2,
        ("kernel32", "GetStdHandle" | "GetFileType" | "CloseHandle") => 1,
        ("kernel32", "SetStdHandle" | "GetCPInfo") => 2,
        ("kernel32", "ReadFile" | "WriteFile") => 5,
        (
            "kernel32",
            "InitializeCriticalSection"
            | "EnterCriticalSection"
            | "TryEnterCriticalSection"
            | "LeaveCriticalSection"
            | "DeleteCriticalSection",
        ) => 1,
        (
            "kernel32",
            "GetTickCount"
            | "GetVersion"
            | "GetProcessHeap"
            | "GetLastError"
            | "GetCommandLineA"
            | "GetEnvironmentStrings"
            | "GetACP"
            | "GetOEMCP",
        ) => 0,
        (
            "kernel32",
            "GetVersionExA" | "GetStartupInfoA" | "SetHandleCount" | "HeapDestroy"
            | "GetModuleHandleA" | "SetLastError" | "ExitProcess",
        ) => 1,
        ("kernel32", "GetModuleFileNameA") => 3,
        ("kernel32", "HeapCreate" | "HeapAlloc" | "HeapFree" | "HeapSize") => 3,
        ("kernel32", "HeapReAlloc") => 4,
        (
            "user32",
            "RegisterClassExA" | "TranslateMessage" | "DispatchMessageA" | "DestroyWindow"
            | "UpdateWindow" | "PostQuitMessage" | "GetLastActivePopup",
        ) => 1,
        ("user32", "ShowWindow" | "GetClientRect" | "BeginPaint" | "EndPaint") => 2,
        ("user32", "InvalidateRect") => 3,
        ("user32", "GetMessageA" | "PostMessageA" | "SendMessageA" | "DefWindowProcA") => 4,
        ("user32", "PeekMessageA") => 5,
        ("user32", "CreateWindowExA") => 12,
        ("gdi32", "CreateCompatibleDC" | "DeleteDC" | "DeleteObject" | "UnrealizeObject" | "CreatePenIndirect" | "CreateBrushIndirect") => 1,
        ("gdi32", "SelectObject" | "GetCurrentObject") => 2,
        ("gdi32", "CreateDIBSection") => 6,
        ("gdi32", "BitBlt") => 9,
        (
            "gdi32",
            "GetObjectType" | "GetTextCharacterExtra" | "GetPolyFillMode" | "GetMapperFlags"
            | "GetLayout",
        ) => 1,
        (
            "gdi32",
            "GetNearestColor"
            | "SetTextCharacterExtra"
            | "SetPolyFillMode"
            | "SetMapperFlags"
            | "CreateDIBPatternBrushPt",
        ) => 2,
        (
            "gdi32",
            "GetObjectW" | "GetTextFaceA" | "CreateCompatibleBitmap" | "GetBitmapBits"
            | "SetBitmapBits" | "FillRect" | "GetRegionData" | "GetBoundsRect"
            | "SetBoundsRect",
        ) => 3,
        ("gdi32", "GetDeviceGammaRamp" | "SetDeviceGammaRamp" | "ResetDCA" | "ResetDCW") => 2,
        ("gdi32", "GetCharWidthA") => 4,
        // MSVCRT's C ABI is cdecl. The few compiler intrinsics with other
        // conventions are not listed here and therefore stay explicit errors.
        (
            "msvcrt",
            "_adjust_fdiv" | "__p__acmdln" | "__p__commode" | "__p__fmode" | "?terminate@@YAXXZ"
            | "abort",
        ) => 0,
        (
            "msvcrt",
            "??2@YAPAXI@Z"
            | "??3@YAXPAX@Z"
            | "?set_terminate@@YAP6AXXZP6AXXZ@Z"
            | "malloc"
            | "free"
            | "strlen"
            | "atoi"
            | "abs"
            | "_exit"
            | "exit"
            | "_onexit"
            | "atexit"
            | "__set_app_type",
        ) => 1,
        (
            "msvcrt",
            "calloc" | "realloc" | "strcpy" | "strcmp" | "strchr" | "strrchr" | "_controlfp"
            | "_control87" | "_XcptFilter" | "_initterm",
        ) => 2,
        ("msvcrt", "memset" | "memcpy" | "memmove" | "memcmp" | "strncmp") => 3,
        ("msvcrt", "__dllonexit") => 3,
        ("msvcrt", "__CxxFrameHandler" | "_except_handler3") => 4,
        ("msvcrt", "__getmainargs") => 5,
        _ => return None,
    };
    Some(Signature {
        convention: if (library == "user32" && symbol == "wsprintfA") || library == "msvcrt" {
            Convention::Cdecl
        } else {
            Convention::Stdcall
        },
        argument_bytes: count * 4,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conventions_cleanup_return_address_and_arguments() {
        assert_eq!(
            signature("USER32.dll", "CreateWindowExA")
                .unwrap()
                .return_pop(),
            52
        );
        assert_eq!(signature("kernel32", "GetVersion").unwrap().return_pop(), 4);
        assert_eq!(
            signature("user32", "SetRectEmpty").unwrap().argument_bytes,
            4
        );
        assert_eq!(signature("user32", "SetRectEmpty").unwrap().return_pop(), 8);
        assert_eq!(
            Signature {
                convention: Convention::Cdecl,
                argument_bytes: 16
            }
            .return_pop(),
            4
        );
        assert_eq!(
            signature("kernel32", "FileTimeToDosDateTime")
                .unwrap()
                .return_pop(),
            16
        );
        for symbol in ["FileTimeToLocalFileTime", "GetWindowsDirectoryA"] {
            let signature = signature("kernel32", symbol).unwrap();
            assert_eq!(signature.argument_bytes, 8, "{symbol}");
            assert_eq!(signature.return_pop(), 12, "{symbol}");
        }
        // Versioned CRT DLLs share the msvcrt signatures, and the C ABI is
        // cdecl: only the return address is popped by the callee.
        let getmainargs = signature("MSVCRT20.dll", "__getmainargs").unwrap();
        assert_eq!(getmainargs.argument_bytes, 20);
        assert_eq!(getmainargs.convention, Convention::Cdecl);
        assert_eq!(getmainargs.return_pop(), 4);
        assert_eq!(signature("msvcrt", "_initterm").unwrap().argument_bytes, 8);
        assert_eq!(signature("ucrtbase.dll", "exit").unwrap().argument_bytes, 4);
        assert!(signature("unknown", "CreateWindowExA").is_none());
        assert!(signature("user32", "Unknown").is_none());
    }
}
