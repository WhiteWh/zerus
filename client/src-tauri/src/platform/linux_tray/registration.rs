//! UI-only registration, polled by the existing tray worker while its bounded
//! raw badge receivers remain active. No badge scope/charge is captured here.
use super::*;
use std::future::Future;

pub(super) type Task = Pin<Box<dyn Future<Output = zbus::Result<Option<String>>> + Send>>;

pub(super) fn start(connection: zbus::Connection, name: String, registered: String) -> Task {
    Box::pin(async move {
        let dbus = zbus::fdo::DBusProxy::new(&connection).await?;
        let owner = match dbus.get_name_owner(WATCHER.try_into().unwrap()).await {
            Ok(owner) => owner.to_string(),
            Err(zbus::fdo::Error::NameHasNoOwner(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if owner != registered {
            let watcher = zbus::Proxy::new(
                &connection,
                owner.as_str(),
                "/StatusNotifierWatcher",
                WATCHER,
            )
            .await?;
            watcher
                .call::<_, _, ()>("RegisterStatusNotifierItem", &(name.as_str(),))
                .await?;
        }
        Ok(Some(owner))
    })
}

#[cfg(test)]
mod tests;
