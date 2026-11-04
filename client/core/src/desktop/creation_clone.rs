//! Exact input for the first service-owned creation effect. These closed values
//! carry no current writer, frontend, profile, exclusion or execution authority.
use crate::{creation, paths::Style, repository, ComputerId, Invocation};
use serde::{de::IgnoredAny, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const ABI: &str = "desktop-creation-clone-v1";
pub const HELP_MARKER: &str = "Desktop creation clone ABI: desktop-creation-clone-v1";
/// Leaves room for the authenticated envelope inside the existing control line.
pub const SOURCE_BYTES: usize = 32 << 10;
pub const PROFILE: &str = "creation-clone-v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CreationClone,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub configuration: String,
    pub service_epoch: String,
}
impl Scope {
    pub fn valid(&self) -> bool {
        super::digest(&self.configuration) && uuid(&self.service_epoch)
    }
}

/// Exact native reducer checkpoint. Imported recovery uses a separate route;
/// neither an import nor this value itself supplies fresh execution consent.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub scope: Scope,
    pub creation_id: String,
    pub clone_request_id: String,
    pub revision: u64,
    pub original: String,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub clone_request_id: String,
    pub expected_machine: String,
    pub url: String,
    pub parent: String,
    pub name: String,
}
impl Plan {
    /// Read-only recovery also accepts a well-formed non-cloning journal state.
    /// It supplies no fresh native admission or execution consent.
    pub fn from_operation(id: &str, value: &Value) -> Option<Self> {
        if !creation::valid_operation(id, value)
            || value["host"].as_str().is_some_and(|host| !host.is_empty())
        {
            return None;
        }
        let text = |key| value[key].as_str().map(str::to_owned);
        let plan = Self {
            clone_request_id: text("clone_id")?,
            expected_machine: text("expected_machine")?,
            url: text("url")?,
            parent: text("parent")?,
            name: text("repository_name")?,
        };
        plan.valid().then_some(plan)
    }
    pub fn valid(&self) -> bool {
        uuid(&self.clone_request_id)
            && uuid(&self.expected_machine)
            && crate::paths::absolute(&self.parent, Style::Posix)
            && self.parent.len() <= 4096
            && !self.parent.chars().any(char::is_control)
            && self.url.len() <= 4096
            && creation::safe_repository_url(&self.url)
            && repository::valid_name(&self.name)
            && self.name.len() <= 240
            && self.name.chars().count() <= 80
    }
    pub fn fingerprint(&self) -> Option<String> {
        self.valid()
            .then(|| hash(&serde_json::to_vec(self).expect("fixed clone plan")))
    }
    pub fn repository_fingerprint(&self) -> Option<String> {
        self.valid().then(|| {
            hash(
                &serde_json::to_vec(&(&self.url, &self.parent, &self.name))
                    .expect("fixed repository identity"),
            )
        })
    }
}
// The checkpoint may contain a submitted prompt. Debug never prints its bytes.
impl std::fmt::Debug for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreationCloneIntent")
            .field("creation_id", &self.creation_id)
            .field("clone_request_id", &self.clone_request_id)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// Persisted per-kind binding, deliberately unrelated to task-start fields.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub kind: Kind,
    pub schema: u32,
    pub configuration: String,
    pub creation_id: String,
    pub clone_request_id: String,
    pub revision: u64,
    pub source_sha256: String,
    pub plan_sha256: String,
    pub consent_sha256: String,
}

pub struct Verified {
    pub intent: Intent,
    pub plan: Plan,
    pub binding: Binding,
}

fn uuid(value: &str) -> bool {
    creation::canonical_uuid(value) && value != "00000000-0000-0000-0000-000000000000"
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Check raw keys before Value can erase ambiguity. Opaque original fields are
// retained byte-for-byte; rejecting their duplicates loses no supported input.
fn unique(bytes: &[u8]) -> Result<(), &'static str> {
    struct Object;
    impl<'de> serde::de::Visitor<'de> for Object {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("one unambiguous creation checkpoint")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
            let mut keys = HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !keys.insert(key) {
                    return Err(serde::de::Error::custom("duplicate creation field"));
                }
                map.next_value::<IgnoredAny>()?;
            }
            Ok(())
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    serde::de::Deserializer::deserialize_map(&mut decoder, Object)
        .map_err(|_| "invalid_creation_checkpoint")?;
    decoder.end().map_err(|_| "invalid_creation_checkpoint")
}

