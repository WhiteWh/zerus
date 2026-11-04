//! WebKit's native navigation metadata is required here: Tauri's generic
//! navigation callbacks do not establish that an external link was clicked.
use super::{Launch, Navigation};
use gtk::glib::prelude::*;
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Url;
use webkit2gtk::{
    LoadEvent, NavigationPolicyDecision, NavigationPolicyDecisionExt, NavigationType,
    PermissionRequestExt, PolicyDecisionExt, PolicyDecisionType, ResponsePolicyDecision,
    ResponsePolicyDecisionExt, SettingsExt, URIRequestExt, URIResponseExt, WebContextExt,
    WebViewExt,
};

const CONTAINER: &str = "zerus-native-workspace-overlay";

#[cfg(test)]
mod tests;

/// Tauri 2's Linux child builder packs into a GtkBox, where Wry ignores
/// bounds. Keep the primary webview as the overlay's sizing child and put
/// native workspaces above it. An overlay never contributes its children's
/// minimum size to the app's layout.
pub(crate) fn mount(main: &tauri::WebviewWindow, child: &tauri::Webview) -> Result<(), ()> {
    let prepared = Arc::new(AtomicBool::new(false));
    let result = prepared.clone();
    main.with_webview(move |platform| {
        let primary = platform.inner();
        let Some(parent) = primary.parent() else {
            return;
        };
        if parent
            .downcast_ref::<gtk::Overlay>()
            .is_some_and(|overlay| {
                overlay.widget_name() == CONTAINER
                    && overlay.child().as_ref() == Some(primary.upcast_ref())
            })
        {
            result.store(true, Ordering::Release);
            return;
        }
        let Some(container) = parent.downcast_ref::<gtk::Box>() else {
            return;
        };
        let overlay = gtk::Overlay::new();
        overlay.set_widget_name(CONTAINER);
        overlay.connect_get_child_position(|overlay, widget| {
            // WebKit's natural size follows its previous viewport, so
            // GtkOverlay's natural-size default cannot bound a child.
            // GTK subtracts these margins from the returned allocation
            // when creating the overlay's native clipping window.
            let (width, height) = widget.size_request();
            let x = widget.margin_start();
            let y = widget.margin_top();
            Some(gtk::gdk::Rectangle::new(
                0,
                0,
                (x + width.max(1)).min(overlay.allocated_width().max(1)),
                (y + height.max(1)).min(overlay.allocated_height().max(1)),
            ))
        });
        container.remove(&primary);
        overlay.add(&primary);
        container.pack_start(&overlay, true, true, 0);
        overlay.show();
        result.store(true, Ordering::Release);
    })
    .map_err(|_| ())?;
    if !prepared.load(Ordering::Acquire) {
        return Err(());
    }
    let mounted = Arc::new(AtomicBool::new(false));
    let result = mounted.clone();
    child
        .with_webview(move |platform| {
            let view = platform.inner();
            let Some(parent) = view.parent() else { return };
            let Some(container) = parent.downcast_ref::<gtk::Box>() else {
                return;
            };
            let overlays: Vec<_> = container
                .children()
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Overlay>().ok())
                .filter(|overlay| overlay.widget_name() == CONTAINER)
                .collect();
            let [overlay] = overlays.as_slice() else {
                return;
            };
            view.hide();
            container.remove(&view);
            view.set_halign(gtk::Align::Start);
            view.set_valign(gtk::Align::Start);
            view.set_hexpand(false);
            view.set_vexpand(false);
            view.set_size_request(1, 1);
            overlay.add_overlay(&view);
            result.store(true, Ordering::Release);
        })
        .map_err(|_| ())?;
    mounted.load(Ordering::Acquire).then_some(()).ok_or(())
}

