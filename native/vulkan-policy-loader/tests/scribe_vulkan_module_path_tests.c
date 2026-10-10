// Compile with the exact function extracted from the authenticated loader.c.
// These tests do not load Vulkan, a driver, or any worker pack.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#ifndef WIN32
#define WIN32 1
#endif
#include <windows.h>
#include <malloc.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

typedef int VkResult;
typedef HMODULE loader_platform_dl_handle;
typedef void (*PFN_vkGetInstanceProcAddr)(void);
struct loader_instance { int unused; };
#define VK_SUCCESS 0
#define VK_ERROR_OUT_OF_HOST_MEMORY (-1)
#define VK_SYSTEM_ALLOCATION_SCOPE_INSTANCE 1
#define COMMON_UNIX_PLATFORMS 0
#define loader_stack_alloc _alloca

static wchar_t query_path[32768];
static DWORD path_length;
static unsigned int query_calls;
static unsigned int maximum_calls;
static unsigned int fail_query;
static unsigned int conversion_calls;
static unsigned int fail_conversion;
static int perpetual_truncation;
static int fail_allocation;
static void *allocation;
static DWORD capacities[9];

static DWORD TestGetModuleFileNameW(HMODULE module, wchar_t *buffer, DWORD capacity) {
    (void)module;
    query_calls++;
    // The old stale-error loop must fail assertions without hanging or growing
    // its stack without limit. At most one extra allocation/call is admitted.
    if (query_calls > maximum_calls || query_calls == fail_query) {
        SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    }
    capacities[query_calls - 1] = capacity;
    if (perpetual_truncation || path_length >= capacity) {
        SetLastError(ERROR_INSUFFICIENT_BUFFER);
        buffer[capacity - 1] = L'\0';
        return capacity;
    }
    memcpy(buffer, query_path, ((size_t)path_length + 1) * sizeof(wchar_t));
    // Intentionally retain last-error after success, as the Win32 contract
    // permits. A successful return length is the only resize authority.
    return path_length;
}

static int TestWideCharToMultiByte(UINT code_page, DWORD flags, const wchar_t *wide, int wide_length,
                                 char *utf8, int utf8_length, const char *fallback, BOOL *used_fallback) {
    conversion_calls++;
    if (conversion_calls == fail_conversion) {
        SetLastError(ERROR_NO_UNICODE_TRANSLATION);
        return 0;
    }
    return WideCharToMultiByte(code_page, flags, wide, wide_length, utf8, utf8_length, fallback, used_fallback);
}

static void *loader_instance_heap_calloc(const struct loader_instance *instance, size_t size, int scope) {
    (void)instance;
    (void)scope;
    if (fail_allocation) { return NULL; }
    allocation = calloc(1, size);
    return allocation;
}

#define GetModuleFileNameW TestGetModuleFileNameW
#define WideCharToMultiByte TestWideCharToMultiByte
#include "module-path-function.h"
#undef WideCharToMultiByte
#undef GetModuleFileNameW

struct TestCase {
    const char *name;
    DWORD length;
    DWORD stale_error;
    unsigned int expected_calls;
    unsigned int fail_query_at;
    unsigned int fail_conversion_at;
    int truncate_forever;
    int allocation_failure;
    int unicode;
    int expected_path;
    VkResult expected_result;
};

int main(void) {
    static const struct TestCase cases[] = {
        {"short", 32, ERROR_SUCCESS, 1, 0, 0, 0, 0, 0, 1, VK_SUCCESS},
        {"stale-short", 32, ERROR_INSUFFICIENT_BUFFER, 1, 0, 0, 0, 0, 0, 1, VK_SUCCESS},
        {"capacity-minus-one", 259, ERROR_INSUFFICIENT_BUFFER, 1, 0, 0, 0, 0, 0, 1, VK_SUCCESS},
        {"capacity", 260, ERROR_SUCCESS, 2, 0, 0, 0, 0, 0, 1, VK_SUCCESS},
        {"one-resize", 300, ERROR_SUCCESS, 2, 0, 0, 0, 0, 0, 1, VK_SUCCESS},
        {"multi-resize", 1200, ERROR_SUCCESS, 4, 0, 0, 0, 0, 0, 1, VK_SUCCESS},
        {"ceiling", 32767, ERROR_SUCCESS, 8, 0, 0, 0, 0, 0, 1, VK_SUCCESS},
        {"perpetual-truncation", 32, ERROR_SUCCESS, 8, 0, 0, 1, 0, 0, 0, VK_SUCCESS},
        {"initial-failure", 32, ERROR_SUCCESS, 1, 1, 0, 0, 0, 0, 0, VK_SUCCESS},
        {"resized-failure", 300, ERROR_SUCCESS, 2, 2, 0, 0, 0, 0, 0, VK_SUCCESS},
        {"unicode", 300, ERROR_SUCCESS, 2, 0, 0, 0, 0, 1, 1, VK_SUCCESS},
        {"allocation-failure", 32, ERROR_SUCCESS, 1, 0, 0, 0, 1, 0, 0, VK_ERROR_OUT_OF_HOST_MEMORY},
        {"conversion-size-failure", 32, ERROR_SUCCESS, 1, 0, 1, 0, 0, 0, 0, VK_SUCCESS},
        {"conversion-data-failure", 32, ERROR_SUCCESS, 1, 0, 2, 0, 0, 0, 0, VK_SUCCESS},
    };
    size_t index;
    unsigned int failures = 0;
    for (index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        const struct TestCase *test = &cases[index];
        DWORD offset;
        char *path = NULL;
        VkResult result;
        int valid;
        path_length = test->length;
        query_calls = conversion_calls = 0;
        maximum_calls = test->expected_calls;
        fail_query = test->fail_query_at;
        fail_conversion = test->fail_conversion_at;
        perpetual_truncation = test->truncate_forever;
        fail_allocation = test->allocation_failure;
        allocation = NULL;
        memset(capacities, 0, sizeof(capacities));
        for (offset = 0; offset < path_length; offset++) { query_path[offset] = L'a'; }
        if (test->unicode) {
            query_path[path_length - 2] = L'\x03a9';
            query_path[path_length - 1] = L'\x4e2d';
        }
        query_path[path_length] = L'\0';
        SetLastError(test->stale_error);
        result = get_library_path_of_dl_handle(NULL, NULL, NULL, &path);
        valid = result == test->expected_result && query_calls == test->expected_calls &&
                (path != NULL) == test->expected_path;
        if (path != NULL) {
            const size_t expected_length = (size_t)path_length + (test->unicode ? 3 : 0);
            valid = valid && strlen(path) == expected_length;
            for (offset = 0; offset < path_length - (test->unicode ? 2 : 0); offset++) {
                valid = valid && path[offset] == 'a';
            }
            if (test->unicode) {
                valid = valid && memcmp(path + path_length - 2, "\xce\xa9\xe4\xb8\xad", 6) == 0;
            }
        }
        if (test->truncate_forever || test->length == 32767) {
            valid = valid && capacities[7] == 32768 && conversion_calls == (test->truncate_forever ? 0U : 2U);
        }
        // Free the fixture-owned allocation even on the upstream optional
        // conversion-failure path; changing its semantics is out of scope.
        free(allocation);
        if (!valid) {
            fprintf(stderr, "module-path case failed: %s calls=%u\n", test->name, query_calls);
            failures++;
        }
    }
    if (failures != 0 || sizeof(cases) / sizeof(cases[0]) != 14) { return 1; }
    puts("SCRIBE_VULKAN_MODULE_PATH_TESTS=PASS cases=14");
    return 0;
}
