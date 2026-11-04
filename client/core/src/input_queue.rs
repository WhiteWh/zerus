//! The agent's native input queue: Qt's "Queued in agent" panel with Send now
//! (`ActivityView`, `SessionsWindow::sendQueuedNow`), Send and push now
//! (`armPush` through `endPush` in `tray/src/SessionsWindowShortcuts.cpp`),
//! and Remove for OpenCode's not-delivered inbox messages
//! (`SessionsWindow::queuedMessageRemovable`).
//!
//! The queue is the inspection's `input_queue`: `{id, text, can_send_now,
//! hint}`, as `hgs` reads it off the agent's terminal. A push never resends
//! text: it only sends the queue snapshot that holds its message.
//!
//! One deviation from Qt: `messageStart` counts the first 24 characters in
//! UTF-16 units and may cut a surrogate pair at that limit; [`message_start`]
//! counts the same units but keeps such a character whole.

use crate::actions::Outcome;
use serde::Serialize;
use serde_json::Value;

/// `sendQueuedNow`'s refusal, an info notice.
pub const NOTHING_QUEUED: &str = "Nothing queued can be sent now";
/// How long a push waits for its message to reach the agent and be offered.
pub const PUSH_SECONDS: f64 = 20.0;

pub const REMOVE_TITLE: &str = "Remove queued message";
pub const REMOVE_TEXT: &str = "Remove this queued message? It is still waiting in OpenCode's queue and runs first with your next message unless you remove it.";
pub const REMOVE_CHANGED: &str = "The queued message changed. Check Activity before removing it.";
pub const REMOVED: &str = "Queued message removed.";
/// Under a not-delivered message: OpenCode keeps it and runs it first.
pub const NOT_DELIVERED_HINT: &str =
    "Still waiting in OpenCode's queue. It runs first with your next message unless you remove it.";

const REQUESTED: &str = "Send now requested. Waiting for the agent to accept the queue.";
const STALE: [&str; 2] = [
    "hgs: The native queue is no longer visible; nothing was sent",
    "The native queue is no longer visible; nothing was sent",
];
const STALE_NOTICE: &str = "The native queue is no longer visible; nothing was sent. Activity is being refreshed; check Terminal or the current queue before retrying.";

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The "Queued in agent" panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct QueueView {
    pub id: String,
    /// Exact inspection identity for display-only receipt reconciliation.
    pub run_id: String,
    pub conversation_id: String,
    /// The queue as the agent's terminal shows it, plain text.
    pub text: String,
    /// The Send now tooltip, without the window chord.
    pub hint: String,
    pub can_send_now: bool,
    pub enabled: bool,
    pub label: String,
}

/// The panel of a tracked session's queue; None without one. Offline or after
/// a failed inspection nothing can be sent now; `sending` is a Send now of
/// this session in flight.
pub fn queue_view(
    details: &Value,
    tracked: bool,
    online: bool,
    inspect_error: bool,
    sending: bool,
) -> Option<QueueView> {
    let queue = &details["input_queue"];
    let id = text(queue, "id");
    if !tracked || id.is_empty() {
        return None;
    }
    let can_send_now = flag(queue, "can_send_now") && online && !inspect_error;
    Some(QueueView {
        id: id.to_owned(),
        run_id: text(details, "run_id").to_owned(),
        conversation_id: text(details, "conversation_id").to_owned(),
        text: text(queue, "text").to_owned(),
        hint: text(queue, "hint").to_owned(),
        can_send_now,
        enabled: can_send_now && !sending,
        label: if sending {
            "Sending\u{2026}"
        } else {
            "Send now"
        }
        .to_owned(),
    })
}

/// Whether Send now may go for `queue_id` (the current queue when empty):
/// online, and the queue still has that id and can be sent now.
pub fn can_send_now(details: &Value, online: bool, queue_id: &str) -> bool {
    let queue = &details["input_queue"];
    let id = if queue_id.is_empty() {
        text(queue, "id")
    } else {
        queue_id
    };
    online && !id.is_empty() && text(queue, "id") == id && flag(queue, "can_send_now")
}

/// A notice the window shows; a transient one goes away by itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub text: String,
    pub error: bool,
    pub transient: bool,
}

impl Notice {
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
            transient: false,
        }
    }
}

