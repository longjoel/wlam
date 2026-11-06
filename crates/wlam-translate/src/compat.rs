//! Compatibility classification for imported Win32 APIs.
//!
//! The compat matrix is the project's honesty mechanism: every imported
//! function is classified, and nothing is silently "assumed to work". The
//! classification here is the *plan* (what V1 intends to do); the host layer
//! and the recompiler update [`Implementation`] as reality lands.

use std::collections::{BTreeMap, BTreeSet};

/// How a given import will be satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Implementation {
    /// Reimplemented in `wlam-host` against web APIs (or pure computation).
    Native,
    /// Emulated: the behavior is reproduced, but not by a direct translation
    /// of the original semantics (e.g. `Sleep` against a virtual clock,
    /// `GetTickCount` from a monotonic clock).
    Emulated,
    /// Provided by the guest runtime itself rather than the OS (CRT startup,
    /// compiler intrinsics, `__security_check_cookie`, ...).
    Replaced,
    /// Satisfied by a user-supplied DLL recompiled into the same guest module.
    Linked,
    /// Deliberately not provided; calls will raise a diagnostic.
    Unsupported,
    /// Not yet decided. This is the state that must shrink to zero.
    Unknown,
}

impl Implementation {
    pub fn label(self) -> &'static str {
        match self {
            Implementation::Native => "native",
            Implementation::Emulated => "emulated",
            Implementation::Replaced => "replaced",
            Implementation::Linked => "linked",
            Implementation::Unsupported => "UNSUPPORTED",
            Implementation::Unknown => "unknown",
        }
    }

    pub fn is_blocking(self) -> bool {
        matches!(self, Implementation::Unsupported | Implementation::Unknown)
    }
}

/// A classification rule for one symbol in one library.
struct Rule {
    library: &'static str,
    symbol: &'static str,
    implementation: Implementation,
    /// Subsystem the API belongs to, for grouping in reports.
    area: &'static str,
}

const K: &str = "kernel32";
const U: &str = "user32";
const G: &str = "gdi32";
const M: &str = "winmm";
const C: &str = "comdlg32";
const S: &str = "shell32";
const W: &str = "shlwapi";
const CC: &str = "comctl32";
const OLE: &str = "ole32";
const ADV: &str = "advapi32";
const CRT: &str = "msvcrt";

macro_rules! rule {
    ($lib:expr, $area:expr, $impl:expr, [$($sym:literal),* $(,)?]) => {
        [$(
            Rule { library: $lib, symbol: $sym, implementation: $impl, area: $area },
        )*]
    };
}

