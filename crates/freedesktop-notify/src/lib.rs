//! The existing Zerus Freedesktop wire protocol, independent of its UI or actor.
//!
//! Callers supply their connection and pinned unique daemon name. They retain
//! notification ownership, durable reservations, ID mapping, retries and worker
//! lifetime. This module creates no connection, thread, store or notification
//! consumer. Bodies are already escaped markup, as in the existing UI adapter.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use zbus::blocking::{Connection, Proxy};
use zbus::names::UniqueName;
use zbus::zvariant::Value;

pub const SERVICE: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";

type Parameters<'a> = (
    &'static str,
    u32,
    &'static str,
    &'a str,
    &'a str,
    Vec<&'static str>,
    BTreeMap<&'static str, Value<'static>>,
    i32,
);
fn parameters<'a>(title: &'a str, body_markup: &'a str) -> Parameters<'a> {
    (
        "Zerus",
        0,
        "hgs-zerus-swarm-symbolic",
        title,
        body_markup,
        vec!["default", "Open session", "open-session", "Open session"],
        BTreeMap::from([
            ("desktop-entry", Value::from("hgs-tray")),
            ("urgency", Value::from(1_u8)),
            ("category", Value::from("im.received")),
        ]),
        -1,
    )
}

/// Issue one call to the selected daemon. A zero ID or an error is returned
/// unchanged: only the caller's original reservation can settle its outcome.
pub fn notify(
    connection: &Connection,
    owner: UniqueName<'_>,
    title: &str,
    body_markup: &str,
) -> zbus::Result<u32> {
    let proxy = Proxy::new(connection, owner, PATH, SERVICE)?;
    proxy.call("Notify", &parameters(title, body_markup))
}

/// Close only on the supplied unique daemon. Namespace/currentness checks and
/// the disposition of an uncertain reply belong to the original caller.
pub fn close_notification(
    connection: &Connection,
    owner: UniqueName<'_>,
    id: u32,
) -> zbus::Result<()> {
    Proxy::new(connection, owner, PATH, SERVICE)?.call("CloseNotification", &(id,))
}

/// The same wire call for a caller that drives its original connection's
/// executor itself. The raw reply preserves its sender for the caller to
/// validate before decoding. No socket, executor, task or retry is created here.
pub async fn notify_async(
    connection: &zbus::Connection,
    owner: UniqueName<'_>,
    title: &str,
    body_markup: &str,
) -> zbus::Result<zbus::Message> {
    connection
        .call_method(
            Some(owner),
            PATH,
            Some(SERVICE),
            "Notify",
            &parameters(title, body_markup),
        )
        .await
}

/// Async form of CloseNotification, with the same caller-owned lifetime and
/// namespace requirements as the blocking form.
pub async fn close_notification_async(
    connection: &zbus::Connection,
    owner: UniqueName<'_>,
    id: u32,
) -> zbus::Result<zbus::Message> {
    connection
        .call_method(
            Some(owner),
            PATH,
            Some(SERVICE),
            "CloseNotification",
            &(id,),
        )
        .await
}

#[derive(Debug, PartialEq, Eq)]
pub enum Signal {
    ActivationToken { id: u32, token: String },
    ActionInvoked { id: u32, action: String },
    NotificationClosed { id: u32, reason: u32 },
}

/// Decode only this daemon's protocol signals. Numeric IDs remain scoped to
/// the caller's bus/owner mapping; decoding grants no activation permission.
pub fn decode_signal(message: &zbus::Message, owner: &UniqueName<'_>) -> Option<Signal> {
    let header = message.header();
    if header.message_type() != zbus::message::Type::Signal
        || header.sender().map(|sender| sender.as_str()) != Some(owner.as_str())
        || header.interface().map(|interface| interface.as_str()) != Some(SERVICE)
        || header.path().map(|path| path.as_str()) != Some(PATH)
    {
        return None;
    }
    match header.member()?.as_str() {
        "ActivationToken" => {
            let (id, token) = message.body().deserialize::<(u32, String)>().ok()?;
            Some(Signal::ActivationToken { id, token })
        }
        "ActionInvoked" => {
            let (id, action) = message.body().deserialize::<(u32, String)>().ok()?;
            Some(Signal::ActionInvoked { id, action })
        }
        "NotificationClosed" => {
            let (id, reason) = message.body().deserialize::<(u32, u32)>().ok()?;
            Some(Signal::NotificationClosed { id, reason })
        }
        _ => None,
    }
}

// The original workers include this source privately. The shared crate also
// runs its socket/auth/frame controls in the ordinary private test job.
#[cfg(test)]
mod ingress;