/// The notice after Send now (`queueSendFinished`).
pub fn send_now_notice(outcome: &Outcome) -> Notice {
    match outcome {
        Outcome::Done { .. } => Notice::info(REQUESTED),
        Outcome::Failed { detail } | Outcome::Uncertain { detail } => {
            if STALE.contains(&detail.trim()) {
                Notice {
                    text: STALE_NOTICE.to_owned(),
                    error: true,
                    transient: true,
                }
            } else {
                Notice::error(detail.clone())
            }
        }
    }
}

/// What a native queue shows of a message as typed (`messageStart`): the
/// attachment references before its first text, and the first 24 non-space
/// characters of that text, whitespace collapsed. `whole`: the text ends
/// there. Empty for a message of attachments only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessageStart {
    pub references: Vec<String>,
    pub text: String,
    pub whole: bool,
}

/// `QString::simplified`: trimmed, each run of white space one space.
fn simplified(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `[Image #N]` or `[File #N]` at or after byte `from`.
fn find_reference(message: &str, from: usize) -> Option<(usize, usize)> {
    let mut search = from;
    while let Some(found) = message[search..].find('[') {
        let start = search + found;
        let rest = &message[start + 1..];
        let after = rest
            .strip_prefix("Image #")
            .or_else(|| rest.strip_prefix("File #"));
        if let Some(after) = after {
            let digits = after.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 && after[digits..].starts_with(']') {
                let end = message.len() - after.len() + digits + 1;
                return Some((start, end));
            }
        }
        search = start + 1;
    }
    None
}

/// `messageStart` (SessionsWindowShortcuts.cpp). The 24 characters are
/// counted in UTF-16 units as Qt counts them; a character outside the Basic
/// Multilingual Plane at the limit is kept whole.
pub fn message_start(message: &str) -> MessageStart {
    let mut start = MessageStart::default();
    let mut offset = 0;
    loop {
        let reference = find_reference(message, offset);
        let segment = match reference {
            Some((at, _)) => &message[offset..at],
            None => &message[offset..],
        };
        let text = simplified(segment);
        if !text.is_empty() {
            let mut count = 0;
            let mut end = 0;
            for (at, c) in text.char_indices() {
                if count >= 24 {
                    break;
                }
                end = at + c.len_utf8();
                if !c.is_whitespace() {
                    count += c.len_utf16();
                }
            }
            start.whole = end == text.len();
            start.text = text[..end].to_owned();
            return start;
        }
        let Some((at, end)) = reference else {
            return MessageStart::default();
        };
        start.references.push(message[at..end].to_owned());
        offset = end;
    }
}

/// The location the CLI pastes after a reference, `(Read the attached file at
/// "PATH")` or `(Inspect the attached image at "PATH")`, with the space a
/// terminal wrap may add before it or its parenthesis: its length at the
/// start of `text`.
fn pasted_location(text: &str) -> Option<usize> {
    let mut at = usize::from(text.starts_with(" ("));
    let rest = text[at..].strip_prefix('(')?;
    at += 1;
    let phrase = ["Read the attached file", "Inspect the attached image"]
        .into_iter()
        .find(|phrase| rest.starts_with(phrase))?;
    at += phrase.len();
    at += text[at..].strip_prefix(" at \"").map(|_| 5)?;
    // A JSON string: any character but a quote or backslash, or an escape.
    let mut chars = text[at..].char_indices();
    let close = loop {
        match chars.next()? {
            (offset, '"') => break at + offset,
            (_, '\\') => {
                chars.next()?;
            }
            _ => {}
        }
    };
    at = close + 1;
    if text[at..].starts_with(" )") {
        Some(at + 2)
    } else if text[at..].starts_with(')') {
        Some(at + 1)
    } else {
        None
    }
}

/// `itemHolds`: the item begins with the message's references (each maybe
/// followed by its pasted location) and then its start; a whole short text
/// also ends a word there.
fn item_holds(item: &str, start: &MessageStart) -> bool {
    let shown = simplified(item);
    let mut at = 0;
    let skip_space = |at: &mut usize| {
        if shown[*at..].starts_with(' ') {
            *at += 1;
        }
    };
    for reference in &start.references {
        skip_space(&mut at);
        if !shown[at..].starts_with(reference.as_str()) {
            return false;
        }
        at += reference.len();
        if let Some(length) = pasted_location(&shown[at..]) {
            at += length;
        }
    }
    skip_space(&mut at);
    let rest = &shown[at..];
    rest.starts_with(start.text.as_str())
        && (!start.whole
            || rest.len() == start.text.len()
            || rest[start.text.len()..].starts_with(char::is_whitespace))
}

/// Whether the queue as shown holds the message (`queueHolds`). An item is a
/// row that begins with an agent's marker (Claude's and Kimi's U+276F, Codex's
/// U+21B3, or U+203A) with the indented rows that continue it; a header row
/// such as Codex's bulleted "Messages to be submitted after next tool call" is
/// never an item.
pub fn queue_holds(queue: &str, message: &str) -> bool {
    let start = message_start(message);
    if start.text.is_empty() {
        return false;
    }
    let mut items: Vec<String> = Vec::new();
    let mut continued = false;
    for row in queue.split('\n') {
        let text = row.trim();
        let Some(first) = text.chars().next() else {
            continue;
        };
        if matches!(first, '\u{276F}' | '\u{21B3}' | '\u{203A}') {
            items.push(text[first.len_utf8()..].to_owned());
            continued = true;
        } else if continued && row.starts_with(char::is_whitespace) && first != '\u{2022}' {
            if let Some(item) = items.last_mut() {
                item.push(' ');
                item.push_str(text);
            }
        } else {
            continued = false;
        }
    }
    items.iter().any(|item| item_holds(item, &start))
}

/// A push armed by Send and push now (`PendingPush`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Push {
    /// The session key the push was armed in.
    pub key: String,
    /// The run and conversation its send request expects.
    pub run: String,
    pub conversation: String,
    pub text: String,
    /// The send request that carries the message.
    pub request: Option<String>,
    /// That send was confirmed.
    pub confirmed: bool,
    /// When the push gives up, in seconds.
    pub deadline: f64,
}

