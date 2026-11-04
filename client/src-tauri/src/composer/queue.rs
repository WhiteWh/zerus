//! The native queue and Send and push now (`SessionsWindowShortcuts.cpp:248-341`,
//! `SessionsWindow.cpp:1859-1873, 3976-3980, 4908-4912`). A push never sends
//! message text twice: after the ordinary durable send it asks the agent to
//! send only the queue snapshot that holds that message.

use serde_json::Value;
use zerus_client_core::input_queue::{
    self as rules, Notice, PushFacts, PushStep, Pusher, QueueView,
};
use zerus_client_core::{Action, Outcome, Target};

use super::send::{outcome_of, SendReply};
use super::{ComposerEvent, Composers, Hub, SessionRef};

#[cfg(test)]
type BeforeSend = Box<dyn FnOnce(&mut Hub) + Send>;

#[derive(Default)]
pub(crate) struct Queue {
    pub(crate) pusher: Pusher,
    /// One Send now across all sessions (`m_queueSendRequest`).
    pub(crate) sending: Option<String>,
    session: Option<SessionRef>,
    #[cfg(test)]
    after_release: Option<super::Job>,
    #[cfg(test)]
    before_send: Option<BeforeSend>,
}

/// Captured under the hub lock, so a selection change cannot redirect a push
/// between its snapshot match and its Send now claim.
struct Delivery {
    key: String,
    target: Target,
    queue_id: String,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn event(notice: Notice) -> ComposerEvent {
    ComposerEvent::Notice {
        text: notice.text,
        error: notice.error,
        transient: notice.transient,
    }
}

impl Composers {
    pub(crate) fn queue_part(&self, hub: &Hub) -> Option<QueueView> {
        let ctx = self.context(hub)?;
        let details = ctx.details.as_deref()?;
        rules::queue_view(
            details,
            ctx.row.tracked,
            ctx.online,
            ctx.facts.inspect_error,
            hub.queue.sending.as_deref() == Some(ctx.key.as_str()),
        )
    }

    /// Qt's `sendQueuedNow` (248-260), while the selection and details are
    /// held. The native action is built only here, never from webview JSON.
    fn begin_queue(&self, hub: &mut Hub, queue_id: Option<String>) -> Option<Delivery> {
        if hub.closed
            || hub.queue.sending.is_some()
            || hub
                .open
                .as_ref()
                .is_some_and(|open| open.subagent.is_some())
        {
            return None;
        }
        let ctx = self.context(hub);
        let details = ctx.as_ref().and_then(|ctx| ctx.details.as_deref());
        let id = queue_id.filter(|id| !id.is_empty()).unwrap_or_else(|| {
            details
                .map(|d| text(&d["input_queue"], "id").to_owned())
                .unwrap_or_default()
        });
        let ready = matches!((&ctx, details), (Some(ctx), Some(details)) if rules::can_send_now(details, ctx.online, &id));
        if !ready {
            self.event(
                hub,
                event(Notice {
                    text: rules::NOTHING_QUEUED.into(),
                    ..Notice::default()
                }),
            );
            return None;
        }
        let ctx = ctx.as_ref().expect("ready requires a selected session");
        let details = details.expect("ready requires details");
        let delivery = Delivery {
            key: ctx.key.clone(),
            target: Target {
                computer: ctx.session.computer.clone(),
                name: ctx.session.name.clone(),
                run_id: text(details, "run_id").to_owned(),
                conversation_id: text(details, "conversation_id").to_owned(),
            },
            queue_id: id,
        };
        // One queue action owns this intent, even before the message's send
        // acknowledgement. A later state must not repeat an uncertain explicit
        // Send now through the older armed push (webui ruling, 2026-10-29).
        if hub.queue.pusher.active().is_some_and(|push| {
            push.key == ctx.key
                && rules::queue_holds(text(&details["input_queue"], "text"), &push.text)
        }) {
            hub.queue.pusher = Pusher::default();
            hub.queue.session = None;
        }
        hub.queue.sending = Some(delivery.key.clone());
        Some(delivery)
    }

    pub fn send_queue_now(&self, queue_id: Option<String>) -> bool {
        let delivery = self.begin_queue(&mut self.lock(), queue_id);
        self.changed();
        let Some(delivery) = delivery else {
            return false;
        };
        self.dispatch_queue(delivery);
        true
    }