/// CSS pixels and GTK allocations are logical pixels on this platform.
pub(crate) fn place(
    child: &tauri::Webview,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), ()> {
    let placed = Arc::new(AtomicBool::new(false));
    let result = placed.clone();
    child
        .with_webview(move |platform| {
            let view = platform.inner();
            if !view.parent().is_some_and(|parent| {
                parent
                    .downcast_ref::<gtk::Overlay>()
                    .is_some_and(|overlay| overlay.widget_name() == CONTAINER)
            }) {
                return;
            }
            view.set_margin_start(x.round() as i32);
            view.set_margin_top(y.round() as i32);
            view.set_size_request(width.round() as i32, height.round() as i32);
            result.store(true, Ordering::Release);
        })
        .map_err(|_| ())?;
    placed.load(Ordering::Acquire).then_some(()).ok_or(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Load {
    Loading,
    Ready,
    Failed,
    Stopped,
}

pub(crate) struct Hooks {
    pub load: Arc<dyn Fn(Load) + Send + Sync>,
    pub browser: Arc<dyn Fn(Url) + Send + Sync>,
}

/// Install on GTK's main thread before navigating. Refuse a non-ephemeral
/// context rather than sharing the app's cookies or an older launch profile.
pub(crate) fn install(
    view: &webkit2gtk::WebView,
    launch: Launch,
    hooks: Hooks,
) -> Result<(), &'static str> {
    if !view
        .web_context()
        .is_some_and(|context| context.is_ephemeral())
    {
        return Err("The native workspace is not isolated.");
    }
    let settings = WebViewExt::settings(view).ok_or("Native UI settings are unavailable.")?;
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_allow_universal_access_from_file_urls(false);
    settings.set_enable_developer_extras(false);
    settings.set_enable_write_console_messages_to_stdout(false);
    // Wry's earlier decide-policy handler answers NavigationAction before
    // later handlers can inspect its native gesture/type metadata. Replace
    // that one instance handler on this owned about:blank child only. The
    // main view is never changed. An unexpected handler graph refuses the
    // child rather than silently bypassing a new framework policy.
    let signal = gtk::glib::subclass::SignalId::lookup("decide-policy", view.type_())
        .ok_or("Native UI navigation guards are unavailable.")?;
    let signal = signal.into_glib();
    // SAFETY: `view` is a live WebKit GObject on GTK's main thread, signal is
    // looked up for its exact type, and MATCH_ID ignores all null arguments.
    // No callbacks or object pointers escape the call. GLib's class handler
    // is not an instance connection and remains installed.
    let blocked = unsafe {
        let handler = gtk::glib::gobject_ffi::g_signal_handler_find(
            view.as_ptr().cast(),
            gtk::glib::gobject_ffi::G_SIGNAL_MATCH_ID,
            signal,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if handler == 0 {
            return Err("Native UI navigation guards are unavailable.");
        }
        gtk::glib::gobject_ffi::g_signal_handlers_block_matched(
            view.as_ptr().cast(),
            gtk::glib::gobject_ffi::G_SIGNAL_MATCH_ID,
            signal,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if blocked != 1 {
        return Err("Native UI navigation guards are unavailable.");
    }
    // window.open reaches WebKit's create signal before a navigation
    // decision. Wry's Deny handler returns None, so this later handler can
    // inspect the native action too, while still returning no new webview.
    // A target=_blank navigation which we ignore below never reaches create.
    let popup_launch = launch.clone();
    let popup_browser = hooks.browser.clone();
    view.connect_create(move |view, action| {
        let mut action = action.clone();
        let next = action
            .request()
            .and_then(|request| request.uri())
            .and_then(|uri| Url::parse(&uri).ok());
        if let Some(next) = next {
            match popup_launch.new_window(&next, action.is_user_gesture() && !action.is_redirect())
            {
                Navigation::Allow => view.load_uri(next.as_str()),
                Navigation::Browser => popup_browser(next),
                Navigation::Deny => {}
            }
        }
        None
    });
    let failed = Arc::new(AtomicBool::new(false));
    let had_document = Arc::new(AtomicBool::new(false));
    let failed_response = failed.clone();
    let document_navigation = had_document.clone();
    let response_load = hooks.load.clone();
    view.connect_decide_policy(move |view, decision, kind| {
        if kind == PolicyDecisionType::Response {
            if let Some(response) = decision.downcast_ref::<ResponsePolicyDecision>() {
                let same_origin = response
                    .response()
                    .and_then(|reply| reply.uri())
                    .and_then(|uri| Url::parse(&uri).ok())
                    .is_some_and(|url| launch.navigation(&url, false) == Navigation::Allow);
                if !same_origin
                    || (response.is_main_frame_main_resource()
                        && response
                            .response()
                            .is_none_or(|reply| reply.status_code() >= 400))
                {
                    failed_response.store(true, Ordering::Relaxed);
                    response_load(Load::Failed);
                    decision.ignore();
                    return true;
                }
                let attachment = response
                    .response()
                    .and_then(|reply| reply.http_headers())
                    .and_then(|headers| headers.one("Content-Disposition"))
                    .is_some_and(|value| {
                        value
                            .split(';')
                            .next()
                            .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("attachment"))
                    });
                if response.is_mime_type_supported() && !attachment {
                    decision.use_();
                } else {
                    // WebKit's download request still has to pass our
                    // exact-target guarded Save dialog before any write.
                    decision.download();
                }
            } else {
                decision.ignore();
            }
            return true;
        }
        if !matches!(
            kind,
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction
        ) {
            decision.ignore();
            return true;
        }
        let Some(mut action) = decision
            .downcast_ref::<NavigationPolicyDecision>()
            .and_then(|decision| decision.navigation_action())
        else {
            decision.ignore();
            return true;
        };
        let Some(next) = action
            .request()
            .and_then(|request| request.uri())
            .and_then(|uri| Url::parse(&uri).ok())
        else {
            decision.ignore();
            return true;
        };
        let popup = kind == PolicyDecisionType::NewWindowAction;
        // Qt opens only NavigationTypeLinkClicked in the system browser.
        // A script executing during a user gesture is still not a link.
        let clicked = action.is_user_gesture()
            && action.navigation_type() == NavigationType::LinkClicked
            && !action.is_redirect();
        let destination = if popup {
            launch.new_window(&next, action.is_user_gesture())
        } else {
            launch.navigation(&next, clicked)
        };
        match destination {
            Navigation::Allow if !popup => decision.use_(),
            Navigation::Allow => {
                decision.ignore();
                view.load_uri(next.as_str());
            }
            Navigation::Browser => {
                decision.ignore();
                (hooks.browser)(next);
            }
            Navigation::Deny => {
                decision.ignore();
                // Refusing a popup never invalidates the workspace which
                // requested it, including during its first load.
                if !popup {
                    if !document_navigation.load(Ordering::Relaxed) {
                        failed_response.store(true, Ordering::Relaxed);
                        response_load(Load::Failed);
                    } else {
                        response_load(Load::Ready);
                    }
                }
            }
        }
        true
    });
    let load = hooks.load.clone();
    let failed_load = failed.clone();
    view.connect_load_changed(move |view, event| match event {
        _ if view.uri().is_some_and(|uri| uri.as_str() == "about:blank") => {}
        LoadEvent::Started => {
            failed_load.store(false, Ordering::Relaxed);
            load(Load::Loading);
        }
        LoadEvent::Finished if !failed_load.load(Ordering::Relaxed) => {
            had_document.store(true, Ordering::Relaxed);
            load(Load::Ready);
        }
        _ => {}
    });
    let load = hooks.load.clone();
    let stopped = failed.clone();
    view.connect_load_failed(move |_, _, _, error| {
        // A refused external navigation or an explicit reload keeps the
        // workspace it already had. Error bodies may contain launch tokens.
        if error.matches(webkit2gtk::NetworkError::Cancelled)
            || error.matches(webkit2gtk::PolicyError::FrameLoadInterruptedByPolicyChange)
        {
            return true;
        }
        failed.store(true, Ordering::Relaxed);
        load(Load::Failed);
        true
    });
    view.connect_web_process_terminated(move |_, _| {
        stopped.store(true, Ordering::Relaxed);
        (hooks.load)(Load::Stopped);
    });
    view.connect_permission_request(|_, request| {
        request.deny();
        true
    });
    Ok(())
}
