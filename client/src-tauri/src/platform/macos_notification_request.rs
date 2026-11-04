//! Original UN request construction/removal shared by GUI and headless host.
//! No policy, permission, delegate or delivery receipt is created here.
use objc2::rc::Retained;
use objc2_foundation::{ns_string, NSArray, NSDictionary, NSString};
use objc2_user_notifications::{
    UNMutableNotificationContent, UNNotificationRequest, UNNotificationSound,
    UNUserNotificationCenter,
};
use zerus_client_core::attention;

/// Shared immutable request construction. The service host supplies its original
/// validated post correlation; legacy GUI delivery keeps its existing userInfo.
pub(crate) fn request(
    token: &str,
    correlation: Option<&str>,
    title: &str,
    body: &str,
) -> Retained<UNNotificationRequest> {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    let token_value = NSString::from_str(token);
    let correlation_value = correlation.map(NSString::from_str);
    let mut keys = vec![ns_string!("hgsAttentionToken")];
    let mut values = vec![&*token_value];
    if let Some(value) = &correlation_value {
        keys.push(ns_string!("hgsAttentionCorrelationV1"));
        values.push(&**value);
    }
    let info = NSDictionary::<NSString, NSString>::from_slices(&keys, &values);
    // SAFETY: a fixed dictionary of strings is valid userInfo; only its element
    // types are erased. No categories or custom actions are installed.
    unsafe {
        let info: Retained<NSDictionary> = Retained::cast_unchecked(info);
        content.setUserInfo(&info);
    }
    UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&attention::request_id(token)),
        &content,
        None,
    )
}

pub(crate) fn withdraw(token: &str) {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let ids = NSArray::from_retained_slice(&[NSString::from_str(&attention::request_id(token))]);
    center.removePendingNotificationRequestsWithIdentifiers(&ids);
    center.removeDeliveredNotificationsWithIdentifiers(&ids);
}