/// Baseline classification of the imports seen in the north-star corpus.
///
/// This is a starting point, not a promise: entries marked [`Implementation`]
/// `Unsupported` are expected to be necessary for specific programs, and the
/// report must show them so the gap is visible.
fn rules() -> Vec<Rule> {
    let mut r = Vec::new();
    r.extend(rule!(
        K,
        "memory",
        Implementation::Native,
        [
            "VirtualAlloc",
            "VirtualFree",
            "VirtualProtect",
            "VirtualQuery",
            "GetTempFileNameA",
            "DeleteFileA",
            "DeleteFileW",
            "HeapCreate",
            "HeapDestroy",
            "HeapAlloc",
            "HeapFree",
            "HeapReAlloc",
            "HeapSize",
            "HeapValidate",
            "HeapSetInformation",
            "LocalAlloc",
            "LocalFree",
            "LocalReAlloc",
            "GlobalAlloc",
            "GlobalFree",
            "GlobalLock",
            "GlobalUnlock",
            "GlobalReAlloc",
            "GlobalSize",
            "GetProcessHeap",
            "GetProcessHeaps",
            "GlobalFlags",
        ]
    ));
    r.extend(rule!(
        K,
        "file",
        Implementation::Native,
        [
            "CreateFileA",
            "CreateFileW",
            "ReadFile",
            "WriteFile",
            "CloseHandle",
            "SetFilePointer",
            "SetFilePointerEx",
            "GetFileSize",
            "GetFileSizeEx",
            "SetEndOfFile",
            "FlushFileBuffers",
            "DeleteFileA",
            "DeleteFileW",
            "MoveFileA",
            "MoveFileW",
            "MoveFileExA",
            "MoveFileExW",
            "CopyFileA",
            "CopyFileW",
            "CreateDirectoryA",
            "CreateDirectoryW",
            "RemoveDirectoryA",
            "RemoveDirectoryW",
            "FindFirstFileA",
            "FindFirstFileW",
            "FindFirstFileExA",
            "FindFirstFileExW",
            "FindNextFileA",
            "FindNextFileW",
            "FindClose",
            "GetFullPathNameA",
            "GetFullPathNameW",
            "GetCurrentDirectoryA",
            "GetCurrentDirectoryW",
            "SetCurrentDirectoryA",
            "SetCurrentDirectoryW",
            "GetTempPathA",
            "GetTempPathW",
            "GetFileAttributesA",
            "GetFileAttributesW",
            "SetFileAttributesA",
            "SetFileAttributesW",
            "CreateFileMappingA",
            "CreateFileMappingW",
            "MapViewOfFile",
            "UnmapViewOfFile",
            "GetFileTime",
            "SetFileTime",
            "FileTimeToDosDateTime",
            "FileTimeToLocalFileTime",
            "GetTempFileNameA",
            "DeleteFileA",
            "DeleteFileW",
            "GetFileType",
            "GetWindowsDirectoryA",
        ]
    ));
    r.extend(rule!(
        K,
        "module",
        Implementation::Native,
        [
            "GetModuleHandleA",
            "GetModuleHandleW",
            "GetModuleHandleExA",
            "GetModuleHandleExW",
            "GetModuleFileNameA",
            "GetModuleFileNameW",
            "LoadLibraryA",
            "LoadLibraryW",
            "LoadLibraryExA",
            "LoadLibraryExW",
            "GetProcAddress",
            "FreeLibrary",
            "GetCommandLineA",
            "GetCommandLineW",
            "GetEnvironmentVariableA",
            "GetEnvironmentVariableW",
            "SetErrorMode",
            "GetSysColor",
            "ExitProcess",
            "GetExitCodeProcess",
            "FatalAppExitA",
            "FatalExit",
            "GetVersion",
            "GetVersionExA",
            "GetVersionExW",
            "SetUnhandledExceptionFilter",
            "GetLocalTime",
            "GetSystemTime",
            "GetSystemTimeAsFileTime",
            "GetSystemTimePreciseAsFileTime",
            "QueryPerformanceCounter",
            "QueryPerformanceFrequency",
            "GetTickCount",
            "GetTickCount64",
            "GetStartupInfoA",
            "GetStartupInfoW",
            "GetSystemInfo",
            "GetCurrentProcess",
            "GetCurrentProcessId",
            "GetCurrentThread",
            "GetCurrentThreadId",
            "ProcessIdToSessionId",
            "GetUserDefaultUILanguage",
            "GetACP",
            "GetOEMCP",
            "IsProcessorFeaturePresent",
            "SetLastError",
            "GetLastError",
            "RaiseException",
            "UnhandledExceptionFilter",
            "IsDebuggerPresent",
            "IsWow64Process",
            "CheckRemoteDebuggerPresent",
            "OutputDebugStringA",
            "OutputDebugStringW",
            "SetThreadAffinityMask",
            "SetThreadPriority",
            "GetThreadPriority",
            "DebugBreak",
            "TerminateProcess",
            "TerminateThread",
            "FindResourceA",
            "FindResourceW",
            "FindResourceExA",
            "FindResourceExW",
            "LoadResource",
            "LockResource",
            "SizeofResource",
            "FreeResource",
            "GetEnvironmentStrings",
            "GetEnvironmentStringsW",
            "FreeEnvironmentStringsA",
            "FreeEnvironmentStringsW",
            "GetCPInfo",
            "GetStringTypeA",
            "GetStringTypeW",
            "GetTimeZoneInformation",
            "GetFileInformationByHandle",
            "SetHandleCount",
            "lstrlenA",
            "lstrlenW",
            "lstrcmpA",
            "lstrcmpW",
            "lstrcmpiA",
            "lstrcmpiW",
            "lstrcpyA",
            "lstrcpyW",
            "lstrcatW",
            "lstrlenW",
            "lstrcpyW",
            "lstrcpynA",
            "lstrcpynW",
            "lstrcatA",
            "lstrcatW",
        ]
    ));

    // Thread-local storage: one wasm instance per thread in V1.
    r.extend(rule!(
        K,
        "tls",
        Implementation::Emulated,
        [
            "TlsAlloc",
            "TlsFree",
            "TlsGetValue",
            "TlsSetValue",
            "FlsAlloc",
            "FlsFree",
            "FlsGetValue",
            "FlsSetValue",
        ]
    ));

    // Structured exception handling. RtlUnwind is only reached by CRT SEH
    // frames; V1 reports the fault and unwinds nothing. This is a known,
    // documented gap, not an accidental one.
    r.extend(rule!(
        K,
        "seh",
        Implementation::Unsupported,
        [
            "RtlUnwind",
            "RtlUnwindEx",
            "RaiseException",
            "AddVectoredExceptionHandler",
            "RemoveVectoredExceptionHandler",
        ]
    ));
    r.extend(rule!(
        K,
        "sync",
        Implementation::Emulated,
        [
            "Sleep",
            "SleepEx",
            "SwitchToThread",
            "WaitForSingleObject",
            "WaitForSingleObjectEx",
            "WaitForMultipleObjects",
            "CreateThread",
            "ExitThread",
            "ResumeThread",
            "SuspendThread",
            "DestroyWindow",
            "CreateEventA",
            "CreateEventW",
            "SetEvent",
            "ResetEvent",
            "CreateMutexA",
            "CreateMutexW",
            "ReleaseMutex",
            "CreateSemaphoreA",
            "CreateSemaphoreW",
            "ReleaseSemaphore",
            "InitializeCriticalSection",
            "EnterCriticalSection",
            "LeaveCriticalSection",
            "DeleteCriticalSection",
            "TryEnterCriticalSection",
            "InitializeCriticalSectionAndSpinCount",
            "InterlockedIncrement",
            "InterlockedDecrement",
            "InterlockedExchange",
            "InterlockedCompareExchange",
            "InterlockedExchangeAdd",
            "SetEvent",
            "GetOverlappedResult",
        ]
    ));
    r.extend(rule!(
        K,
        "console",
        Implementation::Emulated,
        [
            "GetStdHandle",
            "WriteConsoleA",
            "WriteConsoleW",
            "WriteFile",
            "ReadConsoleA",
            "ReadConsoleW",
            "AllocConsole",
            "FreeConsole",
            "SetConsoleTitleA",
            "SetConsoleTitleW",
            "GetConsoleMode",
            "SetConsoleMode",
            "GetConsoleScreenBufferInfo",
        ]
    ));
    r.extend(rule!(
        K,
        "locale",
        Implementation::Native,
        [
            "MultiByteToWideChar",
            "WideCharToMultiByte",
            "LCMapStringA",
            "LCMapStringW",
            "CompareStringA",
            "CompareStringW",
            "GetLocaleInfoA",
            "GetLocaleInfoW",
            "GetSystemDefaultLCID",
            "GetUserDefaultLCID",
            "IsValidCodePage",
            "IsValidLocale",
        ]
    ));
    r.extend(rule!(K, "ints", Implementation::Replaced, ["MulDiv",]));
    r.extend(rule!(
        U,
        "window",
        Implementation::Native,
        [
            "EnumDisplayDevicesA",
            "EnumDisplayDevicesW",
            "EnumDisplaySettingsA",
            "EnumDisplayMonitors",
            "UnregisterClassA",
            "UnregisterClassW",
            "RegisterClassA",
            "RegisterClassW",
            "RegisterClassExA",
            "RegisterClassExW",
            "UnregisterClassA",
            "UnregisterClassW",
            "CreateWindowExA",
            "CreateWindowExW",
            "DestroyWindow",
            "DefWindowProcA",
            "DefWindowProcW",
            "ShowWindow",
            "UpdateWindow",
            "SetWindowPos",
            "GetClientRect",
            "GetWindowRect",
            "AdjustWindowRect",
            "AdjustWindowRectEx",
            "SetWindowLongA",
            "SetWindowLongW",
            "SetWindowLongPtrA",
            "SetWindowLongPtrW",
            "GetWindowLongA",
            "GetWindowLongW",
            "GetWindowLongPtrA",
            "GetWindowLongPtrW",
            "SetWindowTextA",
            "SetWindowTextW",
            "GetWindowTextA",
            "GetWindowTextW",
            "GetWindowTextLengthA",
            "GetWindowTextLengthW",
            "SetFocus",
            "GetFocus",
            "EnableWindow",
            "IsWindowEnabled",
            "IsWindowVisible",
            "SetMenu",
            "GetMenu",
            "SetCursor",
            "GetCursorPos",
            "LoadCursorA",
            "LoadCursorW",
            "LoadIconA",
            "LoadIconW",
            "LoadImageA",
            "LoadImageW",
            "SetCursorPos",
            "GetDC",
            "ReleaseDC",
            "GetWindowDC",
            "ClientToScreen",
            "ScreenToClient",
            "InvalidateRect",
            "ValidateRect",
            "BeginPaint",
            "EndPaint",
            "GetUpdateRect",
            "SetCapture",
            "ReleaseCapture",
            "GetSystemMetrics",
            "GetSystemMenu",
            "TrackPopupMenu",
            "AppendMenuA",
            "AppendMenuW",
            "CheckMenuItem",
            "EnableMenuItem",
            "CreatePopupMenu",
            "DestroyMenu",
            "GetDpiForWindow",
            "SetProcessDpiAwarenessContext",
            "SetProcessDPIAware",
            "GetCapture",
            "GetParent",
            "SetParent",
            "GetCursor",
            "CallWindowProcA",
            "CallWindowProcW",
            "GetClassLongA",
            "GetClassLongW",
            "IsZoomed",
            "MoveWindow",
            "MapWindowPoints",
            "ShowCursor",
            "GetSysColor",
            "GetSysColorBrush",
        ]
    ));
    // Rectangles are textbook pure functions; treat them as part of the host
    // surface because they live in user32 and are called from lifted code.
    r.extend(rule!(
        U,
        "rect",
        Implementation::Native,
        [
            "SetRect",
            "SetRectEmpty",
            "CopyRect",
            "EqualRect",
            "IntersectRect",
            "UnionRect",
            "SubtractRect",
            "IsRectEmpty",
            "InflateRect",
            "OffsetRect",
            "PtInRect",
            "FillRect",
            "FrameRect",
            "InvertRect",
            "GetUpdateRect",
            "ValidateRect",
        ]
    ));
    r.extend(rule!(
        U,
        "accelerator",
        Implementation::Emulated,
        [
            "LoadAcceleratorsA",
            "LoadAcceleratorsW",
            "TranslateAcceleratorA",
            "TranslateAcceleratorW",
            "MapVirtualKeyA",
            "MapVirtualKeyW",
            "MapVirtualKeyExA",
            "MapVirtualKeyExW",
            "LoadStringA",
            "LoadStringW",
        ]
    ));
    r.extend(rule!(
        U,
        "message",
        Implementation::Emulated,
        [
            "GetMessageA",
            "GetMessageW",
            "PeekMessageA",
            "PeekMessageW",
            "TranslateMessage",
            "DispatchMessageA",
            "DispatchMessageW",
            "PostMessageA",
            "PostMessageW",
            "SendMessageA",
            "SendMessageW",
            "PostQuitMessage",
            "RegisterWindowMessageA",
            "RegisterWindowMessageW",
            "SetTimer",
            "KillTimer",
            "GetKeyState",
            "GetAsyncKeyState",
            "GetKeyboardState",
            "RegisterHotKey",
            "UnregisterHotKey",
            "MessageBoxA",
            "MessageBoxW",
            "MessageBeep",
            "SetForegroundWindow",
            "SetWindowPos",
            "WaitMessage",
            "FlashWindow",
            "BringWindowToTop",
        ]
    ));
    r.extend(rule!(
        U,
        "dialog_template",
        Implementation::Emulated,
        [
            "CreateDialogParamA",
            "CreateDialogParamW",
            "DialogBoxParamA",
            "DialogBoxParamW",
            "EndDialog",
            "GetDlgItem",
            "GetDlgItemTextA",
            "GetDlgItemTextW",
            "SetDlgItemTextA",
            "SetDlgItemTextW",
            "SendDlgItemMessageA",
            "SendDlgItemMessageW",
            "IsDialogMessageA",
            "IsDialogMessageW",
            "GetDialogBaseUnits",
            "GetDlgCtrlID",
        ]
    ));
    r.extend(rule!(
        U,
        "text",
        Implementation::Native,
        [
            "DrawTextA",
            "DrawTextW",
            "DrawTextExA",
            "DrawTextExW",
            "wsprintfA",
            "wsprintfW",
            "wvsprintfA",
            "wvsprintfW",
        ]
    ));
    r.extend(rule!(
        G,
        "text",
        Implementation::Native,
        [
            "CreateFontA",
            "CreateFontW",
            "CreateFontIndirectA",
            "CreateFontIndirectW",
            "SelectObject",
            "DeleteObject",
            "DeleteDC",
            "GetStockObject",
            "GetCurrentObject",
            "SetBkMode",
            "GetBkMode",
            "SetTextColor",
            "GetTextColor",
            "SetBkColor",
            "TextOutA",
            "TextOutW",
            "GetTextExtentPoint32A",
            "GetTextExtentPoint32W",
            "GetTextMetricsA",
            "GetTextMetricsW",
            "SetTextAlign",
            "GetTextAlign",
            "GetBkColor",
            "ExtTextOutA",
            "GetTextExtentPointA",
            "GetTextFaceA",
            "GetCharWidthA",
            "SetTextCharacterExtra",
            "GetTextCharacterExtra",
            "SetMapperFlags",
            "GetMapperFlags",
        ]
    ));
    r.extend(rule!(
        G,
        "dc",
        Implementation::Native,
        ["SaveDC", "RestoreDC", "GetDeviceCaps", "GetClipBox", "IntersectClipRect"]
    ));
    r.extend(rule!(
        G,
        "geometry",
        Implementation::Native,
        [
            "GetPixel",
            "SetPixel",
            "SetPixelV",
            "GetROP2",
            "SetROP2",
            "MoveToEx",
            "GetCurrentPositionEx",
            "LineTo",
            "Rectangle",
            "ExtCreatePen",
            "GetPolyFillMode",
            "SetPolyFillMode",
        ]
    ));
    r.extend(rule!(
        G,
        "palette",
        Implementation::Native,
        [
            "CreatePalette",
            "SelectPalette",
            "RealizePalette",
            "GetPaletteEntries",
            "SetPaletteEntries",
            "GetNearestPaletteIndex",
            "GetSystemPaletteEntries",
            "UnrealizeObject",
        ]
    ));
    r.extend(rule!(
        G,
        "pixel_format",
        Implementation::Native,
        [
            "ChoosePixelFormat",
            "DescribePixelFormat",
            "SetPixelFormat",
            "GetPixelFormat",
            "SwapBuffers",
        ]
    ));
    r.extend(rule!(
        G,
        "bitmap",
        Implementation::Native,
        [
            "CreateCompatibleDC",
            "CreateDCA",
            "CreateDCW",
            "CreateCompatibleBitmap",
            "CreateDiscardableBitmap",
            "CreateBitmapIndirect",
            "CreateBitmap",
            "CreateDIBitmap",
            "CreateDIBSection",
            "GetDIBits",
            "SetDIBits",
            "GetDIBColorTable",
            "SetDIBColorTable",
            "SetDIBitsToDevice",
            "VirtualQuery",
            "GetTempFileNameA",
            "DeleteFileA",
            "DeleteFileW",
            "SetDIBits",
            "GetDIBColorTable",
            "SetDIBColorTable",
            "SetDIBitsToDevice",
            "SetDIBitsToDevice",
            "StretchDIBits",
            "BitBlt",
            "StretchBlt",
            "PatBlt",
            "FillRect",
            "FrameRect",
            "CreateSolidBrush",
            "CreateHatchBrush",
            "CreatePatternBrush",
            "CreateBrushIndirect",
            "CreateDIBPatternBrushPt",
            "CreatePen",
            "CreatePenIndirect",
            "SetStretchBltMode",
            "GetStretchBltMode",
            "GetMapMode",
            "SetMapMode",
            "GetViewportExtEx",
            "GetViewportOrgEx",
            "GetWindowExtEx",
            "GetWindowOrgEx",
            "SetViewportExtEx",
            "SetViewportOrgEx",
            "SetWindowExtEx",
            "SetWindowOrgEx",
            "OffsetViewportOrgEx",
            "OffsetWindowOrgEx",
            "SetLayout",
            "GetLayout",
            "GetMiterLimit",
            "SetMiterLimit",
            "CreateRectRgn",
            "CreateRectRgnIndirect",
            "SetRectRgn",
            "GetRgnBox",
            "GetRegionData",
            "GetClipRgn",
            "GetBoundsRect",
            "GetDeviceGammaRamp",
            "ResetDCA",
            "CreateEnhMetaFileA",
            "CreateEnhMetaFileW",
            "CreateMetaFileA",
            "CreateMetaFileW",
            "CloseEnhMetaFile",
            "CloseMetaFile",
            "DeleteEnhMetaFile",
            "DeleteMetaFile",
            "ResetDCW",
            "SetDeviceGammaRamp",
            "SetBoundsRect",
            "SelectClipRgn",
            "SetBrushOrgEx",
            "FixBrushOrgEx",
            "GetBrushOrgEx",
            "GdiFlush",
            "GetObjectA",
            "GetObjectW",
            "GetObjectType",
            "GetBitmapBits",
            "SetBitmapBits",
            "GetDIBits",
            "SetDIBits",
            "GetDIBColorTable",
            "SetDIBColorTable",
            "SetDIBitsToDevice",
            "VirtualQuery",
            "GetTempFileNameA",
            "DeleteFileA",
            "DeleteFileW",
            "GetNearestColor",
            "GetDIBColorTable",
            "SetDIBColorTable",
            "SetDIBitsToDevice",
        ]
    ));
    r.extend(rule!(
        M,
        "audio",
        Implementation::Native,
        [
            "waveOutOpen",
            "waveOutClose",
            "waveOutPrepareHeader",
            "waveOutUnprepareHeader",
            "waveOutWrite",
            "waveOutReset",
            "waveOutPause",
            "waveOutRestart",
            "waveOutGetPosition",
            "waveOutSetVolume",
            "waveOutGetVolume",
            "waveOutGetNumDevs",
            "timeGetTime",
            "timeBeginPeriod",
            "timeEndPeriod",
            "timeSetEvent",
            "timeKillEvent",
            "PlaySoundA",
            "PlaySoundW",
            "sndPlaySoundA",
            "sndPlaySoundW",
        ]
    ));
    r.extend(rule!(
        C,
        "dialog_file",
        Implementation::Emulated,
        [
            "GetOpenFileNameA",
            "GetOpenFileNameW",
            "GetSaveFileNameA",
            "GetSaveFileNameW",
            "ChooseColorA",
            "ChooseColorW",
            "CommDlgExtendedError",
        ]
    ));
    r.extend(rule!(
        S,
        "shell",
        Implementation::Emulated,
        [
            "CommandLineToArgvW",
            "DragQueryFileA",
            "DragQueryFileW",
            "DragFinish",
            "DragAcceptFiles",
            "ShellExecuteA",
            "ShellExecuteW",
            "SHGetFolderPathA",
            "SHGetFolderPathW",
            "SHGetKnownFolderPath",
            "SHBrowseForFolderA",
            "SHBrowseForFolderW",
            "SHGetPathFromIDListA",
            "SHGetPathFromIDListW",
            "ILCreateFromPathA",
            "ILCreateFromPathW",
            "ILFree",
            "SHGetSpecialFolderLocation",
        ]
    ));
    r.extend(rule!(
        W,
        "path",
        Implementation::Native,
        [
            "PathFileExistsA",
            "PathFileExistsW",
            "PathFindExtensionA",
            "PathFindExtensionW",
            "PathRemoveFileSpecA",
            "PathRemoveFileSpecW",
            "PathAppendA",
            "PathAppendW",
            "PathCombineA",
            "PathCombineW",
            "PathGetArgsA",
            "PathGetArgsW",
            "PathStripPathA",
            "PathStripPathW",
            "PathFindFileNameA",
            "PathFindFileNameW",
            "PathIsRelativeA",
            "PathIsRelativeW",
            "PathCanonicalizeA",
            "PathCanonicalizeW",
            "PathIsDirectoryA",
            "PathIsDirectoryW",
            "PathIsPrefixA",
            "PathIsPrefixW",
        ]
    ));
    r.extend(rule!(
        CC,
        "controls",
        Implementation::Native,
        ["InitCommonControls"]
    ));
    r.extend(rule!(
        CC,
        "controls",
        Implementation::Unsupported,
        [
            "InitCommonControlsEx",
            "CreateStatusWindowA",
            "CreateStatusWindowW",
            "DrawStatusTextA",
            "DrawStatusTextW",
            "CreateToolbarEx",
            "CreateUpDownControl",
            "ImageList_Create",
            "ImageList_Add",
            "ImageList_Destroy",
            "ImageList_AddMasked",
            "ImageList_ReplaceIcon",
            "SetWindowTheme",
        ]
    ));
    r.extend(rule!(
        OLE,
        "com",
        Implementation::Unsupported,
        [
            "CoInitialize",
            "CoUninitialize",
            "CoCreateInstance",
            "CoTaskMemAlloc",
            "CoTaskMemFree",
            "OleInitialize",
            "OleUninitialize",
        ]
    ));
    r.extend(rule!(
        ADV,
        "registry",
        Implementation::Unsupported,
        [
            "RegOpenKeyExA",
            "RegOpenKeyExW",
            "RegCreateKeyExA",
            "RegCreateKeyExW",
            "RegQueryValueExA",
            "RegQueryValueExW",
            "RegSetValueExA",
            "RegSetValueExW",
            "RegCloseKey",
            "RegDeleteKeyA",
            "RegDeleteKeyW",
            "RegDeleteValueA",
            "RegDeleteValueW",
            "RegEnumKeyExA",
            "RegEnumKeyExW",
            "RegEnumValueA",
            "RegEnumValueW",
        ]
    ));
    r.extend(rule!(
        CRT,
        "crt",
        Implementation::Replaced,
        [
            "malloc",
            "free",
            "calloc",
            "realloc",
            "memcpy",
            "memmove",
            "memcmp",
            "memset",
            "atoi",
            "abs",
            "strlen",
            "strcpy",
            "strcat",
            "strcmp",
            "strncmp",
            "strchr",
            "strrchr",
            "sprintf",
            "snprintf",
            "_snprintf",
            "vsnprintf",
            "printf",
            "fprintf",
            "fopen",
            "fclose",
            "fread",
            "fwrite",
            "fseek",
            "ftell",
            "exit",
            "_exit",
            "_initterm",
            "_onexit",
            "_onexitbegin",
            "_onexitend",
            "__dllonexit",
            "__getmainargs",
            "__p__acmdln",
            "__p__commode",
            "__p__fmode",
            "_adjust_fdiv",
            "_XcptFilter",
            "_except_handler3",
            "__security_check_cookie",
            "__security_init_cookie",
            "_local_unwind2",
            "__CxxFrameHandler",
            "abort",
            "atexit",
            "_controlfp",
            "_control87",
            "??2@YAPAXI@Z",
            "??3@YAXPAX@Z",
            "?set_terminate@@YAP6AXXZP6AXXZ@Z",
            "?terminate@@YAXXZ",
            "_ftol2",
            "_ftol2_sse",
            "_chkstk",
            "__chkstk",
            "_alloca_probe",
            "_allmul",
            "_alldiv",
            "_allrem",
            "_aulldiv",
            "_aullrem",
            "__set_app_type",
            "pow",
            "sqrt",
            "sin",
            "cos",
            "tan",
            "atan",
            "atan2",
            "exp",
            "log",
            "log10",
            "floor",
            "ceil",
            "fmod",
            "fabs",
            "sinf",
            "cosf",
            "tanf",
            "powf",
            "sqrtf",
            "fabsf",
            "floorf",
            "ceilf",
            "acos",
            "asin",
            "sinh",
            "cosh",
            "tanh",
            "asinh",
            "acosh",
            "atanh",
            "exp",
            "exp2",
            "expm1",
            "log1p",
            "log2",
            "cbrt",
            "hypot",
            "fmin",
            "fmax",
            "round",
            "trunc",
        ]
    ));
    r
}

