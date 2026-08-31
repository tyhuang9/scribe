#include "scribe_macos_power_shim.h"

#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/ps/IOPSKeys.h>
#include <IOKit/ps/IOPowerSources.h>

int32_t scribe_macos_power_source(void) {
    CFTypeRef info = IOPSCopyPowerSourcesInfo();
    if (info == NULL) {
        return SCRIBE_MACOS_POWER_UNKNOWN;
    }
    CFArrayRef sources = IOPSCopyPowerSourcesList(info);
    if (sources == NULL) {
        CFRelease(info);
        return SCRIBE_MACOS_POWER_UNKNOWN;
    }
    int32_t result = SCRIBE_MACOS_POWER_UNKNOWN;
    CFIndex count = CFArrayGetCount(sources);
    for (CFIndex index = 0; index < count; ++index) {
        CFTypeRef source = CFArrayGetValueAtIndex(sources, index);
        CFDictionaryRef description = IOPSGetPowerSourceDescription(info, source);
        if (description == NULL) {
            continue;
        }
        CFStringRef state = CFDictionaryGetValue(description, kIOPSPowerSourceStateKey);
        if (state == NULL || CFGetTypeID(state) != CFStringGetTypeID()) {
            continue;
        }
        if (CFEqual(state, kIOPSBatteryPowerValue)) {
            result = SCRIBE_MACOS_POWER_BATTERY;
            break;
        }
        if (CFEqual(state, kIOPSACPowerValue)) {
            result = SCRIBE_MACOS_POWER_AC;
        }
    }
    CFRelease(sources);
    CFRelease(info);
    return result;
}
