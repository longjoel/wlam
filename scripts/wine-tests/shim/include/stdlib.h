#ifndef WLAM_SHIM_STDLIB_H
#define WLAM_SHIM_STDLIB_H
#include <stddef.h>
void *malloc(size_t);
void *calloc(size_t, size_t);
void *realloc(void *, size_t);
void free(void *);
int atoi(const char *);
long strtol(const char *, char **, int);
int abs(int);
void exit(int);
void abort(void);
void qsort(void *, size_t, size_t, int (*)(const void *, const void *));
char *getenv(const char *);
#endif