/// The compat matrix for an image.
#[derive(Debug, Default)]
pub struct Matrix {
    /// `(library, symbol) -> implementation`
    pub entries: BTreeSet<(String, String, Implementation, &'static str)>,
}

impl Matrix {
    /// Libraries we know about, normalized case-insensitively.
    fn library_key(library: &str) -> Option<(&'static str, &'static str)> {
        let base = library.to_ascii_lowercase();
        let stem = base.strip_suffix(".dll").unwrap_or(&base).to_string();
        // CRT DLLs come in many versioned names (MSVCRT20, MSVCR80,
        // UCRTBASE, VCRUNTIME140, ...); treat the whole family as one.
        if stem == "msvcrt"
            || stem == "ucrtbase"
            || stem == "api-ms-win-crt-runtime-l1-1-0"
            || stem.starts_with("msvcr")
            || stem.starts_with("vcruntime")
        {
            return Some((CRT, "crt"));
        }
        Some(match stem.as_str() {
            "kernel32" | "kernelbase" => (K, ""),
            "user32" => (U, ""),
            "gdi32" => (G, ""),
            "winmm" => (M, ""),
            "comdlg32" => (C, ""),
            "shell32" => (S, ""),
            "shlwapi" => (W, ""),
            "comctl32" => (CC, ""),
            "ole32" => (OLE, ""),
            "advapi32" => (ADV, ""),
            _ => return None,
        })
    }

