//! An agent's questions and approvals, answered from the app: a port of the
//! Qt client's `QuestionCard` (`tray/src/QuestionCard.cpp`), the selection
//! and guards of `SessionsWindow::renderQuestion` and `answerQuestion`, and
//! `QuestionReply` (`tray/src/QuestionReply.h`).
//!
//! The questions are the inspection's `pending_questions`. An answer goes
//! through `hgs answer` ([`crate::actions::Action::Answer`]) only after the
//! layered checks Qt makes, and an uncertain one locks Submit until Allow
//! retry: nothing is ever sent twice by itself.

use crate::cache;
use crate::drafts::{Loaded, StoredDraft};
use crate::text::{Arg, Text};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The review delay before an approval can be granted, while the card is
/// visible and the window active.
pub const REVIEW_MS: u64 = 3000;
/// The draft store's prefix of an answer draft's key.
pub const STORE_PREFIX: &str = "question\n";
pub const CHANGED: &str = "This question changed. Refresh before answering.";
pub const CHANGED_KEPT: &str = "The question changed. Your answers were kept without submitting.";
pub const CONSENT_CHANGED: &str =
    "The question or your answers changed. Your draft was kept; review it before submitting again.";
/// The pending count's tooltip.
pub const PENDING_TIP: &str =
    crate::source!("Questions waiting for an answer, including the questions shown in this card.");
pub const SKIP_TIP: &str = crate::source!(
    "Dismiss this optional question without sending an answer. The agent can continue."
);
pub const RETRY_TIP: &str =
    crate::source!("Check the agent in Terminal before retrying to avoid submitting twice.");

const SAVING: &str = "Saving before submitting\u{2026}";
const RECORDED: &str = "Response recorded.";
const SENT_WAITING: &str = "Answer sent. Waiting for the agent\u{2026}";
const SUBMITTED_CODEX: &str =
    "Answer submitted. Waiting for Codex to record it. You can check the queue above or open Terminal.";
const SUBMITTED_RESTORED: &str =
    "Answer submitted. Waiting for the agent to record it. Check Activity or Terminal.";
const BROADER_NOTE: &str = crate::source!("Applies beyond this request. Asks for confirmation.");
const COLD_TEXT: &str = "Submitting this answer may resume the agent with the entire conversation, increasing token usage, cost or subscription usage. Compaction or clearing can invalidate the pending question. Cancel keeps your answers.";

/// Sources whose questions the agent wrote: their texts are Markdown. Other
/// sources, approvals and trust requests read from a terminal stay literal.
const AUTHORED: [&str; 7] = [
    "hook",
    "codex_async",
    "kimi_wire",
    "kimi_plan_review",
    "dsh_native",
    "opencode_native",
    "claude_native_question",
];
/// Sources only Terminal can answer while the host cannot.
const TERMINAL_ONLY: [&str; 8] = [
    "claude_tool_approval",
    "kimi_tool_approval",
    "claude_terminal_choice",
    "claude_native_question",
    "claude_terminal_safety",
    "claude_usage_limit",
    "startup_terminal_choice",
    "codex_rate_limit",
];
/// Native cards whose body refreshes in place (`refreshNativeBody`).
const REFRESHED: [&str; 3] = [
    "claude_tool_approval",
    "claude_terminal_safety",
    "claude_terminal_choice",
];
/// The fields consent was given for (`consentIdentity`).
const CONSENT_FIELDS: [&str; 16] = [
    "question_id",
    "question_hash",
    "run_id",
    "conversation_id",
    "questions",
    "source",
    "tool",
    "tool_name",
    "tool_call_id",
    "native_question_id",
    "agent_id",
    "answer_transport",
    "approval",
    "approval_choices",
    "trust_request",
    "optional",
];

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `QString::simplified`.
fn simplified(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `limit` UTF-16 units of `value`, never splitting a character.
fn left_units(value: &str, limit: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= limit
        })
        .collect()
}

/// One option of an item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub description: String,
    /// It reaches beyond this one request (`scope: "broader"`).
    pub broader: bool,
}

/// One question of a request, a tab of its card.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub id: String,
    pub question: String,
    pub header: String,
    pub body: String,
    pub multi: bool,
    pub allow_other: bool,
    pub required: bool,
    pub other_label: String,
    pub other_description: String,
    pub options: Vec<Choice>,
    pub default_option_ids: Vec<String>,
    pub default_text: String,
    /// OpenCode's bounds on a multi-select item; 0 is unbounded.
    pub min_items: usize,
    pub max_items: usize,
    /// OpenCode's limit of an answer's characters; 0 is none.
    pub max_length: usize,
}

/// One `pending_questions` entry. `raw` is the entry as listed: the answer
/// pins it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Question {
    pub id: String,
    pub hash: String,
    pub run_id: String,
    pub conversation_id: String,
    pub source: String,
    pub transport: String,
    pub created_at: f64,
    pub optional: bool,
    pub can_answer: bool,
    pub can_skip: bool,
    pub approval: bool,
    pub approval_choices: bool,
    pub trust_request: bool,
    pub plan_review: bool,
    pub unavailable_reason: String,
    pub items: Vec<Item>,
    pub raw: Value,
}

