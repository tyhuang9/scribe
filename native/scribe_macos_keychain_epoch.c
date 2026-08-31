#include "scribe_macos_keychain_epoch.h"

#include <CoreFoundation/CoreFoundation.h>
#include <Security/Security.h>
#include <stdbool.h>
#include <string.h>

static const char *SCRIBE_EPOCH_SERVICE = "com.scribe.gpu.release-security-epoch.v1";

enum {
    SCRIBE_KEYCHAIN_OK = 0,
    SCRIBE_KEYCHAIN_INVALID_ARGUMENT = 1,
    SCRIBE_KEYCHAIN_SECURITY_ERROR = 2,
    SCRIBE_KEYCHAIN_INVALID_RESULT = 3,
    SCRIBE_KEYCHAIN_RESULT_OVERFLOW = 4,
};

static CFStringRef scribe_string(const char *value) {
    if (value == NULL || value[0] == '\0') {
        return NULL;
    }
    return CFStringCreateWithCString(kCFAllocatorDefault, value, kCFStringEncodingUTF8);
}

static CFMutableDictionaryRef scribe_base_query(CFStringRef access_group,
                                                 CFStringRef service) {
    CFMutableDictionaryRef query = CFDictionaryCreateMutable(
        kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks);
    if (query == NULL) {
        return NULL;
    }
    CFDictionarySetValue(query, kSecClass, kSecClassGenericPassword);
    CFDictionarySetValue(query, kSecUseDataProtectionKeychain, kCFBooleanTrue);
    CFDictionarySetValue(query, kSecAttrSynchronizable, kCFBooleanFalse);
    CFDictionarySetValue(query, kSecAttrAccessGroup, access_group);
    CFDictionarySetValue(query, kSecAttrService, service);
    return query;
}

static int scribe_copy_cfstring(CFStringRef value, uint8_t *destination,
                                unsigned long capacity, unsigned long *length) {
    if (value == NULL || CFGetTypeID(value) != CFStringGetTypeID()) {
        return SCRIBE_KEYCHAIN_INVALID_RESULT;
    }
    CFIndex used = 0;
    CFIndex converted = CFStringGetBytes(
        value, CFRangeMake(0, CFStringGetLength(value)), kCFStringEncodingUTF8,
        0, false, destination, (CFIndex)capacity, &used);
    if (converted != CFStringGetLength(value) || used < 0 ||
        (unsigned long)used > capacity) {
        return SCRIBE_KEYCHAIN_INVALID_RESULT;
    }
    *length = (unsigned long)used;
    return SCRIBE_KEYCHAIN_OK;
}

static bool scribe_cfstring_equal(CFTypeRef observed, CFStringRef expected) {
    return observed != NULL && CFGetTypeID(observed) == CFStringGetTypeID() &&
           CFEqual(observed, expected);
}

static int scribe_validate_item(CFDictionaryRef item, CFStringRef access_group,
                                CFStringRef service,
                                scribe_macos_keychain_epoch_marker *marker) {
    if (item == NULL || CFGetTypeID(item) != CFDictionaryGetTypeID() ||
        !scribe_cfstring_equal(CFDictionaryGetValue(item, kSecAttrAccessGroup),
                               access_group) ||
        !scribe_cfstring_equal(CFDictionaryGetValue(item, kSecAttrService), service) ||
        !scribe_cfstring_equal(CFDictionaryGetValue(item, kSecAttrAccessible),
                               kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly)) {
        return SCRIBE_KEYCHAIN_INVALID_RESULT;
    }
    CFTypeRef synchronizable = CFDictionaryGetValue(item, kSecAttrSynchronizable);
    if (synchronizable != NULL && !CFEqual(synchronizable, kCFBooleanFalse)) {
        return SCRIBE_KEYCHAIN_INVALID_RESULT;
    }
    int result = scribe_copy_cfstring(
        (CFStringRef)CFDictionaryGetValue(item, kSecAttrAccount), marker->account,
        SCRIBE_KEYCHAIN_EPOCH_FIELD_CAPACITY, &marker->account_len);
    if (result != SCRIBE_KEYCHAIN_OK) {
        return result;
    }
    CFTypeRef data = CFDictionaryGetValue(item, kSecValueData);
    if (data == NULL || CFGetTypeID(data) != CFDataGetTypeID()) {
        return SCRIBE_KEYCHAIN_INVALID_RESULT;
    }
    CFIndex payload_len = CFDataGetLength((CFDataRef)data);
    if (payload_len < 0 || payload_len > SCRIBE_KEYCHAIN_EPOCH_FIELD_CAPACITY) {
        return SCRIBE_KEYCHAIN_INVALID_RESULT;
    }
    marker->payload_len = (unsigned long)payload_len;
    if (payload_len > 0) {
        memcpy(marker->payload, CFDataGetBytePtr((CFDataRef)data),
               (size_t)payload_len);
    }
    return SCRIBE_KEYCHAIN_OK;
}

