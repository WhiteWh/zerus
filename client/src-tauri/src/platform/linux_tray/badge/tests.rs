use super::*;
use zerus_client_core::desktop::drain::{Gate, Observation};

fn scope() -> (Gate, Arc<Scope>, Control, Completion) {
    let gate = Gate::new(1).unwrap();
    let (scope, control, startup) = Scope::new(gate.admit(Role::Badges).unwrap()).unwrap();
    (gate, scope, control, startup)
}
fn query_message(client: &Connection) -> zbus::Message {
    zbus::Message::method_call(BADGE_PATH, "Query")
        .unwrap()
        .interface(BADGE)
        .unwrap()
        .sender(client.unique_name().unwrap())
        .unwrap()
        .build(&())
        .unwrap()
}
fn dispatch<'a>(
    service: &'a Service,
    server: &'a Connection,
    message: &'a zbus::Message,
) -> zbus::object_server::DispatchResult2<'a> {
    zbus::object_server::Interface::call(
        service,
        server.inner().object_server(),
        server.inner(),
        message,
        "Query".try_into().unwrap(),
    )
}
fn run(reply: zbus::object_server::DispatchResult2<'_>) {
    let zbus::object_server::DispatchResult2::Async(reply) = reply else {
        panic!("generated reply missing")
    };
    tauri::async_runtime::block_on(reply).unwrap();
}
fn reply(stream: &mut zbus::MessageStream, serial: std::num::NonZeroU32) -> zbus::Message {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match Pin::new(&mut *stream).poll_next(&mut TaskContext::from_waker(Waker::noop())) {
            Poll::Ready(Some(Ok(message))) if message.header().reply_serial() == Some(serial) => {
                return message
            }
            Poll::Ready(Some(Err(error))) => panic!("{error}"),
            _ if Instant::now() < until => std::thread::sleep(Duration::from_millis(1)),
            _ => panic!("reply deadline"),
        }
    }
}

#[test]
fn coalesced_unsent_work_is_cancelled_but_startup_and_taken_send_stay_charged() {
    let (gate, scope, control, startup) = scope();
    for count in 0..100 {
        scope.offer(count).unwrap();
    }
    let (count, sending) = scope.take().unwrap();
    assert_eq!(count, 99);
    assert!(scope.take().is_none());
    scope.offer(200).unwrap();
    control.close().unwrap();
    let closed = gate.close();
    assert!(scope.fence().is_none(), "startup is not ready");
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    scope.ready();
    startup.complete();
    scope.fence().unwrap().complete();
    assert_eq!(
        closed.wait_until(Instant::now()),
        Observation::Pending,
        "taken native send remains owned"
    );
    sending.complete();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    assert_eq!(scope.offer(1), Err(Refused::Closed));
    assert!(matches!(scope.query(), Err(Refused::Closed)));
}

#[test]
fn an_actual_generated_reply_future_keeps_admission_until_the_bus_send_returns() {
    let bus = crate::platform::linux_test_bus::Bus::new();
    let server = bus.connect();
    let client = bus.connect();
    let mut incoming = zbus::MessageStream::from(client.inner().clone());
    let (gate, scope, control, startup) = scope();
    scope.ready();
    startup.complete();
    let service = Service {
        inner: BadgeService(Arc::new(Mutex::new(Tray::new(
            Rgb::new(255, 255, 255),
            Arc::new(|_, _| {}),
        )))),
        scope: scope.clone(),
    };
    let message = query_message(&client);
    let held = dispatch(&service, &server, &message);
    control.close().unwrap();
    let closed = gate.close();
    assert!(
        scope.fence().is_none(),
        "generated Query future is still retained"
    );
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    run(held);
    let received = reply(&mut incoming, message.primary_header().serial_num());
    assert_eq!(received.message_type(), zbus::message::Type::MethodReturn);
    let (launcher, props): (String, Props) = received.body().deserialize().unwrap();
    assert_eq!(launcher, LAUNCHER);
    assert_eq!(i64::try_from(&props["count"]).unwrap(), 0);
    scope.fence().unwrap().complete();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    assert!(!control.snapshot().unwrap().pinned);
}

