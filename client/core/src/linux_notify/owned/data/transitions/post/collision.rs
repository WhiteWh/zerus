//! Only the exact negative continuation may write after logical invalidation.
//! Every image remains a projection; acknowledgements are host reports only.
use super::*;
#[cfg(test)]
mod tests;

pub(in super::super) struct Collision {
    key: Key,
    id: NonZeroU32,
    phase: Option<WritePhase>,
    // Retain the internally acknowledged predecessor, never caller input.
    predecessor: Option<Attempt>,
}
impl Collision {
    pub(super) fn new(key: Key, id: NonZeroU32) -> Self {
        Self {
            key,
            id,
            phase: Some(WritePhase::CollisionInvalidations),
            predecessor: None,
        }
    }
}

fn agent_targets(
    store: &Store,
    namespace: &Namespace,
    id: NonZeroU32,
) -> Result<Vec<Value>, Rejection> {
    let bytes = serde_json::to_vec(&store.value).map_err(|_| Rejection::InvalidInput)?;
    let saved = super::super::super::super::decode_owned_for(&bytes, Owner::AgentAttention)
        .map_err(|_| Rejection::InvalidInput)?;
    if saved.has_unknown() {
        return Err(Rejection::InvalidInput);
    }
    Ok(saved
        .rows()
        .ok_or(Rejection::InvalidInput)?
        .positive_claims()
        .filter(|(o, found)| *found == id && same_namespace(o.namespace(), namespace))
        .map(|(o, id)| wire::invalidation_value(o, id, "native_id_collision"))
        .collect())
}

fn unknown_result(value: &mut Value, original: &Original, id: NonZeroU32) -> Result<(), Rejection> {
    let possible = wire::possible_value(original);
    let rows = value["uncertain"]
        .as_array_mut()
        .ok_or(Rejection::InvalidInput)?;
    let matches = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| **row == possible)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(Rejection::NeedsReview);
    }
    let row = &mut rows[matches[0]];
    row["state"] = Value::String("unknown_post".into());
    row["native_id"] = Value::from(id.get());
    Ok(())
}

impl Model {
    pub(super) fn slot_matches_namespace(&self, original: &Original, id: NonZeroU32) -> bool {
        let Some(tokens) =
            self.stores[index(original.scoped().owner())].value["tokens"].as_object()
        else {
            return false;
        };
        let Some(value) = tokens.get(&id.to_string()) else {
            return true;
        };
        let Ok(bytes) = serde_json::to_vec(value) else {
            return false;
        };
        let Ok(row) = wire::claim(
            &bytes,
            original.scoped().owner(),
            Placement::Tokens(id.get()),
        ) else {
            return false;
        };
        !row.has_unknown()
            && matches!(row.data(), ClaimData::Retained(c)
                if same_namespace(c.original().namespace(), original.namespace()))
    }

    pub(super) fn collision_affects_agent(
        &self,
        original: &Original,
        id: NonZeroU32,
    ) -> Result<bool, Rejection> {
        Ok(original.scoped().owner() == Owner::AgentAttention
            || !agent_targets(&self.stores[0], original.namespace(), id)?.is_empty())
    }

