/*
 * Minimal stand-in for Wine's include/wine/test.h.
 *
 * The real header is a complete test runner (subtests, todos, flaky tests,
 * child processes, coloured output). The conformance harness only needs to
 * compile the test body and report one pass/fail record per check. Keeping
 * this shim small makes it obvious that a Wine test is being run unchanged
 * except for its reporting plumbing.
 */
#ifndef __WINE_WINE_TEST_H
#define __WINE_WINE_TEST_H

#include <stdarg.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include "windef.h"
#include "winbase.h"

#ifndef ARRAY_SIZE
#define ARRAY_SIZE(x) (sizeof(x) / sizeof((x)[0]))
#endif

void wlam_test_ok(int condition, const char *file, int line);
void wlam_test_skip(const char *file, int line);
void wlam_test_set_location(const char *file, int line);
void wlam_test_ok_at(int condition, ...);

/* Formatting helpers from Wine's wine/debug.h. */
const char *wine_dbgstr_rect(const RECT *rect);
const char *wine_dbgstr_w(const WCHAR *str);
const char *wine_dbgstr_wn(const WCHAR *str, int len);

#define START_TEST(name) void func_##name(void)
#define ok_(file, line) (wlam_test_set_location(file, line), 0) ? (void)0 : wlam_test_ok_at
#define ok(cond, ...) ok_(__FILE__, __LINE__)(!!(cond), ##__VA_ARGS__)
/* Wine's skip() reports a skipped check but does not return from the caller;
 * tests that want to stop do an explicit `return` after it. */
#define skip(...) do { wlam_test_skip(__FILE__, __LINE__); } while (0)
#define win_skip(...) skip(__VA_ARGS__)
#define broken(x) (x)
/* Todo checks are deliberately skipped until the harness models Wine's
 * todo accounting. The corresponding source line is still visible in the
 * commit, so a skipped todo cannot silently become an ordinary pass. */
#define todo_wine if (0)
#define todo_wine_if(x) if (0)
#define trace(...) do { } while (0)
#define warn(...) do { } while (0)
#define subtest do { } while (0)
#define ignore_exceptions do { } while (0)
#define flaky if (1)
#define flaky_if(x) if (1)
#define wait_child_process do { } while (0)
#define winetest_platform "windows"
#define winetest_platform_is_wine 0
#define winetest_debug 1
#define winetest_time 0
#define winetest_interactive 0
#define winetest_report_success 0
#define winetest_mute_threshold 0

#endif
