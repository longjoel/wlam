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
/// Only explicitly specified signatures are returned. Never infer stack cleanup.
pub fn signature(dll: &str, symbol: &str) -> Option<Signature> {
    let dll = dll.to_ascii_lowercase();
    let dll = dll.strip_suffix(".dll").unwrap_or(&dll);
    let count = match (dll, symbol) {
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
            | "LocalFree"
            | "FreeResource"
            | "LockResource"
            | "GlobalAddAtomA"
            | "GlobalDeleteAtom"
            | "lstrlenA"
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
            | "lstrcmpA"
            | "lstrcmpiA"
            | "GetCurrentDirectoryA",
        ) => 2,
        (
            "kernel32",
            "GlobalReAlloc" | "GlobalGetAtomNameA" | "FindResourceA" | "MulDiv" | "OpenFile",
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
        ("advapi32", "RegOpenKeyA") => 3,
        ("advapi32", "RegOpenKeyExA") => 5,
        ("advapi32", "RegCloseKey") => 1,
        ("wlam.x87", "sin" | "cos" | "tan" | "atan2") => 2,
        ("winmm", "joyGetDevCapsA") => 3,
        ("winmm", "joyGetPos") => 2,
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
            | "ScreenToClient" | "CopyRect" | "EqualRect" | "EnableWindow"
            | "ValidateRect",
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
        ("gdi32", "ChoosePixelFormat") => 2,
        ("gdi32", "DescribePixelFormat") => 4,
        ("gdi32", "SetPixelFormat") => 3,
        ("gdi32", "GetPixelFormat" | "SwapBuffers") => 1,
        ("opengl32", "wglCreateContext" | "wglDeleteContext") => 1,
        ("opengl32", "wglMakeCurrent") => 2,
        ("opengl32", "glBegin" | "glCallList" | "glClear" | "glClearIndex" | "glCullFace" | "glDisable" | "glEnable" | "glFogi" | "glFogf" | "glGenLists" | "glIndexi" | "glMatrixMode" | "glShadeModel" | "glDepthFunc" | "glDrawBuffer") => 1,
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
        ("opengl32", "glEnd" | "glEndList" | "glFlush" | "glLoadIdentity" | "glPopMatrix" | "glPushMatrix") => 0,
        ("glu32", "gluPerspective") => 8,
        ("glu32", "gluLookAt") => 18,
        (
            "user32",
            "LoadStringA" | "MessageBoxA" | "AdjustWindowRectEx" | "RedrawWindow" | "SetTimer"
            | "AppendMenuA",
        ) => 4,
        ("user32", "SetRect") => 5,
        ("user32", "wsprintfA") => 2,
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
            | "SelectClipRgn",
        ) => 2,
        (
            "gdi32",
            "CreatePen" | "GetObjectA" | "SelectPalette" | "LineTo" | "GetPixel" | "PtVisible",
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
            | "OffsetViewportOrgEx"
            | "GetTextExtentPointA",
        ) => 4,
        ("gdi32", "CreateBitmap" | "Rectangle" | "TextOutA") => 5,
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
            | "UpdateWindow" | "PostQuitMessage" | "BringWindowToTop" | "GetLastActivePopup",
        ) => 1,
        ("user32", "ShowWindow" | "GetClientRect" | "BeginPaint" | "EndPaint") => 2,
        ("user32", "InvalidateRect") => 3,
        ("user32", "GetMessageA" | "PostMessageA" | "SendMessageA" | "DefWindowProcA") => 4,
        ("user32", "PeekMessageA") => 5,
        ("user32", "CreateWindowExA") => 12,
        ("gdi32", "CreateCompatibleDC" | "DeleteDC" | "DeleteObject") => 1,
        ("gdi32", "SelectObject") => 2,
        ("gdi32", "CreateDIBSection") => 6,
        ("gdi32", "BitBlt") => 9,
        _ => return None,
    };
    Some(Signature {
        convention: if dll == "user32" && symbol == "wsprintfA" { Convention::Cdecl } else { Convention::Stdcall },
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
        assert_eq!(signature("user32", "SetRectEmpty").unwrap().argument_bytes, 4);
        assert_eq!(signature("user32", "SetRectEmpty").unwrap().return_pop(), 8);
        assert_eq!(
            Signature {
                convention: Convention::Cdecl,
                argument_bytes: 16
            }
            .return_pop(),
            4
        );
        assert!(signature("unknown", "CreateWindowExA").is_none());
        assert!(signature("user32", "Unknown").is_none());
    }
}