/// Where the message is before its send request exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PushFacts {
    /// A dialog is open (such as the cold cache warning).
    pub dialog: bool,
    /// The composer is sending this session's message.
    pub sending: bool,
    /// The composer waits for the session to start or compact.
    pub waiting: bool,
}

/// What an inspection means for the push.
#[derive(Clone, Debug, PartialEq)]
pub enum PushStep {
    Wait,
    /// The push ended, with the notice to show.
    End(Option<Notice>),
    /// The push ended: send this queue now.
    SendNow(String),
}

/// Send and push now: after its send is confirmed, the first inspected queue
/// snapshot of the session that holds the message is sent now, within
/// [`PUSH_SECONDS`]. At most one push at a time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pusher {
    push: Option<Push>,
}

impl Pusher {
    pub fn active(&self) -> Option<&Push> {
        self.push.as_ref()
    }

    fn end(&mut self, notice: Option<Notice>) -> Option<Notice> {
        self.push = None;
        notice
    }

    /// Arms a push for the message `text` about to be sent in session `key`
    /// (`armPush`). The hub calls it only for the selected session; a push
    /// already armed is replaced.
    pub fn arm(&mut self, key: &str, text: &str, now: f64) -> Option<Notice> {
        let replaced = self
            .push
            .take()
            .map(|_| Notice::info("The earlier push was replaced by a new one"));
        self.push = Some(Push {
            key: key.to_owned(),
            text: text.to_owned(),
            deadline: now + PUSH_SECONDS,
            ..Push::default()
        });
        replaced
    }

    /// The send request that carries the armed message, with the run and
    /// conversation it expects (`deliverMessage`).
    pub fn dispatched(
        &mut self,
        key: &str,
        text: &str,
        request: &str,
        run: &str,
        conversation: &str,
    ) {
        if let Some(push) = &mut self.push {
            if push.key == key && push.request.is_none() && push.text == text {
                push.request = Some(request.to_owned());
                push.run = run.to_owned();
                push.conversation = conversation.to_owned();
            }
        }
    }

    /// The outcome of a send request (`pushSendFinished`).
    pub fn send_finished(&mut self, request: &str, ok: bool, uncertain: bool) -> Option<Notice> {
        let push = self.push.as_mut()?;
        if request.is_empty() || push.request.as_deref() != Some(request) {
            return None;
        }
        if uncertain {
            self.end(Some(Notice::error(
                "The message was not pushed: delivery is not confirmed",
            )))
        } else if !ok {
            self.end(Some(Notice::error(
                "The message was not pushed: sending did not complete",
            )))
        } else if message_start(&push.text).text.is_empty() {
            // Without text, nothing tells the message apart in the queue.
            self.end(Some(Notice::info(
                "Sent without push: a message without text cannot be found in the agent's queue",
            )))
        } else {
            push.confirmed = true;
            None
        }
    }

