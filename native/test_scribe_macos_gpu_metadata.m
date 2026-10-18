#import <Foundation/Foundation.h>

#include <stdio.h>
#include <stdlib.h>

// The production implementation is included in this contract translation
// unit so the static array copier can be exercised without substituting a
// second implementation for MTLCopyAllDevices handling.
#import "scribe_macos_gpu_shim.m"

@interface ScribeMetadataDummyDevice : NSObject
@property(nonatomic, readonly) uint64_t fixtureRegistryID;
- (instancetype)initWithRegistryID:(uint64_t)registryID;
@end

@implementation ScribeMetadataDummyDevice

- (instancetype)initWithRegistryID:(uint64_t)registryID {
    self = [super init];
    if (self != nil) {
        _fixtureRegistryID = registryID;
    }
    return self;
}

- (uint64_t)registryID {
    return self.fixtureRegistryID;
}

- (NSString *)name {
    return @"Fixture Metal Device";
}

- (BOOL)isLowPower {
    return YES;
}

- (BOOL)isRemovable {
    return NO;
}

- (BOOL)hasUnifiedMemory {
    return YES;
}

- (NSUInteger)recommendedMaxWorkingSetSize {
    return 8ULL << 30;
}

- (NSUInteger)currentAllocatedSize {
    return 2ULL << 30;
}

@end

static void require_case(BOOL condition, const char *label) {
    if (!condition) {
        fprintf(stderr, "Metal metadata shim contract failed: %s\n", label);
        exit(1);
    }
}

int main(void) {
    @autoreleasepool {
        size_t count = 99;
        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                nil, nil, NULL, 0, &count) ==
                SCRIBE_MACOS_METAL_METADATA_UNAVAILABLE &&
                count == 0,
            "nil array is unavailable");

        count = 99;
        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                @[], nil, NULL, 0, &count) == SCRIBE_MACOS_METAL_METADATA_OK &&
                count == 0,
            "non-nil empty array is a valid empty snapshot");

        ScribeMetadataDummyDevice *single =
            [[ScribeMetadataDummyDevice alloc] initWithRegistryID:7];
        scribe_macos_metal_device copied = {0};
        count = 99;
        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                (NSArray<id<MTLDevice>> *)@[ single ],
                nil,
                &copied,
                1,
                &count) == SCRIBE_MACOS_METAL_METADATA_OK &&
                count == 1 && copied.registry_id == 7 &&
                copied.is_default == 0 && copied.has_unified_memory == 1,
            "nonempty snapshot copies device metadata without default lookup");

        NSMutableArray *maximum = [NSMutableArray array];
        for (NSUInteger index = 0;
             index < (NSUInteger)SCRIBE_MACOS_MAX_METAL_DEVICES;
             ++index) {
            [maximum addObject:[[ScribeMetadataDummyDevice alloc]
                                   initWithRegistryID:index + 1]];
        }
        scribe_macos_metal_device
            maximum_output[SCRIBE_MACOS_MAX_METAL_DEVICES] = {0};
        count = 99;
        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                (NSArray<id<MTLDevice>> *)(NSArray *)maximum,
                nil,
                maximum_output,
                SCRIBE_MACOS_MAX_METAL_DEVICES,
                &count) == SCRIBE_MACOS_METAL_METADATA_OK &&
                count == (size_t)SCRIBE_MACOS_MAX_METAL_DEVICES &&
                maximum_output[0].registry_id == 1 &&
                maximum_output[SCRIBE_MACOS_MAX_METAL_DEVICES - 1].registry_id ==
                    (uint64_t)SCRIBE_MACOS_MAX_METAL_DEVICES,
            "maximum bounded snapshot copies successfully");

        NSMutableArray *oversized = [NSMutableArray array];
        for (NSUInteger index = 0;
             index < (NSUInteger)SCRIBE_MACOS_MAX_METAL_DEVICES + 1;
             ++index) {
            [oversized addObject:[NSNull null]];
        }
        scribe_macos_metal_device
            overflow_output[SCRIBE_MACOS_MAX_METAL_DEVICES];
        scribe_macos_metal_device
            overflow_sentinel[SCRIBE_MACOS_MAX_METAL_DEVICES];
        memset(overflow_output, 0x5a, sizeof(overflow_output));
        memcpy(overflow_sentinel, overflow_output, sizeof(overflow_output));
        count = 99;
        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                (NSArray<id<MTLDevice>> *)(NSArray *)oversized,
                nil,
                overflow_output,
                SCRIBE_MACOS_MAX_METAL_DEVICES,
                &count) == SCRIBE_MACOS_METAL_METADATA_OVERFLOW &&
                count == 0 &&
                memcmp(overflow_output,
                       overflow_sentinel,
                       sizeof(overflow_output)) == 0,
            "oversized array is rejected before device property access");

        NSArray *two_dummy_devices = @[ [NSNull null], [NSNull null] ];
        scribe_macos_metal_device one_device;
        scribe_macos_metal_device one_device_sentinel;
        memset(&one_device, 0x5a, sizeof(one_device));
        memcpy(&one_device_sentinel, &one_device, sizeof(one_device));
        count = 99;
        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                (NSArray<id<MTLDevice>> *)two_dummy_devices,
                nil,
                &one_device,
                1,
                &count) ==
                SCRIBE_MACOS_METAL_METADATA_INSUFFICIENT_CAPACITY &&
                count == 2 &&
                memcmp(&one_device, &one_device_sentinel, sizeof(one_device)) == 0,
            "insufficient capacity is rejected before device property access");

        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                @[], nil, NULL, 0, NULL) ==
                SCRIBE_MACOS_METAL_METADATA_INVALID_ARGUMENT,
            "missing count output is invalid");
        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                @[], nil, &one_device, 0, &count) ==
                SCRIBE_MACOS_METAL_METADATA_INVALID_ARGUMENT,
            "non-null output with zero capacity is invalid");
        require_case(
            scribe_macos_copy_metal_metadata_devices_from_array(
                @[], nil, NULL, 1, &count) ==
                SCRIBE_MACOS_METAL_METADATA_INVALID_ARGUMENT,
            "null output with nonzero capacity is invalid");
    }
    return 0;
}
