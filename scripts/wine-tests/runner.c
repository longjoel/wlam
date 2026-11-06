/* Tiny console driver for one Wine conformance test body. */
#include <stdarg.h>
typedef unsigned int DWORD;
typedef void *HANDLE;
typedef int BOOL;

__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);

#ifndef TEST_FUNC
#error TEST_FUNC must name the Wine test entry point
#endif
#ifndef TEST_NAME
#define TEST_NAME "test"
#endif
#define WLAM_CAT_(a, b) a##b
#define WLAM_CAT(a, b) WLAM_CAT_(a, b)
void WLAM_CAT(func_, TEST_FUNC)(void);

/* MSVC-target clang's marker symbol for floating-point use. */
int _fltused = 0;

/* Minimal stand-ins for Wine's debug formatting helpers. Failure messages
 * still identify the check by file and line; these only replace the formatted
 * detail with a stable placeholder. */
typedef unsigned short WCHAR;
typedef struct { int left, top, right, bottom; } RECT;
const char *wine_dbgstr_rect(const RECT *rect) { (void)rect; return "(rect)"; }
const char *wine_dbgstr_w(const WCHAR *str) { (void)str; return "(wstr)"; }
const char *wine_dbgstr_wn(const WCHAR *str, int len) { (void)str; (void)len; return "(wstr)"; }

static HANDLE output;
static DWORD passes, failures, skips;
static const char *current_file;
static int current_line;

static void write_bytes(const char *text, DWORD length) {
    DWORD written;
    if (output) WriteFile(output, text, length, &written, 0);
}

static void write_str(const char *text) {
    DWORD length = 0;
    while (text[length]) length++;
    write_bytes(text, length);
}

static void write_u32(DWORD value) {
    char buffer[11];
    DWORD at = 11;
    do { buffer[--at] = (char)('0' + value % 10); value /= 10; } while (value);
    write_bytes(buffer + at, 11 - at);
}

static void write_number(unsigned long value, unsigned base, int width, int zero, int upper, int negative) {
    char buffer[24];
    int at = 24;
    const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    do { buffer[--at] = digits[value % base]; value /= base; } while (value);
    if (negative) buffer[--at] = '-';
    while (24 - at < width) buffer[--at] = zero ? '0' : ' ';
    write_bytes(buffer + at, 24 - at);
}

static void write_format(const char *format, va_list args) {
    while (format && *format) {
        if (*format != '%') { write_bytes(format, 1); format++; continue; }
        format++;
        int zero = 0;
        if (*format == '0') { zero = 1; format++; }
        int width = 0;
        while (*format >= '0' && *format <= '9') { width = width * 10 + (*format - '0'); format++; }
        int is_long = 0;
        if (*format == 'l') { is_long = 1; format++; }
        if (*format == 'I') {
            format++;
            if (*format == '6' || *format == '3') format++;
        }
        switch (*format) {
        case 'x':
        case 'X':
            write_number((unsigned long)va_arg(args, unsigned long), 16, width, zero, *format == 'X', 0);
            break;
        case 'u':
            write_number((unsigned long)va_arg(args, unsigned long), 10, width, zero, 0, 0);
            break;
        case 'd':
            {
                long value = (long)va_arg(args, long);
                write_number((unsigned long)(value < 0 ? -value : value), 10, width, zero, 0, value < 0);
            }
            break;
        case 'p':
            write_number((unsigned long)va_arg(args, void *), 16, 8, 1, 0, 0);
            break;
        case 's':
            write_str(va_arg(args, const char *));
            break;
        case 'c':
            {
                char c = (char)va_arg(args, int);
                write_bytes(&c, 1);
            }
            break;
        case '%':
            write_bytes("%", 1);
            break;
        default:
            write_bytes(format, 1);
            break;
        }
        (void)is_long;
        if (*format) format++;
    }
}

void wlam_test_ok(int condition, const char *file, int line) {
    if (condition) {
        passes++;
        if ((passes % 32) == 0) {
            write_str("PROGRESS ");
            write_u32(passes);
            write_str("\n");
        }
        return;
    }
    failures++;
    write_str("FAIL ");
    write_str(file ? file : "?");
    write_str(":");
    write_u32((DWORD)line);
    write_str("\n");
}

void wlam_test_set_location(const char *file, int line) {
    current_file = file;
    current_line = line;
}

void wlam_test_ok_at(int condition, ...) {
    wlam_test_ok(condition, current_file, current_line);
    if (condition) return;
    va_list args;
    va_start(args, condition);
    const char *format = va_arg(args, const char *);
    write_str("  ");
    write_format(format, args);
    write_str("\n");
    va_end(args);
}

void wlam_test_skip(const char *file, int line) {
    skips++;
    write_str("SKIP ");
    write_str(file ? file : "?");
    write_str(":");
    write_u32((DWORD)line);
    write_str("\n");
}

void entry(void) {
    output = GetStdHandle((DWORD)-11);
    write_str("TEST ");
    write_str(TEST_NAME);
    write_str("\n");
    WLAM_CAT(func_, TEST_FUNC)();
    write_str("RESULT ");
    write_u32(passes);
    write_str(" ");
    write_u32(failures);
    write_str(" ");
    write_u32(skips);
    write_str("\n");
    /* Return to the harness instead of exercising the guest's ExitProcess
       continuation; the RESULT line is the authoritative outcome. */
}
