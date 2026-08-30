#import "scribe_macos_gpu_shim.h"

#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

#include <string.h>

static void scribe_copy_device(id<MTLDevice> device,
                               id<MTLDevice> default_device,
                               scribe_macos_metal_device *output) {
    memset(output, 0, sizeof(*output));
    output->registry_id = device.registryID;
    output->is_default = default_device != nil &&
                         device.registryID == default_device.registryID;
    output->is_low_power = device.lowPower;
    output->is_removable = device.removable;
    if (@available(macOS 10.15, *)) {
        output->has_unified_memory = device.hasUnifiedMemory;
    }
    if (@available(macOS 10.12, *)) {
        output->memory_total_bytes = device.recommendedMaxWorkingSetSize;
    }
    if (@available(macOS 10.15, *)) {
        uint64_t allocated = device.currentAllocatedSize;
        output->memory_available_bytes =
            allocated < output->memory_total_bytes
                ? output->memory_total_bytes - allocated
                : 0;
    }
    const char *name = device.name.UTF8String;
    if (name != NULL) {
        strlcpy(output->name, name, sizeof(output->name));
    }
}

size_t scribe_macos_copy_metal_devices(scribe_macos_metal_device *devices,
                                       size_t capacity) {
    @autoreleasepool {
        id<MTLDevice> default_device = MTLCreateSystemDefaultDevice();
        NSArray<id<MTLDevice>> *all_devices = MTLCopyAllDevices();
        if (all_devices.count == 0 && default_device != nil) {
            all_devices = @[ default_device ];
        }
        size_t count = (size_t)all_devices.count;
        if (devices == NULL || capacity == 0) {
            return count;
        }
        size_t copied = count < capacity ? count : capacity;
        for (size_t index = 0; index < copied; ++index) {
            scribe_copy_device(all_devices[index], default_device, &devices[index]);
        }
        return count;
    }
}