impl Question {
    /// Reads one entry.
    pub fn from_value(raw: &Value) -> Self {
        let opencode = text(raw, "source") == "opencode_native";
        let int = |value: &Value, key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0);
        let items = raw
            .get("questions")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        let multi = flag(item, "multi_select");
                        let options = item
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|options| {
                                options
                                    .iter()
                                    .map(|option| Choice {
                                        id: text(option, "id").to_owned(),
                                        label: text(option, "label").to_owned(),
                                        description: text(option, "description").to_owned(),
                                        broader: text(option, "scope") == "broader",
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let default_option_ids = item
                            .get("default_option_ids")
                            .and_then(Value::as_array)
                            .map(|ids| {
                                ids.iter()
                                    .filter_map(|id| id.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let bound = |key: &str| {
                            if opencode && multi {
                                int(item, key).max(0) as usize
                            } else {
                                0
                            }
                        };
                        Item {
                            id: text(item, "id").to_owned(),
                            question: text(item, "question").to_owned(),
                            header: text(item, "header").to_owned(),
                            body: text(item, "body").to_owned(),
                            multi,
                            allow_other: flag(item, "allow_other"),
                            required: item
                                .get("required")
                                .and_then(Value::as_bool)
                                .unwrap_or(true),
                            other_label: simplified(text(item, "other_label")),
                            other_description: simplified(text(item, "other_description")),
                            options,
                            default_option_ids,
                            default_text: text(item, "default_text").to_owned(),
                            min_items: bound("min_items"),
                            max_items: bound("max_items"),
                            max_length: if opencode {
                                int(item, "max_length").clamp(0, 4096) as usize
                            } else {
                                0
                            },
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let unavailable_reason = ["answer_unavailable_reason", "unavailable_reason"]
            .into_iter()
            .map(|key| text(raw, key).trim())
            .find(|reason| !reason.is_empty())
            .unwrap_or("")
            .to_owned();
        Self {
            id: text(raw, "question_id").to_owned(),
            hash: text(raw, "question_hash").to_owned(),
            run_id: text(raw, "run_id").to_owned(),
            conversation_id: text(raw, "conversation_id").to_owned(),
            source: text(raw, "source").to_owned(),
            transport: text(raw, "answer_transport").to_owned(),
            created_at: raw.get("created_at").and_then(Value::as_f64).unwrap_or(0.0),
            optional: flag(raw, "optional"),
            can_answer: flag(raw, "can_answer"),
            can_skip: flag(raw, "can_skip"),
            approval: flag(raw, "approval"),
            approval_choices: flag(raw, "approval_choices"),
            trust_request: flag(raw, "trust_request"),
            plan_review: flag(raw, "plan_review"),
            unavailable_reason,
            items,
            raw: raw.clone(),
        }
    }

    /// Questions the agent wrote are Markdown; approval and trust disclosures
    /// and terminal panels stay as reported.
    fn literal(&self) -> bool {
        self.approval || self.trust_request || !AUTHORED.contains(&self.source.as_str())
    }

    /// Only Terminal can answer it, and the options show read-only.
    fn read_only(&self) -> bool {
        !self.can_answer && TERMINAL_ONLY.contains(&self.source.as_str())
    }

    /// The native options are listed (all but a synthesized Approve/Deny).
    fn listed(&self) -> bool {
        !self.approval || self.approval_choices
    }

    /// OpenCode form answers go through its form reply API: they may start
    /// with `/` or `!`.
    fn form_reply(&self) -> bool {
        self.source == "opencode_native" && !self.approval
    }

    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// The inspection's pending questions, in order.
pub fn pending(details: &Value) -> Vec<Question> {
    details
        .get("pending_questions")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.is_object())
                .map(Question::from_value)
                .collect()
        })
        .unwrap_or_default()
}

/// The session hint when the questions could not be read.
pub fn questions_error(details: &Value) -> Option<String> {
    let error = text(details, "pending_questions_error");
    (!error.is_empty()).then(|| format!("Could not load agent questions: {error}. Check Terminal."))
}

/// What the card shows: the question, how many are pending, and the queue of
/// optional ones (newest first).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub question: Option<Question>,
    /// Questions waiting, counting each item of a request.
    pub pending: usize,
    /// The optional requests' ids, newest first.
    pub optional: Vec<String>,
    pub index: usize,
    /// The remembered optional request; None tells the hub to forget it.
    pub chosen: Option<String>,
}

impl Selection {
    /// The composer's editor shows without a question or for an optional one.
    pub fn input_visible(&self) -> bool {
        self.question
            .as_ref()
            .is_none_or(|question| question.optional)
    }
}

/// `renderQuestion`: a required request wins (one that can be answered over
/// one that cannot); otherwise the remembered optional one, moved by
/// `navigation` (-1, 0 or +1). `submitted` holds for requests already
/// submitted from here.
pub fn select(
    questions: &[Question],
    submitted: &dyn Fn(&Question) -> bool,
    chosen: Option<&str>,
    navigation: i32,
) -> Selection {
    let mut required: Option<&Question> = None;
    let mut optional: Vec<&Question> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut count = 0;
    for question in questions {
        // A card without items still explains itself when it cannot be
        // answered here (hgs empties an oversized OpenCode form).
        let answerable = question
            .raw
            .get("can_answer")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if question.id.is_empty()
            || seen.contains(&question.id)
            || (question.items.is_empty() && answerable)
        {
            continue;
        }
        seen.insert(question.id.clone());
        if submitted(question) {
            continue;
        }
        count += question.items.len().max(1);
        if question.optional {
            optional.push(question);
        } else if required.is_none_or(|first| !first.can_answer && question.can_answer) {
            required = Some(question);
        }
    }
    optional.sort_by(|a, b| {
        b.created_at
            .partial_cmp(&a.created_at)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut index = 0;
    let mut remembered = None;
    if let Some(chosen) = chosen.filter(|chosen| !chosen.is_empty()) {
        if let Some(found) = optional.iter().position(|question| question.id == chosen) {
            index = found;
            remembered = Some(chosen.to_owned());
        }
    }
    let question = match required {
        Some(required) => Some(required.clone()),
        None if !optional.is_empty() => {
            if navigation != 0 {
                let moved = index as i64 + i64::from(navigation);
                index = moved.clamp(0, optional.len() as i64 - 1) as usize;
                remembered = Some(optional[index].id.clone());
            }
            Some(optional[index].clone())
        }
        None => None,
    };
    Selection {
        question,
        pending: count,
        optional: optional
            .iter()
            .map(|question| question.id.clone())
            .collect(),
        index,
        chosen: remembered,
    }
}

/// The session as the card's availability reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnswerFacts {
    pub online: bool,
    /// The session's state (`running`, ...).
    pub state: String,
    /// Terminating, a pending action, or a rename is under way.
    pub updating: bool,
    pub inspect_error: bool,
}

fn live(details: &Value) -> bool {
    text(details, "runtime_state") == "live" && text(details, "process_state") == "running"
}

/// Why the card cannot be answered now, in Qt's order; None when it can.
pub fn unavailable(
    facts: &AnswerFacts,
    question: &Question,
    details: &Value,
) -> Option<&'static str> {
    if !facts.online {
        Some("Machine unavailable. Your answers stay here.")
    } else if facts.state != "running" {
        Some("This session is not running.")
    } else if facts.updating {
        Some("Wait for the session update to finish.")
    } else if facts.inspect_error {
        Some("Refresh the session before answering.")
    } else if question.run_id != text(details, "run_id")
        || question.conversation_id != text(details, "conversation_id")
    {
        Some("Refresh the pending question before answering.")
    } else if !live(details) {
        Some("The agent is no longer running.")
    } else {
        None
    }
}

/// The JSON array `[session_key, id, hash]` as Qt writes it compactly.
fn identity(session_key: &str, id: &str, hash: &str) -> String {
    serde_json::to_string(&[session_key, id, hash]).unwrap_or_default()
}

/// The draft store key of an answer draft (`"question\n" + draftIdentity`);
/// None for a question without an id.
pub fn store_key(session_key: &str, question: &Question) -> Option<String> {
    if question.id.is_empty() {
        return None;
    }
    Some(format!(
        "{STORE_PREFIX}{}\n{}\n{}",
        identity(session_key, &question.id, &question.hash),
        question.run_id,
        question.conversation_id
    ))
}

/// The name Saved drafts shows for an answer draft.
pub fn label(session_key: &str) -> String {
    format!("{} (question answer)", session_key.replace('\n', " / "))
}

/// One item's answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemAnswer {
    pub options: BTreeSet<String>,
    pub text: String,
    /// The free-text "Other" choice is selected.
    pub other: bool,
}

/// One edit from the card.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Edit {
    Option {
        item: String,
        option: String,
        checked: bool,
    },
    Other {
        item: String,
        checked: bool,
    },
    Text {
        item: String,
        text: String,
    },
}

/// The answer being written to one request, with its delivery state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnswerDraft {
    pub answers: BTreeMap<String, ItemAnswer>,
    /// Items holding untouched OpenCode defaults, never saved.
    pub seeded: BTreeSet<String>,
    pub notice: String,
    /// The broader option the user is asked to confirm.
    pub confirm: String,
    pub storage_error: String,
    pub label: String,
    pub page: usize,
    pub sending: bool,
    pub submitted: bool,
    pub answered: bool,
    pub error: bool,
    pub uncertain: bool,
}

