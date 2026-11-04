//! Restricted post results. These handles are policy data, never
//! native jobs, receipts, physical-save evidence or current permissions.
use super::*;
mod collision;
pub(super) use collision::Collision;

#[derive(Clone)]
struct Key {
    model: Weak<()>,
    owner: Owner,
    sequence: u64,
    item: usize,
    original_sha256: String,
    barrier: Attempt,
}
impl Key {
    fn matches(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.model, &other.model)
            && self.owner == other.owner
            && self.sequence == other.sequence
            && self.item == other.item
            && self.original_sha256 == other.original_sha256
            && self.barrier.matches(&other.barrier)
    }
}
pub struct PostProposal {
    key: Key,
    payload: Option<Arc<Post>>,
}
pub enum PostOutcome {
    Delivered(NonZeroU32),
    RefusedBeforeCall,
    Unknown { native_id: Option<NonZeroU32> },
}
pub struct PostReport {
    key: Key,
    // Malformed/oversized namespace input is represented by a bounded marker;
    // a valid contradictory namespace is retained exactly for review.
    namespace: Option<Namespace>,
    outcome: PostOutcome,
}
impl PostProposal {
    pub fn original(&self) -> Option<&Original> {
        self.payload.as_ref().map(|p| &p.original)
    }
    pub fn title(&self) -> Option<&str> {
        self.payload.as_ref().map(|p| p.title.as_str())
    }
    pub fn body(&self) -> Option<&str> {
        self.payload.as_ref().map(|p| p.body.as_str())
    }
    pub fn report(
        &self,
        namespace: Namespace,
        outcome: PostOutcome,
    ) -> Result<PostReport, Rejection> {
        self.payload.as_ref().ok_or(Rejection::StaleKey)?;
        Ok(PostReport {
            key: self.key.clone(),
            namespace: super::super::super::namespace(namespace.bus_id, namespace.owner)
                .ok()
                .flatten(),
            outcome,
        })
    }
}
pub(super) struct Native {
    key: Key,
    payload: Arc<Post>,
    // Accepted terminal review never discards the observed ID/original/payload.
    report: Option<PostReport>,
}
pub(super) struct PendingResult {
    key: Key,
    id: NonZeroU32,
}
fn same_namespace(a: &Namespace, b: &Namespace) -> bool {
    namespace_key(a) == namespace_key(b)
}
fn envelope_matches(value: &Value, namespace: &Namespace) -> bool {
    value["bus_id"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case(&namespace.bus_id))
        && value["owner"].as_str() == Some(namespace.owner.as_str())
}
pub(super) fn adopt_empty(value: &mut Value, posts: &[Arc<Post>]) {
    // Bind a truly empty envelope in its first pre-issue image. Never relabel
    // any retained positive, uncertain, withdrawal or other admitted original.
    if value["bus_id"] == ""
        && value["owner"] == ""
        && value["tokens"].as_object().is_some_and(|m| m.is_empty())
        && value["uncertain"].as_array().is_some_and(|a| a.is_empty())
        && posts
            .iter()
            .all(|p| same_namespace(p.original.namespace(), posts[0].original.namespace()))
    {
        value["bus_id"] = Value::String(posts[0].original.namespace().bus_id.clone());
        value["owner"] = Value::String(posts[0].original.namespace().owner.clone());
    }
}
impl Model {
    fn invalidate(&mut self) {
        self.invalidated = true;
        self.generation = self.generation.saturating_add(1);
    }
    pub fn take_next_native(&mut self) -> Result<Option<PostProposal>, Rejection> {
        if self.invalidated {
            return Err(Rejection::NeedsReview);
        }
        if self.outstanding.is_some() || self.native.is_some() || self.result.is_some() {
            return Ok(None);
        }
        if self.reserved_entries > LIMIT || self.reserved_bytes as u64 > FILE_LIMIT {
            return Err(Rejection::Pressure);
        }
        let Some(p) = self
            .preparations
            .iter()
            .find(|p| p.phase == BatchStatus::BarrierAcknowledged)
        else {
            return Ok(None);
        };
        let payload = p.posts[p.completed].clone();
        if (self.frozen && p.owner == Owner::AgentAttention)
            || !envelope_matches(
                &self.stores[index(p.owner)].value,
                payload.original.namespace(),
            )
        {
            self.invalidate();
            return Err(Rejection::NeedsReview);
        }
        let key = Key {
            model: Arc::downgrade(&self.id),
            owner: p.owner,
            sequence: p.sequence,
            item: p.completed,
            original_sha256: hash(
                &serde_json::to_vec(&wire::possible_value(&payload.original)).unwrap(),
            ),
            barrier: p.barrier.as_ref().ok_or(Rejection::Unsettled)?.clone(),
        };
        self.native = Some(Native {
            key: key.clone(),
            payload: payload.clone(),
            report: None,
        });
        Ok(Some(PostProposal {
            key,
            payload: Some(payload),
        }))
    }
    pub fn finish_post(
        &mut self,
        proposal: &mut PostProposal,
        report: PostReport,
    ) -> Result<(), Rejection> {
        if !Weak::ptr_eq(&proposal.key.model, &Arc::downgrade(&self.id)) {
            return Err(Rejection::ForeignKey);
        }
        let current = self.native.as_ref().ok_or(Rejection::StaleKey)?;
        if !current.key.matches(&proposal.key) {
            return Err(Rejection::StaleKey);
        }
        if !current.key.matches(&report.key) {
            return Err(Rejection::WrongAttempt);
        }
        if self.invalidated || current.report.is_some() {
            return Err(Rejection::NeedsReview);
        }
        // Namespace validation precedes outcome classification, including a
        // claimed before-call refusal. It never becomes a different binding.
        let original = &current.payload.original;
        let id = match &report.outcome {
            PostOutcome::Delivered(id) => Some(*id),
            _ => None,
        };
        let valid_namespace = report
            .namespace
            .as_ref()
            .is_some_and(|n| same_namespace(n, original.namespace()));
        let representable = id.is_some_and(|id| {
            let (bus, daemon) = namespace_key(original.namespace());
            !self.positives.contains(&(bus, daemon, id.get()))
                && self.stores[index(current.key.owner)].value["tokens"]
                    .get(id.get().to_string())
                    .is_none()
        });
        let observed = match &report.outcome {
            PostOutcome::Delivered(id)
            | PostOutcome::Unknown {
                native_id: Some(id),
            } => Some(*id),
            _ => None,
        };
        let collision = if valid_namespace
            && envelope_matches(
                &self.stores[index(current.key.owner)].value,
                original.namespace(),
            )
            && !self.frozen
        {
            observed.filter(|id| {
                let (bus, daemon) = namespace_key(original.namespace());
                self.positives.contains(&(bus, daemon, id.get()))
                    && self.slot_matches_namespace(original, *id)
            })
        } else {
            None
        };
        let continuation = collision.and_then(|id| {
            self.collision_affects_agent(original, id)
                .ok()
                .filter(|affected| *affected)
                .map(|_| Collision::new(current.key.clone(), id))
        });
        if !valid_namespace
            || !representable
            || !envelope_matches(
                &self.stores[index(current.key.owner)].value,
                original.namespace(),
            )
        {
            self.invalidate();
            let current = self.native.as_mut().unwrap();
            let p = self
                .preparations
                .iter_mut()
                .find(|p| p.sequence == current.key.sequence)
                .unwrap();
            p.phase = BatchStatus::Review;
            current.report = Some(report);
            proposal.payload = None;
            self.collision = continuation;
            return Ok(());
        }
        let id = id.unwrap();
        let current = self.native.take().unwrap();
        let (bus, daemon) = namespace_key(current.payload.original.namespace());
        self.positives.insert((bus, daemon, id.get()));
        self.result = Some(PendingResult {
            key: current.key,
            id,
        });
        proposal.payload = None;
        Ok(())
    }
    pub(super) fn next_result_write(&mut self) -> Result<WritePlan, Rejection> {
        let result = self.result.as_ref().ok_or(Rejection::Unsettled)?;
        let i = self
            .preparations
            .iter()
            .position(|p| p.sequence == result.key.sequence)
            .ok_or(Rejection::StaleKey)?;
        let p = &self.preparations[i];
        if self.frozen && p.owner == Owner::AgentAttention {
            return Err(Rejection::Closed);
        }
        let store = &self.stores[index(p.owner)];
        let mut value = store.value.clone();
        let possible = wire::possible_value(&p.posts[result.key.item].original);
        let uncertain = value["uncertain"]
            .as_array_mut()
            .ok_or(Rejection::InvalidInput)?;
        let matches = uncertain
            .iter()
            .enumerate()
            .filter(|(_, row)| **row == possible)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            self.invalidate();
            return Err(Rejection::NeedsReview);
        }
        uncertain.remove(matches[0]);
        let mut delivered = possible;
        delivered["state"] = Value::String("delivered".into());
        delivered["native_id"] = Value::from(result.id.get());
        let tokens = value["tokens"]
            .as_object_mut()
            .ok_or(Rejection::InvalidInput)?;
        if tokens
            .insert(result.id.get().to_string(), delivered)
            .is_some()
        {
            self.invalidate();
            return Err(Rejection::NeedsReview);
        }
        let bytes: Arc<[u8]> = serde_json::to_vec(&value)
            .map_err(|_| Rejection::InvalidInput)?
            .into();
        if bytes.len() as u64 > FILE_LIMIT {
            self.invalidate();
            return Err(Rejection::Pressure);
        }
        let attempt = Attempt {
            model: Arc::downgrade(&self.id),
            owner: p.owner,
            step: p.sequence,
            item: Some(result.key.item),
            phase: WritePhase::PostResult,
            original_sha256: Some(result.key.original_sha256.clone()),
            predecessor: None,
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
            preparation: i,
            unknown: false,
            blocked: false,
        });
        Ok(WritePlan {
            key: WriteKey { attempt },
            bytes: Some(bytes),
        })
    }
    pub(super) fn result_saved(&mut self, preparation: usize) {
        let result = self.result.take().unwrap();
        let p = &mut self.preparations[preparation];
        if p.owner == Owner::AgentAttention {
            self.agent_namespaces
                .insert(namespace_key(p.posts[result.key.item].original.namespace()));
        }
        p.completed += 1;
        if p.completed == p.posts.len() {
            let p = self.preparations.remove(preparation);
            self.queued_bytes -= p.bytes;
            // Retained evidence, request/nonce history and worst-case headroom
            // remain accounted. No fictional raw-state/FILE_LIMIT refund.
        }
    }
}
redacted!(PostProposal, PostReport, PostOutcome);

#[cfg(test)]
mod tests;