int scribe_macos_keychain_epoch_scan(
    const char *access_group_value,
    scribe_macos_keychain_epoch_marker *markers,
    unsigned long capacity, unsigned long *count) {
    if (markers == NULL || count == NULL || capacity == 0 ||
        access_group_value == NULL || access_group_value[0] == '\0') {
        return SCRIBE_KEYCHAIN_INVALID_ARGUMENT;
    }
    *count = 0;
    CFStringRef access_group = scribe_string(access_group_value);
    CFStringRef service = scribe_string(SCRIBE_EPOCH_SERVICE);
    if (access_group == NULL || service == NULL) {
        if (access_group != NULL) CFRelease(access_group);
        if (service != NULL) CFRelease(service);
        return SCRIBE_KEYCHAIN_INVALID_ARGUMENT;
    }
    CFMutableDictionaryRef query = scribe_base_query(access_group, service);
    if (query == NULL) {
        CFRelease(service);
        CFRelease(access_group);
        return SCRIBE_KEYCHAIN_SECURITY_ERROR;
    }
    CFDictionarySetValue(query, kSecReturnAttributes, kCFBooleanTrue);
    CFDictionarySetValue(query, kSecReturnData, kCFBooleanTrue);
    CFDictionarySetValue(query, kSecMatchLimit, kSecMatchLimitAll);

    CFTypeRef result = NULL;
    OSStatus status = SecItemCopyMatching(query, &result);
    CFRelease(query);
    if (status == errSecItemNotFound) {
        CFRelease(service);
        CFRelease(access_group);
        return SCRIBE_KEYCHAIN_OK;
    }
    if (status != errSecSuccess || result == NULL ||
        CFGetTypeID(result) != CFArrayGetTypeID()) {
        if (result != NULL) CFRelease(result);
        CFRelease(service);
        CFRelease(access_group);
        return status == errSecSuccess ? SCRIBE_KEYCHAIN_INVALID_RESULT
                                       : SCRIBE_KEYCHAIN_SECURITY_ERROR;
    }

    CFIndex result_count = CFArrayGetCount((CFArrayRef)result);
    if (result_count < 0 || (unsigned long)result_count > capacity) {
        CFRelease(result);
        CFRelease(service);
        CFRelease(access_group);
        return SCRIBE_KEYCHAIN_RESULT_OVERFLOW;
    }
    int validation = SCRIBE_KEYCHAIN_OK;
    for (CFIndex index = 0; index < result_count; ++index) {
        validation = scribe_validate_item(
            (CFDictionaryRef)CFArrayGetValueAtIndex((CFArrayRef)result, index),
            access_group, service, &markers[index]);
        if (validation != SCRIBE_KEYCHAIN_OK) {
            break;
        }
    }
    if (validation == SCRIBE_KEYCHAIN_OK) {
        *count = (unsigned long)result_count;
    }
    CFRelease(result);
    CFRelease(service);
    CFRelease(access_group);
    return validation;
}