/// What a submission's consent is compared with, the notice aside
/// (`submissionSnapshot`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub answers: BTreeMap<String, ItemAnswer>,
    pub page: usize,
    pub confirm: String,
    pub sending: bool,
    pub submitted: bool,
    pub answered: bool,
    pub uncertain: bool,
    pub storage_error: String,
}

/// The first `limit` characters of `value`.
fn clip_characters(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

impl AnswerDraft {
    /// A loaded answer draft (`restoredDraft`); `label` names it.
    pub fn from_loaded(loaded: &Loaded, label: &str) -> Self {
        let state = &loaded.draft.form_state;
        let mut draft = Self {
            storage_error: loaded.storage_error.clone().unwrap_or_default(),
            notice: loaded.notice.clone().unwrap_or_default(),
            uncertain: loaded.draft.uncertain,
            error: loaded.draft.uncertain,
            label: label.to_owned(),
            page: state.get("page").and_then(Value::as_u64).unwrap_or(0) as usize,
            answered: state
                .get("answered")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            submitted: state
                .get("submitted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ..Self::default()
        };
        if draft.submitted {
            draft.notice = SUBMITTED_RESTORED.to_owned();
        }
        if draft.answered {
            draft.notice = RECORDED.to_owned();
        }
        for answer in state
            .get("answers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let options = answer
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| option.as_str().map(str::to_owned))
                .collect();
            draft.answers.insert(
                text(answer, "id").to_owned(),
                ItemAnswer {
                    options,
                    text: text(answer, "text").to_owned(),
                    other: flag(answer, "other"),
                },
            );
        }
        draft
    }

    /// The draft as saved (`saveDraft`): untouched defaults are left out,
    /// and an answered one keeps no answers or text.
    pub fn stored(&self) -> StoredDraft {
        let mut answers = Vec::new();
        let mut texts = Vec::new();
        for (id, answer) in &self.answers {
            if self.seeded.contains(id) {
                continue;
            }
            answers.push(json!({
                "id": id,
                "text": answer.text,
                "other": answer.other,
                "options": answer.options.iter().collect::<Vec<_>>(),
            }));
            if !answer.text.is_empty() {
                texts.push(answer.text.as_str());
            }
        }
        let mut stored = StoredDraft {
            label: self.label.clone(),
            sending: self.sending,
            uncertain: self.uncertain,
            ..StoredDraft::default()
        };
        if !self.answered && !self.submitted {
            stored.draft.text = texts.join("\n");
        }
        let mut state = Map::new();
        state.insert(
            "answers".into(),
            Value::Array(if self.answered { Vec::new() } else { answers }),
        );
        state.insert("page".into(), json!(self.page));
        state.insert("answered".into(), json!(self.answered));
        state.insert("submitted".into(), json!(self.submitted));
        stored.form_state = state;
        stored
    }

    /// Edits and a new submission wait while it is sent or recorded.
    pub fn locked(&self) -> bool {
        self.sending || self.submitted || self.answered
    }

    /// Seeds OpenCode's reported defaults into items without an answer yet
    /// (`rebuild`): shown and sent, but saved only once edited or submitted.
    pub fn seed(&mut self, question: &Question) {
        if question.source != "opencode_native" || self.locked() {
            return;
        }
        for item in &question.items {
            if item.id.is_empty() || self.answers.contains_key(&item.id) {
                continue;
            }
            let mut seeded = ItemAnswer::default();
            for id in &item.default_option_ids {
                let offered = item.options.iter().any(|option| &option.id == id);
                if !offered || (!item.multi && !seeded.options.is_empty()) {
                    continue;
                }
                seeded.options.insert(id.clone());
            }
            if item.allow_other && item.options.is_empty() {
                let limit = if item.max_length > 0 {
                    item.max_length
                } else {
                    4096
                };
                seeded.text = clip_characters(&item.default_text, limit);
            }
            if !seeded.options.is_empty() || !seeded.text.is_empty() {
                self.answers.insert(item.id.clone(), seeded);
                self.seeded.insert(item.id.clone());
            }
        }
    }

    /// A Codex optional question whose answer the host recorded as submitted
    /// (`restoreSubmittedAnswer`): its answers fill the draft the first time.
    /// True when the answers changed (save the draft then).
    pub fn restore_submitted(&mut self, question: &Question) -> bool {
        let delivery = &question.raw["answer_delivery"];
        let echoed = question.source == "codex_async"
            && question.optional
            && text(delivery, "status") == "submitted"
            && delivery.get("question_id") == question.raw.get("question_id")
            && delivery.get("question_hash") == question.raw.get("question_hash")
            && delivery.get("run_id") == question.raw.get("run_id")
            && delivery.get("conversation_id") == question.raw.get("conversation_id");
        if !echoed {
            return false;
        }
        let mut changed = false;
        if !self.submitted {
            self.answers.clear();
            for answer in delivery
                .get("answers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let options: BTreeSet<String> = answer
                    .get("selected_option_ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|option| option.as_str().map(str::to_owned))
                    .collect();
                let other = options.is_empty();
                self.answers.insert(
                    text(answer, "question_id").to_owned(),
                    ItemAnswer {
                        options,
                        text: text(answer, "text").to_owned(),
                        other,
                    },
                );
            }
            changed = true;
        }
        self.submitted = true;
        self.sending = false;
        self.error = false;
        self.uncertain = false;
        self.notice = SUBMITTED_CODEX.to_owned();
        changed
    }

    /// One edit of the card (`capture`); false when nothing changed or the
    /// card cannot be edited now. Every applied edit disarms Confirm and makes
    /// all answers the user's own (none stays a seeded default).
    pub fn apply(&mut self, question: &Question, edit: Edit) -> bool {
        if self.locked() || question.read_only() {
            return false;
        }
        let id = match &edit {
            Edit::Option { item, .. } | Edit::Other { item, .. } | Edit::Text { item, .. } => item,
        };
        let Some(item) = question.item(id) else {
            return false;
        };
        let mut answer = self.answers.get(&item.id).cloned().unwrap_or_default();
        match edit {
            Edit::Option {
                option, checked, ..
            } => {
                if !item.options.iter().any(|choice| choice.id == option) {
                    return false;
                }
                if item.multi {
                    if checked {
                        answer.options.insert(option);
                    } else {
                        answer.options.remove(&option);
                    }
                } else if checked {
                    // A radio button: the others and Other clear.
                    answer.options = BTreeSet::from([option]);
                    answer.other = false;
                } else {
                    return false;
                }
            }
            Edit::Other { checked, .. } => {
                if !item.allow_other || item.options.is_empty() {
                    return false;
                }
                if item.multi {
                    answer.other = checked;
                } else if checked {
                    answer.other = true;
                    answer.options.clear();
                } else {
                    return false;
                }
            }
            Edit::Text { text, .. } => {
                if !item.allow_other {
                    return false;
                }
                let units = text.encode_utf16().count();
                let max_units = if item.max_length > 0 {
                    2 * item.max_length
                } else {
                    4096
                };
                if units > max_units
                    || (item.max_length > 0 && text.chars().count() > item.max_length)
                {
                    return false;
                }
                answer.text = text;
            }
        }
        self.answers.insert(item.id.clone(), answer);
        // Qt's capture reads every form.
        for item in &question.items {
            self.answers.entry(item.id.clone()).or_default();
        }
        self.confirm.clear();
        self.seeded.clear();
        true
    }

    /// The first Submit of a broader approval option only asks for Confirm
    /// (`submit`): true then. Otherwise the shown answers become the user's
    /// own and false says to send.
    pub fn confirm_step(&mut self, question: &Question) -> bool {
        if question.approval_choices {
            if let Some(broader) = broader(question, self) {
                if self.confirm != broader {
                    self.confirm = broader;
                    return true;
                }
            }
        }
        self.seeded.clear();
        false
    }

    /// The durable save before the answer goes (`prepareSendingAsync`).
    pub fn begin_sending(&mut self) {
        self.sending = true;
        self.error = false;
        self.notice = SAVING.to_owned();
    }

    /// That save failed: nothing was sent.
    pub fn sending_failed(&mut self) {
        if !self.submitted && !self.answered {
            self.sending = false;
            self.notice.clear();
        }
    }

    /// The answer failed (`setError`); an uncertain one locks Submit until
    /// [`AnswerDraft::allow_retry`].
    pub fn set_error(&mut self, detail: &str, uncertain: bool) {
        self.sending = false;
        if self.submitted || self.answered {
            return;
        }
        self.error = true;
        self.uncertain = uncertain;
        self.notice = if uncertain {
            format!("Delivery is not confirmed. Check Terminal before retrying. {detail}")
        } else {
            detail.to_owned()
        };
    }

    /// The host recorded the answer.
    pub fn set_answered(&mut self, optional: bool) {
        self.sending = false;
        self.answered = true;
        self.error = false;
        self.uncertain = false;
        self.notice = if optional { RECORDED } else { SENT_WAITING }.to_owned();
    }

    /// Codex took the answer and will record it.
    pub fn set_submitted(&mut self) {
        self.sending = false;
        self.submitted = true;
        self.error = false;
        self.uncertain = false;
        self.notice = SUBMITTED_CODEX.to_owned();
    }

    /// Allow retry, after the user checked the agent.
    pub fn allow_retry(&mut self) {
        self.uncertain = false;
        self.error = false;
        self.notice.clear();
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            answers: self.answers.clone(),
            page: self.page,
            confirm: self.confirm.clone(),
            sending: self.sending,
            submitted: self.submitted,
            answered: self.answered,
            uncertain: self.uncertain,
            storage_error: self.storage_error.clone(),
        }
    }
}

/// One answer as `hgs answer` takes it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AnswerItem {
    Skip {
        question_id: String,
        skip: bool,
    },
    Choice {
        question_id: String,
        selected_option_ids: Vec<String>,
        text: String,
    },
}

