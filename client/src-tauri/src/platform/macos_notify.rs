//! Attention notifications under the app's identity, as Qt posts them
//! (`tray/src/MacAttentionNotifier.mm`): `UNUserNotificationCenter` gets one
//! request per token (`zerus.attention.<token>`) with the default sound,
//! withdrawn from pending and delivered when the attention clears; the
//! default action opens the token's session. The decisions live in
//! `notifier::Notifier` and the core's `attention::Delivery`; this module
//! only talks to the system. It also tells the desk whether Zerus is the
//! active app, the tracker's foreground test (`AttentionTracker.cpp:44-48`).
use std::ptr::NonNull;
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSApplicationDidBecomeActiveNotification,
    NSApplicationDidResignActiveNotification,
};
use objc2_foundation::{ns_string, NSNotification, NSNotificationCenter, NSString};
use objc2_user_notifications::{
    UNNotification, UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use tauri::{AppHandle, Manager};
use zerus_client_core::attention::Notice;

use crate::desk::DeskInput;
use crate::notifier::Notifier;

mod center;
use center::System;

pub(crate) mod observers;
mod owned_request;

pub struct Ivars {
    app: AppHandle,
    desk: crate::desk::InputSender,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Delegate has no Drop.
    #[unsafe(super(NSObject))]
    #[name = "ZerusAttentionDelegate"]
    #[ivars = Ivars]
    pub struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// Banners also while Zerus is in front (`MacAttentionNotifier.mm:14-19`).
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        /// The default action activates Zerus and hands the token to the
        /// desk, which decodes it off the main thread (`MacAttentionNotifier.mm:20-28`,
        /// `:38-43`; `TrayAgent.cpp:176-177`).
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            crate::macos_notification_diagnostics::response(
                crate::macos_notification_diagnostics::Role::Gui,
                &response.actionIdentifier(),
            );
            if let Some(token) = clicked_token(response) {
                let _ = self.ivars().app.run_on_main_thread(activate);
                if zerus_client_core::updates::is_update_token(&token) {
                    if crate::updates::notification_current(&self.ivars().app, &token) {
                        crate::updates::open_page(&self.ivars().app);
                    }
                } else {
                    let _ = self.ivars().desk.send(DeskInput::Clicked(token));
                }
            }
            completion.call(());
        }
    }
);

impl Delegate {
    fn new(app: AppHandle, desk: crate::desk::InputSender) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { app, desk });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// `[NSApp activateIgnoringOtherApps:YES]`, as Qt's click handler does.
fn activate() {
    if let Some(mtm) = MainThreadMarker::new() {
        // The call Qt makes; `activate` alone may yield to the app in front.
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

/// The token of a default action, when its `userInfo` holds one as a string.
fn clicked_token(response: &UNNotificationResponse) -> Option<String> {
    // SAFETY: the framework's constant action identifier.
    let default = unsafe { UNNotificationDefaultActionIdentifier };
    if !response.actionIdentifier().isEqualToString(default) {
        return None;
    }
    let info = response.notification().request().content().userInfo();
    let key: &AnyObject = ns_string!("hgsAttentionToken");
    let value = info.objectForKey(key)?;
    value
        .downcast::<NSString>()
        .ok()
        .map(|token| token.to_string())
}

/// The notifier, in the app state.
pub struct NotifierState(Notifier);

/// Sets the delegate before the first notification posts and starts
/// following whether Zerus is the active app. Call on the main thread, from
/// setup: a click that launched the app reaches the delegate once set.
pub fn install(app: &AppHandle, desk: crate::desk::InputSender) {
    let delegate = Delegate::new(app.clone(), desk.clone());
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The center keeps a weak reference: the delegate lives as long as the app.
    std::mem::forget(delegate);
    let main = app.clone();
    let notifier = Notifier::new(
        Box::new(System),
        Arc::new(move |work| {
            let _ = main.run_on_main_thread(work);
        }),
        Box::new(move |token, detail| {
            let _ = desk.send(DeskInput::Failed { token, detail });
        }),
    );
    app.manage(NotifierState(notifier));
    follow_activity(app);
}

/// Withdraws, then posts, on the main thread (`TrayAgent::observeAttention`).
pub fn deliver(app: &AppHandle, cleared: Vec<String>, raised: Vec<Notice>) {
    if let Some(state) = app.try_state::<NotifierState>() {
        state.0.deliver(&cleared, &raised);
    }
}

/// Qt's foreground test is `QGuiApplication::applicationState() ==
/// Qt::ApplicationActive` (`AttentionTracker.cpp:44-48`): the app's active
/// state, followed from AppKit's notifications.
fn follow_activity(app: &AppHandle) {
    if let Some(mtm) = MainThreadMarker::new() {
        crate::desk::set_focused(app, NSApplication::sharedApplication(mtm).isActive());
    }
    let became = app.clone();
    let active = RcBlock::new(move |_note: NonNull<NSNotification>| {
        crate::desk::set_focused(&became, true);
    });
    let resigned = app.clone();
    let inactive = RcBlock::new(move |_note: NonNull<NSNotification>| {
        crate::desk::set_focused(&resigned, false);
    });
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: AppKit's constant notification names; no object filter; the
    // blocks run on the posting (main) thread and capture only Send state.
    unsafe {
        let observers = [
            center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidBecomeActiveNotification),
                None,
                None,
                &active,
            ),
            center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidResignActiveNotification),
                None,
                None,
                &inactive,
            ),
        ];
        // Observed for the life of the app.
        std::mem::forget(observers);
    }
}
