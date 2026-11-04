//! Dormant SDK data construction only, never original-post or effect evidence.
//! Current System/Delegate use neither this factory nor these category IDs.
#![allow(dead_code)] // No caller until actual native mappings/resources qualify.
use objc2::rc::Retained;
use objc2_foundation::{ns_string, NSDictionary, NSString};
use objc2_user_notifications::{
    UNMutableNotificationContent, UNNotificationRequest, UNNotificationSound,
};
use zerus_client_core::{
    attention::{self, Notice},
    creation::canonical_uuid,
    native_notice::Owner,
};

const TOKEN_BYTES: usize = 8192;
const TITLE_BYTES: usize = 4096;
const BODY_BYTES: usize = 16384;

fn category(owner: Owner) -> &'static str {
    match owner {
        Owner::AgentAttention => "zerus.attention.agent.v1",
        Owner::SoftwareUpdate => "zerus.attention.update.v1",
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    TooLarge,
    TokenOwner,
    Correlation,
}

/// Validated comparison data, with no claim about a prior or current native post.
struct Input<'a> {
    owner: Owner,
    notice: &'a Notice,
    correlation: &'a str,
}
impl<'a> Input<'a> {
    fn new(owner: Owner, notice: &'a Notice, correlation: &'a str) -> Result<Self, Refused> {
        // Validate the entire input before constructing any Foundation/UN object.
        if notice.token.len() > TOKEN_BYTES
            || notice.title.len() > TITLE_BYTES
            || notice.body.len() > BODY_BYTES
        {
            return Err(Refused::TooLarge);
        }
        if !owner.accepts_token(&notice.token) {
            return Err(Refused::TokenOwner);
        }
        if !canonical_uuid(correlation) {
            return Err(Refused::Correlation);
        }
        Ok(Self {
            owner,
            notice,
            correlation,
        })
    }
}

/// A local immutable SDK request, not a delivered mapping or a native permit.
/// No mutable content escapes, no IDs are generated and no Center is contacted.
struct Draft {
    owner: Owner,
    request: Retained<UNNotificationRequest>,
}
impl std::fmt::Debug for Draft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Draft { .. }")
    }
}
impl Draft {
    fn new(owner: Owner, notice: &Notice, correlation: &str) -> Result<Self, Refused> {
        let input = Input::new(owner, notice, correlation)?;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&input.notice.title));
        content.setBody(&NSString::from_str(&input.notice.body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        content.setCategoryIdentifier(&NSString::from_str(category(input.owner)));
        let token = NSString::from_str(&input.notice.token);
        let correlation = NSString::from_str(input.correlation);
        let info = NSDictionary::<NSString, NSString>::from_slices(
            &[
                ns_string!("hgsAttentionToken"),
                ns_string!("hgsAttentionCorrelationV1"),
            ],
            &[&*token, &*correlation],
        );
        // SAFETY: this fixed dictionary contains two bounded strings and no
        // untrusted objects. Erasing its generic types matches userInfo's API.
        unsafe {
            let info: Retained<NSDictionary> = Retained::cast_unchecked(info);
            content.setUserInfo(&info);
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&attention::request_id(&input.notice.token)),
            &content,
            None,
        );
        // Category registration and all action routing are explicitly separate.
        // No category/action object or custom button is created here. Today's
        // implicit system default/open action must stay intact in that later work.
        Ok(Self {
            owner: input.owner,
            request,
        })
    }
}

#[cfg(test)]
mod tests;