    pub(in super::super) fn next_collision_write(
        &mut self,
    ) -> Result<Option<WritePlan>, Rejection> {
        if self.outstanding.as_ref().is_some_and(|w| w.blocked) {
            return Err(Rejection::NeedsReview);
        }
        if self.outstanding.is_some() {
            return Ok(None);
        }
        let result = self.collision.as_ref().ok_or(Rejection::NeedsReview)?;
        let phase = result.phase.ok_or(Rejection::NeedsReview)?;
        if self.frozen || !self.invalidated {
            return Err(Rejection::NeedsReview);
        }
        let native = self.native.as_ref().ok_or(Rejection::Unsettled)?;
        if !native.key.matches(&result.key) || native.report.is_none() {
            return Err(Rejection::NeedsReview);
        }
        let preparation = self
            .preparations
            .iter()
            .position(|p| p.sequence == result.key.sequence && p.owner == result.key.owner)
            .ok_or(Rejection::StaleKey)?;
        if self.preparations[preparation].phase != BatchStatus::Review {
            return Err(Rejection::NeedsReview);
        }
        let original = &native.payload.original;
        // Another retained claim in this namespace cannot make the incoming
        // owner's occupied foreign/ambiguous numeric slot eligible for repair.
        if !self.slot_matches_namespace(original, result.id) {
            return Err(Rejection::NeedsReview);
        }
        let owner = match phase {
            WritePhase::CollisionInvalidations => Owner::SoftwareUpdate,
            WritePhase::CollisionResult => {
                let previous = result.predecessor.as_ref().ok_or(Rejection::NeedsReview)?;
                if previous.phase != WritePhase::CollisionInvalidations
                    || previous.owner != Owner::SoftwareUpdate
                    || previous.after != self.stores[1].image
                    || result.key.owner != Owner::AgentAttention
                {
                    return Err(Rejection::NeedsReview);
                }
                Owner::AgentAttention
            }
            _ => return Err(Rejection::NeedsReview),
        };
        let store = &self.stores[index(owner)];
        let mut value = store.value.clone();
        if phase == WritePhase::CollisionInvalidations {
            let mut targets = agent_targets(&self.stores[0], original.namespace(), result.id)?;
            if result.key.owner == Owner::AgentAttention {
                targets.push(wire::invalidation_value(
                    original,
                    result.id,
                    "native_id_collision",
                ));
            }
            if targets.is_empty() {
                return Err(Rejection::NeedsReview);
            }
            for target in &targets {
                let raw = serde_json::to_vec(target).map_err(|_| Rejection::InvalidInput)?;
                wire::invalidation(&raw, Owner::SoftwareUpdate).map_err(|_| Rejection::Pressure)?;
            }
            value["invalidations"]
                .as_array_mut()
                .ok_or(Rejection::InvalidInput)?
                .extend(targets);
            if result.key.owner == Owner::SoftwareUpdate {
                unknown_result(&mut value, original, result.id)?;
            }
        } else {
            unknown_result(&mut value, original, result.id)?;
        }
        let bytes: Arc<[u8]> = serde_json::to_vec(&value)
            .map_err(|_| Rejection::InvalidInput)?
            .into();
        let other = serde_json::to_vec(&self.stores[1 - index(owner)].value)
            .map_err(|_| Rejection::InvalidInput)?;
        if self.reserved_entries > LIMIT
            || self.reserved_bytes as u64 > FILE_LIMIT
            || (bytes.len() + other.len() + self.legacy_image.bytes.unwrap_or(0)) as u64
                > FILE_LIMIT
        {
            return Err(Rejection::Pressure);
        }
        let attempt = Attempt {
            model: Arc::downgrade(&self.id),
            owner,
            step: result.key.sequence,
            item: Some(result.key.item),
            phase,
            original_sha256: Some(result.key.original_sha256.clone()),
            predecessor: result.predecessor.clone().map(Box::new),
            before: store.image.clone(),
            after: Image {
                sha256: Some(hash(&bytes)),
                bytes: Some(bytes.len()),
            },
        };
        self.outstanding = Some(Outstanding {
            attempt: attempt.clone(),
            _bytes: bytes.clone(),
            value,
            preparation,
            unknown: false,
            blocked: false,
        });
        Ok(Some(WritePlan {
            key: WriteKey { attempt },
            bytes: Some(bytes),
        }))
    }

    pub(in super::super) fn collision_saved(&mut self, attempt: Attempt) {
        let collision = self.collision.as_mut().unwrap();
        if attempt.phase == WritePhase::CollisionInvalidations
            && collision.key.owner == Owner::AgentAttention
        {
            collision.predecessor = Some(attempt);
            collision.phase = Some(WritePhase::CollisionResult);
        } else {
            collision.phase = None;
        }
        // Never retire the original report/payload/preparation or restore proof.
    }
}
