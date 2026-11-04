//! Dormant original worktree STATUS proposals and closed evidence only.
//! No caller, runner, reducer, filesystem, ledger or current routing authority.
use crate::{
    actions::{Exit, Invocation},
    creation,
    paths::{self, Style},
    worktrees, ComputerId,
};
use serde::{
    de::{IgnoredAny, MapAccess, Visitor},
    Deserialize, Serialize,
};
use serde_json::Value;
use std::{collections::HashSet, fmt, io::Write};
use zerus_desktop_migration::sha256;

pub use super::creation_clone::{Review, RECEIPT_BYTES, REPLY_BYTES, SOURCE_BYTES};
const PATH_BYTES: usize = 8192;
const BRANCH_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CreationWorktreeStatus,
}

/// Expected immutable data. The original source target is not the destination.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub kind: Kind,
    pub schema: u32,
    pub configuration: String,
    pub creation_id: String,
    pub worktree_request_id: String,
    pub host: String,
    pub expected_machine: String,
    pub target: String,
    pub worktree_common: String,
    pub destination: String,
    pub branch: String,
    pub revision: u64,
    pub source_sha256: String,
}
enum State {
    Ready,
    Proposed,
    Done,
}

/// No Debug, Serialize or Clone exposes the retained private source.
pub struct Original {
    binding: Binding,
    _raw: Vec<u8>,
    style: Style,
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

/// Observations only: even effects_started=false cannot rotate an ID or retry.
/// Failed receipts never manufacture paths that the CLI did not return.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Receipt {
    Created {
        schema: u32,
        kind: Kind,
        creation_id: String,
        worktree_request_id: String,
        effects_started: bool,
        path: String,
        common_dir: String,
        branch: String,
    },
    Failed {
        schema: u32,
        kind: Kind,
        creation_id: String,
        worktree_request_id: String,
        effects_started: bool,
    },
}

const ORIGINAL_KEYS: &[&str] = &[
    "id",
    "creation_schema",
    "state",
    "cmd",
    "tag",
    "target",
    "parent",
    "created",
    "host",
    "revision",
    "expected_machine",
    "worktree_id",
    "worktree_common",
    "destination",
    "branch",
    "worktree_done",
    "worktree_receipts",
    "stop_requested",
    "launch_dispatched",
    "launch_id",
    "clone_id",
    "clone_done",
    "repository",
    "url",
    "bound_session",
    "creation_owner",
    "resource_error",
    "effects_started",
    "acquisition_confirmed",
    "auto",
    "show_pending",
    "new_project",
    "implicit_inbox",
    "history",
    "previous_launches",
    "worktree_previous",
];
const REPLY_KEYS: &[&str] = &[
    "schema",
    "request_id",
    "status",
    "effects_started",
    "path",
    "common_dir",
    "branch",
    "resource_error",
    "creation_owner",
    "fingerprint",
    "started_at",
    "finished_at",
    "detail",
];