impl Intent {
    pub fn verify(self) -> Result<Verified, &'static str> {
        if !self.scope.valid()
            || !uuid(&self.creation_id)
            || !uuid(&self.clone_request_id)
            || !(1..=i64::MAX as u64).contains(&self.revision)
            || self.original.is_empty()
            || self.original.len() > SOURCE_BYTES
        {
            return Err("invalid_creation_intent");
        }
        unique(self.original.as_bytes())?;
        let value: Value =
            serde_json::from_str(&self.original).map_err(|_| "invalid_creation_checkpoint")?;
        let text = |key| value[key].as_str().unwrap_or("");
        if !creation::valid_operation(&self.creation_id, &value)
            || value["revision"].as_u64() != Some(self.revision)
            || text("state") != "cloning"
            || value["repository"] != true
            || !text("host").is_empty()
            || text("clone_id") != self.clone_request_id
            || !uuid(text("expected_machine"))
            || [
                "clone_done",
                "worktree_done",
                "launch_dispatched",
                "stop_requested",
            ]
            .iter()
            .any(|key| value[*key] == true)
            || ["worktree_id", "bound_session", "acquisition_confirmed"]
                .iter()
                .any(|key| value.get(*key).is_some())
            || value
                .get("launch_id")
                .is_some_and(|id| !id.as_str().is_some_and(uuid))
        {
            return Err("creation_intent_changed");
        }
        let parent = text("parent");
        let prefix = format!("{}/", parent.trim_end_matches('/'));
        let name = text("target")
            .strip_prefix(&prefix)
            .filter(|name| repository::valid_name(name) && *name == text("repository_name"))
            .ok_or("invalid_clone_destination")?;
        if !crate::paths::absolute(parent, Style::Posix)
            || parent.len() > 4096
            || parent.chars().any(char::is_control)
            || text("url").len() > 4096
            || !creation::safe_repository_url(text("url"))
            || name.chars().count() > 80
        {
            return Err("invalid_clone_destination");
        }
        let plan = Plan {
            clone_request_id: text("clone_id").into(),
            expected_machine: text("expected_machine").into(),
            url: text("url").into(),
            parent: parent.into(),
            name: name.into(),
        };
        if !plan.valid() {
            return Err("invalid_clone_plan");
        }
        let encoded = serde_json::to_vec(&plan).map_err(|_| "invalid_clone_plan")?;
        let mut consent = b"desktop-creation-clone-consent-v1\0".to_vec();
        consent.extend_from_slice(self.original.as_bytes());
        let binding = Binding {
            kind: Kind::CreationClone,
            schema: 1,
            configuration: self.scope.configuration.clone(),
            creation_id: self.creation_id.clone(),
            clone_request_id: plan.clone_request_id.clone(),
            revision: self.revision,
            source_sha256: hash(self.original.as_bytes()),
            plan_sha256: hash(&encoded),
            consent_sha256: hash(&consent),
        };
        Ok(Verified {
            intent: self,
            plan,
            binding,
        })
    }
}

impl Binding {
    pub fn valid(&self) -> bool {
        self.kind == Kind::CreationClone
            && self.schema == 1
            && super::digest(&self.configuration)
            && uuid(&self.creation_id)
            && uuid(&self.clone_request_id)
            && (1..=i64::MAX as u64).contains(&self.revision)
            && [&self.source_sha256, &self.plan_sha256, &self.consent_sha256]
                .into_iter()
                .all(|v| super::digest(v))
    }
    pub fn operation_id(&self) -> Option<String> {
        self.valid().then(|| {
            let mut bytes = b"desktop-creation-clone-key-v1\0".to_vec();
            bytes.extend(
                serde_json::to_vec(&(
                    &self.configuration,
                    &self.creation_id,
                    &self.clone_request_id,
                ))
                .expect("fixed clone key"),
            );
            hash(&bytes)
        })
    }
    pub fn fingerprint(&self) -> Option<String> {
        self.valid()
            .then(|| hash(&serde_json::to_vec(self).expect("fixed clone binding")))
    }
}

