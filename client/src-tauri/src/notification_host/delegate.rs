//! Bounded original-correlation callback ingress. No activation or UI effect.
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject},
    AnyThread, DefinedClass,
};
use objc2_foundation::{ns_string, NSString};
use objc2_user_notifications::{
    UNNotification, UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};
use zerus_client_core::{desktop::notifications::macos::Hold, macos_notify::Envelope};

pub struct Ivars {
    input: mpsc::SyncSender<Envelope>,
    failed: Arc<AtomicBool>,
    _native: Hold,
    _owner: Arc<File>,
}
define_class!(
    // SAFETY: NSObject has no subclassing requirements and there is no custom
    // Drop. Native deallocation releases the retained callback/owner holds.
    #[unsafe(super(NSObject))]
    #[name = "ZerusServiceAttentionDelegate"]
    #[ivars = Ivars]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _: &UNUserNotificationCenter,
            _: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            crate::macos_notification_diagnostics::response(
                crate::macos_notification_diagnostics::Role::Host,
                &response.actionIdentifier(),
            );
            if let Some(envelope) = envelope(response) {
                if self.ivars().input.try_send(envelope).is_err() {
                    self.ivars().failed.store(true, Ordering::Release);
                }
            }
            completion.call(());
        }
    }
);
impl Delegate {
    fn new(ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Compare only bounded native strings and the original UUID. A legacy token
/// without correlation remains unavailable; no Binding or delivered row is
/// reconstructed here. The service actor resolves its own original ledger row.
fn envelope(response: &UNNotificationResponse) -> Option<Envelope> {
    if !response
        .actionIdentifier()
        .isEqualToString(unsafe { UNNotificationDefaultActionIdentifier })
    {
        return None;
    }
    let request = response.notification().request();
    let info = request.content().userInfo();
    if info.count() != 2 {
        return None;
    }
    let token_key: &AnyObject = ns_string!("hgsAttentionToken");
    let correlation_key: &AnyObject = ns_string!("hgsAttentionCorrelationV1");
    let token = info.objectForKey(token_key)?.downcast::<NSString>().ok()?;
    let correlation = info
        .objectForKey(correlation_key)?
        .downcast::<NSString>()
        .ok()?;
    if token.length() > 8192 || correlation.length() != 36 || request.identifier().length() > 8224 {
        return None;
    }
    let envelope = Envelope::new(&token.to_string(), &correlation.to_string()).ok()?;
    (request.identifier().to_string() == envelope.request_id()).then_some(envelope)
}

pub(super) struct Installed {
    delegate: Option<Retained<Delegate>>,
    input: mpsc::Receiver<Envelope>,
    failed: Arc<AtomicBool>,
}
impl Installed {
    pub(super) fn new(native: Hold, owner: Arc<File>) -> Self {
        let (output, input) = mpsc::sync_channel(32);
        let failed = Arc::new(AtomicBool::new(false));
        let delegate = Delegate::new(Ivars {
            input: output,
            failed: failed.clone(),
            _native: native,
            _owner: owner,
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        Self {
            delegate: Some(delegate),
            input,
            failed,
        }
    }
    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    pub(super) fn next(&self) -> Option<Envelope> {
        self.input.try_recv().ok()
    }
}
impl Drop for Installed {
    fn drop(&mut self) {
        UNUserNotificationCenter::currentNotificationCenter().setDelegate(None);
        drop(self.delegate.take());
        // Native in-flight references, if any, retain the same Ivars holds.
    }
}