impl AnswerItem {
    fn is_skip(&self) -> bool {
        matches!(self, Self::Skip { skip: true, .. })
    }
}

/// True when every answer is a Skip.
pub fn all_skips(answers: &[AnswerItem]) -> bool {
    !answers.is_empty() && answers.iter().all(AnswerItem::is_skip)
}

/// Why a custom answer cannot be sent (`customAnswerProblem`).
pub fn custom_answer_problem(text: &str, form_reply: bool) -> Option<&'static str> {
    if text.len() > 4096 {
        return Some(crate::source!("Keep your custom answer under 4 KiB."));
    }
    if text.contains(['\n', '\r']) {
        return Some(crate::source!(
            "Use one line for a custom answer. Multiline input is available in Terminal."
        ));
    }
    // Qt checks UTF-16 units; control characters are all in the BMP.
    if text
        .chars()
        .any(|c| (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32)))
    {
        return Some(crate::source!(
            "Remove control characters from your custom answer."
        ));
    }
    let trimmed = text.trim();
    if !form_reply && (trimmed.starts_with('/') || trimmed.starts_with('!')) {
        return Some(crate::source!(
            "Use Terminal for answers starting with / or !."
        ));
    }
    None
}

/// An item's answer as sent, and whether it is complete.
fn item_answer(question: &Question, item: &Item, answer: &ItemAnswer) -> (AnswerItem, bool, bool) {
    // Source option order, not the set's.
    let selected: Vec<String> = item
        .options
        .iter()
        .filter(|option| answer.options.contains(&option.id))
        .map(|option| option.id.clone())
        .collect();
    let custom = item.allow_other && (item.options.is_empty() || answer.other);
    let text = if custom {
        answer.text.trim().to_owned()
    } else {
        String::new()
    };
    let count = selected.len() + usize::from(!text.is_empty());
    let problem = custom_answer_problem(&text, question.form_reply()).is_some();
    let complete = !((selected.is_empty() && text.is_empty())
        || (custom && text.is_empty())
        || (!item.multi && selected.len() > 1)
        || problem
        || item.id.is_empty()
        || (item.min_items > 0 && count < item.min_items)
        || (item.max_items > 0 && count > item.max_items));
    // The tab's check mark (`updateControls`).
    let filled = (!answer.options.is_empty() || !text.is_empty())
        && (!custom || !text.is_empty())
        && !problem;
    (
        AnswerItem::Choice {
            question_id: item.id.clone(),
            selected_option_ids: selected,
            text,
        },
        complete,
        filled,
    )
}

