//! Pure original task-message recovery proposals. Recover-only can reconcile
//! canonical task metadata, so a future caller still needs G1 effect admission.
//! This module never calls a runner, store, reservation, reducer or release.
use crate::{
    actions::{Exit, Invocation},
    attachments::{Attachment, MAX_ATTACHMENTS, MAX_ATTACHMENT_BYTES, MAX_TOTAL_BYTES},
    creation, task_details, tasks_write,
};
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Serialize,
};
use serde_json::{value::RawValue, Value};
use std::{collections::HashMap, fmt, io::Write};
use zerus_desktop_migration::sha256;

pub const PRIVATE_BYTES: usize = 1 << 20;
pub const REPLY_BYTES: usize = 1 << 20;
pub const RECEIPT_BYTES: usize = 16 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    TaskMessageRecovery,
}

/// Independently retained ORIGINAL expected data, never current permission.
/// Project/task/configuration are not fields observed in the outbox record.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub kind: Kind,
    pub schema: u32,
    pub configuration: String,
    pub project: String,
    pub task_id: String,
    pub request_id: String,
    pub marker_id: String,
    pub mode: String,
    pub dispatch_id: String,
    pub source_sha256: String,
    pub input_sha256: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review {
    TooLarge,
    MissingInput,
    InvalidRecord,
    IdentityChanged,
    MarkerChanged,
    InputChanged,
    Unsupported,
    Historical,
    NoLookup,
    Cancelled,
    Pressure,
    Spent,
    InvalidReply,
    UnknownOutcome,
}
enum State {
    Ready,
    Proposed,
    Done,
}

/// No Debug, Serialize or Clone can expose the private original buffers.
pub struct Original {
    binding: Binding,
    _raw: Vec<u8>,
    _input: Vec<u8>,
    _marker: Vec<u8>,
    invocation: Option<Invocation>,
    eligible: bool,
    state: State,
}
pub struct Proposal {
    binding: Binding,
    invocation: Invocation,
}
impl Proposal {
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    pub fn invocation(&self) -> &Invocation {
        &self.invocation
    }
}
pub enum Input<'a> {
    Review,
    LookupRequested,
    Unknown,
    Reply(&'a Exit),
}
pub enum Decision {
    NeedsReview(Review),
    Proposed(Proposal),
    Observed { binding: Binding, receipt: Receipt },
}

/// Closed historical evidence. Unit serializes as explicit null and cannot
/// carry an inferred executor. This grants no delivered/consumed/release proof.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub schema: u32,
    pub kind: Kind,
    pub task_id: String,
    pub request_id: String,
    pub mode: String,
    pub dispatch_id: String,
    pub state: String,
    pub executor: (),
}

// Borrow only inspected raw values, rejecting known duplicate keys BEFORE a
// Value projection can erase them. Unknown fields remain in retained raw bytes.
fn raw_fields<'a>(bytes: &'a [u8], keys: &[&str]) -> Result<HashMap<String, &'a RawValue>, ()> {
    struct Fields<'k>(&'k [&'k str]);
    impl<'de> Visitor<'de> for Fields<'_> {
        type Value = HashMap<String, &'de RawValue>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("one object")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut fields = HashMap::new();
            while let Some(key) = map.next_key::<String>()? {
                if self.0.contains(&key.as_str()) {
                    if fields.contains_key(&key) {
                        return Err(serde::de::Error::custom("ambiguous known field"));
                    }
                    fields.insert(key, map.next_value::<&'de RawValue>()?);
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(fields)
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let fields =
        serde::de::Deserializer::deserialize_map(&mut decoder, Fields(keys)).map_err(|_| ())?;
    decoder.end().map_err(|_| ())?;
    Ok(fields)
}
fn required<'a>(fields: &HashMap<String, &'a RawValue>, key: &str) -> Result<&'a RawValue, ()> {
    fields.get(key).copied().ok_or(())
}

fn content(bytes: &[u8]) -> Result<Value, ()> {
    let fields = raw_fields(bytes, &["text", "attachments"])?;
    serde_json::from_str::<String>(required(&fields, "text")?.get()).map_err(|_| ())?;
    if let Some(files) = fields.get("attachments") {
        struct Files;
        impl<'de> Visitor<'de> for Files {
            type Value = ();
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("attachment array")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
                let mut count = 0;
                let mut total = 0;
                while let Some(raw) = seq.next_element::<&RawValue>()? {
                    count += 1;
                    if count > MAX_ATTACHMENTS {
                        return Err(serde::de::Error::custom("attachment count"));
                    }
                    raw_fields(
                        raw.get().as_bytes(),
                        &["name", "mime", "data_base64", "reference"],
                    )
                    .map_err(|_| serde::de::Error::custom("attachment fields"))?;
                    let file: Attachment = serde_json::from_str(raw.get())
                        .map_err(|_| serde::de::Error::custom("attachment type"))?;
                    total += file.bytes.len();
                    if file.bytes.is_empty()
                        || file.bytes.len() > MAX_ATTACHMENT_BYTES
                        || total > MAX_TOTAL_BYTES
                    {
                        return Err(serde::de::Error::custom("attachment bound"));
                    }
                }
                Ok(())
            }
        }
        let mut decoder = serde_json::Deserializer::from_str(files.get());
        serde::de::Deserializer::deserialize_seq(&mut decoder, Files).map_err(|_| ())?;
        decoder.end().map_err(|_| ())?;
    }
    serde_json::from_slice(bytes).map_err(|_| ())
}
fn uuid(text: &str) -> bool {
    creation::canonical_uuid(text) && text != "00000000-0000-0000-0000-000000000000"
}
fn digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn version(value: &Value) -> bool {
    ["schema", "version"]
        .iter()
        .all(|key| value.get(key).is_none_or(|v| v.as_u64() == Some(1)))
}