#[test]
fn actual_socket_backpressure_keeps_the_generated_reply_charged_past_close() {
    use std::future::Future;
    let bus = crate::platform::linux_test_bus::Bus::new();
    let server = bus.connect();
    let client = bus.connect();
    let mut incoming = zbus::MessageStream::from(client.inner().clone());
    let (gate, scope, control, startup) = scope();
    scope.ready();
    startup.complete();
    let service = Service {
        inner: BadgeService(Arc::new(Mutex::new(Tray::new(
            Rgb::new(255, 255, 255),
            Arc::new(|_, _| {}),
        )))),
        scope: scope.clone(),
    };
    let message = query_message(&client);
    let zbus::object_server::DispatchResult2::Async(mut reply_future) =
        dispatch(&service, &server, &message)
    else {
        panic!("no generated reply")
    };
    let large = zbus::Message::signal("/Fixture", "org.example.Fixture", "Backpressure")
        .unwrap()
        .sender(server.unique_name().unwrap())
        .unwrap()
        .build(&vec![0_u8; 8 * 1024 * 1024])
        .unwrap();
    let mut sending = Box::pin(server.inner().send(&large));
    let suspended = bus.suspend();
    // This poll enters the real SDK send and retains its socket_write
    // guard while the owned daemon cannot drain the socket buffer.
    assert!(sending
        .as_mut()
        .poll(&mut TaskContext::from_waker(Waker::noop()))
        .is_pending());
    assert!(reply_future
        .as_mut()
        .poll(&mut TaskContext::from_waker(Waker::noop()))
        .is_pending());
    control.close().unwrap();
    let closed = gate.close();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    assert!(scope.fence().is_none());
    drop(suspended);
    tauri::async_runtime::block_on(sending).unwrap();
    tauri::async_runtime::block_on(reply_future).unwrap();
    let response = reply(&mut incoming, message.primary_header().serial_num());
    assert_eq!(response.message_type(), zbus::message::Type::MethodReturn);
    scope.fence().unwrap().complete();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
}

#[test]
fn query_pressure_and_closed_replies_have_no_badge_payload_and_dropped_replies_pin() {
    let bus = crate::platform::linux_test_bus::Bus::new();
    let server = bus.connect();
    let client = bus.connect();
    let mut incoming = zbus::MessageStream::from(client.inner().clone());
    let (gate, scope, control, startup) = scope();
    scope.ready();
    startup.complete();
    let tray = Arc::new(Mutex::new(Tray::new(
        Rgb::new(255, 255, 255),
        Arc::new(|_, _| {}),
    )));
    lock(&tray).badge_count = 987;
    let service = Service {
        inner: BadgeService(tray),
        scope: scope.clone(),
    };
    let message = query_message(&client);
    let held: Vec<_> = (0..16)
        .map(|_| dispatch(&service, &server, &message))
        .collect();
    assert_eq!(lock(&scope.state).queries, 16);
    let pressure = query_message(&client);
    run(dispatch(&service, &server, &pressure));
    let refused = reply(&mut incoming, pressure.primary_header().serial_num());
    assert_eq!(refused.message_type(), zbus::message::Type::Error);
    assert_eq!(
        refused.header().error_name().unwrap().as_str(),
        "org.freedesktop.DBus.Error.LimitsExceeded"
    );
    assert!(refused.body().deserialize::<(String, Props)>().is_err());
    control.close().unwrap();
    let closed_message = query_message(&client);
    run(dispatch(&service, &server, &closed_message));
    assert_eq!(
        reply(&mut incoming, closed_message.primary_header().serial_num()).message_type(),
        zbus::message::Type::Error
    );
    drop(held);
    assert_eq!(lock(&scope.state).queries, 0);
    scope.fence().unwrap().complete();
    assert!(control.snapshot().unwrap().pinned);
    assert_eq!(gate.snapshot().unwrap().count(Role::Badges), 1);
    assert_eq!(
        gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
}

#[test]
fn panicking_or_unexecuted_work_pins_but_an_idle_weak_control_does_not() {
    let (gate, scope, control, startup) = scope();
    scope.ready();
    startup.complete();
    let query = scope.query().unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _held = query;
            panic!("synthetic callback unwind");
        }))
        .is_err()
    );
    control.close().unwrap();
    scope.fence().unwrap().complete();
    drop(scope);
    assert_eq!(control.snapshot(), Err(Refused::Unavailable));
    assert_eq!(
        gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
}