pub const REPLY_BYTES: usize = 24 << 10;
pub const RESULT_BYTES: usize = 16 << 10;
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub abi: String,
    pub request_id: String,
    pub scope: Scope,
    pub accepted: bool,
    pub durable: bool,
    pub code: String,
    pub receipt: Option<Receipt>,
}
impl Admission {
    pub fn matches(&self, intent: &Intent) -> bool {
        self.abi == ABI
            && self.request_id == intent.clone_request_id
            && self.scope == intent.scope
            && self.accepted
            && self.durable
            && self.code.is_empty()
            && self.receipt.as_ref().is_some_and(|r| r.matches(intent))
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Reserved,
    Running,
    Cloned,
    Failed,
    Unknown,
}
impl Phase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Cloned | Self::Failed | Self::Unknown)
    }
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub binding: Binding,
    pub admission_epoch: String,
    pub revision: u64,
    pub phase: Phase,
    pub drained: bool,
    pub code: String,
    pub repository: Option<Value>,
}
impl std::fmt::Debug for Receipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreationCloneReceipt")
            .field("binding", &self.binding)
            .field("revision", &self.revision)
            .field("phase", &self.phase)
            .field("drained", &self.drained)
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}
impl Receipt {
    pub fn valid(&self) -> bool {
        self.binding.valid()
            && uuid(&self.admission_epoch)
            && (1..=i64::MAX as u64).contains(&self.revision)
            && self.code.len() <= 80
            && self
                .code
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            && (!self.drained || self.phase.terminal())
            && self
                .repository
                .as_ref()
                .is_none_or(|v| serde_json::to_vec(v).is_ok_and(|b| b.len() <= RESULT_BYTES))
            && (!matches!(self.phase, Phase::Cloned | Phase::Failed) || self.drained)
            && (self.phase != Phase::Cloned || self.repository.is_some())
    }
    pub fn matches(&self, intent: &Intent) -> bool {
        self.valid()
            && intent
                .clone()
                .verify()
                .is_ok_and(|v| v.binding == self.binding)
    }
    /// Only an exact original repository receipt after actual resource drain
    /// advances the native creation reducer. Queued/unknown never means success.
    pub fn result(&self, plan: &Plan) -> Option<Value> {
        if !self.valid() || !self.drained || self.binding.plan_sha256 != plan.fingerprint()? {
            return None;
        }
        let expected = match self.phase {
            Phase::Cloned => "cloned",
            Phase::Failed => "failed",
            _ => return None,
        };
        let value = self.repository.as_ref()?;
        let result = repository::reply(
            crate::Exit::Code {
                code: 0,
                stdout: serde_json::to_vec(value).ok()?,
                stderr: String::new(),
            },
            &ComputerId::local(),
            repository::Action::Status,
            &plan.name,
            &plan.clone_request_id,
        )
        .ok()?;
        (result["status"] == expected
            && result["fingerprint"] == plan.repository_fingerprint()?
            && result["name"] == plan.name
            && crate::paths::same(result["parent"].as_str()?, &plan.parent, Style::Posix)
            && crate::paths::same(
                result["path"].as_str()?,
                &repository::destination(&plan.parent, &plan.name),
                Style::Posix,
            ))
        .then_some(result)
    }
}

/// Indexed read-only recovery, also usable by imported/recovered creations.
/// It deliberately contains no raw checkpoint or execution consent.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub request_id: String,
    pub scope: Scope,
    pub creation_id: String,
    pub clone_request_id: String,
    pub plan_sha256: String,
}
impl Query {
    pub fn valid(&self) -> bool {
        uuid(&self.request_id)
            && self.scope.valid()
            && uuid(&self.creation_id)
            && uuid(&self.clone_request_id)
            && super::digest(&self.plan_sha256)
    }
    pub fn for_plan(scope: Scope, creation_id: String, plan: &Plan) -> Option<Self> {
        let query = Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            scope,
            creation_id,
            clone_request_id: plan.clone_request_id.clone(),
            plan_sha256: plan.fingerprint()?,
        };
        query.valid().then_some(query)
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub abi: String,
    pub request_id: String,
    pub scope: Scope,
    pub read_only: bool,
    pub found: bool,
    pub receipt: Option<Receipt>,
}
impl Lookup {
    pub fn matches(&self, query: &Query) -> bool {
        query.valid()
            && self.abi == ABI
            && self.request_id == query.request_id
            && self.scope == query.scope
            && self.read_only
            && self.found == self.receipt.is_some()
            && self.receipt.as_ref().is_none_or(|r| {
                r.valid()
                    && r.binding.configuration == query.scope.configuration
                    && r.binding.creation_id == query.creation_id
                    && r.binding.clone_request_id == query.clone_request_id
                    && r.binding.plan_sha256 == query.plan_sha256
            })
    }
}