impl Original {
    pub fn verify(
        binding: Binding,
        raw: &[u8],
        input: &[u8],
        marker: &[u8],
    ) -> Result<Self, Review> {
        if raw.len() > PRIVATE_BYTES || input.len() > PRIVATE_BYTES || marker.len() > 128 {
            return Err(Review::TooLarge);
        }
        if input.is_empty() {
            return Err(Review::MissingInput);
        }
        if binding.schema != 1
            || !digest(&binding.configuration)
            || !uuid(&binding.task_id)
            || !uuid(&binding.request_id)
            || task_details::document_call(&binding.project, &binding.task_id).is_err()
            || !matches!(binding.mode.as_str(), "queue" | "note" | "send" | "reopen")
            || (!binding.dispatch_id.is_empty() && !uuid(&binding.dispatch_id))
            || sha256(raw) != binding.source_sha256
        {
            return Err(Review::IdentityChanged);
        }
        if binding.marker_id != binding.request_id || marker != binding.marker_id.as_bytes() {
            return Err(Review::MarkerChanged);
        }
        if sha256(input) != binding.input_sha256 {
            return Err(Review::InputChanged);
        }
        let fields = raw_fields(raw, &["id", "intent", "schema", "version"])
            .map_err(|_| Review::InvalidRecord)?;
        let intent = raw_fields(
            required(&fields, "intent")
                .map_err(|_| Review::InvalidRecord)?
                .get()
                .as_bytes(),
            &["content", "mode", "dispatch"],
        )
        .map_err(|_| Review::InvalidRecord)?;
        let original_content = content(
            required(&intent, "content")
                .map_err(|_| Review::InvalidRecord)?
                .get()
                .as_bytes(),
        )
        .map_err(|_| Review::InvalidRecord)?;
        let frozen_content = content(input).map_err(|_| Review::InvalidRecord)?;
        if original_content != frozen_content {
            return Err(Review::InputChanged);
        }
        let record = tasks_write::outbox::decode(raw).map_err(|_| Review::InvalidRecord)?;
        if !version(&record) {
            return Err(Review::InvalidRecord);
        }
        if record["id"].as_str() != Some(&binding.request_id)
            || record["intent"]["mode"].as_str() != Some(&binding.mode)
            || record["intent"]["dispatch"].as_str() != Some(&binding.dispatch_id)
        {
            return Err(Review::IdentityChanged);
        }
        // Only the existing builder selects arguments. Its generated content
        // is replaced BEFORE invocation with the exact retained stdin, never
        // used as a reconstructed original. Missing retained input refused above.
        let mut call = tasks_write::message_call(&binding.project, &binding.task_id, &record, true);
        call.input = std::str::from_utf8(input)
            .map_err(|_| Review::InvalidRecord)?
            .to_owned();
        let invocation = call.invocation();
        let eligible = matches!(binding.mode.as_str(), "queue" | "note");
        Ok(Self {
            binding,
            _raw: raw.to_vec(),
            _input: input.to_vec(),
            _marker: marker.to_vec(),
            invocation: Some(invocation),
            eligible,
            state: State::Ready,
        })
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    pub fn step(&mut self, input: Input<'_>, cancelled: bool, pressure: bool) -> Decision {
        if matches!(self.state, State::Done) {
            return Decision::NeedsReview(Review::Spent);
        }
        if cancelled || pressure {
            self.state = State::Done;
            return Decision::NeedsReview(if cancelled {
                Review::Cancelled
            } else {
                Review::Pressure
            });
        }
        match input {
            Input::Review => Decision::NeedsReview(Review::NoLookup),
            Input::Unknown => {
                self.state = State::Done;
                Decision::NeedsReview(Review::UnknownOutcome)
            }
            Input::LookupRequested => {
                if !matches!(self.state, State::Ready) {
                    return Decision::NeedsReview(Review::Spent);
                }
                self.state = State::Done;
                if !self.eligible {
                    return Decision::NeedsReview(Review::Historical);
                }
                let Some(invocation) = self.invocation.take() else {
                    return Decision::NeedsReview(Review::Spent);
                };
                self.state = State::Proposed;
                Decision::Proposed(Proposal {
                    binding: self.binding.clone(),
                    invocation,
                })
            }
            Input::Reply(exit) => {
                let requested = matches!(self.state, State::Proposed);
                self.state = State::Done;
                if !requested {
                    return Decision::NeedsReview(Review::NoLookup);
                }
                self.receipt(exit)
                    .map_or_else(Decision::NeedsReview, |receipt| Decision::Observed {
                        binding: self.binding.clone(),
                        receipt,
                    })
            }
        }
    }

    fn receipt(&self, exit: &Exit) -> Result<Receipt, Review> {
        let Exit::Code {
            code: 0, stdout, ..
        } = exit
        else {
            return Err(Review::UnknownOutcome);
        };
        if stdout.len() > REPLY_BYTES {
            return Err(Review::TooLarge);
        }
        let root = raw_fields(stdout, &["schema", "version", "task", "message"])
            .map_err(|_| Review::InvalidReply)?;
        raw_fields(
            required(&root, "task")
                .map_err(|_| Review::InvalidReply)?
                .get()
                .as_bytes(),
            &["uuid", "project"],
        )
        .map_err(|_| Review::InvalidReply)?;
        let message = raw_fields(
            required(&root, "message")
                .map_err(|_| Review::InvalidReply)?
                .get()
                .as_bytes(),
            &["kind", "request_id", "session", "delivery"],
        )
        .map_err(|_| Review::InvalidReply)?;
        raw_fields(
            required(&message, "delivery")
                .map_err(|_| Review::InvalidReply)?
                .get()
                .as_bytes(),
            &[
                "mode",
                "state",
                "dispatch_id",
                "run_id",
                "conversation_id",
                "submitted_id",
                "prompt",
                "error",
                "stage",
                "stage_at",
            ],
        )
        .map_err(|_| Review::InvalidReply)?;
        let data: Value = serde_json::from_slice(stdout).map_err(|_| Review::InvalidReply)?;
        if !version(&data) {
            return Err(Review::InvalidReply);
        }
        let map = data.as_object().ok_or(Review::InvalidReply)?;
        tasks_write::message_delivered(map, &self.binding.task_id, &self.binding.request_id)
            .map_err(|_| Review::IdentityChanged)?;
        if data["task"]["project"].as_str() != Some(&self.binding.project) {
            return Err(Review::IdentityChanged);
        }
        let message = &data["message"];
        if message["kind"].as_str() != Some("message")
            || message
                .get("session")
                .is_some_and(|session| !session.is_null())
        {
            return Err(Review::InvalidReply);
        }
        let delivery = &message["delivery"];
        let expected_state = if self.binding.mode == "queue" {
            "queued"
        } else {
            "note"
        };
        if delivery["mode"].as_str() != Some(&self.binding.mode)
            || delivery["state"].as_str() != Some(expected_state)
        {
            return Err(Review::UnknownOutcome);
        }
        // TaskDelivery omits dispatch_id when its canonical value is empty.
        // No other missing or malformed identity is synthesized here.
        let dispatch = match delivery.get("dispatch_id") {
            None => "",
            Some(value) => value.as_str().ok_or(Review::InvalidReply)?,
        };
        if dispatch != self.binding.dispatch_id {
            return Err(Review::IdentityChanged);
        }
        for key in [
            "run_id",
            "conversation_id",
            "submitted_id",
            "prompt",
            "error",
        ] {
            if delivery
                .get(key)
                .is_some_and(|value| value.as_str() != Some(""))
            {
                return Err(Review::InvalidReply);
            }
        }
        if delivery.get("stage").is_some_and(|v| !v.is_string())
            || delivery
                .get("stage_at")
                .is_some_and(|v| v.as_i64().is_none())
        {
            return Err(Review::InvalidReply);
        }
        let receipt = Receipt {
            schema: 1,
            kind: Kind::TaskMessageRecovery,
            task_id: self.binding.task_id.clone(),
            request_id: self.binding.request_id.clone(),
            mode: self.binding.mode.clone(),
            dispatch_id: dispatch.into(),
            state: expected_state.into(),
            executor: (),
        };
        receipt_size(&receipt)?;
        Ok(receipt)
    }
}
fn receipt_size(receipt: &Receipt) -> Result<(), Review> {
    struct Limit(usize);
    impl Write for Limit {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > RECEIPT_BYTES - self.0 {
                return Err(std::io::Error::other("receipt bound"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Limit(0), receipt).map_err(|_| Review::TooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id(n: u8) -> String {
        format!("00000000-1111-4111-8111-{n:012}")
    }
    fn record(mode: &str, dispatch: &str) -> Value {
        json!({"version":1,"id":id(2),"intent":{"mode":mode,"dispatch":dispatch,
            "content":{"text":"PRIVATE original / пример","attachments":[],"opaque":{"keep":true}}},
            "opaque":{"private":"retained unchanged"}})
    }
    fn fixture(value: Value) -> (Binding, Vec<u8>, Vec<u8>) {
        let raw = serde_json::to_vec_pretty(&value).unwrap();
        let mut input = serde_json::to_vec_pretty(&value["intent"]["content"]).unwrap();
        input.extend_from_slice(b" \n");
        let b = Binding {
            kind: Kind::TaskMessageRecovery,
            schema: 1,
            configuration: "a".repeat(64),
            project: "fixture-project".into(),
            task_id: id(1),
            request_id: id(2),
            marker_id: id(2),
            mode: value["intent"]["mode"].as_str().unwrap_or("").into(),
            dispatch_id: value["intent"]["dispatch"].as_str().unwrap_or("").into(),
            source_sha256: sha256(&raw),
            input_sha256: sha256(&input),
        };
        (b, raw, input)
    }
    fn original(b: Binding, raw: &[u8], input: &[u8]) -> Original {
        let marker = b.marker_id.as_bytes().to_vec();
        Original::verify(b, raw, input, &marker).unwrap_or_else(|e| panic!("{e:?}"))
    }
    fn proposal(o: &mut Original) -> Proposal {
        match o.step(Input::LookupRequested, false, false) {
            Decision::Proposed(p) => p,
            _ => panic!("missing proposal"),
        }
    }
    fn reply(b: &Binding) -> Value {
        let mut value = json!({"task":{"uuid":b.task_id,"project":b.project,"session":{"current":"NOT ORIGINAL EXECUTOR"}},
            "message":{"kind":"message","request_id":b.request_id,"session":null,
                "delivery":{"mode":b.mode,"state":if b.mode=="queue" {"queued"}else{"note"},"stage":"todo","stage_at":7},
                "text":"PRIVATE REPLY","attachments":[{"private":"discard"}]},"opaque":"PRIVATE EXTRA"});
        if !b.dispatch_id.is_empty() {
            value["message"]["delivery"]["dispatch_id"] = json!(b.dispatch_id);
        }
        value
    }
    fn exit(value: Value) -> Exit {
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&value).unwrap(),
            stderr: "PRIVATE STDERR".into(),
        }
    }
    fn observe(o: &mut Original, value: Value) -> Value {
        match o.step(Input::Reply(&exit(value)), false, false) {
            Decision::Observed { binding, receipt } => {
                assert!(binding == *o.binding());
                serde_json::to_value(receipt).unwrap()
            }
            _ => panic!("missing observation"),
        }
    }
    fn refuse(b: &Binding, raw: &[u8], input: &[u8], value: Value) {
        let mut o = original(b.clone(), raw, input);
        proposal(&mut o);
        assert!(matches!(
            o.step(Input::Reply(&exit(value)), false, false),
            Decision::NeedsReview(_)
        ));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
    }
    #[test]
    fn exact_original_marker_input_and_existing_ninety_second_args_are_kept() {
        let (b, raw, input) = fixture(record("queue", ""));
        let mut o = original(b.clone(), &raw, &input);
        assert_eq!(o._raw, raw);
        assert_eq!(o._input, input);
        assert_eq!(o._marker, id(2).as_bytes());
        assert!(matches!(
            o.step(Input::Review, false, false),
            Decision::NeedsReview(Review::NoLookup)
        ));
        let p = proposal(&mut o);
        assert!(p.binding() == &b);
        assert_eq!(
            p.invocation().args,
            vec![
                "task",
                "message",
                &id(1),
                "--request-id",
                &id(2),
                "--mode",
                "queue",
                "--expect-dispatch",
                "",
                "--content-json",
                "--project",
                "fixture-project",
                "--recover-only"
            ]
        );
        assert_eq!(p.invocation().stdin.as_deref(), Some(input.as_slice()));
        assert!(p.invocation().request_id.is_none());
        assert_eq!(p.invocation().timeout, std::time::Duration::from_secs(90));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
        let v = observe(&mut o, reply(&b));
        assert_eq!(v.as_object().unwrap().len(), 8);
        assert_eq!(v["kind"], "task_message_recovery");
        assert_eq!(v["executor"], Value::Null);
        assert_eq!(v["task_id"], id(1));
        assert_eq!(v["request_id"], id(2));
        assert!(!v.to_string().contains("PRIVATE"));
        assert_eq!(o._raw, raw);
        assert_eq!(o._input, input);
        assert!(matches!(
            o.step(Input::Reply(&exit(reply(&b))), false, false),
            Decision::NeedsReview(Review::Spent)
        ));
    }
    #[test]
    fn queue_and_note_require_their_exact_state_and_original_dispatch() {
        for mode in ["queue", "note"] {
            for dispatch in ["".to_owned(), id(3)] {
                let (b, raw, input) = fixture(record(mode, &dispatch));
                let mut o = original(b.clone(), &raw, &input);
                proposal(&mut o);
                let observed = observe(&mut o, reply(&b));
                assert_eq!(observed["dispatch_id"], dispatch);
                assert_eq!(observed["mode"], mode);
                assert_eq!(
                    observed["state"],
                    if mode == "queue" { "queued" } else { "note" }
                );
                let mut v = reply(&b);
                v["message"]["delivery"]["state"] =
                    json!(if mode == "queue" { "note" } else { "queued" });
                refuse(&b, &raw, &input, v);
                let mut v = reply(&b);
                v["message"]["delivery"]["dispatch_id"] = json!(id(9));
                refuse(&b, &raw, &input, v);
                let mut v = reply(&b);
                v["message"]["delivery"]["dispatch_id"] = Value::Null;
                refuse(&b, &raw, &input, v);
                if !dispatch.is_empty() {
                    let mut v = reply(&b);
                    v["message"]["delivery"]
                        .as_object_mut()
                        .unwrap()
                        .remove("dispatch_id");
                    refuse(&b, &raw, &input, v);
                }
            }
        }
    }
    #[test]
    fn missing_or_changed_marker_and_unretained_input_are_never_reconstructed() {
        let (b, raw, input) = fixture(record("note", ""));
        for marker in [
            vec![],
            id(9).into_bytes(),
            format!("{}\n", id(2)).into_bytes(),
        ] {
            assert!(matches!(
                Original::verify(b.clone(), &raw, &input, &marker),
                Err(Review::MarkerChanged)
            ));
        }
        let mut bound = b.clone();
        bound.marker_id = id(9);
        assert!(matches!(
            Original::verify(bound, &raw, &input, id(9).as_bytes()),
            Err(Review::MarkerChanged)
        ));
        assert!(matches!(
            Original::verify(b.clone(), &raw, b"", id(2).as_bytes()),
            Err(Review::MissingInput)
        ));
        let mut changed = input.clone();
        changed.push(b' ');
        assert!(matches!(
            Original::verify(b.clone(), &raw, &changed, id(2).as_bytes()),
            Err(Review::InputChanged)
        ));
        let mut content: Value = serde_json::from_slice(&input).unwrap();
        content["text"] = json!("other");
        let changed = serde_json::to_vec(&content).unwrap();
        let mut bound = b.clone();
        bound.input_sha256 = sha256(&changed);
        assert!(matches!(
            Original::verify(bound, &raw, &changed, id(2).as_bytes()),
            Err(Review::InputChanged)
        ));
        let mut changed = raw;
        changed.push(b' ');
        assert!(matches!(
            Original::verify(b, &changed, &input, id(2).as_bytes()),
            Err(Review::IdentityChanged)
        ));
    }
    #[test]
    fn source_ids_configuration_and_dispatch_are_exact_canonical_non_nil_data() {
        let (b, raw, input) = fixture(record("queue", ""));
        for key in ["request", "task", "config", "project", "mode", "dispatch"] {
            let mut bound = b.clone();
            match key {
                "request" => bound.request_id = "00000000-0000-0000-0000-000000000000".into(),
                "task" => bound.task_id = "00000000-0000-0000-0000-000000000000".into(),
                "config" => bound.configuration = "future".into(),
                "project" => bound.project = "".into(),
                "mode" => bound.mode = "future-mode".into(),
                _ => bound.dispatch_id = "not-canonical".into(),
            }
            assert!(
                Original::verify(bound, &raw, &input, id(2).as_bytes()).is_err(),
                "{key}"
            );
        }
        for original in [
            format!("{{{}}}", id(2)),
            "ABCDEF00-1111-4111-8111-000000000002".into(),
        ] {
            let mut value = record("queue", "");
            value["id"] = json!(original);
            let (b, raw, input) = fixture(value);
            assert!(Original::verify(b, &raw, &input, id(2).as_bytes()).is_err());
        }
        for version in [json!(2), json!(1.0), Value::Null] {
            let mut value = record("queue", "");
            value["version"] = version;
            let (b, raw, input) = fixture(value);
            assert!(Original::verify(b, &raw, &input, id(2).as_bytes()).is_err());
        }
    }
    #[test]
    fn content_mode_dispatch_and_attachment_fields_are_typed_before_projection() {
        for (path, value) in [
            ("mode", Value::Null),
            ("dispatch", json!(4)),
            ("content", Value::Null),
        ] {
            let mut record = record("queue", "");
            record["intent"][path] = value;
            let (b, raw, input) = fixture(record);
            assert!(Original::verify(b, &raw, &input, id(2).as_bytes()).is_err());
        }
        for value in [
            json!({"text":null}),
            json!({"text":"body","attachments":null}),
            json!({"text":"body","attachments":[{"name":"file","mime":"text/plain","data_base64":"!bad!"}]}),
            json!({"text":"body","attachments":vec![json!({"name":"file","mime":"text/plain","data_base64":"eA=="});MAX_ATTACHMENTS+1]}),
        ] {
            let mut record = record("queue", "");
            record["intent"]["content"] = value;
            let (b, raw, input) = fixture(record);
            assert!(Original::verify(b, &raw, &input, id(2).as_bytes()).is_err());
        }
    }
    #[test]
    fn valid_private_attachment_content_stays_frozen_and_never_enters_receipt() {
        let mut value = record("note", "");
        value["intent"]["content"]["attachments"] = json!([{"name":"PRIVATE.txt","mime":"text/plain","data_base64":"cHJpdmF0ZQ==","reference":"[File #1]","unknown":"kept"}]);
        let (b, raw, input) = fixture(value);
        let mut o = original(b.clone(), &raw, &input);
        let p = proposal(&mut o);
        assert_eq!(p.invocation().stdin.as_deref(), Some(input.as_slice()));
        let receipt = observe(&mut o, reply(&b));
        assert_eq!(receipt.as_object().unwrap().len(), 8);
        assert!(receipt.get("attachments").is_none());
        assert_eq!(o._raw, raw);
    }
    #[test]
    fn duplicate_known_keys_refuse_at_record_intent_content_and_attachment_levels() {
        let (b, raw, input) = fixture(record("queue", ""));
        let text = String::from_utf8(raw.clone()).unwrap();
        for (needle, replacement) in [
            ("\"id\":", "\"id\":null,\"id\":"),
            ("\"mode\":", "\"mode\":null,\"mode\":"),
            ("\"dispatch\":", "\"dispatch\":null,\"dispatch\":"),
            ("\"content\":", "\"content\":null,\"content\":"),
            ("\"text\":", "\"text\":null,\"text\":"),
            ("\"version\":", "\"version\":null,\"version\":"),
        ] {
            let bytes = text.replacen(needle, replacement, 1).into_bytes();
            assert_ne!(bytes, raw);
            let mut bound = b.clone();
            bound.source_sha256 = sha256(&bytes);
            assert!(
                matches!(
                    Original::verify(bound, &bytes, &input, id(2).as_bytes()),
                    Err(Review::InvalidRecord)
                ),
                "{needle}"
            );
        }
        let frozen = String::from_utf8(input.clone())
            .unwrap()
            .replacen("\"text\":", "\"text\":null,\"text\":", 1)
            .into_bytes();
        let mut bound = b;
        bound.input_sha256 = sha256(&frozen);
        assert!(matches!(
            Original::verify(bound, &raw, &frozen, id(2).as_bytes()),
            Err(Review::InvalidRecord)
        ));
        let mut value = record("note", "");
        value["intent"]["content"]["attachments"] =
            json!([{"name":"file","mime":"text/plain","data_base64":"eA=="}]);
        let (mut b, raw, input) = fixture(value);
        let bytes = String::from_utf8(raw)
            .unwrap()
            .replacen("\"name\":", "\"name\":null,\"name\":", 1)
            .into_bytes();
        b.source_sha256 = sha256(&bytes);
        assert!(matches!(
            Original::verify(b, &bytes, &input, id(2).as_bytes()),
            Err(Review::InvalidRecord)
        ));
    }
    #[test]
    fn duplicate_reply_keys_refuse_at_every_inspected_level() {
        let (b, raw, input) = fixture(record("queue", ""));
        let text = serde_json::to_string(&reply(&b)).unwrap();
        for needle in [
            "\"task\":",
            "\"message\":",
            "\"uuid\":",
            "\"project\":",
            "\"kind\":",
            "\"request_id\":",
            "\"session\":",
            "\"delivery\":",
            "\"mode\":",
            "\"state\":",
        ] {
            let bytes = text
                .replacen(needle, &format!("{needle}null,{needle}"), 1)
                .into_bytes();
            let mut o = original(b.clone(), &raw, &input);
            proposal(&mut o);
            assert!(
                matches!(
                    o.step(
                        Input::Reply(&Exit::Code {
                            code: 0,
                            stdout: bytes,
                            stderr: String::new()
                        }),
                        false,
                        false
                    ),
                    Decision::NeedsReview(Review::InvalidReply)
                ),
                "{needle}"
            );
        }
    }
    #[test]
    fn every_unexpected_native_delivery_field_refuses_even_without_session() {
        let (b, raw, input) = fixture(record("note", ""));
        for key in [
            "run_id",
            "conversation_id",
            "submitted_id",
            "prompt",
            "error",
        ] {
            for value in [
                Value::Null,
                json!(false),
                json!({}),
                json!(id(2)),
                json!("PRIVATE"),
            ] {
                let mut v = reply(&b);
                v["message"]["delivery"][key] = value;
                refuse(&b, &raw, &input, v);
            }
            let mut o = original(b.clone(), &raw, &input);
            proposal(&mut o);
            let mut v = reply(&b);
            v["message"]["delivery"][key] = json!("");
            observe(&mut o, v);
        }
        for value in [json!({}), json!(false), json!(""), json!([])] {
            let mut v = reply(&b);
            v["message"]["session"] = value;
            refuse(&b, &raw, &input, v);
        }
        let mut o = original(b.clone(), &raw, &input);
        proposal(&mut o);
        let mut v = reply(&b);
        v["message"].as_object_mut().unwrap().remove("session");
        let r = observe(&mut o, v);
        assert_eq!(r["executor"], Value::Null); // Never fall back to task.session.
    }
    #[test]
    fn incomplete_mismatched_future_and_uncertain_replies_never_settle() {
        let (b, raw, input) = fixture(record("queue", ""));
        for state in ["uncertain", "reserved", "submitted", "failed", "unknown"] {
            let mut v = reply(&b);
            v["message"]["delivery"]["state"] = json!(state);
            refuse(&b, &raw, &input, v);
        }
        for key in ["kind", "request_id", "delivery"] {
            let mut v = reply(&b);
            v["message"].as_object_mut().unwrap().remove(key);
            refuse(&b, &raw, &input, v);
        }
        for (section, key) in [
            ("task", "uuid"),
            ("task", "project"),
            ("message", "kind"),
            ("message", "request_id"),
        ] {
            let mut v = reply(&b);
            v[section][key] = json!("other");
            refuse(&b, &raw, &input, v);
        }
        for version in [json!(2), json!(1.0), Value::Null] {
            let mut v = reply(&b);
            v["schema"] = version;
            refuse(&b, &raw, &input, v);
        }
        for key in ["stage", "stage_at"] {
            let mut v = reply(&b);
            v["message"]["delivery"][key] = json!({});
            refuse(&b, &raw, &input, v);
        }
        refuse(
            &b,
            &raw,
            &input,
            json!({"task":{"uuid":id(1),"project":b.project},"message":null}),
        );
    }
    #[test]
    fn native_send_reopen_and_all_repeated_or_cancelled_proposals_stay_review() {
        for mode in ["send", "reopen"] {
            let (b, raw, input) = fixture(record(mode, &id(3)));
            let mut o = original(b, &raw, &input);
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Historical)
            ));
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Spent)
            ));
        }
        let (b, raw, input) = fixture(record("queue", ""));
        for (cancelled, pressure) in [(true, false), (false, true)] {
            let mut o = original(b.clone(), &raw, &input);
            o.step(Input::LookupRequested, cancelled, pressure);
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Spent)
            ));
        }
        let mut o = original(b.clone(), &raw, &input);
        drop(proposal(&mut o));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
        let mut o = original(b.clone(), &raw, &input);
        proposal(&mut o);
        o.step(Input::Unknown, false, false);
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
        let mut o = original(b.clone(), &raw, &input);
        assert!(matches!(
            o.step(Input::Reply(&exit(reply(&b))), false, false),
            Decision::NeedsReview(Review::NoLookup)
        ));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
    }
    #[test]
    fn bounds_nonzero_and_unknown_exit_fail_closed_without_body_errors() {
        let (b, raw, input) = fixture(record("note", ""));
        assert!(matches!(
            Original::verify(
                b.clone(),
                &vec![0; PRIVATE_BYTES + 1],
                &input,
                id(2).as_bytes()
            ),
            Err(Review::TooLarge)
        ));
        assert!(matches!(
            Original::verify(
                b.clone(),
                &raw,
                &vec![0; PRIVATE_BYTES + 1],
                id(2).as_bytes()
            ),
            Err(Review::TooLarge)
        ));
        assert!(matches!(
            Original::verify(b.clone(), &raw, &input, &[0; 129]),
            Err(Review::TooLarge)
        ));
        for value in [
            Exit::Code {
                code: 0,
                stdout: vec![0; REPLY_BYTES + 1],
                stderr: "PRIVATE".into(),
            },
            Exit::Code {
                code: 1,
                stdout: serde_json::to_vec(&reply(&b)).unwrap(),
                stderr: "PRIVATE".into(),
            },
            Exit::TimedOut,
        ] {
            let mut o = original(b.clone(), &raw, &input);
            proposal(&mut o);
            assert!(matches!(
                o.step(Input::Reply(&value), false, false),
                Decision::NeedsReview(_)
            ));
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Spent)
            ));
        }
        let oversized = Receipt {
            schema: 1,
            kind: Kind::TaskMessageRecovery,
            task_id: id(1),
            request_id: id(2),
            mode: "x".repeat(RECEIPT_BYTES),
            dispatch_id: String::new(),
            state: "note".into(),
            executor: (),
        };
        assert_eq!(receipt_size(&oversized), Err(Review::TooLarge));
    }
}