    /// An accepted inspection of session `key` (`pushInspected`). `queue_busy`:
    /// another Send now is running, so a later snapshot decides.
    pub fn inspected(
        &mut self,
        key: &str,
        details: &Value,
        now: f64,
        facts: PushFacts,
        queue_busy: bool,
    ) -> PushStep {
        let Some(push) = self.push.as_mut().filter(|push| push.key == key) else {
            return PushStep::Wait;
        };
        // Until its request, starting or compacting may still change the run
        // that takes the message. A run's first message adopts its
        // conversation.
        if push.request.is_some() {
            let conversation = text(details, "conversation_id");
            if text(details, "run_id") != push.run
                || (!push.conversation.is_empty() && conversation != push.conversation)
            {
                return PushStep::End(self.end(Some(Notice::error(
                    "The message was not pushed: the session changed",
                ))));
            }
            push.conversation = conversation.to_owned();
        }
        // A snapshot after the deadline never sends the queue.
        if let Some(notice) = self.check(now, facts) {
            return PushStep::End(Some(notice));
        }
        let Some(push) = self.push.as_ref().filter(|push| push.confirmed) else {
            return PushStep::Wait;
        };
        let queue = &details["input_queue"];
        if !flag(queue, "can_send_now")
            || !queue_holds(text(queue, "text"), &push.text)
            || queue_busy
        {
            return PushStep::Wait;
        }
        self.push = None;
        PushStep::SendNow(text(queue, "id").to_owned())
    }

    /// Another session (or none) became the selected one
    /// (`pushSelectionChanged`); `title` names the push's session.
    pub fn selection_changed(&mut self, key: Option<&str>, title: &str) -> Option<Notice> {
        let push = self.push.as_ref()?;
        let key = key.filter(|key| !key.is_empty());
        if key == Some(push.key.as_str()) {
            return None;
        }
        let notice = match key {
            None => {
                format!("The message to {title} was not pushed: its session is no longer selected")
            }
            Some(_) => {
                format!("The message to {title} was not pushed: another session was selected")
            }
        };
        self.end(Some(Notice::info(notice)))
    }

    /// The push's timer and every inspection (`checkPush`): it ends when its
    /// send ended without reaching the agent, or after [`PUSH_SECONDS`].
    pub fn check(&mut self, now: f64, facts: PushFacts) -> Option<Notice> {
        let push = self.push.as_ref()?;
        // The send is still on its way: in transport, behind a dialog, or
        // waiting for the session to start or compact.
        let sending = match push.request {
            Some(_) => !push.confirmed,
            None => facts.dialog || facts.sending || facts.waiting,
        };
        let notice = if !push.confirmed && !sending {
            Notice::error("The message was not pushed: sending did not complete")
        } else if now < push.deadline {
            return None;
        } else if push.confirmed {
            Notice::info("The message was sent without push: the agent did not offer to send it now within 20 seconds")
        } else if push.request.is_some() || facts.waiting {
            Notice::info(
                "The message will be sent without push: it was not delivered within 20 seconds",
            )
        } else {
            Notice::info("The message was not pushed: it was not sent within 20 seconds")
        };
        self.end(Some(notice))
    }
}

/// What decides whether an OpenCode inbox message can be removed.
#[derive(Clone, Copy, Debug)]
pub struct InboxFacts<'a> {
    /// The session's agent command.
    pub cmd: &'a str,
    pub online: bool,
    pub inspect_error: bool,
    /// A removal is running (one at a time across sessions).
    pub busy: bool,
    /// `actions::allowed_with(row, details, "remove_queued")`.
    pub allowed: bool,
    pub details: &'a Value,
    /// The session's Activity events.
    pub events: &'a [Value],
}

/// `queuedMessageRemovable`: an OpenCode session that is online, inspected
/// and allows it, and still reports the message as not delivered.
pub fn inbox_removable(facts: &InboxFacts<'_>, message_id: &str) -> bool {
    facts.cmd == "opencode"
        && facts.online
        && !facts.inspect_error
        && !facts.busy
        && !message_id.is_empty()
        && !text(facts.details, "run_id").is_empty()
        && facts.allowed
        && facts.events.iter().any(|event| {
            text(event, "message_id") == message_id
                && text(event, "delivery_state") == "not_delivered"
        })
}

/// A removal of another session's message that failed (Qt's notice).
pub fn removal_failed_elsewhere(name: &str, error: &str) -> String {
    format!("Could not remove the queued message from {name}: {error}")
}
