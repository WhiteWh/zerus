//! One bounded drawer page, read on demand. No journal or per-team cache.
use super::read::{self, TeamError, TeamRead, CURSOR_BYTES, PAGE};
use super::view::{self, ServiceNote, WaitingItem, NAME_CHARS, TEXT_CHARS};
use crate::actions::{Exit, Invocation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{atomic::AtomicBool, Arc};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DrawerKind {
    Feed,
    Inbox,
    Cards,
}

impl DrawerKind {
    fn command(self) -> &'static str {
        match self {
            Self::Feed => "journal",
            Self::Inbox => "inbox",
            Self::Cards => "cards",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrawerRead {
    pub team: String,
    pub kind: DrawerKind,
    pub after: Option<String>,
    #[serde(default)]
    pub recorded_timings: bool,
    #[serde(default)]
    pub timing_seqs: Option<Vec<u64>>,
    #[serde(default)]
    pub timing_next: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DrawerRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<super::recorded_timings::RecordedFacts>,
    pub seq: u64,
    pub kind: String,
    pub by: String,
    /// The actor's kind as the store wrote it (`person`, `service` or
    /// `session`), so the window can tell you, the team service and a member
    /// apart where `by` alone names the same person.
    pub by_kind: String,
    /// The task and member a journal event is about, from its `refs`
    /// (`node:N`, `member:ID`); never invented from text.
    pub subject_node: Option<u64>,
    pub subject_member: Option<String>,
    pub text: String,
    pub text_truncated: bool,
    pub at_ms: i64,
    pub node: Option<u64>,
    pub generation: Option<u64>,
    pub state: String,
    pub reply: Option<String>,
    pub notes: Vec<ServiceNote>,
    pub card: Option<WaitingItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DrawerPage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<super::recorded_timings::TimingSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeline: Option<super::timeline::RecordedCoverage>,
    pub team: String,
    pub kind: DrawerKind,
    pub rows: Vec<DrawerRow>,
    pub next: Option<String>,
    pub after: Option<String>,
}

fn base(asked: &DrawerRead) -> TeamRead {
    TeamRead::Queue {
        team: asked.team.clone(),
        after: asked.after.clone(),
    }
}

pub fn invocation(asked: &DrawerRead) -> Result<Invocation, TeamError> {
    if asked.recorded_timings && asked.kind != DrawerKind::Feed {
        return Err(invalid("Recorded timings require the journal."));
    }
    if let Some(seqs) = &asked.timing_seqs {
        if !asked.recorded_timings
            || seqs.len() > PAGE as usize
            || seqs
                .iter()
                .any(|id| *id == 0 || *id > 9_007_199_254_740_991)
            || seqs.windows(2).any(|ids| ids[0] >= ids[1])
        {
            return Err(invalid("Invalid selected timing event identities."));
        }
        read::invocation(&TeamRead::Queue {
            team: asked.team.clone(),
            after: asked.timing_next.clone(),
        })?;
    } else if asked.timing_next.is_some() {
        return Err(invalid("No selected event window for this cursor."));
    }
    let mut call = read::invocation(&base(asked))?;
    call.args[1] = asked.kind.command().into();
    if asked.recorded_timings {
        call.args.insert(2, "--recorded-timings".into());
    }
    if let Some(seqs) = &asked.timing_seqs {
        call.args.insert(
            2,
            if seqs.is_empty() {
                "none".into()
            } else {
                seqs.iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            },
        );
        call.args.insert(2, "--timing-seqs".into());
    }
    Ok(call)
}

fn invalid(message: &str) -> TeamError {
    TeamError::Invalid {
        message: message.into(),
    }
}

pub fn decode(asked: &DrawerRead, exit: Exit) -> Result<DrawerPage, TeamError> {
    invocation(asked)?;
    // All three CLI pages share the queue's envelope, refusal and team guard.
    let reply = read::decode(&base(asked), exit)?;
    let timings = if asked.recorded_timings {
        Some(super::recorded_timings::snapshot(&reply["timings"])?)
    } else {
        None
    };
    let rows = reply["rows"]
        .as_array()
        .ok_or_else(|| invalid("No drawer rows."))?;
    if rows.len() > PAGE as usize {
        return Err(invalid("The drawer page exceeds its row limit."));
    }
    let next = match reply.get("next") {
        Some(Value::Null) => None,
        Some(Value::String(cursor))
            if !cursor.is_empty()
                && cursor.len() <= CURSOR_BYTES
                && !cursor.starts_with('-')
                && !cursor.chars().any(|c| c.is_control() || c.is_whitespace()) =>
        {
            Some(cursor.clone())
        }
        _ => return Err(invalid("Not a drawer page cursor.")),
    };
    let mut projected = Vec::with_capacity(rows.len());
    for row in rows {
        let seq = row["seq"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("No drawer row identity."))?;
        let kind = row["kind"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("No drawer row kind."))?;
        let text = row["text"]
            .as_str()
            .ok_or_else(|| invalid("No drawer row text."))?;
        let actor = row["actor"]
            .as_object()
            .ok_or_else(|| invalid("No drawer row actor."))?;
        let by = ["member", "person", "kind"]
            .iter()
            .find_map(|key| actor.get(*key).and_then(Value::as_str))
            .unwrap_or("");
        let by_kind = actor.get("kind").and_then(Value::as_str).unwrap_or("");
        let reference = |prefix: &str| {
            row.get("refs").and_then(Value::as_array).and_then(|refs| {
                refs.iter()
                    .filter_map(Value::as_str)
                    .find_map(|item| item.strip_prefix(prefix))
            })
        };
        let subject_node = reference("node:")
            .and_then(|node| node.parse::<u64>().ok())
            .filter(|node| *node > 0);
        let subject_member = reference("member:")
            .filter(|member| !member.is_empty())
            .map(|member| view::cut(member, NAME_CHARS));
        let at = if asked.kind == DrawerKind::Feed {
            "at_ms"
        } else {
            "created_ms"
        };
        let at_ms = row[at]
            .as_i64()
            .ok_or_else(|| invalid("No drawer row timestamp."))?;
        let node = match (asked.kind, row.get("node")) {
            (DrawerKind::Inbox, Some(value)) => {
                Some(value.as_u64().ok_or_else(|| invalid("No inbox node."))?)
            }
            (DrawerKind::Inbox, None) => return Err(invalid("No inbox node.")),
            (_, Some(value)) if !value.is_null() => Some(
                value
                    .as_u64()
                    .ok_or_else(|| invalid("Invalid drawer node."))?,
            ),
            _ => None,
        };
        let generation = if asked.kind == DrawerKind::Inbox {
            Some(
                row["generation"]
                    .as_u64()
                    .ok_or_else(|| invalid("No inbox generation."))?,
            )
        } else {
            None
        };
        let state = if asked.kind == DrawerKind::Feed {
            ""
        } else {
            row["state"]
                .as_str()
                .ok_or_else(|| invalid("No drawer row state."))?
        };
        let reply_text = match row.get("reply") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text),
            _ => return Err(invalid("Invalid inbox reply.")),
        };
        let card = if asked.kind == DrawerKind::Cards {
            let mut value = row.clone();
            value["team"] = Value::String(asked.team.clone());
            Some(
                view::waiting(&serde_json::json!({"rows": [value]}))
                    .items
                    .pop()
                    .ok_or_else(|| invalid("Invalid owner card."))?,
            )
        } else {
            None
        };
        projected.push(DrawerRow {
            timing: if asked.recorded_timings {
                Some(super::recorded_timings::facts(&row["timing"])?)
            } else {
                None
            },
            seq,
            kind: view::cut(kind, NAME_CHARS),
            by: view::cut(by, NAME_CHARS),
            by_kind: view::cut(by_kind, NAME_CHARS),
            subject_node,
            subject_member,
            text: view::cut(text, TEXT_CHARS),
            text_truncated: text.chars().count() > TEXT_CHARS
                || reply_text.is_some_and(|text| text.chars().count() > TEXT_CHARS),
            at_ms,
            node,
            generation,
            state: view::cut(state, NAME_CHARS),
            reply: reply_text.map(|text| view::cut(text, TEXT_CHARS)),
            notes: view::service_notes(row),
            card,
        });
    }
    let next = if let Some(seqs) = &asked.timing_seqs {
        if projected.iter().map(|row| row.seq).ne(seqs.iter().copied()) {
            return Err(invalid(
                "Selected journal events changed; read the page again.",
            ));
        }
        asked.timing_next.clone()
    } else {
        next
    };
    let timeline = if asked.kind == DrawerKind::Feed {
        Some(super::timeline::coverage(
            &projected,
            asked.after.as_deref(),
            next.as_deref(),
        )?)
    } else {
        None
    };
    Ok(DrawerPage {
        timings,
        timeline,
        team: asked.team.clone(),
        kind: asked.kind,
        rows: projected,
        next,
        after: asked.after.clone(),
    })
}

/// Retain admission through physical child and pipe retirement, including a
/// bounded error return. This token grants no authority to signal or resend.
pub fn read(
    hgs: &Path,
    asked: &DrawerRead,
    resources: Arc<dyn Send + Sync>,
) -> Result<DrawerPage, TeamError> {
    let exit = crate::run::run_accounted(
        hgs,
        &invocation(asked)?,
        &[],
        crate::reads::READ_LIMIT,
        &AtomicBool::new(false),
        resources,
    )
    .map_err(|stopped| match stopped {
        crate::run::Stopped::TooLarge(too_large) => TeamError::TooLarge {
            limit: too_large.cap,
        },
        crate::run::Stopped::Cancelled => TeamError::Cancelled,
    })?;
    decode(asked, exit)
}

#[cfg(test)]
mod tests;