#[test]
fn exact_match_removal_acknowledgement_releases_both_shell_readers() {
    let bus = crate::platform::linux_test_bus::Bus::new();
    let server = bus.connect();
    let (gate, scope, control, startup) = scope();
    let streams: Vec<_> = SHELLS
        .into_iter()
        .map(|s| ShellStream::new(&server, s, &scope).unwrap())
        .collect();
    scope.ready();
    startup.complete();
    control.close().unwrap();
    let closed = gate.close();
    scope.fence().unwrap().complete();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    for removal in streams
        .into_iter()
        .map(ShellStream::stop)
        .collect::<Vec<_>>()
    {
        removal.close(&server).unwrap();
    }
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
}

#[test]
fn failed_match_removal_and_disconnected_worker_never_report_idle() {
    let bus = crate::platform::linux_test_bus::Bus::new();
    let server = bus.connect();
    let (gate, scope, control, startup) = scope();
    let stream = ShellStream::new(&server, SHELLS[0], &scope).unwrap();
    // The real daemon now refuses the original exact removal as absent.
    zbus::blocking::fdo::DBusProxy::new(&server)
        .unwrap()
        .remove_match_rule(stream.rule.clone())
        .unwrap();
    assert!(stream.stop().close(&server).is_err());
    let (wake, rx) = std::sync::mpsc::sync_channel(1);
    drop(rx);
    scope.bind_worker(wake);
    scope.ready();
    startup.complete();
    assert_eq!(control.close(), Err(Refused::Unavailable));
    assert!(control.snapshot().unwrap().pinned);
    assert_eq!(
        gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
}

#[test]
fn a_real_failed_reply_is_unknown_even_after_the_connection_is_closed() {
    let bus = crate::platform::linux_test_bus::Bus::new();
    let server = bus.connect();
    let client = bus.connect();
    let (gate, scope, control, startup) = scope();
    scope.ready();
    startup.complete();
    let service = Service {
        inner: BadgeService(Arc::new(Mutex::new(Tray::new(
            Rgb::new(255, 255, 255),
            Arc::new(|_, _| {}),
        )))),
        scope: scope.clone(),
    };
    let message = query_message(&client);
    let zbus::object_server::DispatchResult2::Async(reply) = dispatch(&service, &server, &message)
    else {
        panic!("no reply")
    };
    control.close().unwrap();
    server.clone().close().unwrap();
    assert!(tauri::async_runtime::block_on(reply).is_err());
    scope.fence().unwrap().complete();
    assert!(control.snapshot().unwrap().pinned);
    assert_eq!(
        gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
}

#[test]
fn cancelling_a_queued_zero_cannot_forget_the_previous_positive_native_badge() {
    let (_gate, scope, control, startup) = scope();
    scope.ready();
    startup.complete();
    scope.offer(4).unwrap();
    let (count, work) = scope.take().unwrap();
    scope.sent(count);
    work.complete();
    scope.offer(0).unwrap();
    control.close().unwrap();
    assert!(
        scope.needs_clear(),
        "unissued zero is not a successful native clear"
    );
    scope.fence().unwrap().complete();
}

#[test]
fn addressed_messages_between_match_preparations_cannot_block_the_second_ack() {
    let bus = crate::platform::linux_test_bus::Bus::new();
    let server = bus.connect();
    let sender = bus.connect();
    let (gate, scope, control, startup) = scope();
    let first = ShellStream::prepare(&server, SHELLS[0], &scope).unwrap();
    for index in 0..3_i32 {
        sender
            .emit_signal(
                Some(server.unique_name().unwrap().as_str()),
                "/Fixture",
                "org.example.Fixture",
                "Traffic",
                &(index,),
            )
            .unwrap();
    }
    // A barrier on this other connection proves the daemon has handled all
    // three sends before the receiving connection requests its second match.
    zbus::blocking::fdo::DBusProxy::new(&sender)
        .unwrap()
        .get_id()
        .unwrap();
    let before = Instant::now();
    let second = ShellStream::prepare(&server, SHELLS[1], &scope).unwrap();
    assert!(
        before.elapsed() < Duration::from_secs(2),
        "AddMatch waited behind an unpolled raw receiver"
    );
    let streams = [first.activate(&server), second.activate(&server)];
    scope.ready();
    startup.complete();
    control.close().unwrap();
    let closed = gate.close();
    scope.fence().unwrap().complete();
    let removals = streams
        .into_iter()
        .map(ShellStream::stop)
        .collect::<Vec<_>>();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    for removal in removals {
        removal.close(&server).unwrap();
    }
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
}
