// Public os_log literal formats remain visible without private-data logging.
// Do not accept a string, format argument, identifier or notification payload.
#include <os/log.h>
#include <stdint.h>

void zerus_macos_notification_callback(uint32_t role, uint32_t kind) {
    if (role > 1 || kind > 2) return;
    switch (role * 3 + kind) {
    case 0: os_log(OS_LOG_DEFAULT, "zerus-client: notification callback role=gui kind=default"); break;
    case 1: os_log(OS_LOG_DEFAULT, "zerus-client: notification callback role=gui kind=dismiss"); break;
    case 2: os_log(OS_LOG_DEFAULT, "zerus-client: notification callback role=gui kind=other"); break;
    case 3: os_log(OS_LOG_DEFAULT, "zerus-client: notification callback role=host kind=default"); break;
    case 4: os_log(OS_LOG_DEFAULT, "zerus-client: notification callback role=host kind=dismiss"); break;
    case 5: os_log(OS_LOG_DEFAULT, "zerus-client: notification callback role=host kind=other"); break;
    }
}
