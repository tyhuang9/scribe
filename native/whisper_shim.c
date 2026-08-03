// Opaque dynamic-loader shim for whisper.cpp f049fff / v1.9.1.
//
// All upstream structs that cross Whisper's ABI by value remain in this C
// translation unit, compiled against the vendored upstream headers. Rust only
// sees opaque handles, primitives, and callback data.

#include "whisper-f049fff/whisper.h"

#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif

typedef void (*scribe_whisper_segment_callback)(
    void * user_data,
    const char * text,
    int64_t start_ticks,
    int64_t end_ticks);

struct scribe_whisper_runtime {
#ifdef _WIN32
    HMODULE module;
#else
    void * module;
#endif
    struct whisper_context * context;

    struct whisper_context_params * (*context_default_params_by_ref)(void);
    struct whisper_full_params * (*full_default_params_by_ref)(enum whisper_sampling_strategy);
    void (*free_context_params)(struct whisper_context_params * params);
    void (*free_params)(struct whisper_full_params * params);
    struct whisper_context * (*init_from_file_with_params)(
        const char * path_model,
        struct whisper_context_params params);
    int (*full)(
        struct whisper_context * context,
        struct whisper_full_params params,
        const float * samples,
        int n_samples);
    int (*full_n_segments)(struct whisper_context * context);
    const char * (*full_get_segment_text)(struct whisper_context * context, int i_segment);
    int64_t (*full_get_segment_t0)(struct whisper_context * context, int i_segment);
    int64_t (*full_get_segment_t1)(struct whisper_context * context, int i_segment);
    int (*full_lang_id)(struct whisper_context * context);
    const char * (*lang_str)(int id);
    void (*free_context)(struct whisper_context * context);
    void (*backend_load_all_from_path)(const char * dir_path);
};

static char * scribe_strdup(const char * source) {
    size_t length;
    char * copy;

    if (source == NULL) {
        return NULL;
    }
    length = strlen(source) + 1;
    copy = (char *) malloc(length);
    if (copy != NULL) {
        memcpy(copy, source, length);
    }
    return copy;
}

static void scribe_set_error(char ** out_error, const char * format, ...) {
    va_list args;
    va_list copied_args;
    int required;
    char * message;

    if (out_error == NULL) {
        return;
    }
    *out_error = NULL;
    va_start(args, format);
    va_copy(copied_args, args);
    required = vsnprintf(NULL, 0, format, copied_args);
    va_end(copied_args);
    if (required < 0) {
        va_end(args);
        *out_error = scribe_strdup("native Whisper shim could not format an error");
        return;
    }
    message = (char *) malloc((size_t) required + 1);
    if (message != NULL) {
        (void) vsnprintf(message, (size_t) required + 1, format, args);
    }
    va_end(args);
    *out_error = message;
}

#ifdef _WIN32
static HMODULE scribe_open_library(const char * path, char ** out_error) {
    int required;
    wchar_t * wide_path;
    HMODULE module;

    required = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, NULL, 0);
    if (required <= 0) {
        scribe_set_error(out_error, "native Whisper DLL path is not valid UTF-8");
        return NULL;
    }
    wide_path = (wchar_t *) malloc((size_t) required * sizeof(wchar_t));
    if (wide_path == NULL) {
        scribe_set_error(out_error, "native Whisper shim could not allocate a DLL path");
        return NULL;
    }
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide_path, required) <= 0) {
        free(wide_path);
        scribe_set_error(out_error, "native Whisper DLL path is not valid UTF-8");
        return NULL;
    }
    module = LoadLibraryExW(
        wide_path,
        NULL,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    free(wide_path);
    if (module == NULL) {
        scribe_set_error(
            out_error,
            "could not load native Whisper DLL (Windows error %lu): %s",
            (unsigned long) GetLastError(),
            path);
    }
    return module;
}

static void * scribe_find_symbol(HMODULE module, const char * name) {
    return (void *) GetProcAddress(module, name);
}

static void scribe_close_library(HMODULE module) {
    if (module != NULL) {
        (void) FreeLibrary(module);
    }
}