/// The answers to send and whether every item is complete (`answers`).
pub fn answers(question: &Question, draft: &AnswerDraft) -> (Vec<AnswerItem>, bool) {
    let mut complete = !question.items.is_empty();
    let mut result = Vec::with_capacity(question.items.len());
    for item in &question.items {
        let answer = draft.answers.get(&item.id).cloned().unwrap_or_default();
        let (sent, done, _) = item_answer(question, item, &answer);
        complete &= done;
        result.push(sent);
    }
    (result, complete)
}

/// Skip: one Skip per item.
pub fn skip_answers(question: &Question) -> Vec<AnswerItem> {
    question
        .items
        .iter()
        .map(|item| AnswerItem::Skip {
            question_id: item.id.clone(),
            skip: true,
        })
        .collect()
}

/// Approve once (`allow`) or Deny (`deny`) of an approval card.
pub fn decision(question: &Question, allow: bool) -> Option<Vec<AnswerItem>> {
    let item = question.items.first().filter(|_| question.approval)?;
    Some(vec![AnswerItem::Choice {
        question_id: item.id.clone(),
        selected_option_ids: vec![if allow { "allow" } else { "deny" }.to_owned()],
        text: String::new(),
    }])
}

/// The label of the first chosen option that reaches beyond this request
/// (`broaderSelection`).
pub fn broader(question: &Question, draft: &AnswerDraft) -> Option<String> {
    question.items.iter().find_map(|item| {
        let chosen = draft.answers.get(&item.id)?;
        item.options
            .iter()
            .find(|option| option.broader && chosen.options.contains(&option.id))
            .map(|option| option.label.clone())
    })
}

