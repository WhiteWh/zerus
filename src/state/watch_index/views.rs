//! Bounded projections of appended records. A bound exceeded means falling
//! back to the canonical scanner, never an incomplete result presented as empty.
use super::*;
use std::collections::VecDeque;

const PUBLIC_BYTES: usize = 4 * 1024 * 1024;
const STATE_BYTES: usize = 2 * 1024 * 1024;
const ROWS: usize = 8192;

struct Public {
    start: u64,
    end: u64,
    size: usize,
    projection: provider_messages::Projection,
}

pub(super) struct Views {
    public: VecDeque<Public>,
    public_bytes: usize,
    dropped: u64,
    length: u64,
    usage: Option<Value>,
    usage_from: u64,
    usage_strict: bool,
    verified: bool,
    settings: Option<(u64, u64, String, String)>,
    errors: VecDeque<(u64, Value)>,
    progress: VecDeque<(u64, f64)>,
    questions: Option<Value>,
    child: bool,
}

impl Default for Views {
    fn default() -> Self {
        Self {
            public: VecDeque::new(),
            public_bytes: 0,
            dropped: 0,
            length: 0,
            usage: Some(json!({"totals":{},"seen":{},"context":{},"history":[],"codex_seen":{}})),
            usage_from: 0,
            usage_strict: false,
            verified: false,
            settings: None,
            errors: VecDeque::new(),
            progress: VecDeque::new(),
            questions: Some(json!({"items":{}})),
            child: false,
        }
    }
}

impl Views {
    pub(super) fn verified(&self) -> bool {
        self.verified
    }
    pub(super) fn begin(&mut self, record: &Value, length: u64) {
        self.verified = record["agent"] != "codex";
        if record["agent"] == "codex" {
            self.usage_from = length.saturating_sub(WINDOW);
            self.usage_strict = self.usage_from > 0;
        }
    }

    pub(super) fn observe(
        &mut self,
        record: &Value,
        start: u64,
        end: u64,
        line: &[u8],
    ) -> Result<()> {
        if record["agent"] == "codex" && std::str::from_utf8(line).is_err() {
            self.questions = None;
        }
        let cutoff = end.saturating_sub(WINDOW);
        if cutoff > 0 {
            while self
                .progress
                .front()
                .is_some_and(|(start, _)| *start <= cutoff)
            {
                self.progress.pop_front();
            }
        }
        let event = serde_json::from_slice::<Value>(line);
        if start == 0 && record["agent"] == "codex" {
            let header = event.as_ref().ok();
            if line.len() > 256 * 1024
                || header.is_none_or(|header| {
                    header["type"] != "session_meta"
                        || header["payload"]["id"] != record["conversation_id"]
                })
            {
                return Err("Conversation transcript identity mismatch".into());
            }
            self.verified = true;
            self.child =
                header.is_some_and(|header| !header["payload"]["source"]["subagent"].is_null());
        }
        let Ok(event) = event else {
            return Ok(());
        };
        let contains = |needle: &[u8]| line.windows(needle.len()).any(|part| part == needle);
        if record["agent"] == "codex" && !self.child && self.questions.is_some() {
            let relevant = [
                b"request_user_input_async".as_slice(),
                b"function_call_output",
                b"\"user_message\"",
                b"\"task_started\"",
                b"\"turn_started\"",
                b"\"task_complete\"",
                b"\"turn_complete\"",
                b"\"turn_aborted\"",
                b"send_user_message_question_reply",
            ]
            .iter()
            .any(|needle| contains(needle));
            if relevant {
                let state = self.questions.as_mut().unwrap();
                codex_questions::watch_observe(record, state, &event);
                if state["items"]
                    .as_object()
                    .is_some_and(|items| items.len() > 128)
                    || serde_json::to_vec(state).is_ok_and(|bytes| bytes.len() > STATE_BYTES)
                {
                    self.questions = None;
                }
            }
        }
        let settings = if record["agent"] == "claude" {
            ["assistant", "user", "system"].contains(&string(&event, "type"))
                && [
                    b"\"requestedModel\"".as_slice(),
                    b"Set model to ",
                    b"Model set to ",
                ]
                .iter()
                .any(|needle| contains(needle))
        } else {
            contains(b"turn_context")
        };
        if settings {
            if let Some((model, effort)) = effort::watch_settings(record, &event) {
                self.settings = Some((start, end, model, effort));
            }
        }
        if record["agent"] == "codex"
            && [
                b"\"error\"".as_slice(),
                b"\"task_started\"",
                b"\"task_complete\"",
                b"\"turn_complete\"",
            ]
            .iter()
            .any(|needle| contains(needle))
        {
            let (error, advanced) = provider_errors::watch_event(&event);
            if let Some(error) = error {
                self.errors.push_back((start, error));
                if self.errors.len() > 8 {
                    self.errors.pop_front();
                }
            }
            if advanced > 0. {
                while self.progress.back().is_some_and(|(_, at)| *at <= advanced) {
                    self.progress.pop_back();
                }
                self.progress.push_back((start, advanced));
            }
        }
        if start >= self.usage_from
            && (!self.usage_strict || start > self.usage_from)
            && self.usage.is_some()
        {
            let usage = self.usage.as_mut().unwrap();
            usage::watch_observe(
                usage,
                &event,
                string(record, "agent"),
                string(record, "conversation_id"),
            );
            // Only usage-bearing rows can grow this state. Avoid serializing
            // it for the many tool/progress lines that change no counters.
            let relevant = event["type"] == "assistant"
                || event["type"] == "cost-state"
                || event["payload"]["type"] == "token_count";
            if relevant && serde_json::to_vec(usage).is_ok_and(|bytes| bytes.len() > STATE_BYTES) {
                self.usage = None;
            }
        }
        let projection = provider_messages::project(record, &event, line);
        let size = match &projection {
            provider_messages::Projection::Messages(messages)
            | provider_messages::Projection::Blob(messages) => {
                if messages.is_empty() {
                    return Ok(());
                }
                serde_json::to_vec(messages)
                    .map_err(|error| error.to_string())?
                    .len()
            }
            provider_messages::Projection::Request(at) => at.to_string().len(),
        };
        self.public.push_back(Public {
            start,
            end,
            size,
            projection,
        });
        self.public_bytes += size;
        while self.public_bytes > PUBLIC_BYTES || self.public.len() > ROWS {
            let old = self.public.pop_front().unwrap();
            self.public_bytes -= old.size;
            self.dropped = old.end;
        }
        Ok(())
    }