    /// Classify one import. `known_symbols` is the set of symbol names the
    /// image actually calls; classification is per-symbol so ordinals are
    /// reported as unknown.
    pub fn classify(library: &str, symbol: Option<&str>) -> (Implementation, &'static str) {
        let Some(sym) = symbol else {
            return (
                Implementation::Unknown,
                "ordinal import is never inferable from the name",
            );
        };
        let Some((lib, lib_area)) = Self::library_key(library) else {
            return (Implementation::Unsupported, "library outside the V1 scope");
        };
        if !lib_area.is_empty() {
            return (Implementation::Replaced, lib_area);
        }
        for rule in rules().iter().filter(|r| r.library == lib) {
            if rule.symbol == sym {
                return (rule.implementation, rule.area);
            }
        }
        (Implementation::Unknown, "not in the classification table")
    }

    /// Build the matrix for a parsed image.
    pub fn for_image(pe: &wlam_pe::Pe) -> Matrix {
        let mut m = Matrix::default();
        for imp in &pe.imports {
            let (implementation, area) = Self::classify(&imp.library, imp.name.as_deref());
            m.entries
                .insert((imp.library.clone(), imp.symbol(), implementation, area));
        }
        m
    }

    /// Counts per implementation state.
    pub fn summary(&self) -> Vec<(Implementation, usize)> {
        let mut out: Vec<(Implementation, usize)> = Implementation::all()
            .iter()
            .map(|s| (*s, self.entries.iter().filter(|e| e.2 == *s).count()))
            .collect();
        out.retain(|(_, n)| *n > 0);
        out
    }