// Inspect raw keys before Value can erase duplicates. Opaque unknown bytes are
// retained unchanged and never projected into receipts or errors.
fn unique_known(bytes: &[u8], keys: &[&str]) -> Result<(), ()> {
    struct Unique<'a>(&'a [&'a str]);
    impl<'de> Visitor<'de> for Unique<'_> {
        type Value = ();
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("one object")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
            let mut seen = HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if self.0.contains(&key.as_str()) && !seen.insert(key) {
                    return Err(serde::de::Error::custom("ambiguous known field"));
                }
                map.next_value::<IgnoredAny>()?;
            }
            Ok(())
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    serde::de::Deserializer::deserialize_map(&mut decoder, Unique(keys)).map_err(|_| ())?;
    decoder.end().map_err(|_| ())
}
fn uuid(value: &str) -> bool {
    creation::canonical_uuid(value) && value != "00000000-0000-0000-0000-000000000000"
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn bounded(value: &str, cap: usize) -> bool {
    value.len() <= cap && !value.chars().any(char::is_control)
}
fn flag(value: &Value, key: &str) -> bool {
    value[key].as_bool().unwrap_or(false)
}
fn resource_available(value: &Value) -> bool {
    value
        .get("resource_error")
        .is_none_or(|error| error.as_str() == Some(""))
}

impl Original {
    pub fn verify(binding: Binding, raw: &[u8]) -> Result<Self, Review> {
        if raw.len() > SOURCE_BYTES {
            return Err(Review::TooLarge);
        }
        unique_known(raw, ORIGINAL_KEYS).map_err(|_| Review::InvalidRecord)?;
        let value: Value = serde_json::from_slice(raw).map_err(|_| Review::InvalidRecord)?;
        if binding.schema != 1
            || !digest(&binding.configuration)
            || !uuid(&binding.creation_id)
            || !uuid(&binding.worktree_request_id)
            || !uuid(&binding.expected_machine)
            || !bounded(&binding.host, 512)
            || [
                &binding.target,
                &binding.worktree_common,
                &binding.destination,
            ]
            .iter()
            .any(|p| !bounded(p, PATH_BYTES))
            || binding.branch.is_empty()
            || !bounded(&binding.branch, BRANCH_BYTES)
            || binding.revision > i64::MAX as u64
            || sha256(raw) != binding.source_sha256
        {
            return Err(Review::IdentityChanged);
        }
        for (field, expected) in [
            ("id", &binding.creation_id),
            ("worktree_id", &binding.worktree_request_id),
            ("expected_machine", &binding.expected_machine),
            ("host", &binding.host),
            ("target", &binding.target),
            ("worktree_common", &binding.worktree_common),
            ("destination", &binding.destination),
            ("branch", &binding.branch),
        ] {
            if value[field].as_str() != Some(expected) {
                return Err(Review::IdentityChanged);
            }
        }
        if value["revision"].as_u64() != Some(binding.revision) {
            return Err(Review::IdentityChanged);
        }
        if !creation::valid_operation(&binding.creation_id, &value)
            || ["launch_id", "clone_id"].iter().any(|key| {
                value
                    .get(key)
                    .is_some_and(|id| !id.as_str().is_some_and(uuid))
            })
        {
            return Err(Review::InvalidRecord);
        }
        let style = if binding.host.is_empty() && paths::windows_shaped(&binding.destination) {
            Style::Windows
        } else {
            Style::Posix
        };
        if [
            &binding.target,
            &binding.worktree_common,
            &binding.destination,
        ]
        .iter()
        .any(|p| !paths::absolute(p, style))
        {
            return Err(Review::Unsupported);
        }
        let clone_settled = if value.get("clone_id").is_some() {
            flag(&value, "repository") && flag(&value, "clone_done")
        } else {
            !flag(&value, "repository") && !flag(&value, "clone_done")
        };
        let eligible = matches!(value["state"].as_str(), Some("worktree" | "unknown"))
            && flag(&value, "worktree_receipts")
            && !flag(&value, "worktree_done")
            && !flag(&value, "stop_requested")
            && !flag(&value, "launch_dispatched")
            && value.get("bound_session").is_none()
            && value.get("creation_owner").is_none()
            && value.get("acquisition_confirmed").is_none()
            && resource_available(&value)
            && clone_settled;
        Ok(Self {
            binding,
            _raw: raw.to_vec(),
            style,
            eligible,
            state: State::Ready,
        })
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    /// Private state spends the original before returning its sole proposal.
    /// No cancellation, lost result or effects=false can regenerate it.
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
                let invocation = worktrees::receipt_invocation(
                    &ComputerId::from(self.binding.host.as_str()),
                    "status",
                    &self.binding.worktree_request_id,
                    &self.binding.expected_machine,
                );
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
        unique_known(stdout, REPLY_KEYS).map_err(|_| Review::InvalidReply)?;
        let data = worktrees::receipt(
            Exit::Code {
                code: 0,
                stdout: stdout.clone(),
                stderr: String::new(),
            },
            &self.binding.worktree_request_id,
        )
        .map_err(|_| Review::InvalidReply)?;
        if !resource_available(&data) {
            return Err(Review::UnknownOutcome);
        }
        let created = match data["status"].as_str() {
            Some("created") => true,
            Some("failed") => false,
            _ => return Err(Review::UnknownOutcome),
        };
        let effects_started = data["effects_started"]
            .as_bool()
            .ok_or(Review::InvalidReply)?;
        if created && !effects_started {
            return Err(Review::InvalidReply);
        }
        for (key, expected) in [
            ("path", &self.binding.destination),
            ("common_dir", &self.binding.worktree_common),
        ] {
            if let Some(observed) = data.get(key) {
                let observed = observed.as_str().ok_or(Review::InvalidReply)?;
                if !paths::absolute(observed, self.style)
                    || !paths::same(observed, expected, self.style)
                {
                    return Err(Review::IdentityChanged);
                }
            } else if created {
                return Err(Review::InvalidReply);
            }
        }
        if let Some(branch) = data.get("branch") {
            if branch.as_str() != Some(&self.binding.branch) {
                return Err(Review::IdentityChanged);
            }
        } else if created {
            return Err(Review::InvalidReply);
        }
        let receipt = if created {
            Receipt::Created {
                schema: 1,
                kind: Kind::CreationWorktreeStatus,
                creation_id: self.binding.creation_id.clone(),
                worktree_request_id: self.binding.worktree_request_id.clone(),
                effects_started,
                path: data["path"].as_str().unwrap().into(),
                common_dir: data["common_dir"].as_str().unwrap().into(),
                branch: data["branch"].as_str().unwrap().into(),
            }
        } else {
            Receipt::Failed {
                schema: 1,
                kind: Kind::CreationWorktreeStatus,
                creation_id: self.binding.creation_id.clone(),
                worktree_request_id: self.binding.worktree_request_id.clone(),
                effects_started,
            }
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
    fn record() -> Value {
        json!({"creation_schema":1,"id":id(1),"worktree_id":id(2),"expected_machine":id(3),
            "host":"peer-fixture","target":"/srv/repository","worktree_common":"/srv/repository/.git",
            "destination":"/srv/checkout","branch":"fixture-branch","revision":7,"state":"worktree",
            "cmd":"codex","tag":"fixture","created":1,"worktree_receipts":true,
            "launch_id":id(4),"opaque":{"private":"kept / пример"}})
    }
    fn fixture(value: Value) -> (Binding, Vec<u8>) {
        let raw = serde_json::to_vec_pretty(&value).unwrap();
        let text = |k: &str| value[k].as_str().unwrap_or("").to_owned();
        let binding = Binding {
            kind: Kind::CreationWorktreeStatus,
            schema: 1,
            configuration: "a".repeat(64),
            creation_id: id(1),
            worktree_request_id: id(2),
            expected_machine: id(3),
            host: text("host"),
            target: text("target"),
            worktree_common: text("worktree_common"),
            destination: text("destination"),
            branch: text("branch"),
            revision: 7,
            source_sha256: sha256(&raw),
        };
        (binding, raw)
    }
    fn original(binding: Binding, raw: &[u8]) -> Original {
        Original::verify(binding, raw).unwrap_or_else(|e| panic!("{e:?}"))
    }
    fn proposal(o: &mut Original) -> Proposal {
        match o.step(Input::LookupRequested, false, false) {
            Decision::Proposed(p) => p,
            _ => panic!("missing proposal"),
        }
    }
    fn exit(value: Value) -> Exit {
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&value).unwrap(),
            stderr: "PRIVATE STDERR".into(),
        }
    }
    fn created(b: &Binding) -> Value {
        json!({"schema":1,"request_id":b.worktree_request_id,"status":"created","effects_started":true,
            "path":b.destination,"common_dir":b.worktree_common,"branch":b.branch,
            "creation_owner":{"private":"discard"},"fingerprint":"PRIVATE","detail":"PRIVATE"})
    }
    fn observe(o: &mut Original, value: Value) -> Value {
        match o.step(Input::Reply(&exit(value)), false, false) {
            Decision::Observed { binding, receipt } => {
                assert!(binding == *o.binding());
                serde_json::to_value(receipt).unwrap()
            }
            _ => panic!("missing receipt"),
        }
    }
    fn refused(b: &Binding, raw: &[u8], value: Value) {
        let mut o = original(b.clone(), raw);
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
    fn original_bytes_distinct_destination_and_one_existing_status_invocation() {
        let (b, raw) = fixture(record());
        let mut o = original(b.clone(), &raw);
        assert_eq!(o._raw, raw);
        assert_ne!(b.target, b.destination);
        assert!(matches!(
            o.step(Input::Review, false, false),
            Decision::NeedsReview(Review::NoLookup)
        ));
        let p = proposal(&mut o);
        assert!(p.binding() == &b);
        assert_eq!(
            p.invocation().args,
            vec![
                "@peer-fixture",
                "--expect-machine",
                &id(3),
                "worktrees",
                "status",
                "--request-id",
                &id(2),
                "--json"
            ]
        );
        assert!(p.invocation().stdin.is_none());
        assert!(p.invocation().request_id.is_none());
        assert_eq!(p.invocation().timeout, std::time::Duration::from_secs(15));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
        let receipt = observe(&mut o, created(&b));
        assert_eq!(receipt.as_object().unwrap().len(), 9);
        assert_eq!(receipt["kind"], "creation_worktree_status");
        assert_eq!(receipt["path"], b.destination);
        assert_eq!(receipt["creation_id"], id(1));
        assert_eq!(receipt["worktree_request_id"], id(2));
        assert!(!receipt.to_string().contains("PRIVATE"));
        assert_eq!(o._raw, raw);
    }
    #[test]
    fn real_cli_shaped_failed_receipts_keep_only_six_observed_fields() {
        let (b, raw) = fixture(record());
        for effects in [false, true] {
            let mut o = original(b.clone(), &raw);
            proposal(&mut o);
            let value = json!({"schema":1,"request_id":id(2),"status":"failed","effects_started":effects,
                "detail":"PRIVATE","fingerprint":"PRIVATE","started_at":1,"finished_at":2});
            assert!(worktrees::receipt(exit(value.clone()), &id(2)).is_ok());
            let receipt = observe(&mut o, value);
            assert_eq!(receipt.as_object().unwrap().len(), 6);
            assert_eq!(receipt["effects_started"], effects);
            assert_eq!(receipt["status"], "failed");
            for key in [
                "path",
                "common_dir",
                "branch",
                "detail",
                "fingerprint",
                "started_at",
                "finished_at",
            ] {
                assert!(receipt.get(key).is_none(), "{key}");
            }
            assert_eq!(o._raw, raw);
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Spent)
            ));
        }
    }
    #[test]
    fn created_needs_both_prepared_paths_branch_and_true_effects() {
        let (b, raw) = fixture(record());
        for key in ["path", "common_dir", "branch", "effects_started"] {
            let mut v = created(&b);
            v.as_object_mut().unwrap().remove(key);
            refused(&b, &raw, v);
            let mut v = created(&b);
            v[key] = Value::Null;
            refused(&b, &raw, v);
        }
        let mut v = created(&b);
        v["effects_started"] = json!(false);
        refused(&b, &raw, v);
        let mut v = created(&b);
        v["path"] = json!(b.target);
        refused(&b, &raw, v);
        for key in ["common_dir", "branch"] {
            let mut v = created(&b);
            v[key] = json!("changed");
            refused(&b, &raw, v);
        }
    }
    #[test]
    fn failed_optional_identities_must_match_but_are_not_copied() {
        let (b, raw) = fixture(record());
        let mut v = created(&b);
        v["status"] = json!("failed");
        let mut o = original(b.clone(), &raw);
        proposal(&mut o);
        let receipt = observe(&mut o, v.clone());
        assert_eq!(receipt.as_object().unwrap().len(), 6);
        for key in ["path", "common_dir", "branch"] {
            let mut changed = v.clone();
            changed[key] = json!("other");
            refused(&b, &raw, changed);
            let mut changed = v.clone();
            changed[key] = Value::Null;
            refused(&b, &raw, changed);
        }
        for effects in [Value::Null, json!(0), json!("false")] {
            let mut changed = v.clone();
            changed["effects_started"] = effects;
            refused(&b, &raw, changed);
        }
        v.as_object_mut().unwrap().remove("effects_started");
        refused(&b, &raw, v);
    }
    #[test]
    fn resource_errors_unknown_status_and_future_or_missing_schema_stay_review() {
        let (b, raw) = fixture(record());
        for error in [
            Value::Null,
            json!(false),
            json!({}),
            json!("PRIVATE resource detail"),
        ] {
            let mut v = created(&b);
            v["resource_error"] = error;
            refused(&b, &raw, v);
        }
        let mut o = original(b.clone(), &raw);
        proposal(&mut o);
        let mut v = created(&b);
        v["resource_error"] = json!("");
        observe(&mut o, v);
        for state in [
            "running",
            "unconfirmed",
            "not_found",
            "unknown",
            "unsupported",
        ] {
            let mut v = created(&b);
            v["status"] = json!(state);
            refused(&b, &raw, v);
        }
        for schema in [Value::Null, json!(2), json!(1.0), json!("1")] {
            let mut v = created(&b);
            v["schema"] = schema;
            refused(&b, &raw, v);
        }
        let mut v = created(&b);
        v.as_object_mut().unwrap().remove("schema");
        refused(&b, &raw, v);
        let mut v = created(&b);
        v["request_id"] = json!(b.creation_id);
        refused(&b, &raw, v);
    }
    #[test]
    fn known_duplicates_are_rejected_before_value_in_both_inputs() {
        let (b, raw) = fixture(record());
        let text = String::from_utf8(raw.clone()).unwrap();
        for key in [
            "id",
            "worktree_common",
            "destination",
            "branch",
            "revision",
            "effects_started",
            "resource_error",
        ] {
            let bytes = format!("{{\"{key}\":null,\"{key}\":null,{}", &text[1..]).into_bytes();
            let mut bound = b.clone();
            bound.source_sha256 = sha256(&bytes);
            assert!(matches!(
                Original::verify(bound, &bytes),
                Err(Review::InvalidRecord)
            ));
        }
        let text = serde_json::to_string(&created(&b)).unwrap();
        for key in [
            "schema",
            "request_id",
            "effects_started",
            "path",
            "common_dir",
            "branch",
            "resource_error",
        ] {
            let mut o = original(b.clone(), &raw);
            proposal(&mut o);
            let bytes = format!("{{\"{key}\":null,\"{key}\":null,{}", &text[1..]).into_bytes();
            assert!(matches!(
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
            ));
        }
    }
    #[test]
    fn missing_changed_float_and_nil_original_identity_never_defaults() {
        let (b, raw) = fixture(record());
        for key in [
            "id",
            "worktree_id",
            "expected_machine",
            "host",
            "target",
            "worktree_common",
            "destination",
            "branch",
            "revision",
        ] {
            for remove in [false, true] {
                let mut v = record();
                if remove {
                    v.as_object_mut().unwrap().remove(key);
                } else {
                    v[key] = if key == "revision" {
                        json!(8)
                    } else {
                        json!("changed")
                    };
                }
                let bytes = serde_json::to_vec(&v).unwrap();
                let mut expected = b.clone();
                expected.source_sha256 = sha256(&bytes);
                assert!(Original::verify(expected, &bytes).is_err(), "{key}");
            }
        }
        for rev in [Value::Null, json!(7.0), json!(-1)] {
            let mut v = record();
            v["revision"] = rev;
            let (b, raw) = fixture(v);
            assert!(Original::verify(b, &raw).is_err());
        }
        for key in [
            "id",
            "worktree_id",
            "expected_machine",
            "clone_id",
            "launch_id",
        ] {
            let mut v = record();
            v[key] = json!("00000000-0000-0000-0000-000000000000");
            let (b, raw) = fixture(v);
            assert!(Original::verify(b, &raw).is_err());
        }
        let mut bound = b.clone();
        bound.configuration = "future".into();
        assert!(Original::verify(bound, &raw).is_err());
        let mut bytes = raw;
        bytes.push(b' ');
        assert!(Original::verify(b, &bytes).is_err());
    }
    #[test]
    fn only_pending_supported_worktrees_with_settled_clone_can_propose() {
        for state in creation::STATES {
            let mut v = record();
            v["state"] = json!(state);
            let (b, raw) = fixture(v);
            let mut o = original(b, &raw);
            assert_eq!(
                matches!(
                    o.step(Input::LookupRequested, false, false),
                    Decision::Proposed(_)
                ),
                matches!(state, "worktree" | "unknown")
            );
        }
        for (key, value) in [
            ("worktree_receipts", json!(false)),
            ("worktree_done", json!(true)),
            ("stop_requested", json!(true)),
            ("launch_dispatched", json!(true)),
            ("bound_session", Value::Null),
            ("creation_owner", json!({})),
            ("acquisition_confirmed", json!(true)),
            ("resource_error", Value::Null),
            ("repository", json!(true)),
            ("clone_done", json!(true)),
        ] {
            let mut v = record();
            v[key] = value;
            let (b, raw) = fixture(v);
            let mut o = original(b, &raw);
            assert!(
                matches!(
                    o.step(Input::LookupRequested, false, false),
                    Decision::NeedsReview(Review::Historical)
                ),
                "{key}"
            );
        }
        for done in [false, true] {
            let mut v = record();
            v["repository"] = json!(true);
            v["url"] = json!("https://example.test/repo.git");
            v["clone_id"] = json!(id(5));
            v["clone_done"] = json!(done);
            let (b, raw) = fixture(v);
            let mut o = original(b, &raw);
            assert_eq!(
                matches!(
                    o.step(Input::LookupRequested, false, false),
                    Decision::Proposed(_)
                ),
                done
            );
        }
    }
    #[test]
    fn cancelled_pressure_lost_proposal_unknown_and_unrequested_reply_do_not_regenerate() {
        let (b, raw) = fixture(record());
        for (cancelled, pressure) in [(true, false), (false, true)] {
            let mut o = original(b.clone(), &raw);
            o.step(Input::LookupRequested, cancelled, pressure);
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Spent)
            ));
        }
        let mut o = original(b.clone(), &raw);
        drop(proposal(&mut o));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
        for before_proposal in [false, true] {
            let mut o = original(b.clone(), &raw);
            if !before_proposal {
                proposal(&mut o);
            }
            o.step(Input::Unknown, false, false);
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Spent)
            ));
        }
        let mut o = original(b.clone(), &raw);
        assert!(matches!(
            o.step(Input::Reply(&exit(created(&b))), false, false),
            Decision::NeedsReview(Review::NoLookup)
        ));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
    }
    #[test]
    fn original_local_windows_style_keeps_observed_spelling_and_remote_posix_refuses_drive() {
        let mut v = record();
        v["host"] = json!("");
        v["target"] = json!("C:/Repo");
        v["worktree_common"] = json!("C:/Repo/.git");
        v["destination"] = json!("C:/Checkout");
        let (b, raw) = fixture(v.clone());
        let mut o = original(b.clone(), &raw);
        proposal(&mut o);
        let mut reply = created(&b);
        reply["path"] = json!("c:\\CHECKOUT");
        reply["common_dir"] = json!("c:\\REPO\\.GIT");
        let r = observe(&mut o, reply);
        assert_eq!(r["path"], "c:\\CHECKOUT");
        assert_eq!(r["common_dir"], "c:\\REPO\\.GIT");
        v["host"] = json!("peer");
        let (b, raw) = fixture(v);
        assert!(matches!(
            Original::verify(b, &raw),
            Err(Review::Unsupported)
        ));
        let (b, raw) = fixture(record());
        let mut v = created(&b);
        v["path"] = json!("/srv//checkout");
        refused(&b, &raw, v);
    }
    #[test]
    fn full_encoded_receipt_and_raw_reply_source_caps_refuse_without_clipping() {
        let (b, raw) = fixture(record());
        assert!(matches!(
            Original::verify(b.clone(), &vec![0; SOURCE_BYTES + 1]),
            Err(Review::TooLarge)
        ));
        for bytes in [vec![0; REPLY_BYTES + 1], b"{bad".to_vec()] {
            let mut o = original(b.clone(), &raw);
            proposal(&mut o);
            assert!(matches!(
                o.step(
                    Input::Reply(&Exit::Code {
                        code: 0,
                        stdout: bytes,
                        stderr: String::new()
                    }),
                    false,
                    false
                ),
                Decision::NeedsReview(_)
            ));
        }
        let mut v = record();
        v["worktree_common"] = json!(format!("/{}", "c".repeat(8180)));
        v["destination"] = json!(format!("/{}", "d".repeat(8180)));
        let (b, raw) = fixture(v);
        let mut o = original(b.clone(), &raw);
        proposal(&mut o);
        assert!(matches!(
            o.step(Input::Reply(&exit(created(&b))), false, false),
            Decision::NeedsReview(Review::TooLarge)
        ));
    }
    #[test]
    fn nonzero_or_uncertain_exit_never_accepts_a_valid_looking_body() {
        let (b, raw) = fixture(record());
        let Exit::Code { stdout, .. } = exit(created(&b)) else {
            unreachable!()
        };
        for result in [
            Exit::Code {
                code: 1,
                stdout,
                stderr: "PRIVATE".into(),
            },
            Exit::TimedOut,
            Exit::StartFailed("PRIVATE".into()),
        ] {
            let mut o = original(b.clone(), &raw);
            proposal(&mut o);
            assert!(matches!(
                o.step(Input::Reply(&result), false, false),
                Decision::NeedsReview(Review::UnknownOutcome)
            ));
        }
    }
}