/// A body refresh of the same native card keeps the form, its choices and
/// the review delay (`refreshNativeBody`).
pub fn same_form(old: &Question, new: &Question) -> bool {
    if !REFRESHED.contains(&new.source.as_str())
        || new.source != old.source
        || new.hash.is_empty()
        || new.hash != old.hash
    {
        return false;
    }
    let items = |question: &Question| {
        question.raw["questions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    let (old_items, new_items) = (items(old), items(new));
    if old_items.len() != new_items.len() || new_items.len() != old.items.len() {
        return false;
    }
    old_items
        .iter()
        .zip(&new_items)
        .all(|(old_item, new_item)| {
            let without_body = |item: &Value| {
                let mut item = item.clone();
                if let Some(object) = item.as_object_mut() {
                    object.remove("body");
                }
                item
            };
            if without_body(old_item) != without_body(new_item) {
                return false;
            }
            let (old_body, new_body) = (text(old_item, "body"), text(new_item, "body"));
            // A changed body needs the old one shown, and a new one to show.
            old_body == new_body
                || (!new_body.is_empty()
                    && !old_body.is_empty()
                    && old_body != text(old_item, "question"))
        })
}

/// What the card's surroundings say about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardContext {
    pub available: bool,
    pub unavailable_reason: String,
    /// The answer draft is still loading.
    pub loading: bool,
    /// An answer of this request is being sent.
    pub in_flight: bool,
    /// [`Selection::pending`].
    pub pending: usize,
    pub queue_index: usize,
    pub queue_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ButtonView {
    pub visible: bool,
    pub enabled: bool,
    pub label: Text,
    /// While the review delay runs the button shows "{review_label} (N)" and
    /// cannot be clicked.
    pub review_label: Option<Text>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OptionView {
    pub id: String,
    /// The agent's label as it is, or the card's own "Other".
    pub label: Text,
    pub description: Text,
    /// The description is Markdown.
    pub markdown: bool,
    pub checked: bool,
    pub enabled: bool,
    pub visible: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TextView {
    pub visible: bool,
    pub enabled: bool,
    pub value: String,
    pub placeholder: Text,
    pub accessible_name: Text,
    /// The field's limit in UTF-16 units.
    pub max_units: usize,
    /// OpenCode's limit in characters; 0 is none.
    pub max_chars: usize,
}

/// One item's tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FormView {
    pub id: String,
    pub tab: Text,
    pub prompt: String,
    /// Empty when it repeats the prompt.
    pub body: String,
    /// The prompt and body are Markdown.
    pub markdown: bool,
    pub monospace_body: bool,
    pub multi: bool,
    pub mode: Text,
    pub mode_visible: bool,
    pub options: Vec<OptionView>,
    pub other: Option<OptionView>,
    pub text: Option<TextView>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct QueueNav {
    pub previous: bool,
    pub next: bool,
    pub previous_tip: Text,
    pub next_tip: Text,
}

/// The question card.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CardView {
    pub heading: Text,
    /// "N of M answered", "Awaiting agent", or None when hidden.
    pub progress: Option<Text>,
    /// "N pending", or None.
    pub pending: Option<Text>,
    pub pending_tip: Text,
    pub nav: Option<QueueNav>,
    pub tabs_visible: bool,
    pub page: usize,
    pub forms: Vec<FormView>,
    /// The card's notice: its own prose, or a stored/remote message verbatim.
    pub status: Text,
    pub status_error: bool,
    pub available: bool,
    pub locked: bool,
    /// The review delay applies: an approval card.
    pub review: bool,
    pub created_at: f64,
    pub submit: ButtonView,
    pub approve: ButtonView,
    pub deny: ButtonView,
    pub skip: ButtonView,
    pub previous: ButtonView,
    pub next: ButtonView,
    /// Allow retry shows.
    pub retry: bool,
    pub skip_tip: Text,
    pub retry_tip: Text,
}

fn multi_mode(item: &Item) -> Text {
    match (item.min_items, item.max_items) {
        (min, max) if min > 0 && max > 0 => crate::text!("Select %1 to %2", min, max),
        (min, _) if min > 0 => crate::text!("Select at least %1", min),
        (_, max) if max > 0 => crate::text!("Select up to %1", max),
        _ => crate::text!("Select one or more"),
    }
}

/// The card for `question` with `draft` (`rebuild` and `updateControls`).
pub fn card(question: &Question, draft: &AnswerDraft, ctx: &CardContext) -> CardView {
    let locked = draft.locked() || ctx.in_flight || ctx.loading;
    let read_only = question.read_only();
    let literal = question.literal();
    let listed = question.listed();
    let supported = question.can_answer;
    let (_, complete) = answers(question, draft);
    let last = question.items.len().saturating_sub(1);
    let page = draft.page.min(last);
    let choices = question.approval && question.approval_choices;
    let broader = if choices {
        broader(question, draft)
    } else {
        None
    };
    let confirming = broader
        .as_deref()
        .is_some_and(|label| draft.confirm == label);

    let mut answered = 0;
    let forms: Vec<FormView> = question
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let answer = draft.answers.get(&item.id).cloned().unwrap_or_default();
            let (_, _, filled) = item_answer(question, item, &answer);
            answered += usize::from(filled);
            let header = simplified(&item.header);
            let title = if header.is_empty() {
                crate::text!("Question %1", index + 1)
            } else {
                crate::text!("%1: %2", index + 1, left_units(&header, 32))
            };
            let options = item
                .options
                .iter()
                .map(|option| {
                    let description = if !option.broader {
                        Text::verbatim(option.description.clone())
                    } else if option.description.is_empty() {
                        Text::new(BROADER_NOTE)
                    } else {
                        crate::text!(
                            "%1\n%2",
                            Arg::Verbatim(option.description.clone()),
                            Text::new(BROADER_NOTE)
                        )
                    };
                    OptionView {
                        id: option.id.clone(),
                        label: Text::verbatim(option.label.clone()),
                        markdown: !literal && !option.broader && !option.description.is_empty(),
                        description,
                        checked: answer.options.contains(&option.id),
                        enabled: !locked && !read_only,
                        visible: listed,
                    }
                })
                .collect::<Vec<_>>();
            let other = (item.allow_other && !item.options.is_empty()).then(|| OptionView {
                id: String::new(),
                label: if item.other_label.is_empty() {
                    crate::text!("Other")
                } else {
                    Text::verbatim(item.other_label.clone())
                },
                description: if item.other_description.is_empty() {
                    crate::text!("Write your own answer")
                } else {
                    Text::verbatim(item.other_description.clone())
                },
                markdown: false,
                checked: answer.other,
                enabled: !locked && !read_only,
                visible: true,
            });
            let custom = item.options.is_empty() || answer.other;
            let text = item.allow_other.then(|| TextView {
                visible: custom,
                enabled: custom && !locked && !read_only,
                value: answer.text.clone(),
                placeholder: crate::text!("Write your answer…"),
                accessible_name: crate::text!("Your answer to %1", item.question.clone()),
                max_units: if item.max_length > 0 {
                    2 * item.max_length
                } else {
                    4096
                },
                max_chars: item.max_length,
            });
            FormView {
                id: item.id.clone(),
                tab: if filled {
                    crate::text!("✓ %1", title)
                } else {
                    title
                },
                prompt: item.question.clone(),
                body: if item.body != item.question {
                    item.body.clone()
                } else {
                    String::new()
                },
                markdown: !literal,
                monospace_body: literal && question.approval,
                multi: item.multi,
                mode: if item.multi {
                    multi_mode(item)
                } else {
                    crate::text!("Select one")
                },
                mode_visible: !item.options.is_empty() && listed && !question.plan_review,
                options,
                other,
                text,
            }
        })
        .collect();

    let nav = (question.optional && ctx.queue_count > 1 && ctx.queue_index < ctx.queue_count).then(
        || {
            let position = crate::text!(
                "Request %1 of %2, newest first. Draft answers are kept when switching.",
                ctx.queue_index + 1,
                ctx.queue_count
            );
            QueueNav {
                previous: ctx.queue_index > 0,
                next: ctx.queue_index + 1 < ctx.queue_count,
                previous_tip: crate::text!(
                    "%1\n%2",
                    crate::text!("Previous queued question (newer)"),
                    position.clone()
                ),
                next_tip: crate::text!(
                    "%1\n%2",
                    crate::text!("Next queued question (older)"),
                    position
                ),
            }
        },
    );
    let heading = if question.approval || question.trust_request || question.plan_review {
        crate::text!("Agent needs your approval")
    } else if question.optional && nav.is_some() {
        crate::text!(
            "Optional question #%1/%2",
            ctx.queue_index + 1,
            ctx.queue_count
        )
    } else if question.optional {
        crate::text!("Optional question")
    } else {
        crate::text!("Agent needs your answer")
    };
    let awaiting = draft.submitted && !draft.answered;
    let progress = if question.approval || (question.plan_review && !awaiting) {
        None
    } else if awaiting {
        Some(crate::text!("Awaiting agent"))
    } else if forms.is_empty() {
        None
    } else {
        Some(crate::text!("%1 of %2 answered", answered, forms.len()))
    };
    let pending = ctx.pending.max(question.items.len());

    // Stored notices and remote reasons stay as written; the rest is the card's own prose.
    let stored = |value: &str| (!value.is_empty()).then(|| Text::verbatim(left_units(value, 600)));
    let mut notice = if draft.storage_error.is_empty() {
        stored(&draft.notice)
    } else {
        stored(&draft.storage_error)
    };
    if notice.is_none() && !ctx.available {
        notice = stored(&ctx.unavailable_reason);
    }
    if notice.is_none() && !supported {
        notice = stored(&question.unavailable_reason);
    }
    if notice.is_none() && !supported {
        notice = Some(crate::text!("Answer this request in Terminal."));
    }
    if notice.is_none() && ctx.loading {
        notice = Some(crate::text!("Loading your saved answer…"));
    }
    if notice.is_none() {
        for item in &question.items {
            let answer = draft.answers.get(&item.id).cloned().unwrap_or_default();
            if !item.allow_other || (!item.options.is_empty() && !answer.other) {
                continue;
            }
            if let Some(problem) = custom_answer_problem(answer.text.trim(), question.form_reply())
            {
                notice = Some(Text::new(problem));
                break;
            }
        }
    }
    if notice.is_none() && confirming {
        notice = Some(crate::text!(
            "“%1” changes the agent's permissions beyond this request. Select Confirm to send it.",
            left_units(broader.as_deref().unwrap_or_default(), 500)
        ));
    }
    if notice.is_none() && choices {
        notice = Some(crate::text!(
            "Review the request, then choose one of the agent's options."
        ));
    }
    if notice.is_none() && question.plan_review && !complete {
        notice = Some(crate::text!(
            "Review the plan, then choose how the agent continues."
        ));
    }
    let notice = notice.unwrap_or_else(|| {
        if question.approval {
            crate::text!("Approval applies only to this command. Review it before continuing.")
        } else if question.optional {
            crate::text!("The agent can continue. Answer when ready or skip this question.")
        } else if complete {
            crate::text!("Your choices are ready to send.")
        } else {
            crate::text!("Answer each question to continue.")
        }
    });

    let sending = draft.sending || ctx.in_flight;
    let submit_label = if awaiting {
        crate::text!("Submitted")
    } else if sending {
        crate::text!("Submitting…")
    } else if confirming {
        crate::text!("Confirm")
    } else if forms.len() == 1 {
        crate::text!("Submit answer")
    } else {
        crate::text!("Submit answers")
    };
    let buttons = question.approval
        && !choices
        && supported
        && question
            .items
            .first()
            .is_some_and(|item| !item.options.is_empty());
    let can_approve = buttons && ctx.available && supported && !locked && !draft.uncertain;
    CardView {
        heading,
        progress,
        pending: (pending > 0).then(|| crate::text!("%1 pending", pending)),
        pending_tip: Text::new(PENDING_TIP),
        nav,
        tabs_visible: forms.len() > 1,
        page,
        status: notice,
        status_error: draft.error || !draft.storage_error.is_empty(),
        available: ctx.available,
        locked,
        review: question.approval,
        created_at: question.created_at,
        submit: ButtonView {
            visible: !read_only
                && !forms.is_empty()
                && (!question.approval || (choices && supported))
                && page == last,
            enabled: ctx.available && supported && complete && !locked && !draft.uncertain,
            label: submit_label,
            review_label: (choices && !awaiting && !sending).then(|| crate::text!("Submit answer")),
        },
        approve: ButtonView {
            visible: buttons,
            enabled: can_approve,
            label: crate::text!("Approve once"),
            review_label: Some(crate::text!("Approve once")),
        },
        deny: ButtonView {
            visible: buttons,
            enabled: can_approve,
            label: crate::text!("Deny"),
            review_label: None,
        },
        skip: ButtonView {
            visible: question.optional,
            enabled: question.optional
                && ctx.available
                && question.can_skip
                && !locked
                && !draft.uncertain,
            label: crate::text!("Skip"),
            review_label: None,
        },
        previous: ButtonView {
            visible: forms.len() > 1,
            enabled: page > 0 && !locked,
            label: crate::text!("← Back"),
            review_label: None,
        },
        next: ButtonView {
            visible: page < last,
            enabled: !locked,
            label: crate::text!("Next →"),
            review_label: None,
        },
        retry: draft.uncertain && !locked,
        skip_tip: Text::new(SKIP_TIP),
        retry_tip: Text::new(RETRY_TIP),
        forms,
    }
}

/// What a click on Submit, Approve, Deny or Skip may do.
#[derive(Clone, Debug, PartialEq)]
pub enum Actionable {
    Ready(Box<Question>),
    /// The session is not ready: nothing to say.
    Silent,
    /// The request changed: show [`CHANGED`] on the card.
    Changed,
}

/// `actionableQuestion`: whether the answer may go now. The hub checks
/// first that no answer of the session is in flight and the request was not
/// already submitted.
pub fn actionable(
    details: &Value,
    facts: &AnswerFacts,
    question_id: &str,
    skipping: bool,
) -> Actionable {
    if !facts.online
        || facts.state != "running"
        || facts.updating
        || facts.inspect_error
        || !live(details)
    {
        return Actionable::Silent;
    }
    let Some(question) = pending(details)
        .into_iter()
        .find(|question| question.id == question_id)
    else {
        return Actionable::Changed;
    };
    let can = if skipping {
        question.can_skip
    } else {
        question.can_answer
    };
    if !can
        || question.run_id != text(details, "run_id")
        || question.conversation_id != text(details, "conversation_id")
    {
        return Actionable::Changed;
    }
    Actionable::Ready(Box::new(question))
}

/// The fields consent was given for: an answer goes only while they stay the
/// same (`consentIdentity`).
pub fn consent_identity(question: &Value) -> Value {
    let mut identity = Map::new();
    for field in CONSENT_FIELDS {
        if let Some(value) = question.get(field) {
            identity.insert(field.to_owned(), value.clone());
        }
    }
    Value::Object(identity)
}

/// What the check right before sending compares (`answerQuestion`'s callback).
#[derive(Clone, Copy, Debug)]
pub struct Recheck<'a> {
    /// The consent identity at the click.
    pub pinned: &'a Value,
    pub question: &'a Question,
    /// The session's inspection now (`sendEvidence`).
    pub evidence: &'a Value,
    /// The inspection captured at the click.
    pub captured: &'a Value,
    /// The session row is still the one clicked (host, name, creation).
    pub row_same: bool,
    /// Its run and conversation are the clicked ones, or the question's.
    pub row_identity: bool,
    pub facts: &'a AnswerFacts,
    pub skipping: bool,
    /// The user accepted the cold cache warning.
    pub consent: bool,
    /// The request was submitted in the meantime.
    pub submitted: bool,
    pub now: f64,
}

