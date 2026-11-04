//! Isolated compositor protocol fixture; never connects to the user's display.
use super::*;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use wayland_protocols::ext::idle_notify::v1::server::{
    ext_idle_notification_v1 as notification, ext_idle_notifier_v1 as notifier,
};
use wayland_server::protocol::wl_seat as seat;
use wayland_server::{
    Client, DataInit, Dispatch as ServerDispatch, Display, DisplayHandle, GlobalDispatch, New,
};

#[derive(Default)]
struct Compositor {
    input_notification: Option<notification::ExtIdleNotificationV1>,
}

impl GlobalDispatch<seat::WlSeat, ()> for Compositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<seat::WlSeat>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl GlobalDispatch<notifier::ExtIdleNotifierV1, ()> for Compositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<notifier::ExtIdleNotifierV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl ServerDispatch<notifier::ExtIdleNotifierV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &notifier::ExtIdleNotifierV1,
        request: notifier::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            notifier::Request::GetInputIdleNotification { id, timeout, .. } => {
                assert_eq!(timeout, 1000);
                assert!(state.input_notification.is_none());
                state.input_notification = Some(init.init(id, ()));
            }
            notifier::Request::Destroy => {}
            _ => panic!("sampler must not use inhibitor-sensitive idle"),
        }
    }
}

impl ServerDispatch<seat::WlSeat, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &seat::WlSeat,
        _: seat::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
impl ServerDispatch<notification::ExtIdleNotificationV1, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &notification::ExtIdleNotificationV1,
        _: notification::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

fn pair(version: u32) -> (InputIdle, Display<Compositor>, Compositor) {
    let display = Display::<Compositor>::new().unwrap();
    let mut handle = display.handle();
    handle.create_global::<Compositor, seat::WlSeat, _>(1, ());
    handle.create_global::<Compositor, notifier::ExtIdleNotifierV1, _>(version, ());
    let (server, client) = UnixStream::pair().unwrap();
    handle.insert_client(server, Arc::new(())).unwrap();
    let connection = Connection::from_socket(client).unwrap();
    let queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    (
        InputIdle {
            connection,
            queue,
            state: State::default(),
        },
        display,
        Compositor::default(),
    )
}

fn pump(
    client: &mut InputIdle,
    display: &mut Display<Compositor>,
    state: &mut Compositor,
) -> Option<Idle> {
    display.dispatch_clients(state).unwrap();
    display.flush_clients().unwrap();
    client.sample().unwrap()
}

#[test]
fn kde_input_protocol_startup_idle_resume_and_liveness() {
    let (mut client, mut display, mut compositor) = pair(2);
    for _ in 0..20 {
        assert_eq!(pump(&mut client, &mut display, &mut compositor), None);
    }
    let notification = compositor
        .input_notification
        .clone()
        .expect("v2 input subscription");
    notification.idled();
    let idle = pump(&mut client, &mut display, &mut compositor).unwrap();
    assert!(idle.elapsed >= THRESHOLD);
    assert!(!idle.since_input, "startup lower bound is not recent input");
    assert!(idle.elapsed < Duration::from_secs(2));
    notification.resumed();
    assert_eq!(
        pump(&mut client, &mut display, &mut compositor),
        Some(estimate(Duration::ZERO, true))
    );
    notification.idled();
    assert!(
        pump(&mut client, &mut display, &mut compositor)
            .unwrap()
            .since_input
    );
    // A stalled compositor cannot keep reporting a fresh active seat forever.
    client.state.last_sync = Some(Instant::now() - SYNC_TIMEOUT - Duration::from_secs(1));
    assert_eq!(client.state.sample(Instant::now()), None);
    client.state.invalid = true;
    assert_eq!(client.sample(), Err(()));
}

#[test]
fn version_one_cannot_turn_an_idle_inhibitor_into_user_activity() {
    let (mut client, mut display, mut compositor) = pair(1);
    for _ in 0..20 {
        assert_eq!(pump(&mut client, &mut display, &mut compositor), None);
    }
    assert!(compositor.input_notification.is_none());
}

#[test]
fn reconnecting_while_away_does_not_report_recent_input() {
    for _ in 0..2 {
        let (mut client, mut display, mut compositor) = pair(2);
        for _ in 0..20 {
            assert_eq!(pump(&mut client, &mut display, &mut compositor), None);
        }
        compositor.input_notification.as_ref().unwrap().idled();
        let idle = pump(&mut client, &mut display, &mut compositor).unwrap();
        assert!(idle.elapsed < Duration::from_secs(60));
        assert!(
            !idle.since_input,
            "an away-threshold writer cannot call this presence"
        );
    }
}
