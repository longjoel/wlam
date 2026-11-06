#ifndef WLAM_SHIM_IO_H
#define WLAM_SHIM_IO_H
#include <stddef.h>
int _open(const char *, int, ...);
int _close(int);
int _read(int, void *, unsigned);
int _write(int, const void *, unsigned);
long _lseek(int, long, int);
#endif
