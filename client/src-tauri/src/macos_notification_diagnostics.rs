//! Fixed-content callback diagnostics. Never format native notification data.
use objc2_foundation::NSString;
use objc2_user_notifications::{
    UNNotificationDefaultActionIdentifier, UNNotificationDismissActionIdentifier,
};

pub(crate) enum Role {
    #[cfg(feature = "live")]
    Gui,
    Host,
}

// The public os_log macro performs compiler-assisted literal encoding in C.
// Only closed numeric labels cross the bridge; no native strings or format
// arguments do. Logs are observations, never callback/permission authority.
unsafe extern "C" {
    fn zerus_macos_notification_callback(role: u32, kind: u32);
}

/// Called only by an already installed delegate, without changing its behavior.
pub(crate) fn response(role: Role, action: &NSString) {
    let role = match role {
        #[cfg(feature = "live")]
        Role::Gui => 0,
        Role::Host => 1,
    };
    let kind = match kind(action) {
        "default" => 0,
        "dismiss" => 1,
        _ => 2,
    };
    // SAFETY: the bridge accepts plain numbers and logs only static literals.
    unsafe { zerus_macos_notification_callback(role, kind) };
}

fn kind(action: &NSString) -> &'static str {
    // SAFETY: immutable identifiers supplied by UserNotifications.framework.
    if action.isEqualToString(unsafe { UNNotificationDefaultActionIdentifier }) {
        "default"
    } else if action.isEqualToString(unsafe { UNNotificationDismissActionIdentifier }) {
        "dismiss"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_log_transport_smoke_without_a_native_callback() {
        // This is a logging transport probe, never an observed OS callback.
        println!("UNIFIED_LOG_TRANSPORT_SMOKE_PID {}", std::process::id());
        response(Role::Host, &NSString::from_str("private-fixture-action"));
    }

    #[test]
    fn native_identifiers_are_classified_without_exposing_other_action_text() {
        // SAFETY: immutable SDK constants; no center, permission or notification.
        assert_eq!(
            kind(unsafe { UNNotificationDefaultActionIdentifier }),
            "default"
        );
        assert_eq!(
            kind(unsafe { UNNotificationDismissActionIdentifier }),
            "dismiss"
        );
        for text in ["", "private-fixture-action", "default", "dismiss"] {
            assert_eq!(kind(&NSString::from_str(text)), "other");
        }
    }
}
