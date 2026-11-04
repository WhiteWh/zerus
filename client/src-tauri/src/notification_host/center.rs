//! SDK calls for the original service host, after native channel establishment.
//! No Tauri window, durable ledger, discovery or transport is constructed here.
use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::NSError;
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNNotificationSettings, UNUserNotificationCenter,
};
use std::{
    fs::File,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use zerus_client_core::{
    desktop::notifications::macos::{Callback, Center},
    macos_notify::Envelope,
};

/// The fixed native entry creates this only after verifying its own signed
/// bundle and the actual inherited service socket. The plain shared File keeps
/// the inherited flock alive; no unlock-on-drop wrapper is ever used.
pub(crate) struct HostCenter {
    owner: Arc<File>,
}
impl HostCenter {
    pub(crate) fn new(owner: Arc<File>) -> Self {
        Self { owner }
    }
}

fn authorization_reply(reply: Callback, owner: Arc<File>) -> RcBlock<dyn Fn(Bool, *mut NSError)> {
    RcBlock::new(move |granted: Bool, error: *mut NSError| {
        let _retained = &owner;
        // complete is once-only, but both captures remain alive through ALL
        // native block copies and call return, including after completion.
        reply.complete(granted.as_bool() && error.is_null());
    })
}
fn post_reply(reply: Callback, owner: Arc<File>) -> RcBlock<dyn Fn(*mut NSError)> {
    RcBlock::new(move |error: *mut NSError| {
        let _retained = &owner;
        reply.complete(error.is_null());
    })
}
impl Center for HostCenter {
    fn authorize(&self, reply: Callback) {
        let owner = self.owner.clone();
        let seen = AtomicBool::new(false);
        let settings = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // Native settings copies cannot issue several authorization calls.
            // Keep reply/owner captured even when this original was consumed.
            if seen.swap(true, Ordering::AcqRel) {
                return;
            }
            // SAFETY: the framework supplies a live settings object for this call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            if status == UNAuthorizationStatus::NotDetermined {
                let next = authorization_reply(reply.clone(), owner.clone());
                UNUserNotificationCenter::currentNotificationCenter()
                    .requestAuthorizationWithOptions_completionHandler(
                        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                        &next,
                    );
            } else {
                reply.complete(
                    status == UNAuthorizationStatus::Authorized
                        || status == UNAuthorizationStatus::Provisional,
                );
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&settings);
    }
    fn post(&self, envelope: &Envelope, title: &str, body: &str, reply: Callback) {
        let request = crate::platform::macos_notification_request::request(
            envelope.token(),
            Some(envelope.correlation()),
            title,
            body,
        );
        let done = post_reply(reply, self.owner.clone());
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&done));
    }
    fn withdraw(&self, token: &str) {
        crate::platform::macos_notification_request::withdraw(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, sync::Mutex, time::Instant};
    use zerus_client_core::{
        attention,
        desktop::notifications::{self as wire, macos::Host},
    };

    fn command() -> wire::Command {
        let id = |n: u8| format!("00000000-0000-4000-8000-{n:012x}");
        wire::Command {
            binding: wire::Binding {
                session: wire::Session {
                    configuration: "a".repeat(64),
                    service_epoch: id(1),
                    writer_epoch: id(2),
                    adapter: id(3),
                    host_epoch: id(4),
                    lease: id(5),
                    platform: wire::Platform::Macos,
                },
                request_id: id(6),
                intent_id: "b".repeat(64),
                revision: 1,
                seq: 1,
            },
            action: wire::Action::Post {
                token: attention::encode_token(
                    serde_json::json!({"host":"peer.example",
                    "name":"fixture","run":"fixture-run","notice":id(7)})
                    .as_object()
                    .unwrap(),
                ),
                title: "Fixture".into(),
                body: "Original post".into(),
            },
        }
    }
    #[derive(Default)]
    struct Fake(Mutex<Option<Callback>>);
    impl Center for Fake {
        fn authorize(&self, reply: Callback) {
            *self.0.lock().unwrap() = Some(reply);
        }
        fn post(&self, _: &Envelope, _: &str, _: &str, reply: Callback) {
            *self.0.lock().unwrap() = Some(reply);
        }
        fn withdraw(&self, _: &str) {}
    }

    fn retained_block(post: bool) {
        let command = command();
        let mut host = Host::new(command.binding.session.clone()).unwrap();
        drop(host.next_output());
        let fake = Fake::default();
        let now = Instant::now();
        host.receive(wire::ToHost::Command(command), &fake, now)
            .unwrap();
        if post {
            fake.0.lock().unwrap().take().unwrap().complete(true);
            host.poll(&fake, now);
        }
        let callback = fake.0.lock().unwrap().take().unwrap();
        let root = std::env::temp_dir().join(format!("zerus-host-block-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let owner = Arc::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join("owner"))
                .unwrap(),
        );
        let weak = Arc::downgrade(&owner);
        // Only native Block_copy/call/drop and data constructors; never a
        // UNUserNotificationCenter method, GUI, permission or real host entry.
        if post {
            let block = post_reply(callback, owner);
            let copy = block.copy();
            block.call((std::ptr::null_mut(),));
            copy.call((std::ptr::null_mut(),));
            host.poll(&fake, now);
            assert!(matches!(
                host.next_output().unwrap().frame,
                wire::FromHost::Receipt(_)
            ));
            drop(block);
            host.poll(&fake, now);
            assert!(host.next_output().is_none());
            assert!(weak.upgrade().is_some());
            drop(copy);
        } else {
            let block = authorization_reply(callback, owner);
            let copy = block.copy();
            block.call((Bool::NO, std::ptr::null_mut()));
            copy.call((Bool::YES, std::ptr::null_mut()));
            host.poll(&fake, now);
            assert!(matches!(
                host.next_output().unwrap().frame,
                wire::FromHost::Receipt(_)
            ));
            drop(block);
            host.poll(&fake, now);
            assert!(host.next_output().is_none());
            assert!(weak.upgrade().is_some());
            drop(copy);
        }
        host.poll(&fake, now);
        assert!(matches!(
            host.next_output().unwrap().frame,
            wire::FromHost::Released { .. }
        ));
        assert!(weak.upgrade().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn completed_authorization_block_retains_the_job_and_owner_through_its_final_copy() {
        retained_block(false);
    }
    #[test]
    fn completed_post_block_retains_the_job_and_owner_through_its_final_copy() {
        retained_block(true);
    }
    #[test]
    fn request_data_preserves_original_uuid_and_legacy_default_action() {
        use objc2_foundation::{ns_string, NSString};
        let command = command();
        let envelope = Envelope::new(command.action.token(), &command.binding.request_id).unwrap();
        let request = crate::platform::macos_notification_request::request(
            envelope.token(),
            Some(envelope.correlation()),
            "Fixture",
            "Original post",
        );
        assert_eq!(request.identifier().to_string(), envelope.request_id());
        let info = request.content().userInfo();
        assert_eq!(info.count(), 2);
        let token = info
            .objectForKey(ns_string!("hgsAttentionToken"))
            .unwrap()
            .downcast::<NSString>()
            .unwrap();
        let correlation = info
            .objectForKey(ns_string!("hgsAttentionCorrelationV1"))
            .unwrap()
            .downcast::<NSString>()
            .unwrap();
        assert_eq!(token.to_string(), envelope.token());
        assert_eq!(correlation.to_string(), envelope.correlation());
        assert_eq!(request.content().categoryIdentifier().length(), 0);
        let legacy = crate::platform::macos_notification_request::request(
            envelope.token(),
            None,
            "Fixture",
            "Original post",
        );
        assert_eq!(legacy.content().userInfo().count(), 1);
        assert_eq!(legacy.content().categoryIdentifier().length(), 0);
    }
}