    fn dispatch_queue(&self, delivery: Delivery) {
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || {
            let closed = me.lock().closed;
            if closed {
                me.queue_sent(&delivery.key, None);
                return;
            }
            let called = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (me.0.deps.dispatch)(
                    delivery.target,
                    Action::SendNow {
                        queue_id: delivery.queue_id,
                    },
                    uuid::Uuid::new_v4().to_string(),
                )
            }));
            // Lane::Queue busy means no call ran. Qt stays quiet (251).
            let outcome = match called {
                Ok(Err(error)) if error == crate::actions::BUSY => None,
                other => Some(outcome_of(other, rules::NOTHING_QUEUED)),
            };
            me.queue_sent(&delivery.key, outcome.as_ref());
        }));
    }

    fn queue_sent(&self, key: &str, outcome: Option<&Outcome>) {
        let mut hub = self.lock();
        hub.queue.sending = None;
        let current = hub.open_key.as_deref() == Some(key) && outcome.is_some();
        if current {
            // Remove the actual held queue too: the next chord must not use
            // the snapshot we just acted on (Qt 1863).
            if let Some(details) = hub.followed.details.as_mut() {
                if let Some(fields) = std::sync::Arc::make_mut(details).as_object_mut() {
                    fields.remove("input_queue");
                }
            }
            self.event(
                &mut hub,
                event(rules::send_now_notice(
                    outcome.expect("a finished action has an outcome"),
                )),
            );
        }
        drop(hub);
        self.changed();
        if current {
            self.refresh_details();
        }
    }

    /// The composer chord only (`MessageComposer::push`, 834-840). An empty
    /// composer or a message in flight asks to send the existing queue now.
    pub fn send_push(
        &self,
        handle: u64,
        rev: u64,
        epoch: u64,
        text: String,
        position: i64,
        anchor: i64,
    ) -> SendReply {
        self.edit(handle, rev, epoch, text.clone(), position, anchor);
        let mut hub = self.lock();
        let Some(key) = hub.open_key.clone() else {
            self.hand_back(hub, handle);
            return SendReply::Busy;
        };
        let Some(composer) = hub.composers.get(&key).filter(|c| c.handle == handle) else {
            self.hand_back(hub, handle);
            return SendReply::Busy;
        };
        if hub
            .open
            .as_ref()
            .is_some_and(|open| open.subagent.is_some())
        {
            self.hand_back(hub, handle);
            return SendReply::Busy;
        }
        // A required card replaces the editor and both Send chords, even
        // when a native queue is visible (A2-3/A2-4 review ruling).
        if !self.question_part(&hub).1 {
            self.hand_back(hub, handle);
            return SendReply::Busy;
        }
        if composer.sending || !composer.has_content() {
            // Claim the chord's queue before releasing its selected session.
            let delivery = self.begin_queue(&mut hub, None);
            #[cfg(test)]
            let between = hub.queue.after_release.take();
            self.hand_back(hub, handle);
            #[cfg(test)]
            if let Some(between) = between {
                between();
            }
            if let Some(delivery) = delivery {
                self.dispatch_queue(delivery);
            }
            self.changed();
            return SendReply::Busy;
        }
        if composer.text_epoch != epoch || composer.draft.text != text {
            self.hand_back(hub, handle);
            return SendReply::Busy;
        }
        if !self.view_of(&hub, &key).is_some_and(|v| v.can_send) {
            self.hand_back(hub, handle);
            return SendReply::Busy;
        }
        let message = composer.draft.text.clone();
        if let Some(notice) = hub.queue.pusher.arm(&key, &message, self.now()) {
            self.event(&mut hub, event(notice));
        }
        hub.queue.session = hub.open.clone();
        // Keep the selected-session arm and the ordinary send's checks under
        // one lock, exactly as send_message holds them. Its save is unchanged.
        #[cfg(test)]
        if let Some(between) = hub.queue.before_send.take() {
            between(&mut hub);
        }
        let reply = self.send_held(&mut hub, handle, epoch, &text);
        if matches!(reply, SendReply::Busy | SendReply::Blocked { .. }) {
            self.hand_back(hub, handle);
        } else {
            drop(hub);
        }
        self.changed();
        reply
    }

    pub(crate) fn push_finished(&self, request: &str, ok: bool, uncertain: bool) {
        let mut hub = self.lock();
        if let Some(notice) = hub.queue.pusher.send_finished(request, ok, uncertain) {
            self.event(&mut hub, event(notice));
            drop(hub);
            self.changed();
        }
    }

    fn push_facts(&self, hub: &Hub) -> PushFacts {
        let Some(push) = hub.queue.pusher.active() else {
            return PushFacts::default();
        };
        let composer = hub.composers.get(&push.key);
        PushFacts {
            dialog: hub.prompts.values().any(|p| p.key == push.key) || hub.answers.cold.is_some(),
            sending: composer.is_some_and(|c| c.sending),
            waiting: composer.is_some_and(|c| c.waiting.is_some()),
        }
    }

    pub(crate) fn queue_observed(&self) {
        let mut hub = self.lock();
        let (Some(key), Some(details)) = (hub.open_key.clone(), hub.followed.details.clone())
        else {
            return;
        };
        let facts = self.push_facts(&hub);
        let busy = hub.queue.sending.is_some();
        let step = hub
            .queue
            .pusher
            .inspected(&key, &details, self.now(), facts, busy);
        let delivery = match step {
            PushStep::Wait => return,
            PushStep::End(notice) => {
                if let Some(notice) = notice {
                    self.event(&mut hub, event(notice));
                }
                None
            }
            PushStep::SendNow(id) => self.begin_queue(&mut hub, Some(id)),
        };
        drop(hub);
        self.changed();
        if let Some(delivery) = delivery {
            self.dispatch_queue(delivery);
        }
    }

    pub(crate) fn push_selection(&self, hub: &mut Hub, key: Option<&str>) {
        let Some(push) = hub.queue.pusher.active() else {
            return;
        };
        if key == Some(push.key.as_str()) {
            return;
        }
        let fallback = push.key.split('\n').nth(1).unwrap_or_default().to_owned();
        let title = hub
            .queue
            .session
            .as_ref()
            .and_then(|s| (self.0.deps.rows)(&s.computer, &s.key, &s.name))
            .map(|(row, _)| row.display_title)
            .filter(|title| !title.is_empty())
            .unwrap_or(fallback);
        if let Some(notice) = hub.queue.pusher.selection_changed(key, &title) {
            self.event(hub, event(notice));
        }
    }

    pub(crate) fn push_check(&self) {
        let mut hub = self.lock();
        let facts = self.push_facts(&hub);
        if let Some(notice) = hub.queue.pusher.check(self.now(), facts) {
            self.event(&mut hub, event(notice));
            drop(hub);
            self.changed();
        }
    }
}