    /// Entries that block progress (unsupported or unclassified).
    pub fn blockers(
        &self,
    ) -> impl Iterator<Item = &(String, String, Implementation, &'static str)> {
        self.entries.iter().filter(|e| e.2.is_blocking())
    }

    /// Render as markdown for `docs/compat-matrix.md`.
    /// Render as a markdown table, grouped by subsystem. `notes` lets the
    /// caller attach provenance (e.g. an ordinal resolved to a name) to a
    /// specific `(library, symbol)` pair.
    pub fn to_markdown_with_notes(
        &self,
        title: &str,
        notes: &BTreeMap<(String, String), String>,
    ) -> String {
        let mut out = String::new();
        out.push_str(&format!("# Compatibility matrix: {title}\n\n"));
        out.push_str(
            "Generated by `wlam analyze`. Every imported symbol is listed. `unknown` entries \
             are not yet classified and block progress; `UNSUPPORTED` entries are deliberate \
             gaps with their reason recorded, not oversights.\n\n",
        );
        out.push_str("| State | Count |\n|---|---|\n");
        for (state, n) in self.summary() {
            out.push_str(&format!("| {} | {} |\n", state.label(), n));
        }
        // Group by (library, area) so related APIs sit together.
        let mut groups: BTreeMap<
            (String, &'static str),
            Vec<&(String, String, Implementation, &'static str)>,
        > = BTreeMap::new();
        for e in &self.entries {
            groups.entry((e.0.clone(), e.3)).or_default().push(e);
        }
        for ((lib, area), entries) in groups {
            out.push_str(&format!("\n## {lib} — {area}\n\n"));
            out.push_str("| Symbol | State | Note |\n|---|---|---|\n");
            for (_, sym, imp, _) in entries {
                let note = notes
                    .get(&(lib.clone(), sym.clone()))
                    .map(String::as_str)
                    .unwrap_or("");
                out.push_str(&format!("| `{}` | {} | {} |\n", sym, imp.label(), note));
            }
        }
        out
    }

    /// Render without provenance notes.
    pub fn to_markdown(&self, title: &str) -> String {
        self.to_markdown_with_notes(title, &BTreeMap::new())
    }
}

impl Implementation {
    pub fn all() -> [Implementation; 6] {
        [
            Implementation::Native,
            Implementation::Emulated,
            Implementation::Replaced,
            Implementation::Linked,
            Implementation::Unsupported,
            Implementation::Unknown,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_symbols() {
        assert_eq!(
            Implementation::Native,
            Matrix::classify("KERNEL32.dll", Some("VirtualAlloc")).0
        );
        assert_eq!(
            Implementation::Emulated,
            Matrix::classify("USER32.dll", Some("PeekMessageA")).0
        );
        assert_eq!(
            Implementation::Native,
            Matrix::classify("WINMM.DLL", Some("waveOutWrite")).0
        );
        assert_eq!(
            Implementation::Replaced,
            Matrix::classify("MSVCRT.dll", Some("malloc")).0
        );
        assert_eq!(
            Implementation::Replaced,
            Matrix::classify("api-ms-win-crt-runtime-l1-1-0.dll", Some("exit")).0
        );
        for symbol in [
            "FileTimeToDosDateTime",
            "FileTimeToLocalFileTime",
            "GetWindowsDirectoryA",
        ] {
            assert_eq!(
                Implementation::Native,
                Matrix::classify("KERNEL32.dll", Some(symbol)).0,
                "{symbol}"
            );
        }
        // Versioned CRT DLL names (MSVCRT20, MSVCR80, UCRTBASE, ...) and their
        // C entry points are the same host-provided family.
        for symbol in [
            "exit",
            "_initterm",
            "__getmainargs",
            "__p__acmdln",
            "??2@YAPAXI@Z",
        ] {
            assert_eq!(
                Implementation::Replaced,
                Matrix::classify("MSVCRT20.dll", Some(symbol)).0,
                "{symbol}"
            );
        }
    }

    #[test]
    fn unknown_symbols_are_explicit_not_silently_ok() {
        let (imp, _) = Matrix::classify("KERNEL32.dll", Some("DefinitelyNotAnApi"));
        assert_eq!(Implementation::Unknown, imp);
        assert!(imp.is_blocking());
        // Ordinal imports can never be classified by name.
        let (imp, _) = Matrix::classify("KERNEL32.dll", None);
        assert_eq!(Implementation::Unknown, imp);
        // Unknown libraries are unsupported, with a reason.
        let (imp, area) = Matrix::classify("something.dll", Some("Foo"));
        assert_eq!(Implementation::Unsupported, imp);
        assert!(!area.is_empty());
    }

    #[test]
    fn library_names_are_case_and_suffix_insensitive() {
        let a = Matrix::classify("KERNEL32.dll", Some("Sleep"));
        let b = Matrix::classify("kernel32", Some("Sleep"));
        assert_eq!(a, b);
    }

    #[test]
    fn matrix_marks_unclassified_as_blocking() {
        let mut m = Matrix::default();
        m.entries.insert((
            "k.dll".into(),
            "k.dll!A".into(),
            Implementation::Unknown,
            "x",
        ));
        m.entries.insert((
            "k.dll".into(),
            "k.dll!B".into(),
            Implementation::Native,
            "x",
        ));
        assert_eq!(1, m.blockers().count());
        assert!(m.to_markdown("t").contains("unknown"));
    }
}
