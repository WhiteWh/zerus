//! Native-only clone checkpoints on the existing Settings writer. These are
//! never preferences, Qt seeds or a source of permission to repeat a clone.
use super::*;
use crate::repository::{Pending, Store};
use serde::{Deserialize, Serialize};

const FIELD: &str = "repository_clones";
const REFUSED: &str = "Clone recovery information cannot be read or saved.";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub parent: Option<String>,
    pub pending: Option<Pending>,
    pub previous: Option<Pending>,
}
impl Checkpoint {
    pub fn apply(&mut self, effect: &Store) {
        match effect {
            Store::Pending(pending) => self.pending = Some(pending.clone()),
            Store::Settled { parent } => {
                self.parent = Some(parent.clone());
                self.pending = None;
            }
            Store::Retired => self.previous = self.pending.take(),
        }
    }
    fn valid(&self) -> bool {
        let path = |s: &str| !s.is_empty() && s.len() <= 32768 && !s.chars().any(char::is_control);
        self.parent.as_deref().is_none_or(path)
            && [&self.pending, &self.previous]
                .into_iter()
                .flatten()
                .all(|p| {
                    crate::creation::canonical_uuid(&p.request_id)
                        && !p.url.is_empty()
                        && p.url.len() <= 4096
                        && !p.url.chars().any(char::is_control)
                        && crate::repository::valid_name(&p.name)
                        && p.name.chars().count() <= 80
                        && path(&p.parent)
                        && path(&p.path)
                })
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Records {
    version: u8,
    #[serde(deserialize_with = "unique_entries")]
    entries: BTreeMap<String, Checkpoint>,
}
fn unique_entries<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Checkpoint>, D::Error> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = BTreeMap<String, Checkpoint>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("distinct clone checkpoint scopes")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate clone checkpoint scope"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Entries)
}

/// The existing bounded Settings/clone decoders, without a live store, lock,
/// status call or writer. Retain the exact original extension JSON privately;
/// no typed projection is serialized as source evidence. Missing is distinct
/// from a malformed/null family, which remains unsupported.
pub(crate) fn snapshot(
    bytes: &[u8],
) -> Result<Option<Box<serde_json::value::RawValue>>, SnapshotError> {
    if bytes.len() as u64 > MAX_STORE {
        return Err(SnapshotError::TooLarge);
    }
    let contents = Contents::parse(bytes).ok_or(SnapshotError::Unsupported)?;
    let records = records(&contents).map_err(|_| SnapshotError::Unsupported)?;
    if records.entries.values().any(|record| {
        [&record.pending, &record.previous]
            .into_iter()
            .flatten()
            .any(|pending| !crate::creation::safe_repository_url(&pending.url))
    }) {
        return Err(SnapshotError::Unsupported);
    }
    #[derive(Deserialize)]
    struct Original {
        repository_clones: Option<Box<serde_json::value::RawValue>>,
    }
    let original: Original =
        serde_json::from_slice(bytes).map_err(|_| SnapshotError::Unsupported)?;
    if let Some(raw) = &original.repository_clones {
        // Parse the original too so duplicate known fields/scopes cannot be
        // hidden by the compatibility Settings Value projection.
        serde_json::from_str::<Records>(raw.get()).map_err(|_| SnapshotError::Unsupported)?;
    }
    Ok(original.repository_clones)
}
fn key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn records(contents: &Contents) -> Result<Records, String> {
    let Some(value) = contents.extra.get(FIELD) else {
        return Ok(Records {
            version: 1,
            ..Records::default()
        });
    };
    if value
        .get("entries")
        .and_then(Value::as_object)
        .is_some_and(|entries| {
            entries.values().any(|record| {
                ["pending", "previous"].iter().any(|field| {
                    record
                        .get(field)
                        .and_then(Value::as_object)
                        .is_some_and(|p| {
                            p.keys().any(|key| {
                                !["request_id", "url", "parent", "name", "path"]
                                    .contains(&key.as_str())
                            })
                        })
                })
            })
        })
    {
        return Err(REFUSED.into());
    }
    let records: Records = serde_json::from_value(value.clone()).map_err(|_| REFUSED)?;
    if records.version != 1
        || records.entries.len() > 64
        || records.entries.iter().any(|(k, v)| !key(k) || !v.valid())
    {
        return Err(REFUSED.into());
    }
    Ok(records)
}
impl Settings {
    pub fn repository_checkpoint(&self, source: &str) -> Result<Checkpoint, String> {
        let state = self.loaded();
        if !key(source) || state.unreadable.is_some() || state.closed {
            return Err(REFUSED.into());
        }
        Ok(records(&state.contents)?
            .entries
            .remove(source)
            .unwrap_or_default())
    }
    /// Exact CAS and durable flush precede a fresh clone's delivery. A failed
    /// flush never authorizes delivery; the in-memory pending record stays put.
    pub fn replace_repository_checkpoint(
        &self,
        source: &str,
        expected: &Checkpoint,
        next: Checkpoint,
    ) -> Result<(), String> {
        if !key(source) || !next.valid() {
            return Err(REFUSED.into());
        }
        {
            let mut state = self.loaded();
            if state.unreadable.is_some() || state.closed || state.capturing {
                return Err(REFUSED.into());
            }
            let mut records = records(&state.contents)?;
            if records.entries.get(source).cloned().unwrap_or_default() != *expected {
                return Err(REFUSED.into());
            }
            records.entries.insert(source.to_owned(), next);
            if records.entries.len() > 64 {
                return Err(REFUSED.into());
            }
            let mut candidate = state.contents.clone();
            candidate.extra.insert(
                FIELD.into(),
                serde_json::to_value(records).map_err(|_| REFUSED)?,
            );
            if candidate.to_bytes().len() as u64 > MAX_STORE {
                return Err(REFUSED.into());
            }
            state.contents = candidate;
            state.dirty = true;
            self.shared.signal.notify_all();
        }
        if !self.flush_within(Duration::from_secs(10)) {
            return Err(REFUSED.into());
        }
        Ok(())
    }
}
