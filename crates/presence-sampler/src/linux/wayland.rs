use crate::{Idle, IdleSource, MAX_IDLE};
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::time::{Duration, Instant};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};

const THRESHOLD: Duration = Duration::from_secs(1);
const MAX_SEATS: usize = 8;
const MAX_READS: usize = 64;
const SYNC_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default)]
enum InputClock {
    #[default]
    Starting,
    Idle(Instant, bool),
    Active,
}

impl InputClock {
    fn idled(&mut self, now: Instant) -> Result<(), ()> {
        if matches!(self, Self::Idle(_, _)) {
            return Err(());
        }
        // The compositor supplies no historical timestamp. This lower bound
        // starts at the event, not at sampler construction or each sample.
        let since_input = matches!(self, Self::Active);
        *self = Self::Idle(now.checked_sub(THRESHOLD).ok_or(())?, since_input);
        Ok(())
    }

    fn resumed(&mut self) -> Result<(), ()> {
        if !matches!(self, Self::Idle(_, _)) {
            return Err(());
        }
        *self = Self::Active;
        Ok(())
    }

    fn minimum_idle(self, now: Instant) -> Option<Idle> {
        match self {
            Self::Starting => None,
            // Continued input does not send more resumed events. Until idled,
            // elapsed time since resumed must NOT accumulate as idle time.
            Self::Active => Some(estimate(Duration::ZERO, true)),
            Self::Idle(since, since_input) => now
                .checked_duration_since(since)
                .filter(|v| *v <= MAX_IDLE)
                .map(|elapsed| estimate(elapsed, since_input)),
        }
    }
}

struct Seat {
    proxy: wl_seat::WlSeat,
    notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
    clock: InputClock,
}

#[derive(Default)]
struct State {
    notifier: Option<(u32, ext_idle_notifier_v1::ExtIdleNotifierV1)>,
    seats: BTreeMap<u32, Seat>,
    invalid: bool,
    last_sync: Option<Instant>,
    sync_pending: Option<Instant>,
}

impl State {
    fn subscribe(&mut self, qh: &QueueHandle<Self>) {
        let Some((_, notifier)) = &self.notifier else {
            return;
        };
        for (&name, seat) in &mut self.seats {
            if seat.notification.is_none() {
                // Version 2 explicitly ignores idle inhibitors (video, etc.).
                // Version 1's inhibited idle is not a user-input idle source.
                seat.notification = Some(notifier.get_input_idle_notification(
                    THRESHOLD.as_millis() as u32,
                    &seat.proxy,
                    qh,
                    name,
                ));
            }
        }
    }

    fn sample(&self, now: Instant) -> Option<Idle> {
        if self.invalid
            || self.notifier.is_none()
            || self
                .last_sync
                .is_none_or(|last| now.duration_since(last) > SYNC_TIMEOUT)
        {
            return None;
        }
        // An unobserved seat must not make the whole desktop appear away.
        minimum_idle(self.seats.values().map(|s| s.clock.minimum_idle(now)))
    }
}

pub(super) struct InputIdle {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl InputIdle {
    pub fn connect() -> Result<Self, ()> {
        let connection = Connection::connect_to_env().map_err(|_| ())?;
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Ok(Self {
            connection,
            queue,
            state: State::default(),
        })
    }

    pub fn sample(&mut self) -> Result<Option<Idle>, ()> {
        if self
            .state
            .sync_pending
            .is_some_and(|sent| sent.elapsed() > SYNC_TIMEOUT)
        {
            return Err(());
        }
        if self.state.sync_pending.is_none() {
            self.connection.display().sync(&self.queue.handle(), ());
            self.state.sync_pending = Some(Instant::now());
        }
        // Own connection/queue only; never share a reading guard with GUI code.
        // No blocking roundtrip, worker, timer, or app event loop is required.
        for _ in 0..MAX_READS {
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|_| ())?;
            if self.state.invalid {
                return Err(());
            }
            match self.connection.flush() {
                Ok(()) => {}
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(_) => return Err(()),
            }
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {
                    return Ok(self.state.sample(Instant::now()));
                }
                Err(_) => return Err(()),
            }
        }
        // Do not publish a value while an event backlog is still unread.
        Ok(None)
    }
}

fn estimate(elapsed: Duration, since_input: bool) -> Idle {
    Idle {
        elapsed,
        source: IdleSource::WaylandInputLowerBound,
        since_input,
    }
}