static void * scribe_find_backend_loader(void) {
    HMODULE ggml_module = GetModuleHandleW(L"ggml.dll");
    if (ggml_module == NULL) {
        return NULL;
    }
    return (void *) GetProcAddress(ggml_module, "ggml_backend_load_all_from_path");
}
#else
static void * scribe_open_library(const char * path, char ** out_error) {
    void * module = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (module == NULL) {
        scribe_set_error(out_error, "could not load native Whisper library %s: %s", path, dlerror());
    }
    return module;
}

static void * scribe_find_symbol(void * module, const char * name) {
    return dlsym(module, name);
}

static void scribe_close_library(void * module) {
    if (module != NULL) {
        (void) dlclose(module);
    }
}

static void * scribe_find_backend_loader(void) {
    return dlsym(RTLD_DEFAULT, "ggml_backend_load_all_from_path");
}
#endif

static char * scribe_parent_directory(const char * path) {
    char * parent = scribe_strdup(path);
    char * slash;
    char * backslash;

    if (parent == NULL) {
        return NULL;
    }
    slash = strrchr(parent, '/');
    backslash = strrchr(parent, '\\');
    if (backslash != NULL && (slash == NULL || backslash > slash)) {
        slash = backslash;
    }
    if (slash == NULL) {
        free(parent);
        return NULL;
    }
    *slash = '\0';
    return parent;
}

#define SCRIBE_LOAD_SYMBOL(runtime, field, symbol_name, out_error) \
    do { \
        (runtime)->field = (void *) scribe_find_symbol((runtime)->module, (symbol_name)); \
        if ((runtime)->field == NULL) { \
            scribe_set_error((out_error), "native Whisper library is missing required symbol %s", (symbol_name)); \
            goto failed; \
        } \
    } while (0)

struct scribe_whisper_runtime * scribe_whisper_runtime_open(
    const char * library_path,
    char ** out_error) {
    struct scribe_whisper_runtime * runtime;
    char * backend_directory;

    if (out_error != NULL) {
        *out_error = NULL;
    }
    if (library_path == NULL) {
        scribe_set_error(out_error, "native Whisper bootstrap requires a library path");
        return NULL;
    }

    runtime = (struct scribe_whisper_runtime *) calloc(1, sizeof(*runtime));
    if (runtime == NULL) {
        scribe_set_error(out_error, "native Whisper shim could not allocate runtime state");
        return NULL;
    }
    runtime->module = scribe_open_library(library_path, out_error);
    if (runtime->module == NULL) {
        goto failed;
    }

    SCRIBE_LOAD_SYMBOL(runtime, context_default_params_by_ref, "whisper_context_default_params_by_ref", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, full_default_params_by_ref, "whisper_full_default_params_by_ref", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, free_context_params, "whisper_free_context_params", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, free_params, "whisper_free_params", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, init_from_file_with_params, "whisper_init_from_file_with_params", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, full, "whisper_full", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, full_n_segments, "whisper_full_n_segments", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, full_get_segment_text, "whisper_full_get_segment_text", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, full_get_segment_t0, "whisper_full_get_segment_t0", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, full_get_segment_t1, "whisper_full_get_segment_t1", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, full_lang_id, "whisper_full_lang_id", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, lang_str, "whisper_lang_str", out_error);
    SCRIBE_LOAD_SYMBOL(runtime, free_context, "whisper_free", out_error);
    runtime->backend_load_all_from_path = (void (*)(const char *)) scribe_find_backend_loader();
    if (runtime->backend_load_all_from_path == NULL) {
        scribe_set_error(out_error, "native Whisper package is missing ggml_backend_load_all_from_path");
        goto failed;
    }
    backend_directory = scribe_parent_directory(library_path);
    if (backend_directory == NULL) {
        scribe_set_error(out_error, "native Whisper shim could not resolve its backend directory");
        goto failed;
    }
    runtime->backend_load_all_from_path(backend_directory);
    free(backend_directory);

    return runtime;

failed:
    if (runtime != NULL) {
        scribe_close_library(runtime->module);
        free(runtime);
    }
    return NULL;
}