    pub(super) fn oversized(&mut self, end: u64) {
        // The canonical usage scanner accepts lines up to 32 MiB. Keep that
        // scanner for this exceptional transcript rather than omit a sample.
        if end > self.usage_from {
            self.usage = None;
        }
        self.questions = None;
    }

    pub(super) fn finish(&mut self, _record: &Value, length: u64) {
        self.length = length;
        let cutoff = length.saturating_sub(WINDOW);
        if cutoff > 0 {
            while self
                .errors
                .front()
                .is_some_and(|(start, _)| *start <= cutoff)
            {
                self.errors.pop_front();
            }
            while self
                .progress
                .front()
                .is_some_and(|(start, _)| *start <= cutoff)
            {
                self.progress.pop_front();
            }
        }
        while self
            .public
            .front()
            .is_some_and(|line| cutoff > 0 && line.start <= cutoff)
        {
            let old = self.public.pop_front().unwrap();
            self.public_bytes -= old.size;
        }
    }

    pub(super) fn messages(&self, record: &Value) -> Option<Vec<Value>> {
        if !self.verified || self.dropped > self.length.saturating_sub(WINDOW) {
            return None;
        }
        Some(provider_messages::projected(
            record,
            self.public.iter().map(|line| &line.projection),
        ))
    }

    pub(super) fn usage(&self, record: &Value, partial: bool) -> Option<Value> {
        if !self.verified {
            return None;
        }
        self.usage
            .as_ref()
            .map(|state| usage::watch_output(state, string(record, "agent"), partial))
    }

    pub(super) fn usage_ready(&self) -> bool {
        self.usage.is_some()
    }

    pub(super) fn seed_usage(&mut self, state: Option<Value>, length: u64) {
        self.usage = state;
        self.usage_from = length;
        self.usage_strict = false;
    }

    pub(super) fn settings(&self) -> Option<(String, String, u64)> {
        if !self.verified {
            return None;
        }
        let cutoff = self.length.saturating_sub(WINDOW);
        Some(match &self.settings {
            Some((start, end, model, effort)) if cutoff == 0 || *start > cutoff => {
                (model.clone(), effort.clone(), *end)
            }
            _ => (String::new(), String::new(), 0),
        })
    }

    pub(super) fn errors(&self, record: &Value) -> Option<(Vec<Value>, f64)> {
        if !self.verified || record["agent"] != "codex" {
            return None;
        }
        Some((
            self.errors.iter().map(|(_, error)| error.clone()).collect(),
            self.progress.front().map_or(0., |(_, at)| *at),
        ))
    }

    pub(super) fn questions(
        &mut self,
        record: &Value,
        deadline: Option<Instant>,
    ) -> Option<Result<Option<Value>>> {
        if !self.verified || record["agent"] != "codex" {
            return None;
        }
        let state = self.questions.as_mut()?;
        if self.child {
            return Some(Ok(Some(state.clone())));
        }
        let result = codex_questions::watch_complete(record, state, deadline);
        if serde_json::to_vec(state).is_ok_and(|bytes| bytes.len() > STATE_BYTES) {
            self.questions = None;
            return None;
        }
        if result
            .as_ref()
            .err()
            .is_some_and(|error| error.starts_with("watch question overlay"))
        {
            self.questions = None;
            return None;
        }
        Some(result)
    }

    #[cfg(test)]
    pub(super) fn raw_questions(&self) -> Option<Value> {
        self.questions.clone()
    }

    #[cfg(test)]
    pub(super) fn public_size(&self) -> (usize, usize) {
        (self.public_bytes, self.public.len())
    }

    pub(super) fn clock_messages(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for line in &self.public {
            match &line.projection {
                provider_messages::Projection::Request(_) => ids.clear(),
                provider_messages::Projection::Blob(messages) => ids.extend(
                    messages
                        .iter()
                        .filter_map(|message| message["message_id"].as_str().map(str::to_owned)),
                ),
                _ => {}
            }
        }
        ids
    }
}
