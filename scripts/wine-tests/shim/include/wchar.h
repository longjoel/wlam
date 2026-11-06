#ifndef WLAM_SHIM_WCHAR_H
#define WLAM_SHIM_WCHAR_H
#include <stddef.h>
typedef unsigned short wchar_t;
size_t wcslen(const wchar_t *);
wchar_t *wcscpy(wchar_t *, const wchar_t *);
int wcscmp(const wchar_t *, const wchar_t *);
#endif
