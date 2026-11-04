//! Pure post admission, result acknowledgements and restricted collision policy.
//! No filesystem, native call, withdrawal/release, runtime or effect ticket.
//! Host reports and policy fences never prove actual durability or drain.
use super::*;
use crate::linux_notify::owned::inspection::{Inspection, Slot};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Weak};

const PREPARATIONS: usize = 64;
const ITEMS: usize = 128;
const BATCH_BYTES: usize = 64 << 10;
const QUEUED_BYTES: usize = 4 << 20;
// Closed-schema margins for positive-ID/state/key growth and both namespace
// headers; no future native path may spend more without a new capacity check.
const RESULT_MARGIN: usize = 65;
const HEADERS_MARGIN: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    InvalidInput,
    WrongOwner,
    WrongHost,
    ReusedNonce,
    ReusedRequest,
    Suppressed,
    Pressure,
    Closed,
    CrossScope,
    ForeignKey,
    StaleKey,
    WrongAttempt,
    NeedsReview,
    Unsettled,
}
pub struct Post {
    pub original: Original,
    pub title: String,
    pub body: String,
}
/// Post-only first slice. No token-only or loaded-binding clear API exists.
pub struct PostBatch {
    pub raised: Vec<Post>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    sha256: Option<String>,
    bytes: Option<usize>,
}
impl Image {
    pub fn sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }
    pub fn bytes(&self) -> Option<usize> {
        self.bytes
    }
    pub fn present(&self) -> bool {
        self.sha256.is_some()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritePhase {
    PreIssue,
    PostResult,
    CollisionInvalidations,
    CollisionResult,
}
impl WritePhase {
    fn collision(self) -> bool {
        matches!(self, Self::CollisionInvalidations | Self::CollisionResult)
    }
}
#[derive(Clone)]
pub struct Attempt {
    model: Weak<()>,
    owner: Owner,
    step: u64,
    item: Option<usize>,
    phase: WritePhase,
    original_sha256: Option<String>,
    predecessor: Option<Box<Attempt>>,
    before: Image,
    after: Image,
}
impl Attempt {
    pub fn phase(&self) -> WritePhase {
        self.phase
    }
    pub fn owner(&self) -> Owner {
        self.owner
    }
    pub fn before(&self) -> &Image {
        &self.before
    }
    pub fn after(&self) -> &Image {
        &self.after
    }
    fn matches(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.model, &other.model)
            && self.owner == other.owner
            && self.step == other.step
            && self.item == other.item
            && self.phase == other.phase
            && self.original_sha256 == other.original_sha256
            && match (&self.predecessor, &other.predecessor) {
                (Some(a), Some(b)) => a.matches(b),
                (None, None) => true,
                _ => false,
            }
            && self.before == other.before
            && self.after == other.after
    }
}
pub struct BatchKey {
    model: Weak<()>,
    owner: Owner,
    sequence: u64,
}
pub struct WriteKey {
    attempt: Attempt,
}
pub struct WritePlan {
    key: WriteKey,
    bytes: Option<Arc<[u8]>>,
}
impl WritePlan {
    pub fn key(&self) -> &WriteKey {
        &self.key
    }
    pub fn attempt(&self) -> Attempt {
        self.key.attempt.clone()
    }
    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Saved,
    RefusedBeforeWrite,
    Unknown,
}
pub struct WriteOutcome {
    pub attempt: Attempt,
    pub disposition: Disposition,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchStatus {
    Queued,
    BarrierAcknowledged,
    Review,
}
struct Preparation {
    owner: Owner,
    sequence: u64,
    posts: Vec<Arc<Post>>,
    bytes: usize,
    completed: usize,
    barrier: Option<Attempt>,
    phase: BatchStatus,
}
struct Store {
    value: Value,
    image: Image,
}
struct Outstanding {
    attempt: Attempt,
    // Retain the one exact prepared buffer after a lost handle/unknown result.
    _bytes: Arc<[u8]>,
    value: Value,
    preparation: usize,
    unknown: bool,
    blocked: bool,
}
pub struct FenceData {
    model: Weak<()>,
    generation: u64,
    agent_image: Image,
}
impl FenceData {
    pub fn agent_image(&self) -> &Image {
        &self.agent_image
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Diagnostics {
    pub agent_inputs_closed: bool,
    pub agent_writes_frozen: bool,
    pub proof_invalidated: bool,
    pub invalidation_generation: u64,
    pub pending_preparations: usize,
    pub retained_unsettled: usize,
    pub queued_utf8_bytes: usize,
    pub write_pending: bool,
    pub write_unknown: bool,
    pub native_pending: bool,
    pub result_pending: bool,
}
pub struct ImageReport<'a> {
    pub last_acknowledged: &'a Image,
    /// Potentially changed as soon as a plan is handed out, including a lost
    /// handle before any explicit outcome. Neither field proves actual disk state.
    pub attempted_unconfirmed: Option<&'a Image>,
}
pub struct Model {
    id: Arc<()>,
    host: String,
    stores: [Store; 2],
    legacy: BTreeSet<String>,
    legacy_image: Image,
    tokens: BTreeSet<ScopedToken>,
    agent_namespaces: BTreeSet<(String, String)>,
    preparations: Vec<Preparation>,
    outstanding: Option<Outstanding>,
    native: Option<post::Native>,
    result: Option<post::PendingResult>,
    collision: Option<post::Collision>,
    positives: BTreeSet<(String, String, u32)>,
    next_sequence: u64,
    highest_nonce: u64,
    requests: BTreeSet<String>,
    retained_unsettled: usize,
    queued_bytes: usize,
    reserved_entries: usize,
    reserved_bytes: usize,
    closed: bool,
    frozen: bool,
    invalidated: bool,
    generation: u64,
}
fn index(owner: Owner) -> usize {
    match owner {
        Owner::AgentAttention => 0,
        Owner::SoftwareUpdate => 1,
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn namespace_key(namespace: &Namespace) -> (String, String) {
    // Derived collision lookup only; original provenance is never normalized.
    (
        namespace.bus_id.to_ascii_lowercase(),
        namespace.owner.clone(),
    )
}
fn image(inspection: &Inspection, slot: Slot) -> Image {
    Image {
        sha256: inspection.sha256(slot).map(str::to_owned),
        bytes: inspection.raw_len(slot),
    }
}
impl Model {
    pub fn from_selected(inspection: Inspection, host: String) -> Result<Self, Rejection> {
        if !crate::creation::canonical_uuid(&host) || host == "00000000-0000-0000-0000-000000000000"
        {
            return Err(Rejection::InvalidInput);
        }
        let mut stores = Vec::new();
        let mut tokens = BTreeSet::new();
        let mut agent_namespaces = BTreeSet::new();
        let mut highest_nonce = 0;
        let mut requests = BTreeSet::new();
        let mut retained_unsettled = 0;
        let mut positives = BTreeSet::new();
        let mut reserved_entries = inspection.raw_entries();
        let mut reserved_bytes = inspection.raw_len(Slot::Legacy).unwrap_or(0) + HEADERS_MARGIN;
        for (slot, expected) in [
            (Slot::Agent, Owner::AgentAttention),
            (Slot::Update, Owner::SoftwareUpdate),
        ] {
            let saved = inspection.data(slot).ok_or(Rejection::InvalidInput)?;
            let rows = saved.rows().ok_or(Rejection::InvalidInput)?;
            if saved.has_unknown() || rows.missing_provenance() || !rows.invalidations().is_empty()
            {
                return Err(Rejection::InvalidInput);
            }
            for row in rows.claims() {
                if row.scoped().owner() != expected {
                    return Err(Rejection::WrongOwner);
                }
                let ClaimData::Retained(c) = row.data() else {
                    return Err(Rejection::InvalidInput);
                };
                if c.native_id().is_none() {
                    return Err(Rejection::NeedsReview);
                }
                reserved_bytes += RESULT_MARGIN;
                retained_unsettled += usize::from(!matches!(c, RetainedClaim::Delivered(_)));
                tokens.insert(c.original().scoped().clone());
                if c.original().poster_instance() == host {
                    highest_nonce = highest_nonce.max(c.original().post_nonce().get());
                    requests.insert(c.original().request_id().to_owned());
                }
            }
            for (o, id) in rows.positive_claims() {
                let (bus, daemon) = namespace_key(o.namespace());
                if !positives.insert((bus, daemon, id.get())) {
                    return Err(Rejection::InvalidInput);
                }
            }
            if expected == Owner::AgentAttention {
                for (o, _) in rows.positive_claims() {
                    agent_namespaces.insert(namespace_key(o.namespace()));
                    // Conservative headroom for every static invalidation reason.
                    reserved_entries += 4;
                    reserved_bytes +=
                        wire::invalidation_budget(o, NonZeroU32::new(u32::MAX).unwrap())
                            .map_err(|_| Rejection::Pressure)?;
                }
            }
            let bytes = saved.encode().map_err(|_| Rejection::InvalidInput)?;
            reserved_bytes += bytes.len();
            stores.push(Store {
                value: serde_json::from_slice(&bytes).map_err(|_| Rejection::InvalidInput)?,
                image: image(&inspection, slot),
            });
        }
        let legacy = inspection
            .data(Slot::Legacy)
            .map(|s| s.legacy_uncertain().map(str::to_owned).collect())
            .unwrap_or_default();
        let legacy_image = image(&inspection, Slot::Legacy);
        Ok(Self {
            id: Arc::new(()),
            host,
            stores: stores.try_into().map_err(|_| Rejection::InvalidInput)?,
            legacy,
            legacy_image,
            tokens,
            agent_namespaces,
            preparations: Vec::new(),
            outstanding: None,
            native: None,
            result: None,
            collision: None,
            positives,
            next_sequence: 1,
            highest_nonce,
            requests,
            retained_unsettled,
            queued_bytes: 0,
            reserved_entries,
            reserved_bytes,
            closed: false,
            frozen: false,
            invalidated: false,
            generation: 1,
        })
    }
    /// Complete expected-owner batch validation before mutating even a counter.
    pub fn prepare_batch(
        &mut self,
        expected: Owner,
        batch: PostBatch,
    ) -> Result<BatchKey, Rejection> {
        if self.invalidated {
            return Err(Rejection::NeedsReview);
        }
        if expected == Owner::AgentAttention && self.closed {
            return Err(Rejection::Closed);
        }
        if batch.raised.is_empty() || batch.raised.len() > ITEMS {
            return Err(Rejection::Pressure);
        }
        let mut bytes = 0_usize;
        let mut nonce = self.highest_nonce;
        let mut seen = BTreeSet::new();
        let mut requests = BTreeSet::new();
        let mut entries = self.reserved_entries;
        let mut reserve = self.reserved_bytes;
        for post in &batch.raised {
            let o = &post.original;
            if o.scoped().owner() != expected {
                return Err(Rejection::WrongOwner);
            }
            if o.poster_instance() != self.host {
                return Err(Rejection::WrongHost);
            }
            if o.post_nonce().get() <= nonce {
                return Err(Rejection::ReusedNonce);
            }
            if self.requests.contains(o.request_id()) || !requests.insert(o.request_id().to_owned())
            {
                return Err(Rejection::ReusedRequest);
            }
            nonce = o.post_nonce().get();
            if self.legacy.contains(o.scoped().token())
                || self.tokens.contains(o.scoped())
                || !seen.insert(o.scoped().clone())
            {
                return Err(Rejection::Suppressed);
            }
            if self.frozen
                && self
                    .agent_namespaces
                    .contains(&namespace_key(o.namespace()))
            {
                return Err(Rejection::CrossScope);
            }
            bytes += post.title.len()
                + post.body.len()
                + o.scoped().token().len()
                + o.request_id().len()
                + o.poster_instance().len()
                + o.namespace().bus_id.len()
                + o.namespace().owner.len()
                + o.actions().iter().map(|a| a.key().len()).sum::<usize>()
                + o.post_nonce().get().to_string().len()
                + o.namespace_generation().get().to_string().len();
            if bytes > BATCH_BYTES {
                return Err(Rejection::Pressure);
            }
            entries += 1;
            reserve += serde_json::to_vec(&wire::possible_value(o))
                .map_err(|_| Rejection::InvalidInput)?
                .len()
                + RESULT_MARGIN;
            if expected == Owner::AgentAttention {
                entries += 4;
                reserve += wire::invalidation_budget(o, NonZeroU32::new(u32::MAX).unwrap())
                    .map_err(|_| Rejection::Pressure)?;
            }
        }
        if self.preparations.len() >= PREPARATIONS
            || self.queued_bytes + bytes > QUEUED_BYTES
            || entries > LIMIT
            || reserve as u64 > FILE_LIMIT
            || self.requests.len() + requests.len() > LIMIT
            || self.next_sequence == u64::MAX
        {
            return Err(Rejection::Pressure);
        }
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.highest_nonce = nonce;
        self.queued_bytes += bytes;
        self.reserved_entries = entries;
        self.reserved_bytes = reserve;
        self.tokens.extend(seen);
        self.requests.extend(requests);
        self.preparations.push(Preparation {
            owner: expected,
            sequence,
            posts: batch.raised.into_iter().map(Arc::new).collect(),
            bytes,
            completed: 0,
            barrier: None,
            phase: BatchStatus::Queued,
        });
        Ok(BatchKey {
            model: Arc::downgrade(&self.id),
            owner: expected,
            sequence,
        })
    }
    /// One output buffer; required result plans precede unrelated barriers.
    pub fn next_write(&mut self) -> Result<Option<WritePlan>, Rejection> {
        if self.invalidated {
            if self.collision.is_some() {
                return self.next_collision_write();
            }
            return Err(Rejection::NeedsReview);
        }
        if self.outstanding.is_some() || self.native.is_some() {
            return Ok(None);
        }
        if self.result.is_some() {
            return self.next_result_write().map(Some);
        }
        let Some(i) = self
            .preparations
            .iter()
            .position(|p| p.phase == BatchStatus::Queued)
        else {
            return Ok(None);
        };
        let p = &self.preparations[i];
        if self.frozen && p.owner == Owner::AgentAttention {
            return Err(Rejection::Closed);
        }
        let store = &self.stores[index(p.owner)];
        let mut value = store.value.clone();
        post::adopt_empty(&mut value, &p.posts);
        let uncertain = value["uncertain"]
            .as_array_mut()
            .ok_or(Rejection::InvalidInput)?;
        uncertain.extend(p.posts.iter().map(|p| wire::possible_value(&p.original)));
        let bytes: Arc<[u8]> = serde_json::to_vec(&value)
            .map_err(|_| Rejection::InvalidInput)?
            .into();
        if bytes.len() as u64 > FILE_LIMIT {
            return Err(Rejection::Pressure);
        }
        let attempt = Attempt {
            model: Arc::downgrade(&self.id),
            owner: p.owner,
            step: p.sequence,
            item: None,
            phase: WritePhase::PreIssue,
            original_sha256: None,
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
        Ok(Some(WritePlan {
            key: WriteKey { attempt },
            bytes: Some(bytes),
        }))
    }
    pub fn ack_write(
        &mut self,
        plan: &mut WritePlan,
        outcome: WriteOutcome,
    ) -> Result<(), Rejection> {
        let key = &plan.key;
        if !Weak::ptr_eq(&key.attempt.model, &Arc::downgrade(&self.id)) {
            return Err(Rejection::ForeignKey);
        }
        let current = self.outstanding.as_ref().ok_or(Rejection::StaleKey)?;
        if !current.attempt.matches(&key.attempt) {
            return Err(Rejection::StaleKey);
        }
        if !current.attempt.matches(&outcome.attempt) {
            return Err(Rejection::WrongAttempt);
        }
        if current.blocked || (self.invalidated && !current.attempt.phase.collision()) {
            return Err(Rejection::NeedsReview);
        }
        if outcome.disposition == Disposition::Unknown
            || (current.attempt.item.is_some()
                && outcome.disposition == Disposition::RefusedBeforeWrite)
        {
            // Sticky logical invalidation precedes any dependent release/status.
            self.invalidated = true;
            self.generation = self.generation.saturating_add(1);
            let current = self.outstanding.as_mut().unwrap();
            current.unknown = outcome.disposition == Disposition::Unknown;
            current.blocked = true;
            if outcome.disposition == Disposition::RefusedBeforeWrite {
                plan.bytes = None;
            }
            self.preparations[current.preparation].phase = BatchStatus::Review;
            return Ok(());
        }
        let current = self.outstanding.take().unwrap();
        // Successful/verified-refused acknowledgement retires the public
        // payload handle too: callers cannot accumulate acknowledged images.
        plan.bytes = None;
        if outcome.disposition == Disposition::Saved {
            self.stores[index(current.attempt.owner)] = Store {
                value: current.value,
                image: current.attempt.after.clone(),
            };
            if current.attempt.phase.collision() {
                self.collision_saved(current.attempt);
            } else if current.attempt.item.is_some() {
                self.result_saved(current.preparation);
            } else {
                self.preparations[current.preparation].barrier = Some(current.attempt.clone());
                self.preparations[current.preparation].phase = BatchStatus::BarrierAcknowledged;
            }
        } else {
            self.preparations[current.preparation].phase = BatchStatus::Review;
        }
        Ok(())
    }
    pub fn batch_status(&self, key: &BatchKey) -> Result<BatchStatus, Rejection> {
        if !Weak::ptr_eq(&key.model, &Arc::downgrade(&self.id)) {
            return Err(Rejection::ForeignKey);
        }
        self.preparations
            .iter()
            .find(|p| p.sequence == key.sequence && p.owner == key.owner)
            .map(|p| p.phase)
            .ok_or(Rejection::StaleKey)
    }
    /// Negative policy data only. Every queued no-ID scope blocks immediately,
    /// even before its barrier write or any future native completion.
    pub fn namespace_blocked(&self, namespace: &Namespace) -> bool {
        self.invalidated
            || self.preparations.iter().flat_map(|p| &p.posts).any(|p| {
                p.original
                    .namespace()
                    .bus_id
                    .eq_ignore_ascii_case(&namespace.bus_id)
                    && p.original.namespace().owner == namespace.owner
            })
    }
    pub fn close_agent_inputs(&mut self) {
        self.closed = true;
    }
    pub fn freeze_agent_writes(&mut self) -> Result<FenceData, Rejection> {
        if self.invalidated {
            return Err(Rejection::NeedsReview);
        }
        if !self.closed {
            return Err(Rejection::Closed);
        }
        if !self.frozen
            && (self.retained_unsettled != 0
                || !self.preparations.is_empty()
                || self.outstanding.is_some()
                || self.native.is_some()
                || self.result.is_some())
        {
            return Err(Rejection::Unsettled);
        }
        self.frozen = true;
        Ok(FenceData {
            model: Arc::downgrade(&self.id),
            generation: self.generation,
            agent_image: self.stores[0].image.clone(),
        })
    }
    /// Equality of policy fence data, never actual resource or current proof.
    pub fn matches_fence(&self, fence: &FenceData) -> bool {
        self.frozen
            && !self.invalidated
            && self.generation == fence.generation
            && Weak::ptr_eq(&fence.model, &Arc::downgrade(&self.id))
    }
    pub fn image_report(&self, owner: Owner) -> ImageReport<'_> {
        ImageReport {
            last_acknowledged: &self.stores[index(owner)].image,
            attempted_unconfirmed: self
                .outstanding
                .as_ref()
                .filter(|w| w.attempt.owner == owner)
                .map(|w| &w.attempt.after),
        }
    }
    pub fn legacy_image(&self) -> &Image {
        &self.legacy_image
    }
    pub fn snapshot(&self) -> Diagnostics {
        Diagnostics {
            agent_inputs_closed: self.closed,
            agent_writes_frozen: self.frozen,
            proof_invalidated: self.invalidated,
            invalidation_generation: self.generation,
            pending_preparations: self.preparations.len(),
            retained_unsettled: self.retained_unsettled,
            queued_utf8_bytes: self.queued_bytes,
            write_pending: self.outstanding.is_some(),
            write_unknown: self.outstanding.as_ref().is_some_and(|w| w.unknown),
            native_pending: self.native.is_some(),
            result_pending: self.result.is_some(),
        }
    }
}
macro_rules! redacted {
    ($($t:ty),+) => { $(impl std::fmt::Debug for $t {
        fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str(concat!(stringify!($t),"([policy data])")) }
    })+ };
}
mod post;
pub use post::{PostOutcome, PostProposal, PostReport};
redacted!(
    Post,
    PostBatch,
    Image,
    Attempt,
    BatchKey,
    WriteKey,
    WritePlan,
    WriteOutcome,
    FenceData,
    ImageReport<'_>,
    Model
);

#[cfg(test)]
mod tests;
