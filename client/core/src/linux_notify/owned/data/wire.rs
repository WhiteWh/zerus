use super::*;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Deserialize)]
struct RawOriginal {
    request_id: String,
    poster_instance: String,
    post_nonce: u64,
    bus_id: String,
    daemon_owner: String,
    namespace_generation: u64,
    actions: RawActions,
    #[serde(flatten)]
    unknown: BTreeMap<String, IgnoredAny>,
}
struct RawActions(AllowedActions);
impl<'de> Deserialize<'de> for RawActions {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Actions;
        impl<'de> Visitor<'de> for Actions {
            type Value = RawActions;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("one or two existing action keys")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut actions = Vec::new();
                while let Some(key) = seq.next_element::<String>()? {
                    let action = match key.as_str() {
                        "default" => Action::Default,
                        "open-session" => Action::OpenSession,
                        _ => return Err(de::Error::custom("unknown action key")),
                    };
                    if actions.len() == 2 || actions.contains(&action) {
                        return Err(de::Error::custom("action membership"));
                    }
                    actions.push(action);
                }
                if actions.is_empty() {
                    return Err(de::Error::custom("empty actions"));
                }
                Ok(RawActions(AllowedActions(actions)))
            }
        }
        d.deserialize_seq(Actions)
    }
}
struct RawEntry {
    owner: String,
    token: String,
    original: Option<RawOriginal>,
    state: Option<String>,
    native_id: Option<u32>,
    unknown: bool,
}
impl<'de> Deserialize<'de> for RawEntry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Entry;
        impl<'de> Visitor<'de> for Entry {
            type Value = RawEntry;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("retained notification row")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let (mut owner, mut token, mut original, mut state, mut native_id) =
                    (None, None, None, None, None);
                let mut unknown = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "owner" => once(&mut map, &mut owner, "owner")?,
                        "token" => once(&mut map, &mut token, "token")?,
                        "original" => once(&mut map, &mut original, "original")?,
                        "state" => once(&mut map, &mut state, "state")?,
                        "native_id" => once(&mut map, &mut native_id, "native_id")?,
                        _ => {
                            unknown = true;
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(RawEntry {
                    owner: owner.ok_or_else(|| de::Error::missing_field("owner"))?,
                    token: token.ok_or_else(|| de::Error::missing_field("token"))?,
                    original,
                    state,
                    native_id,
                    unknown,
                })
            }
        }
        d.deserialize_map(Entry)
    }
}
fn once<'de, M: MapAccess<'de>, T: Deserialize<'de>>(
    map: &mut M,
    value: &mut Option<T>,
    key: &'static str,
) -> Result<(), M::Error> {
    if value.is_some() {
        return Err(de::Error::duplicate_field(key));
    }
    // T is non-optional: explicit null is malformed, never historical absence.
    *value = Some(map.next_value()?);
    Ok(())
}
fn parse<'de, T: Deserialize<'de>>(bytes: &'de [u8], limit: u64) -> Result<T, Refused> {
    if bytes.len() as u64 > limit {
        return Err(Refused::TooLarge);
    }
    serde_json::from_slice(bytes).map_err(|_| Refused::Malformed)
}
fn scoped(owner: &str, token: String, expected: Owner) -> Result<ScopedToken, Refused> {
    let tag = match expected {
        Owner::AgentAttention => "agent_attention",
        Owner::SoftwareUpdate => "software_update",
    };
    if owner != tag {
        return Err(Refused::InvalidIdentity);
    }
    ScopedToken::new(expected, token)
}
fn original(raw: RawOriginal, scoped: ScopedToken) -> Result<(Original, bool), Refused> {
    Ok((
        Original::checked(
            scoped,
            OriginalFields {
                request_id: raw.request_id,
                poster_instance: raw.poster_instance,
                post_nonce: raw.post_nonce,
                namespace: Namespace {
                    bus_id: raw.bus_id,
                    owner: raw.daemon_owner,
                },
                namespace_generation: raw.namespace_generation,
                actions: raw.actions.0,
            },
        )?,
        !raw.unknown.is_empty(),
    ))
}
pub(super) fn claim(
    bytes: &[u8],
    expected: Owner,
    placement: Placement,
) -> Result<ClaimRow, Refused> {
    let raw: RawEntry = parse(bytes, FILE_LIMIT)?;
    if matches!(placement, Placement::Tokens(0)) {
        return Err(Refused::InvalidIdentity);
    }
    let token = scoped(&raw.owner, raw.token, expected)?;
    let mut has_unknown = raw.unknown;
    let data = match (raw.original, raw.state) {
        (None, None) if raw.native_id.is_none() => ClaimData::Historical(token),
        (Some(o), Some(state)) => {
            let (o, unknown) = original(o, token)?;
            has_unknown |= unknown;
            let id = raw
                .native_id
                .map(|id| NonZeroU32::new(id).ok_or(Refused::InvalidIdentity))
                .transpose()?;
            if let Placement::Tokens(key) = placement {
                if id.map(NonZeroU32::get) != Some(key) {
                    return Err(Refused::InvalidIdentity);
                }
            }
            let c = match (state.as_str(), id) {
                ("possible_post", None) if placement == Placement::Uncertain => {
                    RetainedClaim::PossiblePost(o)
                }
                ("unknown_post", id) => RetainedClaim::UnknownPost {
                    original: o,
                    native_id: id,
                },
                ("delivered", Some(id)) => RetainedClaim::Delivered(NativeBinding {
                    original: o,
                    native_id: id,
                }),
                ("withdrawal_pending", Some(id)) => {
                    RetainedClaim::WithdrawalPending(NativeBinding {
                        original: o,
                        native_id: id,
                    })
                }
                ("unknown_withdrawal", Some(id)) => {
                    RetainedClaim::UnknownWithdrawal(NativeBinding {
                        original: o,
                        native_id: id,
                    })
                }
                _ => return Err(Refused::InvalidIdentity),
            };
            ClaimData::Retained(c)
        }
        _ => return Err(Refused::InvalidIdentity),
    };
    Ok(ClaimRow {
        data,
        placement,
        has_unknown,
        raw_bytes: bytes.len(),
    })
}
#[derive(Deserialize)]
struct RawInvalidation {
    target: RawEntry,
    reason: String,
    #[serde(flatten)]
    unknown: BTreeMap<String, IgnoredAny>,
}
pub(super) fn invalidation(bytes: &[u8], expected: Owner) -> Result<InvalidationRow, Refused> {
    if expected != Owner::SoftwareUpdate {
        return Err(Refused::InvalidIdentity);
    }
    let raw: RawInvalidation = parse(bytes, 16384)?;
    let target = raw.target;
    if target.state.is_some() {
        return Err(Refused::InvalidIdentity);
    }
    let scoped = scoped(&target.owner, target.token, Owner::AgentAttention)?;
    let (original, unknown) = original(target.original.ok_or(Refused::InvalidIdentity)?, scoped)?;
    let id = target
        .native_id
        .and_then(NonZeroU32::new)
        .ok_or(Refused::InvalidIdentity)?;
    let reason = match raw.reason.as_str() {
        "native_id_collision" => InvalidationReason::NativeIdCollision,
        "namespace_changed" => InvalidationReason::NamespaceChanged,
        "withdrawal_conflict" => InvalidationReason::WithdrawalConflict,
        "unverified_result" => InvalidationReason::UnverifiedResult,
        _ => return Err(Refused::InvalidIdentity),
    };
    Ok(InvalidationRow {
        data: Invalidation {
            target: NativeBinding {
                original,
                native_id: id,
            },
            reason,
        },
        has_unknown: unknown || target.unknown || !raw.unknown.is_empty(),
        raw_bytes: bytes.len(),
    })
}
fn origin_value(o: &Original) -> Value {
    json!({"request_id":o.request_id,"poster_instance":o.poster_instance,"post_nonce":o.post_nonce.get(),
        "bus_id":o.namespace.bus_id,"daemon_owner":o.namespace.owner,"namespace_generation":o.namespace_generation.get(),
        "actions":o.actions.iter().map(Action::key).collect::<Vec<_>>()})
}
fn token_value(token: &ScopedToken) -> Value {
    let tag = match token.owner() {
        Owner::AgentAttention => "agent_attention",
        Owner::SoftwareUpdate => "software_update",
    };
    json!({"owner":tag,"token":token.token()})
}
pub(super) fn possible_value(o: &Original) -> Value {
    let mut value = token_value(&o.scoped);
    value["original"] = origin_value(o);
    value["state"] = json!("possible_post");
    value
}
pub(super) fn invalidation_value(o: &Original, id: NonZeroU32, reason: &str) -> Value {
    let mut target = token_value(&o.scoped);
    target["original"] = origin_value(o);
    target["native_id"] = json!(id.get());
    json!({"target":target,"reason":reason})
}
pub(super) fn invalidation_budget(o: &Original, id: NonZeroU32) -> Result<usize, Refused> {
    [
        "native_id_collision",
        "namespace_changed",
        "withdrawal_conflict",
        "unverified_result",
    ]
    .into_iter()
    .try_fold(0, |total, reason| {
        let size = serde_json::to_vec(&invalidation_value(o, id, reason))
            .unwrap()
            .len();
        if size > 16384 {
            return Err(Refused::TooLarge);
        }
        Ok(total + size + 1)
    })
}
fn output(value: Value, unknown: bool, limit: u64) -> Result<Vec<u8>, Refused> {
    if unknown {
        return Err(Refused::UnknownFields);
    }
    let bytes = serde_json::to_vec(&value).map_err(|_| Refused::Malformed)?;
    if bytes.len() as u64 > limit {
        return Err(Refused::TooLarge);
    }
    Ok(bytes)
}
pub(super) fn encode_claim(row: &ClaimRow) -> Result<Vec<u8>, Refused> {
    let mut value = token_value(row.scoped());
    if let ClaimData::Retained(c) = &row.data {
        value["original"] = origin_value(c.original());
        value["state"] = json!(c.tag());
        if let Some(id) = c.native_id() {
            value["native_id"] = json!(id.get());
        }
    }
    output(value, row.has_unknown, FILE_LIMIT)
}
pub(super) fn encode_invalidation(row: &InvalidationRow) -> Result<Vec<u8>, Refused> {
    let b = &row.data.target;
    let mut target = token_value(&b.original.scoped);
    target["original"] = origin_value(&b.original);
    target["native_id"] = json!(b.native_id.get());
    output(
        json!({"target":target,"reason":row.data.reason.tag()}),
        row.has_unknown,
        16384,
    )
}
