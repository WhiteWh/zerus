//! Dormant original clone STATUS policy. Proposals and receipts are data only:
//! no runner, reducer, writer, filesystem, ledger or current routing authority.
use crate::{
    actions::{Exit, Invocation},
    creation,
    paths::{self, Style},
    repository, ComputerId,
};
use serde::{
    de::{IgnoredAny, MapAccess, Visitor},
    Deserialize, Serialize,
};
use serde_json::Value;
use std::{collections::HashSet, fmt, io::Write};
use zerus_desktop_migration::sha256;

pub const SOURCE_BYTES: usize = 1 << 20;
pub const REPLY_BYTES: usize = 64 << 10;
pub const RECEIPT_BYTES: usize = 16 << 10;
const PATH_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CreationCloneStatus,
}

/// Immutable expected data; it cannot select a current source, route or owner.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub kind: Kind,
    pub schema: u32,
    pub configuration: String,
    pub creation_id: String,
    pub clone_request_id: String,
    pub host: String,
    pub expected_machine: String,
    pub target: String,
    pub parent: String,
    pub revision: u64,
    pub source_sha256: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review {
    TooLarge,
    InvalidRecord,
    IdentityChanged,
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

/// No Debug, Serialize or Clone: the complete private original stays private.
pub struct Original {
    binding: Binding,
    // Keep the exact private bytes even though this dormant slice has no writer.
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

/// Private host replies never enter Debug or body-bearing errors.
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
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub schema: u32,
    pub kind: Kind,
    pub creation_id: String,
    pub clone_request_id: String,
    pub status: String,
    pub parent: String,
    pub path: String,
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
    "repository",
    "url",
    "clone_id",
    "expected_machine",
    "clone_done",
    "stop_requested",
    "launch_dispatched",
    "worktree_id",
    "launch_id",
    "worktree_done",
    "auto",
    "show_pending",
    "new_project",
    "implicit_inbox",
    "worktree_receipts",
    "history",
    "previous_launches",
    "worktree_previous",
    "bound_session",
    "acquisition_confirmed",
];
const REPLY_KEYS: &[&str] = &["schema", "request_id", "status", "parent", "path", "name"];

// Check the raw object BEFORE a Value can erase duplicate known keys. Nested
// opaque fields remain opaque; all original bytes are retained unchanged.
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
fn bounded(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}
fn flag(value: &Value, name: &str) -> bool {
    value[name].as_bool().unwrap_or(false)
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
            || !uuid(&binding.clone_request_id)
            || !uuid(&binding.expected_machine)
            || !bounded(&binding.host, 512)
            || !bounded(&binding.target, PATH_BYTES)
            || !bounded(&binding.parent, PATH_BYTES)
            || binding.revision > i64::MAX as u64
            || sha256(raw) != binding.source_sha256
        {
            return Err(Review::IdentityChanged);
        }
        for (name, expected) in [
            ("id", &binding.creation_id),
            ("clone_id", &binding.clone_request_id),
            ("expected_machine", &binding.expected_machine),
            ("host", &binding.host),
            ("target", &binding.target),
            ("parent", &binding.parent),
        ] {
            if value[name].as_str() != Some(expected) {
                return Err(Review::IdentityChanged);
            }
        }
        if value["revision"].as_u64() != Some(binding.revision) {
            return Err(Review::IdentityChanged);
        }
        if !creation::valid_operation(&binding.creation_id, &value)
            || !creation::safe_repository_url(value["url"].as_str().unwrap_or(""))
            || value
                .get("launch_id")
                .is_some_and(|id| !id.as_str().is_some_and(uuid))
        {
            return Err(Review::InvalidRecord);
        }
        let style = if binding.host.is_empty() && paths::windows_shaped(&binding.parent) {
            Style::Windows
        } else {
            Style::Posix
        };
        if !paths::absolute(&binding.parent, style) || !paths::absolute(&binding.target, style) {
            return Err(Review::Unsupported);
        }
        // Absent clone_done retains the existing absent-false semantics. A
        // worktree claim, even malformed/null, is ambiguous in this first kind.
        let eligible = matches!(value["state"].as_str(), Some("cloning" | "unknown"))
            && flag(&value, "repository")
            && !flag(&value, "clone_done")
            && !flag(&value, "stop_requested")
            && !flag(&value, "launch_dispatched")
            && !flag(&value, "worktree_done")
            && value.get("bound_session").is_none()
            && value.get("acquisition_confirmed").is_none()
            && value.get("worktree_id").is_none();
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

    /// One retained instance can propose only once, including a lost proposal.
    /// Cancellation and pressure are policy data, never runtime admission proof.
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
                let computer = ComputerId::from(self.binding.host.as_str());
                let invocation = repository::invocation(
                    &computer,
                    repository::Action::Status,
                    "",
                    "",
                    "",
                    &self.binding.clone_request_id,
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
                match self.receipt(exit) {
                    Ok(receipt) => Decision::Observed {
                        binding: self.binding.clone(),
                        receipt,
                    },
                    Err(reason) => Decision::NeedsReview(reason),
                }
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
        let data: Value = serde_json::from_slice(stdout).map_err(|_| Review::InvalidReply)?;
        // repository-clone-v1 receipts are currently unversioned. An explicit
        // future/malformed schema cannot masquerade as that existing format.
        if data.get("schema").is_some_and(|s| s.as_u64() != Some(1)) {
            return Err(Review::InvalidReply);
        }
        let computer = ComputerId::from(self.binding.host.as_str());
        let data = repository::reply(
            Exit::Code {
                code: 0,
                stdout: stdout.clone(),
                stderr: String::new(),
            },
            &computer,
            repository::Action::Status,
            "",
            &self.binding.clone_request_id,
        )
        .map_err(|_| Review::InvalidReply)?;
        let parent = data["parent"].as_str().ok_or(Review::InvalidReply)?;
        let path = data["path"].as_str().ok_or(Review::InvalidReply)?;
        if !paths::same(parent, &self.binding.parent, self.style)
            || !paths::same(path, &self.binding.target, self.style)
            || !paths::absolute(parent, self.style)
            || !paths::absolute(path, self.style)
        {
            return Err(Review::IdentityChanged);
        }
        let status = data["status"].as_str().ok_or(Review::InvalidReply)?;
        if !matches!(status, "cloned" | "failed") {
            return Err(Review::UnknownOutcome);
        }
        let receipt = Receipt {
            schema: 1,
            kind: Kind::CreationCloneStatus,
            creation_id: self.binding.creation_id.clone(),
            clone_request_id: self.binding.clone_request_id.clone(),
            status: status.into(),
            parent: parent.into(),
            path: path.into(),
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
    fn fixture() -> (Binding, Vec<u8>) {
        let value = json!({"creation_schema":1,"id":id(1),"clone_id":id(2),"expected_machine":id(3),
            "host":"peer-fixture","target":"/srv/repo","parent":"/srv","revision":7,"state":"cloning",
            "cmd":"codex","tag":"fixture","created":1,"repository":true,"url":"https://example.test/repo.git",
            "launch_id":id(4),"opaque":{"private":"kept / пример"}});
        changed(value)
    }
    fn changed(value: Value) -> (Binding, Vec<u8>) {
        let raw = serde_json::to_vec_pretty(&value).unwrap();
        let binding = Binding {
            kind: Kind::CreationCloneStatus,
            schema: 1,
            configuration: "a".repeat(64),
            creation_id: id(1),
            clone_request_id: id(2),
            expected_machine: id(3),
            host: value["host"].as_str().unwrap_or("").into(),
            target: value["target"].as_str().unwrap_or("").into(),
            parent: value["parent"].as_str().unwrap_or("").into(),
            revision: 7,
            source_sha256: sha256(&raw),
        };
        (binding, raw)
    }
    fn reply(binding: &Binding, status: &str) -> Exit {
        Exit::Code { code:0,stdout:serde_json::to_vec(&json!({"request_id":binding.clone_request_id,
            "parent":binding.parent,"path":binding.target,"status":status,"detail":"PRIVATE DIAGNOSTIC",
            "url":"PRIVATE URL","task":{"private":"discard"}})).unwrap(),stderr:String::new() }
    }
    fn original(binding: Binding, raw: &[u8]) -> Original {
        Original::verify(binding, raw).unwrap_or_else(|e| panic!("{e:?}"))
    }
    fn proposed(original: &mut Original) -> Proposal {
        match original.step(Input::LookupRequested, false, false) {
            Decision::Proposed(p) => p,
            _ => panic!("missing proposal"),
        }
    }
    fn observed(original: &mut Original, exit: &Exit) -> Receipt {
        match original.step(Input::Reply(exit), false, false) {
            Decision::Observed { receipt, .. } => receipt,
            _ => panic!("missing receipt"),
        }
    }
    #[test]
    fn exact_original_unknown_bytes_and_existing_status_args_are_preserved() {
        let (binding, raw) = fixture();
        let mut o = original(binding.clone(), &raw);
        assert!(o._raw == raw);
        assert!(matches!(
            o.step(Input::Review, false, false),
            Decision::NeedsReview(Review::NoLookup)
        ));
        let p = proposed(&mut o);
        assert!(p.binding() == &binding);
        let expected = repository::invocation(
            &ComputerId::from(binding.host.as_str()),
            repository::Action::Status,
            "",
            "",
            "",
            &id(2),
            &id(3),
        );
        assert_eq!(p.invocation().args, expected.args);
        assert!(p.invocation().stdin.is_none());
        assert_eq!(p.invocation().timeout, std::time::Duration::from_secs(30));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
        let r = observed(&mut o, &reply(&binding, "cloned"));
        let value = serde_json::to_value(r).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 7);
        assert_eq!(value["kind"], "creation_clone_status");
        assert_eq!(value["clone_request_id"], id(2));
        assert_eq!(value["creation_id"], id(1));
        assert!(!value.to_string().contains("PRIVATE"));
        assert!(o._raw == raw);
        assert!(matches!(
            o.step(Input::Reply(&reply(&binding, "cloned")), false, false),
            Decision::NeedsReview(Review::Spent)
        ));
    }
    #[test]
    fn matching_path_and_request_do_not_hide_a_mismatched_parent() {
        let (b, raw) = fixture();
        let mut data: Value = serde_json::from_slice(match &reply(&b, "cloned") {
            Exit::Code { stdout, .. } => stdout,
            _ => unreachable!(),
        })
        .unwrap();
        data["parent"] = json!("/other");
        let exit = Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&data).unwrap(),
            stderr: String::new(),
        };
        assert!(repository::reply(
            exit.clone(),
            &ComputerId::from(b.host.as_str()),
            repository::Action::Status,
            "",
            &b.clone_request_id
        )
        .is_ok());
        let mut o = original(b, &raw);
        proposed(&mut o);
        assert!(matches!(
            o.step(Input::Reply(&exit), false, false),
            Decision::NeedsReview(Review::IdentityChanged)
        ));
    }
    #[test]
    fn every_identity_and_integer_revision_is_exact_non_nil_data() {
        let (b, raw) = fixture();
        for field in [
            "id",
            "clone_id",
            "expected_machine",
            "host",
            "parent",
            "target",
            "revision",
        ] {
            let mut v: Value = serde_json::from_slice(&raw).unwrap();
            v[field] = if field == "revision" {
                json!(8)
            } else {
                json!("changed")
            };
            let bytes = serde_json::to_vec(&v).unwrap();
            let mut bound = b.clone();
            bound.source_sha256 = sha256(&bytes);
            assert!(Original::verify(bound, &bytes).is_err(), "{field}");
        }
        for field in ["id", "clone_id", "expected_machine"] {
            let mut bound = b.clone();
            match field {
                "id" => bound.creation_id = "00000000-0000-0000-0000-000000000000".into(),
                "clone_id" => {
                    bound.clone_request_id = "00000000-0000-0000-0000-000000000000".into()
                }
                _ => bound.expected_machine = "00000000-0000-0000-0000-000000000000".into(),
            }
            assert!(Original::verify(bound, &raw).is_err());
        }
        for revision in [Value::Null, json!(7.0), json!(-1)] {
            let mut v: Value = serde_json::from_slice(&raw).unwrap();
            v["revision"] = revision;
            let (bound, bytes) = changed(v);
            assert!(Original::verify(bound, &bytes).is_err());
        }
        let mut changed_config = b.clone();
        changed_config.configuration = "future".into();
        assert!(Original::verify(changed_config, &raw).is_err());
        let mut bytes = raw;
        bytes.push(b' ');
        assert!(Original::verify(b, &bytes).is_err());
    }
    #[test]
    fn raw_duplicate_known_fields_refuse_before_value_projection() {
        let (mut b, raw) = fixture();
        let text = String::from_utf8(raw).unwrap();
        for field in ["id", "clone_id", "revision", "parent", "repository"] {
            let v: Value = serde_json::from_str(&text).unwrap();
            let raw = format!("{{\"{field}\":{},{}", v[field], &text[1..]).into_bytes();
            b.source_sha256 = sha256(&raw);
            assert!(matches!(
                Original::verify(b.clone(), &raw),
                Err(Review::InvalidRecord)
            ));
        }
        let (b, raw) = fixture();
        for field in ["request_id", "status", "parent", "path", "schema"] {
            let mut o = original(b.clone(), &raw);
            proposed(&mut o);
            let Exit::Code { stdout, .. } = reply(&b, "cloned") else {
                unreachable!()
            };
            let s = String::from_utf8(stdout).unwrap();
            let mut v: Value = serde_json::from_str(&s).unwrap();
            if field == "schema" {
                v[field] = json!(1);
            }
            let data = format!(
                "{{\"{field}\":{},{}",
                v[field],
                &serde_json::to_string(&v).unwrap()[1..]
            );
            let exit = Exit::Code {
                code: 0,
                stdout: data.into_bytes(),
                stderr: String::new(),
            };
            assert!(matches!(
                o.step(Input::Reply(&exit), false, false),
                Decision::NeedsReview(Review::InvalidReply)
            ));
        }
    }
    #[test]
    fn only_original_pending_clone_states_are_lookup_eligible() {
        let (_, raw) = fixture();
        let base: Value = serde_json::from_slice(&raw).unwrap();
        for state in creation::STATES {
            let mut v = base.clone();
            v["state"] = json!(state);
            let (b, raw) = changed(v);
            let mut o = original(b, &raw);
            assert_eq!(
                matches!(
                    o.step(Input::LookupRequested, false, false),
                    Decision::Proposed(_)
                ),
                matches!(state, "cloning" | "unknown")
            );
        }
        for field in [
            "repository",
            "clone_done",
            "stop_requested",
            "launch_dispatched",
            "worktree_id",
        ] {
            let mut v = base.clone();
            v[field] = match field {
                "repository" => json!(false),
                "worktree_id" => json!(id(9)),
                _ => json!(true),
            };
            let (b, raw) = changed(v);
            let mut o = original(b, &raw);
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Historical)
            ));
        }
        for url in [
            "https://u:token@example.test/repo",
            "https://example.test/repo?token=x",
            "https://example.test/repo#private",
        ] {
            let mut v = base.clone();
            v["url"] = json!(url);
            let (b, raw) = changed(v);
            assert!(Original::verify(b, &raw).is_err());
        }
    }
    #[test]
    fn cancelled_pressure_lost_and_unknown_results_never_regenerate() {
        let (b, raw) = fixture();
        for (cancelled, pressure) in [(true, false), (false, true)] {
            let mut o = original(b.clone(), &raw);
            assert!(matches!(
                o.step(Input::LookupRequested, cancelled, pressure),
                Decision::NeedsReview(_)
            ));
            assert!(matches!(
                o.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Spent)
            ));
        }
        let mut o = original(b.clone(), &raw);
        drop(proposed(&mut o));
        assert!(matches!(
            o.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
        let mut o = original(b.clone(), &raw);
        proposed(&mut o);
        assert!(matches!(
            o.step(Input::Reply(&reply(&b, "unconfirmed")), false, false),
            Decision::NeedsReview(Review::UnknownOutcome)
        ));
        assert!(matches!(
            o.step(Input::Reply(&reply(&b, "cloned")), false, false),
            Decision::NeedsReview(Review::Spent)
        ));
    }
    #[test]
    fn local_windows_and_remote_posix_compare_without_rewriting_observations() {
        let (_, raw) = fixture();
        let base: Value = serde_json::from_slice(&raw).unwrap();
        let mut v = base.clone();
        v["host"] = json!("");
        v["parent"] = json!("C:/Work");
        v["target"] = json!("C:/Work/Repo");
        let (b, raw) = changed(v);
        let mut o = original(b.clone(), &raw);
        proposed(&mut o);
        let mut v = json!({"request_id":b.clone_request_id,"status":"failed","parent":"c:\\WORK","path":"c:\\WORK\\REPO"});
        let exit = Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&v).unwrap(),
            stderr: String::new(),
        };
        let r = observed(&mut o, &exit);
        assert_eq!(r.parent, "c:\\WORK");
        assert_eq!(r.path, "c:\\WORK\\REPO");
        v["host"] = json!("peer");
        let mut raw_value: Value = serde_json::from_slice(&raw).unwrap();
        raw_value["host"] = json!("peer");
        let (b, raw) = changed(raw_value);
        assert!(Original::verify(b, &raw).is_err());
        let (b, raw) = fixture();
        let mut o = original(b.clone(), &raw);
        proposed(&mut o);
        let r = observed(&mut o, &reply(&b, "failed"));
        assert_eq!(r.parent, b.parent);
    }
    #[test]
    fn reply_source_and_encoded_receipt_bounds_refuse_without_clipping() {
        let (b, raw) = fixture();
        assert!(matches!(
            Original::verify(b.clone(), &vec![b' '; SOURCE_BYTES + 1]),
            Err(Review::TooLarge)
        ));
        for bytes in [
            vec![b'x'; REPLY_BYTES + 1],
            b"{\"schema\":2}".to_vec(),
            b"{bad".to_vec(),
        ] {
            let mut o = original(b.clone(), &raw);
            proposed(&mut o);
            let e = Exit::Code {
                code: 0,
                stdout: bytes,
                stderr: "PRIVATE".into(),
            };
            assert!(matches!(
                o.step(Input::Reply(&e), false, false),
                Decision::NeedsReview(_)
            ));
        }
        let mut o = original(b.clone(), &raw);
        proposed(&mut o);
        let Exit::Code { stdout, .. } = reply(&b, "cloned") else {
            unreachable!()
        };
        let e = Exit::Code {
            code: 7,
            stdout,
            stderr: "PRIVATE".into(),
        };
        assert!(matches!(
            o.step(Input::Reply(&e), false, false),
            Decision::NeedsReview(Review::UnknownOutcome)
        ));
        let r = Receipt {
            schema: 1,
            kind: Kind::CreationCloneStatus,
            creation_id: id(1),
            clone_request_id: id(2),
            status: "failed".into(),
            parent: "/".repeat(RECEIPT_BYTES),
            path: "/x".into(),
        };
        assert_eq!(receipt_size(&r), Err(Review::TooLarge));
    }

    #[test]
    fn missing_raw_identity_and_revision_never_become_defaults() {
        let (binding, raw) = fixture();
        for field in [
            "id",
            "clone_id",
            "expected_machine",
            "host",
            "target",
            "parent",
            "revision",
        ] {
            let mut value: Value = serde_json::from_slice(&raw).unwrap();
            value.as_object_mut().unwrap().remove(field);
            let bytes = serde_json::to_vec(&value).unwrap();
            let mut expected = binding.clone();
            expected.source_sha256 = sha256(&bytes);
            assert!(Original::verify(expected, &bytes).is_err(), "{field}");
        }
    }

    #[test]
    fn contradictory_later_stage_and_ambiguous_optional_ids_refuse() {
        let (_, raw) = fixture();
        let base: Value = serde_json::from_slice(&raw).unwrap();
        for (field, value) in [
            ("worktree_done", json!(true)),
            ("bound_session", json!("codex/fixture/bound")),
            ("bound_session", Value::Null),
            ("acquisition_confirmed", json!(true)),
            ("worktree_id", Value::Null),
            ("worktree_id", json!("")),
        ] {
            let mut record = base.clone();
            record[field] = value;
            let (binding, bytes) = changed(record);
            if let Ok(mut original) = Original::verify(binding, &bytes) {
                assert!(
                    matches!(
                        original.step(Input::LookupRequested, false, false),
                        Decision::NeedsReview(Review::Historical)
                    ),
                    "{field}"
                );
            }
        }
        for id in [
            Value::Null,
            json!(""),
            json!("00000000-0000-0000-0000-000000000000"),
            json!("ABCDEF00-1111-4111-8111-000000000001"),
        ] {
            let mut record = base.clone();
            record["launch_id"] = id;
            let (binding, bytes) = changed(record);
            assert!(Original::verify(binding, &bytes).is_err());
        }
    }

    #[test]
    fn missing_or_changed_reply_identity_and_schema_stays_spent() {
        let (binding, raw) = fixture();
        let Exit::Code { stdout, .. } = reply(&binding, "cloned") else {
            unreachable!()
        };
        let base: Value = serde_json::from_slice(&stdout).unwrap();
        let mut invalid = Vec::new();
        for field in ["request_id", "status", "parent", "path"] {
            let mut value = base.clone();
            value.as_object_mut().unwrap().remove(field);
            invalid.push(value);
        }
        let mut wrong_id = base.clone();
        wrong_id["request_id"] = json!(binding.creation_id);
        invalid.push(wrong_id);
        for schema in [json!(2), json!(1.0), Value::Null, json!("1")] {
            let mut value = base.clone();
            value["schema"] = schema;
            invalid.push(value);
        }
        for value in invalid {
            let mut original = original(binding.clone(), &raw);
            proposed(&mut original);
            let exit = Exit::Code {
                code: 0,
                stdout: serde_json::to_vec(&value).unwrap(),
                stderr: String::new(),
            };
            assert!(matches!(
                original.step(Input::Reply(&exit), false, false),
                Decision::NeedsReview(_)
            ));
            assert!(matches!(
                original.step(Input::LookupRequested, false, false),
                Decision::NeedsReview(Review::Spent)
            ));
        }
    }

    #[test]
    fn receipt_cap_applies_to_whole_observed_reply_not_only_expected_paths() {
        let (_, raw) = fixture();
        let mut value: Value = serde_json::from_slice(&raw).unwrap();
        let parent = format!("/{}", "p".repeat(8180));
        value["parent"] = json!(parent);
        value["target"] = json!(format!("{parent}/repo"));
        let (binding, raw) = changed(value);
        let mut original = original(binding.clone(), &raw);
        proposed(&mut original);
        // Both exact paths fit their individual limits, while their complete
        // encoded receipt with its identity fields exceeds 16 KiB. Never clip.
        assert!(binding.parent.len() <= PATH_BYTES && binding.target.len() <= PATH_BYTES);
        let exit = reply(&binding, "cloned");
        assert!(matches!(
            original.step(Input::Reply(&exit), false, false),
            Decision::NeedsReview(Review::TooLarge)
        ));
        assert!(matches!(
            original.step(Input::LookupRequested, false, false),
            Decision::NeedsReview(Review::Spent)
        ));
    }
}