impl Plan {
    pub fn invocation(&self, action: repository::Action) -> Invocation {
        repository::invocation(
            &ComputerId::local(),
            action,
            &self.url,
            &self.parent,
            &self.name,
            &self.clone_request_id,
            &self.expected_machine,
        )
    }
}

#[cfg(test)]
pub(super) fn test_intent() -> Intent {
    tests::checkpoint()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creation::{Answer, Call, Creations, Effect, Journal, Placement};
    use serde_json::json;
    use std::collections::VecDeque;

    const ID: &str = "11111111-1111-4111-8111-111111111111";
    const MACHINE: &str = "22222222-2222-4222-8222-222222222222";
    const EPOCH: &str = "33333333-3333-4333-8333-333333333333";
    struct NeverPlace;
    impl Placement for NeverPlace {
        fn resolve(&mut self, _: &mut Value, _: &Value) -> Result<(), String> {
            panic!("the clone leaf never performs project placement")
        }
    }
    pub(super) fn checkpoint() -> Intent {
        let mut n = 10u64;
        let mut reducer = Creations::new(
            || 1,
            move || {
                n += 1;
                format!("00000000-0000-4000-8000-{n:012x}")
            },
        );
        reducer.load(Journal::Missing);
        let (admitted, effects) = reducer.submit_reserved(
            json!({
                "cmd":"codex", "tag":"example", "target":"/srv/repo", "parent":"/srv",
                "host":"", "repository":true, "url":"https://example.invalid/repo.git",
                "repository_name":"repo", "auto":false
            }),
            ID,
        );
        assert_eq!(admitted.unwrap(), ID);
        let mut effects: VecDeque<_> = effects.into();
        let mut saved = None;
        while let Some(effect) = effects.pop_front() {
            match effect {
                Effect::Write { write, bytes, .. } => {
                    saved = Some(bytes);
                    effects.extend(reducer.written(write, true));
                }
                Effect::Call {
                    flight,
                    call: Call::CreationDirectory { .. },
                } => {
                    effects.extend(reducer.answered(
                        flight,
                        Answer {
                            ok: true,
                            data: json!({"creation_identity_v1":true,"computer_id":MACHINE}),
                            error: String::new(),
                        },
                        &mut NeverPlace,
                    ));
                }
                Effect::Call {
                    flight,
                    call:
                        Call::Repository {
                            action: repository::Action::Check,
                            ..
                        },
                } => {
                    effects.extend(reducer.answered(flight, Answer {
                        ok:true, data:json!({"status":"ready","parent":"/srv","path":"/srv/repo","name":"repo"}),
                        error:String::new()
                    }, &mut NeverPlace));
                }
                Effect::Call {
                    call:
                        Call::Repository {
                            action: repository::Action::Clone,
                            request_id,
                            ..
                        },
                    ..
                } => {
                    let journal: Value = serde_json::from_slice(&saved.unwrap()).unwrap();
                    let value = &journal["operations"][ID];
                    assert_eq!(value["clone_id"], request_id);
                    return Intent {
                        scope: Scope {
                            configuration: "a".repeat(64),
                            service_epoch: EPOCH.into(),
                        },
                        creation_id: ID.into(),
                        clone_request_id: request_id.clone(),
                        revision: value["revision"].as_u64().unwrap(),
                        original: serde_json::to_string(value).unwrap(),
                    };
                }
                Effect::Changed { .. } => (),
                _ => panic!("unexpected effect before the original clone"),
            }
        }
        panic!("the real creation reducer did not produce its clone")
    }

    #[test]
    fn accepts_the_real_reducers_durable_clone_checkpoint_with_reserved_launch_identity() {
        let intent = checkpoint();
        let original = intent.original.clone();
        let verified = intent.verify().unwrap();
        assert_eq!(verified.intent.original, original);
        assert_eq!(verified.binding.source_sha256, hash(original.as_bytes()));
        assert_eq!(verified.plan.name, "repo");
        let invocation = verified.plan.invocation(repository::Action::Clone);
        assert!(invocation.args.contains(&verified.plan.clone_request_id));
        assert!(invocation.args.contains(&MACHINE.into()));
    }

    fn completed(verified: &Verified) -> Receipt {
        Receipt {
            binding: verified.binding.clone(),
            admission_epoch: EPOCH.into(),
            revision: 3,
            phase: Phase::Cloned,
            drained: true,
            code: String::new(),
            repository: Some(
                json!({"status":"cloned","request_id":verified.plan.clone_request_id,
                "fingerprint":verified.plan.repository_fingerprint().unwrap(),"parent":"/srv","path":"/srv/repo","name":"repo"}),
            ),
        }
    }

    #[test]
    fn only_the_exact_drained_original_result_can_advance_a_creation() {
        let verified = checkpoint().verify().unwrap();
        let receipt = completed(&verified);
        assert!(receipt.matches(&verified.intent));
        assert_eq!(receipt.result(&verified.plan).unwrap()["status"], "cloned");
        let mut unconfirmed = receipt.clone();
        unconfirmed.phase = Phase::Unknown;
        assert!(unconfirmed.valid());
        assert!(unconfirmed.result(&verified.plan).is_none());
        unconfirmed = receipt.clone();
        unconfirmed.drained = false;
        assert!(!unconfirmed.valid());
        for (field, value) in [
            ("request_id", "changed"),
            ("parent", "/other"),
            ("path", "/srv/other"),
            ("fingerprint", "changed"),
            ("name", "other"),
        ] {
            let mut changed = receipt.clone();
            changed.repository.as_mut().unwrap()[field] = json!(value);
            assert!(
                changed.result(&verified.plan).is_none(),
                "accepted changed {field}"
            );
        }
        let mut changed = verified.intent.clone();
        changed.clone_request_id = "44444444-4444-4444-8444-444444444444".into();
        assert!(changed.verify().is_err());
    }

    #[test]
    fn recovery_lookup_is_scoped_and_never_turns_absence_into_execution() {
        let verified = checkpoint().verify().unwrap();
        let query =
            Query::for_plan(verified.intent.scope.clone(), ID.into(), &verified.plan).unwrap();
        let mut lookup = Lookup {
            abi: ABI.into(),
            request_id: query.request_id.clone(),
            scope: query.scope.clone(),
            read_only: true,
            found: true,
            receipt: Some(completed(&verified)),
        };
        assert!(lookup.matches(&query));
        lookup.receipt.as_mut().unwrap().admission_epoch =
            "44444444-4444-4444-8444-444444444444".into();
        assert!(
            lookup.matches(&query),
            "receipt epoch is historical, query scope is current"
        );
        lookup.scope.service_epoch = uuid::Uuid::new_v4().to_string();
        assert!(!lookup.matches(&query));
        lookup.scope = query.scope.clone();
        lookup.receipt.as_mut().unwrap().binding.plan_sha256 = "b".repeat(64);
        assert!(!lookup.matches(&query));
        lookup.receipt = None;
        lookup.found = false;
        assert!(lookup.matches(&query));
        assert!(lookup.receipt.is_none()); // There is no executable proposal in this reply.
        lookup.read_only = false;
        assert!(!lookup.matches(&query));
    }

    #[test]
    fn unknown_original_fields_are_retained_and_bound_to_consent() {
        let first = checkpoint().verify().unwrap();
        let mut changed = checkpoint();
        let mut value: Value = serde_json::from_str(&changed.original).unwrap();
        value["opaque_future"] = json!({"keep":"original bytes"});
        changed.original = serde_json::to_string(&value).unwrap();
        let second = changed.verify().unwrap();
        assert!(second.intent.original.contains("opaque_future"));
        assert_eq!(first.binding.plan_sha256, second.binding.plan_sha256);
        assert_ne!(first.binding.source_sha256, second.binding.source_sha256);
        assert_ne!(first.binding.consent_sha256, second.binding.consent_sha256);
    }

    #[test]
    fn ambiguous_historical_or_changed_checkpoints_never_become_fresh_clone_input() {
        for (key, value) in [
            ("state", json!("unknown")),
            ("host", json!("peer")),
            ("launch_dispatched", json!(true)),
            ("clone_done", json!(true)),
            ("stop_requested", json!(true)),
            ("repository_name", json!("different")),
            ("expected_machine", json!("")),
            ("revision", json!(0)),
        ] {
            let mut intent = checkpoint();
            let mut raw: Value = serde_json::from_str(&intent.original).unwrap();
            raw[key] = value;
            intent.original = serde_json::to_string(&raw).unwrap();
            assert!(intent.verify().is_err(), "accepted changed field {key}");
        }
        let mut intent = checkpoint();
        intent.original = intent.original.replacen('{', "{\"state\":\"cloning\",", 1);
        assert!(intent.verify().is_err());
        let mut intent = checkpoint();
        intent.original = " ".repeat(SOURCE_BYTES + 1);
        assert!(intent.verify().is_err());
    }
}