/// Kimi's own cache panel asks the user already: no cold cache warning.
pub fn native_cache_choice(question: &Question) -> bool {
    question.source == "kimi_cache_hint" && question.transport == "kimi_tui"
}

/// Whether the answer may still go after its durable save; otherwise the
/// draft shows [`CHANGED_KEPT`].
pub fn still_answerable(r: &Recheck<'_>) -> bool {
    if !r.row_same
        || !r.row_identity
        || !r.facts.online
        || r.facts.state != "running"
        || r.facts.updating
        || r.facts.inspect_error
        || !live(r.evidence)
        || text(r.evidence, "run_id") != r.question.run_id
        || text(r.evidence, "conversation_id") != r.question.conversation_id
    {
        return false;
    }
    let refreshed = r
        .evidence
        .get("pending_questions")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| text(entry, "question_id") == r.question.id)
        })
        .cloned()
        .unwrap_or(Value::Null);
    if &consent_identity(&refreshed) != r.pinned {
        return false;
    }
    if !native_cache_choice(r.question)
        && !r.consent
        && (cache::expired(r.captured, r.now) || cache::expired(r.evidence, r.now))
    {
        return false;
    }
    let can = if r.skipping {
        flag(&refreshed, "can_skip")
    } else {
        flag(&refreshed, "can_answer")
    };
    can && !r.submitted
}