int scribe_whisper_runtime_load_model(
    struct scribe_whisper_runtime * runtime,
    const char * model_path,
    int use_gpu,
    int gpu_device,
    char ** out_error) {
    struct whisper_context_params * defaults;
    struct whisper_context_params context_params;

    if (out_error != NULL) {
        *out_error = NULL;
    }
    if (runtime == NULL || model_path == NULL) {
        scribe_set_error(out_error, "native Whisper model bootstrap requires a loaded runtime and model path");
        return -1;
    }
    if (runtime->context != NULL) {
        scribe_set_error(out_error, "native Whisper runtime already has a retained model");
        return -1;
    }

    defaults = runtime->context_default_params_by_ref();
    if (defaults == NULL) {
        scribe_set_error(out_error, "native Whisper could not allocate default context parameters");
        return -1;
    }
    context_params = *defaults;
    runtime->free_context_params(defaults);
    context_params.use_gpu = use_gpu != 0;
    context_params.gpu_device = gpu_device;
    runtime->context = runtime->init_from_file_with_params(model_path, context_params);
    if (runtime->context == NULL) {
        scribe_set_error(out_error, "native Whisper could not load model: %s", model_path);
        return -1;
    }

    return 0;
}

int scribe_whisper_runtime_transcribe(
    struct scribe_whisper_runtime * runtime,
    const float * samples,
    size_t sample_count,
    scribe_whisper_segment_callback callback,
    void * user_data,
    char ** out_language,
    char ** out_error) {
    struct whisper_full_params * defaults;
    struct whisper_full_params full_params;
    int result;
    int segment_count;
    int language_id;
    int segment_index;

    if (out_error != NULL) {
        *out_error = NULL;
    }
    if (out_language != NULL) {
        *out_language = NULL;
    }
    if (runtime == NULL || runtime->context == NULL) {
        scribe_set_error(out_error, "native Whisper runtime is not loaded");
        return -1;
    }
    if ((samples == NULL && sample_count != 0) || sample_count > INT_MAX) {
        scribe_set_error(out_error, "native Whisper received an invalid audio buffer");
        return -1;
    }

    defaults = runtime->full_default_params_by_ref(WHISPER_SAMPLING_GREEDY);
    if (defaults == NULL) {
        scribe_set_error(out_error, "native Whisper could not allocate default decode parameters");
        return -1;
    }
    full_params = *defaults;
    runtime->free_params(defaults);
    full_params.print_progress = false;
    full_params.print_realtime = false;
    full_params.print_timestamps = false;
    full_params.no_context = true;
    full_params.no_timestamps = false;
    full_params.language = "en";
    full_params.detect_language = false;

    result = runtime->full(runtime->context, full_params, samples, (int) sample_count);
    if (result != 0) {
        scribe_set_error(out_error, "native Whisper inference failed with code %d", result);
        return result;
    }

    if (out_language != NULL) {
        language_id = runtime->full_lang_id(runtime->context);
        if (language_id >= 0) {
            *out_language = scribe_strdup(runtime->lang_str(language_id));
        }
    }

    segment_count = runtime->full_n_segments(runtime->context);
    for (segment_index = 0; segment_index < segment_count; ++segment_index) {
        if (callback != NULL) {
            callback(
                user_data,
                runtime->full_get_segment_text(runtime->context, segment_index),
                runtime->full_get_segment_t0(runtime->context, segment_index),
                runtime->full_get_segment_t1(runtime->context, segment_index));
        }
    }
    return 0;
}

void scribe_whisper_runtime_destroy(struct scribe_whisper_runtime * runtime) {
    if (runtime == NULL) {
        return;
    }
    if (runtime->context != NULL && runtime->free_context != NULL) {
        runtime->free_context(runtime->context);
    }
    scribe_close_library(runtime->module);
    free(runtime);
}

void scribe_whisper_string_free(char * value) {
    free(value);
}