fn main_origin(label: &str) -> Result<(), String> {
    if label == crate::MAIN_WINDOW {
        Ok(())
    } else {
        Err("Composer actions are available only in the main view.".into())
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn queue_send_now(
    webview: tauri::Webview,
    state: tauri::State<'_, Composers>,
    queue_id: Option<String>,
) -> Result<bool, String> {
    main_origin(webview.label())?;
    let composers = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || composers.send_queue_now(queue_id))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)] // Webview and managed state accompany editor coordinates.
pub async fn send_push(
    webview: tauri::Webview,
    state: tauri::State<'_, Composers>,
    handle: u64,
    rev: u64,
    epoch: u64,
    text: String,
    position: i64,
    anchor: i64,
) -> Result<SendReply, String> {
    main_origin(webview.label())?;
    let composers = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        composers.send_push(handle, rev, epoch, text, position, anchor)
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::send::tests::{opened, ready, receipt, send_hub, sent, state};
    use crate::composer::tests::{dir, until};
    use crate::composer::{lock, ComposerEvent};
    use serde_json::json;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn working() -> Value {
        let mut details = ready();
        details["activity"] = json!("busy");
        details["phase"] = json!("working");
        details
    }

    fn queued(text: &str, can: bool) -> Value {
        let mut details = working();
        details["input_queue"] = json!({"id": "queue-1", "text": text, "can_send_now": can, "hint": "Send the queued messages now"});
        details
    }

    fn submitted(action: &Action, id: &str) -> Outcome {
        match action {
            Action::SendNow { queue_id } => Outcome::Done {
                receipt: json!({"status": "submitted", "request_id": id, "queue_id": queue_id}),
                cleanup: None,
            },
            _ => receipt(action, id),
        }
    }

    fn send_nows(fake: &crate::composer::send::tests::Fake) -> usize {
        sent(fake)
            .iter()
            .filter(|(_, action, _)| matches!(action, Action::SendNow { .. }))
            .count()
    }

    fn wait_for(done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn notice(text: &str) -> ComposerEvent {
        ComposerEvent::Notice {
            text: text.into(),
            error: false,
            transient: false,
        }
    }

    #[test]
    fn send_now_runs_once_and_hides_the_panel_until_the_next_state() {
        let root = dir("send-now");
        let (c, fake) = send_hub(&root);
        let (gate, wait) = std::sync::mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        *lock(&fake.outcome) = Box::new(move |action, id| {
            if matches!(action, Action::SendNow { .. }) {
                let _ = lock(&wait).recv();
            }
            submitted(action, id)
        });
        let view = opened(&c, queued("❯ first", true));
        let queue = view.queue.clone().unwrap();
        assert_eq!(
            (queue.text.as_str(), queue.label.as_str(), queue.enabled),
            ("❯ first", "Send now", true)
        );
        assert!(c.send_queue_now(Some("queue-1".into())));
        until(&c, |v| {
            v.queue
                .as_ref()
                .is_some_and(|q| q.label == "Sending…" && !q.enabled)
        });
        assert!(!c.send_queue_now(None), "one send-now at a time");
        gate.send(()).unwrap();
        until(&c, |v| v.queue.is_none());
        assert_eq!(
            c.take_events(),
            vec![notice(
                "Send now requested. Waiting for the agent to accept the queue."
            )]
        );
        c.observe(&state(queued("❯ first", true)));
        assert!(
            c.current().unwrap().queue.is_some(),
            "the next state shows the queue again"
        );
        assert_eq!(send_nows(&fake), 1);
    }

    #[test]
    fn nothing_queued_says_so() {
        let root = dir("nothing-queued");
        let (c, fake) = send_hub(&root);
        opened(&c, working());
        assert!(!c.send_queue_now(None));
        assert_eq!(
            c.take_events(),
            vec![notice("Nothing queued can be sent now")]
        );
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_push_sends_the_queue_once_the_agent_queues_it() {
        let root = dir("push");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        let view = opened(&c, working());
        assert_eq!(
            c.send_push(
                view.handle,
                1,
                view.text_epoch,
                "please also run lint".into(),
                20,
                20
            ),
            SendReply::Sending
        );
        until(&c, |v| !v.sending && v.text.is_empty());
        assert_eq!(send_nows(&fake), 0, "nothing is queued yet");
        c.observe(&state(queued("❯ please also run lint", true)));
        wait_for(|| send_nows(&fake) == 1);
        let calls = sent(&fake);
        assert_eq!(
            calls
                .iter()
                .filter(|(_, action, _)| matches!(action, Action::Send { .. }))
                .count(),
            1,
            "a push never sends text again"
        );
        let Some((_, Action::SendNow { queue_id }, _)) = calls.last() else {
            panic!("no send-now")
        };
        assert_eq!(queue_id, "queue-1");
        c.observe(&state(queued("❯ please also run lint", true)));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(send_nows(&fake), 1, "the push ended with its one send-now");
    }

    #[test]
    fn a_push_with_an_empty_field_is_send_now() {
        let root = dir("push-empty");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        let view = opened(&c, queued("❯ earlier", true));
        assert_eq!(
            c.send_push(view.handle, 1, view.text_epoch, String::new(), 0, 0),
            SendReply::Busy
        );
        wait_for(|| send_nows(&fake) == 1);
    }

    #[test]
    fn selecting_another_session_ends_the_push() {
        let root = dir("push-switch");
        let (c, fake) = send_hub(&root);
        lock(&fake.row).title = "review".into();
        lock(&fake.row).display_title = "sample-project / review".into();
        *lock(&fake.outcome) = Box::new(submitted);
        let view = opened(&c, working());
        c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5);
        until(&c, |v| !v.sending && v.text.is_empty());
        c.take_events();
        c.open(Some(SessionRef {
            computer: "studio".into(),
            key: "run:run-2".into(),
            name: "codex/sample-project/other".into(),
            archive_id: None,
            subagent: None,
        }));
        assert_eq!(
            c.take_events(),
            vec![notice(
                "The message to sample-project / review was not pushed: another session was selected"
            )]
        );
    }

    #[test]
    fn a_push_ends_after_twenty_seconds() {
        let root = dir("push-late");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        let view = opened(&c, working());
        c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5);
        until(&c, |v| !v.sending && v.text.is_empty());
        c.take_events();
        *lock(&fake.now) += 21.0;
        c.tick();
        assert_eq!(c.take_events(), vec![notice("The message was sent without push: the agent did not offer to send it now within 20 seconds")]);
        c.observe(&state(queued("❯ later", true)));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            send_nows(&fake),
            0,
            "a snapshot after the deadline never sends the queue"
        );
    }

    #[test]
    fn a_busy_queue_lane_is_quiet_and_keeps_the_queue_available() {
        let root = dir("queue-busy");
        let (c, fake) = send_hub(&root);
        opened(&c, queued("❯ first", true));
        *lock(&fake.taken) = Box::new(|action| matches!(action, Action::SendNow { .. }));
        assert!(c.send_queue_now(None));
        wait_for(|| c.lock().queue.sending.is_none());
        assert!(c.take_events().is_empty());
        assert!(c.current().unwrap().queue.unwrap().enabled);
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn an_uncertain_queue_action_waits_for_an_explicit_new_action() {
        let root = dir("queue-uncertain");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "Check Terminal".into(),
        });
        opened(&c, queued("❯ first", true));
        assert!(c.send_queue_now(None));
        wait_for(|| c.lock().queue.sending.is_none());
        assert!(c.current().unwrap().queue.is_none());
        assert!(c
            .lock()
            .followed
            .details
            .as_ref()
            .unwrap()
            .get("input_queue")
            .is_none());
        assert_eq!(
            c.take_events(),
            vec![ComposerEvent::Notice {
                text: "Check Terminal".into(),
                error: true,
                transient: false
            }]
        );
        c.resend();
        c.observe(&state(queued("❯ first", true)));
        c.tick();
        assert_eq!(send_nows(&fake), 1, "fresh state and reload never retry");
        *lock(&fake.outcome) = Box::new(submitted);
        assert!(c.send_queue_now(None));
        wait_for(|| c.lock().queue.sending.is_none());
        let calls = sent(&fake);
        assert_eq!(calls.len(), 2);
        assert_ne!(
            calls[0].2, calls[1].2,
            "an explicit action makes a new request"
        );
        for (_, _, id) in calls {
            assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
        }
    }

    #[test]
    fn a_stale_queue_uses_qts_full_transient_notice() {
        let root = dir("queue-stale");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: The native queue is no longer visible; nothing was sent".into(),
        });
        opened(&c, queued("❯ first", true));
        assert!(c.send_queue_now(None));
        wait_for(|| c.lock().queue.sending.is_none());
        assert_eq!(c.take_events(), vec![ComposerEvent::Notice { text: "The native queue is no longer visible; nothing was sent. Activity is being refreshed; check Terminal or the current queue before retrying.".into(), error: true, transient: true }]);
        assert!(c
            .lock()
            .followed
            .details
            .as_ref()
            .unwrap()
            .get("input_queue")
            .is_none());
        assert!(!c.send_queue_now(Some("queue-1".into())));
        assert_eq!(
            c.take_events(),
            vec![notice("Nothing queued can be sent now")]
        );
        assert_eq!(send_nows(&fake), 1);
    }

    #[test]
    fn a_queue_dispatch_panic_releases_the_window_wide_request() {
        let root = dir("queue-panic");
        let (c, fake) = send_hub(&root);
        *lock(&fake.taken) = Box::new(|_| panic!("synthetic queue dispatch panic"));
        opened(&c, queued("❯ first", true));
        assert!(c.send_queue_now(None));
        wait_for(|| c.lock().queue.sending.is_none());
        assert_eq!(
            c.take_events(),
            vec![ComposerEvent::Notice {
                text: "The request ended abnormally.".into(),
                error: true,
                transient: false
            }]
        );
        assert!(c.current().unwrap().queue.is_none());
    }

    #[test]
    fn send_now_keeps_its_target_and_is_quiet_after_selection_changes() {
        let root = dir("queue-switch");
        let (c, fake) = send_hub(&root);
        let (release, held) = std::sync::mpsc::channel();
        let held = Mutex::new(held);
        *lock(&fake.outcome) = Box::new(move |action, id| {
            if matches!(action, Action::SendNow { .. }) {
                lock(&held).recv_timeout(Duration::from_secs(5)).unwrap();
            }
            submitted(action, id)
        });
        opened(&c, queued("❯ first", true));
        assert!(c.send_queue_now(None));
        c.open(None);
        assert!(
            !c.send_queue_now(None),
            "the one in-flight action blocks every selection quietly"
        );
        release.send(()).unwrap();
        wait_for(|| c.lock().queue.sending.is_none());
        assert!(c.take_events().is_empty());
        let calls = sent(&fake);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            Target {
                computer: "studio".into(),
                name: "claude/sample-project/review".into(),
                run_id: "run-1".into(),
                conversation_id: "conv-1".into()
            }
        );
    }

    #[test]
    fn a_push_whose_send_is_uncertain_never_pushes_a_later_queue() {
        let root = dir("push-uncertain");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "unknown delivery".into(),
        });
        let view = opened(&c, working());
        assert_eq!(
            c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5),
            SendReply::Sending
        );
        until(&c, |v| v.uncertain);
        assert_eq!(
            c.take_events(),
            vec![ComposerEvent::Notice {
                text: "The message was not pushed: delivery is not confirmed".into(),
                error: true,
                transient: false
            }]
        );
        c.resend();
        c.observe(&state(queued("❯ later", true)));
        c.tick();
        assert_eq!(send_nows(&fake), 0);
        assert_eq!(sent(&fake).len(), 1, "the message is never sent again");
        assert!(c.lock().queue.pusher.active().is_none());
    }

    #[test]
    fn a_push_cannot_arm_for_another_handle_or_a_stale_editor() {
        let root = dir("push-stale-editor");
        let (c, fake) = send_hub(&root);
        let view = opened(&c, working());
        c.edit(view.handle, 1, view.text_epoch, "kept".into(), 4, 4);
        assert_eq!(
            c.send_push(view.handle + 1, 2, view.text_epoch, "kept".into(), 4, 4),
            SendReply::Busy
        );
        assert!(c.lock().queue.pusher.active().is_none());
        assert_eq!(
            c.send_push(view.handle, 2, view.text_epoch + 1, "kept".into(), 4, 4),
            SendReply::Busy
        );
        assert!(c.lock().queue.pusher.active().is_none());
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_push_waits_for_its_durable_message_save() {
        let root = dir("push-durable");
        let store = crate::composer::tests::drafts(&root);
        let (c, fake) = crate::composer::send::tests::send_hub_on(&store);
        *lock(&fake.outcome) = Box::new(submitted);
        let view = opened(&c, working());
        let release = crate::composer::tests::hold(&store);
        assert_eq!(
            c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5),
            SendReply::Sending
        );
        assert!(sent(&fake).is_empty());
        c.observe(&state(queued("❯ later", true)));
        assert_eq!(
            send_nows(&fake),
            0,
            "a matching queue never bypasses the save or its send acknowledgement"
        );
        release.send(()).unwrap();
        until(&c, |v| !v.sending);
        c.observe(&state(queued("❯ later", true)));
        wait_for(|| send_nows(&fake) == 1);
        assert_eq!(
            sent(&fake)
                .iter()
                .filter(|(_, a, _)| matches!(a, Action::Send { .. }))
                .count(),
            1
        );
    }
    type Recorded = std::sync::Arc<Mutex<Vec<(Target, Action, String)>>>;

    fn overlap_hub(
        root: &crate::composer::tests::Scratch,
        outcome: Outcome,
    ) -> (Composers, Recorded, std::sync::mpsc::Sender<()>) {
        use std::sync::Arc;
        let store = crate::composer::tests::drafts(root);
        let (release, held) = std::sync::mpsc::channel();
        let held = Mutex::new(held);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let c = Composers::new(
            store,
            crate::composer::Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            crate::composer::Deps {
                spawn: Arc::new(|job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(|| 1_793_000_100.0),
                rows: Arc::new(|_, _, _| {
                    Some((
                        crate::composer::send::tests::row("claude/sample-project/review"),
                        true,
                    ))
                }),
                facts: Arc::new(|_, _| None),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| Err("timed out".into())),
                dispatch: Arc::new(move |target, action, id| {
                    lock(&recorded).push((target, action.clone(), id.clone()));
                    if matches!(&action, Action::Send { .. }) {
                        lock(&held).recv_timeout(Duration::from_secs(5)).unwrap();
                        Ok(submitted(&action, &id))
                    } else {
                        Ok(outcome.clone())
                    }
                }),
            },
        );
        (c, calls, release)
    }

    #[test]
    fn explicit_send_now_consumes_a_matching_push_before_the_text_ack() {
        for (label, outcome) in [
            (
                "overlap-uncertain",
                Outcome::Uncertain {
                    detail: "queue outcome unknown".into(),
                },
            ),
            (
                "overlap-failed",
                Outcome::Failed {
                    detail: "queue action failed".into(),
                },
            ),
            (
                "overlap-done",
                Outcome::Done {
                    receipt: json!({"status": "submitted"}),
                    cleanup: None,
                },
            ),
        ] {
            let root = dir(label);
            let expected_notice = event(rules::send_now_notice(&outcome));
            let (c, calls, release) = overlap_hub(&root, outcome);
            let view = opened(&c, working());
            assert_eq!(
                c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5),
                SendReply::Sending
            );
            wait_for(|| {
                lock(&calls)
                    .iter()
                    .any(|(_, a, _)| matches!(a, Action::Send { .. }))
            });
            assert!(c.lock().queue.pusher.active().is_some());
            c.observe(&state(queued("❯ later", true)));
            assert!(c.send_queue_now(None));
            wait_for(|| c.lock().queue.sending.is_none());
            assert!(
                c.lock().queue.pusher.active().is_none(),
                "the explicit action owns this intent even without its text acknowledgement"
            );
            assert_eq!(
                c.take_events(),
                vec![expected_notice],
                "no extra push notice"
            );
            release.send(()).unwrap();
            until(&c, |v| !v.sending);
            c.observe(&state(queued("❯ later", true)));
            c.tick();
            assert!(c.take_events().is_empty(), "the consumed push stays quiet");
            assert_eq!(
                lock(&calls)
                    .iter()
                    .filter(|(_, a, _)| matches!(a, Action::SendNow { .. }))
                    .count(),
                1,
                "a fresh matching snapshot must never repeat the queue action"
            );
            assert_eq!(
                lock(&calls)
                    .iter()
                    .filter(|(_, a, _)| matches!(a, Action::Send { .. }))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn an_explicit_send_of_an_unrelated_queue_preserves_the_push() {
        let root = dir("overlap-other-queue");
        let (c, calls, release) = overlap_hub(
            &root,
            Outcome::Uncertain {
                detail: "older queue outcome unknown".into(),
            },
        );
        let view = opened(&c, working());
        c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5);
        wait_for(|| {
            lock(&calls)
                .iter()
                .any(|(_, a, _)| matches!(a, Action::Send { .. }))
        });
        c.observe(&state(queued("❯ older message", true)));
        assert!(c.send_queue_now(None));
        wait_for(|| c.lock().queue.sending.is_none());
        assert!(c.lock().queue.pusher.active().is_some());
        release.send(()).unwrap();
        until(&c, |v| !v.sending);
        let mut fresh = queued("❯ later", true);
        fresh["input_queue"]["id"] = json!("queue-2");
        c.observe(&state(fresh));
        wait_for(|| {
            lock(&calls)
                .iter()
                .filter(|(_, a, _)| matches!(a, Action::SendNow { .. }))
                .count()
                == 2
        });
        let ids: Vec<_> = lock(&calls)
            .iter()
            .filter_map(|(_, a, _)| match a {
                Action::SendNow { queue_id } => Some(queue_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            ["queue-1", "queue-2"],
            "only the distinct snapshot holding the push is sent next"
        );
    }

    type HeldJobs = std::sync::Arc<Mutex<Vec<crate::composer::Job>>>;

    fn held_queue_hub(
        root: &crate::composer::tests::Scratch,
    ) -> (
        Composers,
        std::sync::Arc<crate::composer::send::tests::Fake>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        HeldJobs,
    ) {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let holding = Arc::new(AtomicBool::new(false));
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let (hold, queued) = (Arc::clone(&holding), Arc::clone(&jobs));
        let store = crate::composer::tests::drafts(root);
        let (c, fake) = crate::composer::send::tests::send_hub_with(
            &store,
            Arc::new(move |job| {
                if hold.load(Ordering::SeqCst) {
                    lock(&queued).push(job);
                } else {
                    std::thread::spawn(job);
                }
            }),
        );
        (c, fake, holding, jobs)
    }

    #[test]
    fn an_automatic_queue_action_keeps_the_snapshot_target_until_dispatch() {
        use std::sync::atomic::Ordering;
        let root = dir("push-captured-target");
        let (c, fake, holding, jobs) = held_queue_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        let view = opened(&c, working());
        c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5);
        until(&c, |v| !v.sending);
        holding.store(true, Ordering::SeqCst);
        c.observe(&state(queued("❯ later", true)));
        assert_eq!(send_nows(&fake), 0);
        assert!(c.lock().queue.sending.is_some());
        c.open(None);
        let pending = std::mem::take(&mut *lock(&jobs));
        for job in pending {
            job();
        }
        let calls = sent(&fake);
        let target = calls
            .iter()
            .find_map(|(t, a, _)| matches!(a, Action::SendNow { .. }).then_some(t))
            .unwrap();
        assert_eq!(
            target,
            &Target {
                computer: "studio".into(),
                name: "claude/sample-project/review".into(),
                run_id: "run-1".into(),
                conversation_id: "conv-1".into()
            }
        );
        assert!(
            c.take_events().is_empty(),
            "the original target's outcome is quiet after navigation"
        );
    }

    #[test]
    fn a_send_now_job_queued_at_exit_never_starts() {
        use std::sync::atomic::Ordering;
        let root = dir("queue-exit");
        let (c, fake, holding, jobs) = held_queue_hub(&root);
        opened(&c, queued("❯ first", true));
        holding.store(true, Ordering::SeqCst);
        assert!(c.send_queue_now(None));
        c.flush();
        let pending = std::mem::take(&mut *lock(&jobs));
        for job in pending {
            job();
        }
        assert!(sent(&fake).is_empty());
        assert!(c.lock().queue.sending.is_none());
        assert!(!c.send_queue_now(None));
    }

    #[test]
    fn a_missing_changed_or_unavailable_queue_sends_nothing() {
        let root = dir("queue-readiness");
        let (c, fake) = send_hub(&root);
        opened(&c, queued("❯ first", true));
        assert!(!c.send_queue_now(Some("older-queue".into())));
        assert_eq!(
            c.take_events(),
            vec![notice("Nothing queued can be sent now")]
        );
        *lock(&fake.online) = false;
        assert!(!c.send_queue_now(None));
        assert_eq!(
            c.take_events(),
            vec![notice("Nothing queued can be sent now")]
        );
        *lock(&fake.online) = true;
        c.observe(&state(queued("❯ first", false)));
        assert!(!c.send_queue_now(None));
        assert_eq!(
            c.take_events(),
            vec![notice("Nothing queued can be sent now")]
        );
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_push_ends_when_the_followed_run_changes() {
        let root = dir("push-run-changed");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        let view = opened(&c, working());
        c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5);
        until(&c, |v| !v.sending);
        let mut changed = queued("❯ later", true);
        changed["run_id"] = json!("run-2");
        c.observe(&state(changed));
        assert_eq!(
            c.take_events(),
            vec![ComposerEvent::Notice {
                text: "The message was not pushed: the session changed".into(),
                error: true,
                transient: false
            }]
        );
        assert_eq!(send_nows(&fake), 0);
    }

    #[test]
    fn ended_frames_and_failed_inspects_never_push_the_held_snapshot() {
        use std::sync::atomic::Ordering;
        let root = dir("push-fresh-details-only");
        let (c, fake, holding, jobs) = held_queue_hub(&root);
        let (release, held) = std::sync::mpsc::channel::<()>();
        let held = Mutex::new(held);
        *lock(&fake.outcome) = Box::new(move |action, id| {
            if matches!(action, Action::Send { .. }) {
                lock(&held).recv_timeout(Duration::from_secs(5)).unwrap();
            }
            submitted(action, id)
        });
        let view = opened(&c, working());
        c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5);
        wait_for(|| {
            c.lock()
                .queue
                .pusher
                .active()
                .is_some_and(|p| p.request.is_some())
        });
        c.observe(&state(queued("❯ later", true)));
        release.send(()).unwrap();
        until(&c, |v| !v.sending);
        assert_eq!(send_nows(&fake), 0);
        holding.store(true, Ordering::SeqCst);
        c.observe(&crate::composer::send::tests::ended());
        assert!(
            c.lock().queue.sending.is_none(),
            "an ended frame contains no new details"
        );
        assert!(lock(&jobs).is_empty());
        c.refresh_details();
        let inspect = std::mem::take(&mut *lock(&jobs));
        assert_eq!(inspect.len(), 1);
        for job in inspect {
            job();
        }
        assert!(c.lock().followed.inspect_error.is_some());
        assert!(
            c.lock().queue.sending.is_none(),
            "a failed inspect contains no new details"
        );
        assert!(lock(&jobs).is_empty());
        assert!(c.lock().queue.pusher.active().is_some());
        c.observe(&state(queued("❯ later", true)));
        let dispatch = std::mem::take(&mut *lock(&jobs));
        assert_eq!(dispatch.len(), 1, "an accepted state may push");
        for job in dispatch {
            job();
        }
        assert_eq!(send_nows(&fake), 1);
    }

    #[test]
    fn a_matching_snapshot_while_another_send_now_runs_preserves_the_push() {
        use std::sync::atomic::Ordering;
        let root = dir("push-queue-busy-snapshot");
        let (c, fake, holding, jobs) = held_queue_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        let view = opened(&c, working());
        c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5);
        until(&c, |v| !v.sending);
        c.observe(&state(queued("❯ older", true)));
        holding.store(true, Ordering::SeqCst);
        assert!(c.send_queue_now(None));
        c.observe(&state(queued("❯ later", true)));
        assert!(
            c.lock().queue.pusher.active().is_some(),
            "wait, do not consume while busy"
        );
        assert_eq!(lock(&jobs).len(), 1);
        let pending = std::mem::take(&mut *lock(&jobs));
        for job in pending {
            job();
        }
        assert_eq!(send_nows(&fake), 1);
        c.observe(&state(queued("❯ later", true)));
        assert!(c.lock().queue.pusher.active().is_none());
        assert_eq!(lock(&jobs).len(), 1);
        let pending = std::mem::take(&mut *lock(&jobs));
        for job in pending {
            job();
        }
        assert_eq!(send_nows(&fake), 2);
    }

    #[test]
    fn an_accepted_inspection_shows_the_queue_again_after_send_now() {
        let root = dir("queue-inspect-restores");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        opened(&c, queued("❯ earlier", true));
        *lock(&fake.inspected) = Some(queued("❯ returned by inspection", true));
        c.lock().followed.streamed = false;
        assert!(c.send_queue_now(None));
        until(&c, |v| {
            v.queue
                .as_ref()
                .is_some_and(|q| q.text == "❯ returned by inspection")
        });
        assert!(fake.inspects.load(std::sync::atomic::Ordering::SeqCst) > 0);
    }

    #[test]
    fn another_sessions_queue_never_shows_sending_for_the_inflight_action() {
        use std::sync::atomic::Ordering;
        let root = dir("queue-other-session-label");
        let (c, fake, holding, jobs) = held_queue_hub(&root);
        opened(&c, queued("❯ original", true));
        holding.store(true, Ordering::SeqCst);
        assert!(c.send_queue_now(None));
        c.open(Some(SessionRef {
            computer: "studio".into(),
            key: "run:run-2".into(),
            name: "claude/sample-project/other".into(),
            archive_id: None,
            subagent: None,
        }));
        let mut second = state(queued("❯ other", true));
        second.activity[0].key = "run:run-2".into();
        c.observe(&second);
        until(&c, |v| {
            v.loaded && v.queue.as_ref().is_some_and(|q| q.label == "Send now")
        });
        assert_eq!(c.current().unwrap().queue.unwrap().label, "Send now");
        c.open(None);
        for job in std::mem::take(&mut *lock(&jobs)) {
            job();
        }
        assert_eq!(send_nows(&fake), 1);
    }

    #[test]
    fn every_busy_push_reply_hands_the_editor_back() {
        for empty in [false, true] {
            let root = dir(if empty {
                "push-empty-hand-back"
            } else {
                "push-disabled-hand-back"
            });
            let (c, fake) = send_hub(&root);
            let view = opened(&c, working());
            *lock(&fake.online) = false;
            let text = if empty { "" } else { "later" };
            assert_eq!(
                c.send_push(view.handle, 1, view.text_epoch, text.into(), 5, 5),
                SendReply::Busy
            );
            assert!(c.current().unwrap().text_epoch > view.text_epoch);
            assert_eq!(c.current().unwrap().text, text);
        }
    }

    #[test]
    fn an_empty_field_chord_captures_its_session_before_unlocking() {
        let root = dir("push-empty-captured-selection");
        let (c, fake) = send_hub(&root);
        let view = opened(&c, queued("❯ original", true));
        let changed = c.clone();
        c.lock().queue.after_release = Some(Box::new(move || {
            changed.open(Some(SessionRef {
                computer: "studio".into(),
                key: "run:run-2".into(),
                name: "claude/sample-project/other".into(),
                archive_id: None,
                subagent: None,
            }));
            let mut other = state(queued("❯ other", true));
            other.activity[0].key = "run:run-2".into();
            changed.observe(&other);
        }));
        assert_eq!(
            c.send_push(view.handle, 1, view.text_epoch, String::new(), 0, 0),
            SendReply::Busy
        );
        wait_for(|| send_nows(&fake) == 1);
        let calls = sent(&fake);
        assert_eq!(calls[0].0.name, "claude/sample-project/review");
        assert_eq!(calls[0].0.run_id, "run-1");
        assert_eq!(
            c.current().unwrap().key.as_str(),
            "studio\nclaude/sample-project/other"
        );
    }

    #[test]
    fn a_push_refused_by_the_held_send_hands_the_editor_back() {
        let root = dir("push-held-busy-hand-back");
        let (c, fake) = send_hub(&root);
        let view = opened(&c, working());
        c.lock().queue.before_send = Some(Box::new(|hub| {
            let key = hub.open_key.as_ref().unwrap();
            hub.composers.get_mut(key).unwrap().attaching = true;
        }));
        assert_eq!(
            c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5),
            SendReply::Busy
        );
        let view_after = c.current().unwrap();
        assert_eq!(view_after.text, "later");
        assert!(view_after.text_epoch > view.text_epoch);
        assert!(sent(&fake).is_empty());
    }

    fn required_card_blocks_push(text: &str) {
        let root = dir(if text.is_empty() {
            "required-empty-push"
        } else {
            "required-text-push"
        });
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        let mut details = queued("❯ earlier", true);
        details["pending_questions"] = json!([crate::composer::questions::tests::card_json(
            "tool:required"
        )]);
        opened(&c, details);
        let view = crate::composer::questions::tests::with_card(&c);
        assert!(!view.input_visible);
        assert_eq!(
            c.send_push(view.handle, 1, view.text_epoch, text.into(), 5, 5),
            SendReply::Busy
        );
        let next = c.current().unwrap();
        assert!(
            next.text_epoch > view.text_epoch,
            "the hidden editor is handed back"
        );
        assert_eq!(next.text, text);
        let hub = c.lock();
        assert!(hub.queue.pusher.active().is_none(), "no push was armed");
        assert!(
            hub.queue.sending.is_none(),
            "no native queue action was claimed"
        );
        drop(hub);
        assert!(sent(&fake).is_empty());
        assert!(c.take_events().is_empty());
    }

    #[test]
    fn a_required_card_blocks_push_of_text() {
        required_card_blocks_push("later");
    }

    #[test]
    fn a_required_card_blocks_push_of_an_empty_queue_chord() {
        required_card_blocks_push("");
    }

    #[test]
    fn queue_commands_refuse_a_child_webview() {
        assert_eq!(main_origin("main"), Ok(()));
        assert!(main_origin("native-ui").is_err());
    }

    #[test]
    fn an_explicit_queue_action_cannot_consume_another_sessions_push() {
        let root = dir("push-other-session");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(submitted);
        opened(&c, queued("❯ later", true));
        c.lock().queue.pusher.arm(
            "studio\nclaude/sample-project/other",
            "later",
            1_793_000_100.0,
        );
        assert!(c.send_queue_now(None));
        wait_for(|| c.lock().queue.sending.is_none());
        assert_eq!(
            c.lock().queue.pusher.active().unwrap().key,
            "studio\nclaude/sample-project/other"
        );
    }

    #[test]
    fn a_push_never_arms_while_send_is_disabled() {
        let root = dir("push-send-disabled");
        let (c, fake) = send_hub(&root);
        let view = opened(&c, working());
        *lock(&fake.online) = false;
        assert_eq!(
            c.send_push(view.handle, 1, view.text_epoch, "later".into(), 5, 5),
            SendReply::Busy
        );
        assert!(c.lock().queue.pusher.active().is_none());
        assert!(sent(&fake).is_empty());
    }
}
