mod wayland;

use crate::{Idle, IdleSource, Sample, MAX_IDLE};
use std::time::{Duration, Instant};
use zbus::blocking::{connection::Builder, Connection, Proxy};
use zbus::proxy::MethodFlags;

const CALL_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_POLL_GAP: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct LinuxSampler {
    bus: Option<Connection>,
    wayland: Option<wayland::InputIdle>,
    last_poll: Option<Instant>,
}

impl LinuxSampler {
    pub fn sample(&mut self) -> Sample {
        let now = Instant::now();
        if self
            .last_poll
            .is_some_and(|last| now.duration_since(last) > MAX_POLL_GAP)
        {
            // Do not replay queued, arbitrarily old resumed events as new input.
            self.wayland = None;
        }
        self.last_poll = Some(now);
        if self.bus.is_none() {
            self.bus = Builder::session()
                .ok()
                .and_then(|b| b.method_timeout(CALL_TIMEOUT).build().ok());
        }
        let is_wayland = wayland_session(
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()),
        );
        let mut idle = None;
        if is_wayland {
            if self.wayland.is_none() {
                self.wayland = wayland::InputIdle::connect().ok();
            }
            if let Some(source) = &mut self.wayland {
                match source.sample() {
                    Ok(value) => idle = value,
                    Err(()) => self.wayland = None,
                }
            }
        } else {
            self.wayland = None;
        }
        self.sample_bus(idle, is_wayland)
    }

    fn sample_bus(&mut self, mut idle: Option<Idle>, is_wayland: bool) -> Sample {
        let Some(bus) = &self.bus else {
            return Sample { idle, locked: None };
        };
        let locked = screen_locked(bus);
        if idle.is_none() {
            idle = dbus_idle(bus, is_wayland);
        }
        // A disconnected bus is retried on the next call; no old lock/idle value
        // is cached as if it were a fresh observation.
        if bus.is_closed() {
            self.bus = None;
        }
        Sample { idle, locked }
    }
}

fn wayland_session(session_type: Option<&str>, display: bool) -> bool {
    session_type.is_some_and(|v| v.eq_ignore_ascii_case("wayland")) || display
}

fn proxy<'a>(
    bus: &'a Connection,
    name: &'a str,
    path: &'a str,
    interface: &'a str,
) -> Option<Proxy<'a>> {
    Proxy::new(bus, name, path, interface).ok()
}

fn screen_locked(bus: &Connection) -> Option<bool> {
    [
        (
            "org.freedesktop.ScreenSaver",
            "/org/freedesktop/ScreenSaver",
            "org.freedesktop.ScreenSaver",
        ),
        (
            "org.gnome.ScreenSaver",
            "/org/gnome/ScreenSaver",
            "org.gnome.ScreenSaver",
        ),
    ]
    .into_iter()
    .find_map(|(name, path, interface)| {
        proxy(bus, name, path, interface)?
            .call_with_flags("GetActive", MethodFlags::NoAutoStart.into(), &())
            .ok()
            .flatten()
    })
}

fn dbus_idle(bus: &Connection, is_wayland: bool) -> Option<Idle> {
    let mutter: Option<u64> = proxy(
        bus,
        "org.gnome.Mutter.IdleMonitor",
        "/org/gnome/Mutter/IdleMonitor/Core",
        "org.gnome.Mutter.IdleMonitor",
    )?
    .call_with_flags("GetIdletime", MethodFlags::NoAutoStart.into(), &())
    .ok()
    .flatten();
    if let Some(value) = mutter {
        return milliseconds(value).map(|elapsed| Idle {
            elapsed,
            source: IdleSource::Mutter,
            since_input: true,
        });
    }
    // KDE's method is milliseconds, not the seconds some other screensavers
    // return. It is denied on KDE Wayland; never interpret a Wayland/Xwayland
    // fallback's zero as activity. GNOME is handled by Mutter above.
    if is_wayland || !kde_screensaver(bus) {
        return None;
    }
    let value: u32 = proxy(
        bus,
        "org.freedesktop.ScreenSaver",
        "/org/freedesktop/ScreenSaver",
        "org.freedesktop.ScreenSaver",
    )?
    .call_with_flags("GetSessionIdleTime", MethodFlags::NoAutoStart.into(), &())
    .ok()
    .flatten()?;
    milliseconds(u64::from(value)).map(|elapsed| Idle {
        elapsed,
        source: IdleSource::KdeScreenSaver,
        since_input: true,
    })
}

