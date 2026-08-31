#ifndef SCRIBE_MACOS_POWER_SHIM_H
#define SCRIBE_MACOS_POWER_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
    SCRIBE_MACOS_POWER_UNKNOWN = 0,
    SCRIBE_MACOS_POWER_AC = 1,
    SCRIBE_MACOS_POWER_BATTERY = 2,
};

int32_t scribe_macos_power_source(void);

#ifdef __cplusplus
}
#endif

#endif
