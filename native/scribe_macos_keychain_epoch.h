#ifndef SCRIBE_MACOS_KEYCHAIN_EPOCH_H
#define SCRIBE_MACOS_KEYCHAIN_EPOCH_H

#include <stddef.h>
#include <stdint.h>

#define SCRIBE_KEYCHAIN_EPOCH_FIELD_CAPACITY 96

typedef struct scribe_macos_keychain_epoch_marker {
    uint8_t account[SCRIBE_KEYCHAIN_EPOCH_FIELD_CAPACITY];
    unsigned long account_len;
    uint8_t payload[SCRIBE_KEYCHAIN_EPOCH_FIELD_CAPACITY];
    unsigned long payload_len;
} scribe_macos_keychain_epoch_marker;

int scribe_macos_keychain_epoch_scan(
    const char *access_group,
    scribe_macos_keychain_epoch_marker *markers,
    unsigned long capacity,
    unsigned long *count);

int scribe_macos_keychain_epoch_append(
    const char *access_group,
    const uint8_t *account,
    unsigned long account_len,
    const uint8_t *payload,
    unsigned long payload_len);

#endif
