#ifndef SCRIBE_MACOS_GPU_SHIM_H
#define SCRIBE_MACOS_GPU_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
    SCRIBE_MACOS_POWER_UNKNOWN = 0,
    SCRIBE_MACOS_POWER_AC = 1,
    SCRIBE_MACOS_POWER_BATTERY = 2,
};

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

size_t scribe_macos_copy_metal_devices(scribe_macos_metal_device *devices,
                                       size_t capacity);
int32_t scribe_macos_power_source(void);

#ifdef __cplusplus
}
#endif

#endif