fn minimum_idle(values: impl Iterator<Item = Option<Idle>>) -> Option<Idle> {
    let mut minimum: Option<Idle> = None;
    for value in values {
        let value = value?;
        minimum = Some(match minimum {
            Some(previous) if previous.elapsed < value.elapsed => previous,
            Some(previous) if previous.elapsed == value.elapsed => Idle {
                since_input: previous.since_input && value.since_input,
                ..previous
            },
            _ => value,
        });
    }
    minimum
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Liveness only. A roundtrip/heartbeat never changes InputClock.
        state.last_sync = Some(Instant::now());
        state.sync_pending = None;
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                if interface == "ext_idle_notifier_v1" && version >= 2 && state.notifier.is_none() {
                    state.notifier = Some((name, registry.bind(name, 2, qh, ())));
                } else if interface == "wl_seat" && version >= 1 {
                    if state.seats.len() >= MAX_SEATS {
                        state.invalid = true;
                        return;
                    }
                    state.seats.insert(
                        name,
                        Seat {
                            proxy: registry.bind(name, 1, qh, ()),
                            notification: None,
                            clock: InputClock::Starting,
                        },
                    );
                }
                state.subscribe(qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if state.notifier.as_ref().is_some_and(|(id, _)| *id == name) {
                    state.invalid = true;
                }
                if let Some(seat) = state.seats.remove(&name) {
                    if let Some(notification) = seat.notification {
                        notification.destroy();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(seat) = state.seats.get_mut(name) else {
            return;
        };
        let result = match event {
            ext_idle_notification_v1::Event::Idled => seat.clock.idled(Instant::now()),
            ext_idle_notification_v1::Event::Resumed => seat.clock.resumed(),
            _ => Err(()),
        };
        if result.is_err() {
            state.invalid = true;
        }
    }
}

wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(State: ignore ext_idle_notifier_v1::ExtIdleNotifierV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_and_protocol_errors_do_not_manufacture_activity() {
        let now = Instant::now();
        let mut clock = InputClock::default();
        assert_eq!(clock.minimum_idle(now), None);
        assert_eq!(clock.resumed(), Err(()));
        clock.idled(now).unwrap();
        assert_eq!(clock.minimum_idle(now), Some(estimate(THRESHOLD, false)));
        assert_eq!(clock.idled(now), Err(()));
    }

    #[test]
    fn idle_samples_age_without_renewing_last_activity() {
        let now = Instant::now();
        let mut clock = InputClock::default();
        clock.idled(now).unwrap();
        assert_eq!(
            clock.minimum_idle(now + Duration::from_secs(30)),
            Some(estimate(Duration::from_secs(31), false))
        );
        assert_eq!(
            clock.minimum_idle(now + Duration::from_secs(60)),
            Some(estimate(Duration::from_secs(61), false))
        );
        clock.resumed().unwrap();
        assert_eq!(
            clock.minimum_idle(now + Duration::from_secs(60)),
            Some(estimate(Duration::ZERO, true))
        );
    }

    #[test]
    fn continued_typing_does_not_accumulate_idle_from_the_first_input() {
        let now = Instant::now();
        let mut clock = InputClock::Idle(now, false);
        clock.resumed().unwrap();
        assert_eq!(
            clock.minimum_idle(now + Duration::from_secs(600)),
            Some(estimate(Duration::ZERO, true))
        );
        clock.idled(now + Duration::from_secs(600)).unwrap();
        assert_eq!(
            clock.minimum_idle(now + Duration::from_secs(600)),
            Some(estimate(THRESHOLD, true))
        );
    }

    #[test]
    fn unavailable_protocol_or_seats_stay_unknown() {
        let state = State::default();
        assert_eq!(state.sample(Instant::now()), None);
    }

    #[test]
    fn every_seat_must_be_known_and_the_most_recent_input_wins() {
        assert_eq!(minimum_idle([].into_iter()), None);
        assert_eq!(
            minimum_idle([Some(estimate(Duration::from_secs(60), false)), None].into_iter()),
            None
        );
        assert_eq!(
            minimum_idle(
                [
                    Some(estimate(Duration::from_secs(60), false)),
                    Some(estimate(Duration::ZERO, true))
                ]
                .into_iter()
            ),
            Some(estimate(Duration::ZERO, true))
        );
        assert_eq!(
            minimum_idle(
                [
                    Some(estimate(Duration::from_secs(60), false)),
                    Some(estimate(Duration::from_secs(15), true))
                ]
                .into_iter()
            ),
            Some(estimate(Duration::from_secs(15), true))
        );
    }

    #[test]
    fn observation_only_seat_minimum_keeps_its_provenance() {
        let startup = estimate(Duration::from_secs(1), false);
        let input = estimate(Duration::from_secs(30), true);
        assert_eq!(
            minimum_idle([Some(input), Some(startup)].into_iter()),
            Some(startup)
        );
        assert_eq!(
            minimum_idle([Some(startup), Some(input)].into_iter()),
            Some(startup)
        );
        assert!(
            !minimum_idle([Some(startup), Some(estimate(startup.elapsed, true))].into_iter())
                .unwrap()
                .since_input
        );
    }
}

#[cfg(test)]
#[path = "wayland_fixture.rs"]
mod fixture;