static int scribe_validate_exact_duplicate(
    CFStringRef access_group, CFStringRef service, CFStringRef account,
    const uint8_t *account_value, unsigned long account_len,
    const uint8_t *payload, unsigned long payload_len) {
    CFMutableDictionaryRef query = scribe_base_query(access_group, service);
    if (query == NULL) {
        return SCRIBE_KEYCHAIN_SECURITY_ERROR;
    }
    CFDictionarySetValue(query, kSecAttrAccount, account);
    CFDictionarySetValue(query, kSecReturnAttributes, kCFBooleanTrue);
    CFDictionarySetValue(query, kSecReturnData, kCFBooleanTrue);
    CFDictionarySetValue(query, kSecMatchLimit, kSecMatchLimitOne);
    CFTypeRef item = NULL;
    OSStatus status = SecItemCopyMatching(query, &item);
    CFRelease(query);
    if (status != errSecSuccess || item == NULL) {
        if (item != NULL) CFRelease(item);
        return SCRIBE_KEYCHAIN_SECURITY_ERROR;
    }
    scribe_macos_keychain_epoch_marker observed = {0};
    int validation = scribe_validate_item((CFDictionaryRef)item, access_group,
                                          service, &observed);
    if (validation == SCRIBE_KEYCHAIN_OK &&
        (observed.account_len != account_len ||
         memcmp(observed.account, account_value, account_len) != 0 ||
         observed.payload_len != payload_len ||
         memcmp(observed.payload, payload, payload_len) != 0)) {
        validation = SCRIBE_KEYCHAIN_INVALID_RESULT;
    }
    CFRelease(item);
    return validation;
}

int scribe_macos_keychain_epoch_append(
    const char *access_group_value, const uint8_t *account_value,
    unsigned long account_len, const uint8_t *payload,
    unsigned long payload_len) {
    if (access_group_value == NULL || access_group_value[0] == '\0' ||
        account_value == NULL || account_len == 0 ||
        account_len > SCRIBE_KEYCHAIN_EPOCH_FIELD_CAPACITY || payload == NULL ||
        payload_len == 0 || payload_len > SCRIBE_KEYCHAIN_EPOCH_FIELD_CAPACITY) {
        return SCRIBE_KEYCHAIN_INVALID_ARGUMENT;
    }
    CFStringRef access_group = scribe_string(access_group_value);
    CFStringRef service = scribe_string(SCRIBE_EPOCH_SERVICE);
    CFStringRef account = CFStringCreateWithBytes(
        kCFAllocatorDefault, account_value, (CFIndex)account_len,
        kCFStringEncodingUTF8, false);
    CFDataRef data = CFDataCreate(kCFAllocatorDefault, payload, (CFIndex)payload_len);
    if (access_group == NULL || service == NULL || account == NULL || data == NULL) {
        if (data != NULL) CFRelease(data);
        if (account != NULL) CFRelease(account);
        if (service != NULL) CFRelease(service);
        if (access_group != NULL) CFRelease(access_group);
        return SCRIBE_KEYCHAIN_INVALID_ARGUMENT;
    }
    CFMutableDictionaryRef item = scribe_base_query(access_group, service);
    if (item == NULL) {
        CFRelease(data);
        CFRelease(account);
        CFRelease(service);
        CFRelease(access_group);
        return SCRIBE_KEYCHAIN_SECURITY_ERROR;
    }
    CFDictionarySetValue(item, kSecAttrAccount, account);
    CFDictionarySetValue(item, kSecAttrAccessible,
                         kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly);
    CFDictionarySetValue(item, kSecValueData, data);
    OSStatus status = SecItemAdd(item, NULL);
    CFRelease(item);

    int result = SCRIBE_KEYCHAIN_OK;
    if (status == errSecDuplicateItem) {
        result = scribe_validate_exact_duplicate(
            access_group, service, account, account_value, account_len, payload,
            payload_len);
    } else if (status != errSecSuccess) {
        result = SCRIBE_KEYCHAIN_SECURITY_ERROR;
    }
    CFRelease(data);
    CFRelease(account);
    CFRelease(service);
    CFRelease(access_group);
    return result;
}