fn kde_screensaver(bus: &Connection) -> bool {
    proxy(
        bus,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .and_then(|p| {
        p.call::<_, _, bool>("NameHasOwner", &("org.kde.screensaver",))
            .ok()
    }) == Some(true)
}

fn milliseconds(value: u64) -> Option<Duration> {
    let duration = Duration::from_millis(value);
    (duration <= MAX_IDLE).then_some(duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    struct Mutter(Option<u64>);
    #[zbus::interface(name = "org.gnome.Mutter.IdleMonitor")]
    impl Mutter {
        fn get_idletime(&self) -> zbus::fdo::Result<u64> {
            self.0
                .ok_or_else(|| zbus::fdo::Error::UnknownMethod("Unavailable".into()))
        }
    }

    struct ScreenSaver {
        idle: u32,
        locked: bool,
    }
    #[zbus::interface(name = "org.freedesktop.ScreenSaver")]
    impl ScreenSaver {
        fn get_active(&self) -> bool {
            self.locked
        }
        fn get_session_idle_time(&self) -> u32 {
            self.idle
        }
    }

    struct Names;
    #[zbus::interface(name = "org.freedesktop.DBus")]
    impl Names {
        fn name_has_owner(&self, name: &str) -> bool {
            name == "org.kde.screensaver"
        }
    }

    // Private peer connection, not the real desktop/session bus. Exercises the
    // actual D-Bus method names, serialization and error/fallback behavior.
    fn desktop(mutter: Option<u64>, locked: bool) -> (Connection, Connection) {
        let (server, client) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            Builder::async_io_unix_stream(server)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .serve_at("/org/gnome/Mutter/IdleMonitor/Core", Mutter(mutter))
                .unwrap()
                .serve_at(
                    "/org/freedesktop/ScreenSaver",
                    ScreenSaver { idle: 4321, locked },
                )
                .unwrap()
                .serve_at("/org/freedesktop/DBus", Names)
                .unwrap()
                .build()
                .unwrap()
        });
        let client = Builder::async_io_unix_stream(client)
            .p2p()
            .method_timeout(CALL_TIMEOUT)
            .build()
            .unwrap();
        (client, server.join().unwrap())
    }

    #[test]
    fn gnome_methods_return_independent_idle_and_lock_samples() {
        let (bus, _server) = desktop(Some(12345), false);
        assert_eq!(screen_locked(&bus), Some(false));
        assert_eq!(
            dbus_idle(&bus, true),
            Some(Idle {
                elapsed: Duration::from_millis(12345),
                source: IdleSource::Mutter,
                since_input: true,
            })
        );
    }

    #[test]
    fn kde_x11_fallback_is_refused_on_wayland_even_if_it_returns_zero_or_data() {
        let (bus, _server) = desktop(None, true);
        assert_eq!(screen_locked(&bus), Some(true));
        assert_eq!(
            dbus_idle(&bus, false),
            Some(Idle {
                elapsed: Duration::from_millis(4321),
                source: IdleSource::KdeScreenSaver,
                since_input: true,
            })
        );
        assert_eq!(dbus_idle(&bus, true), None);
    }

    #[test]
    fn missing_screensaver_does_not_discard_a_healthy_bus() {
        let (bus, server) = desktop(None, false);
        server
            .object_server()
            .remove::<ScreenSaver, _>("/org/freedesktop/ScreenSaver")
            .unwrap();
        let mut sampler = LinuxSampler {
            bus: Some(bus),
            ..LinuxSampler::default()
        };
        for _ in 0..3 {
            assert_eq!(sampler.sample_bus(None, true), Sample::default());
            assert!(
                sampler.bus.is_some(),
                "unsupported method is not a transport failure"
            );
        }
        server.close().unwrap();
        for _ in 0..100 {
            if sampler.bus.as_ref().unwrap().is_closed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(sampler.sample_bus(None, true), Sample::default());
        assert!(sampler.bus.is_none(), "a disconnected bus must be retried");
    }

    #[test]
    fn xwayland_is_never_selected_as_the_real_input_idle_source() {
        assert!(wayland_session(Some("wayland"), false));
        assert!(wayland_session(Some("x11"), true));
        assert!(!wayland_session(Some("x11"), false));
        assert!(!wayland_session(None, false));
    }

    #[test]
    fn dbus_idle_is_milliseconds_and_rejects_invalid_ranges() {
        assert_eq!(milliseconds(1234), Some(Duration::from_millis(1234)));
        assert_eq!(milliseconds(0), Some(Duration::ZERO));
        assert_eq!(milliseconds(u64::MAX), None);
    }
}