/// The cold cache question before an answer.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ColdAnswer {
    pub title: String,
    pub text: String,
    pub proceed: String,
}

/// "Submit with a cold cache?" when the prompt cache expired, except for
/// Kimi's own cache panel. Cancel keeps the answers.
pub fn cold_answer(details: &Value, question: &Question, now: f64) -> Option<ColdAnswer> {
    if native_cache_choice(question) || !cache::expired(details, now) {
        return None;
    }
    Some(ColdAnswer {
        title: "Submit with a cold cache?".to_owned(),
        text: format!("{}\n\n{COLD_TEXT}", cache::warning(details, now)),
        proceed: "Submit with full context".to_owned(),
    })
}

/// What follows a recorded answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FollowUp {
    /// Hook review and Codex's installer continue in Terminal.
    pub open_terminal: bool,
    pub notice: Option<String>,
}

/// After Codex's hooks review or Update now (`questionAnswered`). `open_run`
/// is the selected session's run, None when another session is selected;
/// `target` names the session as "{name} ({machine})".
pub fn answered_follow_up(
    question: &Question,
    receipt: &Value,
    open_run: Option<&str>,
    target: &str,
) -> FollowUp {
    let hash_matches = question.hash == text(receipt, "question_hash");
    let hooks = question.id.starts_with("codex-hooks-trust:");
    let update = question.id.starts_with("codex-update:");
    let open_terminal = (hooks || update)
        && hash_matches
        && open_run.is_some_and(|run| run == text(receipt, "run_id"))
        && flag(receipt, "open_terminal");
    let notice = (update && hash_matches)
        .then(|| match text(receipt, "codex_update") {
            "installing" => Some(format!(
                "Codex in {target} is installing its update in Terminal. Resume the session after Codex exits."
            )),
            "exited" => Some(format!(
                "Codex in {target} exited after its update. Check the result in Terminal, then resume the session."
            )),
            _ => None,
        })
        .flatten();
    FollowUp {
        open_terminal,
        notice,
    }
}

/// One of the user's answers that a prompt carries.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Reply {
    pub question: String,
    pub answer: String,
}

const REPLY_START: &str = "<send_user_message_question_reply>";
const REPLY_END: &str = "</send_user_message_question_reply>";

/// The answers in a prompt (`QuestionReply::parse`); empty when it holds none.
pub fn replies(text: &str) -> Vec<Reply> {
    let mut text = text.trim();
    if text.starts_with("# Context from my IDE setup:\n") {
        const MARKER: &str = "\n## My request for Codex:\n";
        let Some(at) = text.rfind(MARKER) else {
            return Vec::new();
        };
        text = text[at + MARKER.len()..].trim();
    }
    let Some(body) = text
        .strip_prefix(REPLY_START)
        .and_then(|rest| rest.strip_suffix(REPLY_END))
    else {
        return Vec::new();
    };
    let Ok(document) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let items = match document {
        Value::Object(_) => vec![document],
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    let mut result = Vec::with_capacity(items.len());
    for item in &items {
        let field = |key: &str| item.get(key).and_then(Value::as_str);
        let valid = item.is_object()
            && field("questionItemId").is_some_and(|id| !id.is_empty())
            && field("question").is_some_and(|question| !question.trim().is_empty())
            && field("answer").is_some();
        if !valid {
            return Vec::new();
        }
        result.push(Reply {
            question: field("question").unwrap_or_default().to_owned(),
            answer: field("answer").unwrap_or_default().to_owned(),
        });
    }
    result
}

/// "Your answer: a" or "Your answers: a / b"; empty without answers.
pub fn replies_preview(replies: &[Reply]) -> String {
    if replies.is_empty() {
        return String::new();
    }
    let answers: Vec<&str> = replies
        .iter()
        .map(|reply| {
            if reply.answer.is_empty() {
                "(empty answer)"
            } else {
                reply.answer.as_str()
            }
        })
        .collect();
    let lead = if replies.len() == 1 {
        "Your answer"
    } else {
        "Your answers"
    };
    format!("{lead}: {}", answers.join(" / "))
}

fn preview_of(value: &str) -> String {
    if value.contains(REPLY_START) {
        replies_preview(&replies(value))
    } else {
        String::new()
    }
}

/// A row's prompt as its card shows it (`promptPreview`).
pub fn prompt_preview(row: &Value) -> String {
    let prompt = text(row, "prompt");
    let answers = preview_of(prompt);
    if answers.is_empty() {
        prompt.to_owned()
    } else {
        answers
    }
}

/// A row's activity detail as its card shows it (`activityDetailPreview`):
/// the answers it carries, or those of the prompt when the detail is the
/// prompt's clipped excerpt.
pub fn activity_detail_preview(row: &Value) -> String {
    let detail = text(row, "activity_detail");
    let answers = preview_of(detail);
    if !answers.is_empty() {
        return answers;
    }
    let excerpt = detail.strip_suffix('\u{2026}').unwrap_or(detail);
    let prompt = text(row, "prompt");
    if !excerpt.is_empty() && prompt.starts_with(excerpt) {
        let prompt_answers = preview_of(prompt);
        if !prompt_answers.is_empty() {
            return prompt_answers;
        }
    }
    detail.to_owned()
}

const USER_EVENTS: [&str; 4] = [
    "UserPromptSubmit",
    "UserPromptQueued",
    "TurnStarted",
    "QuestionAnswered",
];

fn annotate_event(event: &mut Value) {
    if !USER_EVENTS.contains(&text(event, "type")) {
        return;
    }
    let found = replies(text(event, "detail"));
    if !found.is_empty() {
        event["question_replies"] = serde_json::to_value(found).unwrap_or(Value::Null);
    }
}

/// Marks the user's answers in an Activity payload, off the UI thread: each
/// user event whose detail holds answers gets `question_replies`, and a
/// state's prompt gets `prompt_replies`.
pub fn annotate_replies(payload: &mut Value) {
    if let Some(events) = payload.get_mut("events").and_then(Value::as_array_mut) {
        events.iter_mut().for_each(annotate_event);
    }
    if let Some(event) = payload.get_mut("event").filter(|event| event.is_object()) {
        annotate_event(event);
    }
    if let Some(value) = payload.get_mut("value").filter(|value| value.is_object()) {
        let found = replies(text(value, "prompt"));
        if !found.is_empty() {
            value["prompt_replies"] = serde_json::to_value(found).unwrap_or(Value::Null);
        }
    }
}
