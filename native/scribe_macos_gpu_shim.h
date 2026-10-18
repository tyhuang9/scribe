#ifndef SCRIBE_MACOS_GPU_SHIM_H
#define SCRIBE_MACOS_GPU_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct scribe_macos_metal_device {
    uint64_t registry_id;
    uint64_t memory_total_bytes;
    uint64_t memory_available_bytes;
    uint8_t is_default;
    uint8_t is_low_power;
    uint8_t is_removable;
    uint8_t has_unified_memory;
    char name[256];
} scribe_macos_metal_device;

typedef uint32_t scribe_macos_metal_metadata_status;

enum {
    SCRIBE_MACOS_METAL_METADATA_OK = 0,
    SCRIBE_MACOS_METAL_METADATA_UNAVAILABLE = 1,
    SCRIBE_MACOS_METAL_METADATA_OVERFLOW = 2,
    SCRIBE_MACOS_METAL_METADATA_INVALID_ARGUMENT = 3,
    SCRIBE_MACOS_METAL_METADATA_INSUFFICIENT_CAPACITY = 4,
};

size_t scribe_macos_copy_metal_devices(scribe_macos_metal_device *devices,
                                       size_t capacity);

scribe_macos_metal_metadata_status
scribe_macos_copy_metal_metadata_devices(scribe_macos_metal_device *devices,
                                         size_t capacity,
                                         size_t *count);

#ifdef __cplusplus
}
#endif

#endif
