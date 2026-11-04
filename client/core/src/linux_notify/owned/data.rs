//! Passive row data for the next codec extension. These parsers take supplied
//! bytes, not files, and grant no current mapping, receipt, freshness or effect.
//! The existing envelope decoder deliberately remains unchanged until a later
//! integration slice. No method advances a claim or releases suppression.
use super::{Namespace, Refused, ScopedToken, FILE_LIMIT, LIMIT};
use crate::native_notice::Owner;
use std::collections::BTreeSet;
use std::num::{NonZeroU32, NonZeroU64};

#[cfg(test)]
mod tests;
pub mod transitions;
mod wire;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Default,
    OpenSession,
}
impl Action {
    pub fn key(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::OpenSession => "open-session",
        }
    }
}
#[derive(PartialEq, Eq)]
pub struct AllowedActions(Vec<Action>);
impl AllowedActions {
    fn checked(actions: Vec<Action>) -> Result<Self, Refused> {
        if actions.is_empty()
            || actions.len() > 2
            || (actions.len() == 2 && actions[0] == actions[1])
        {
            return Err(Refused::InvalidIdentity);
        }
        Ok(Self(actions))
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Action> + '_ {
        self.0.iter().copied()
    }
}
#[derive(PartialEq, Eq)]
pub struct Original {
    scoped: ScopedToken,
    request_id: String,
    poster_instance: String,
    post_nonce: NonZeroU64,
    namespace: Namespace,
    namespace_generation: NonZeroU64,
    actions: AllowedActions,
}
/// Explicit untrusted host input, not an identity factory or native ticket.
pub struct OriginalInput {
    pub token: String,
    pub request_id: String,
    pub poster_instance: String,
    pub post_nonce: u64,
    pub namespace: Namespace,
    pub namespace_generation: u64,
    pub actions: Vec<Action>,
}
struct OriginalFields {
    request_id: String,
    poster_instance: String,
    post_nonce: u64,
    namespace: Namespace,
    namespace_generation: u64,
    actions: AllowedActions,
}
impl Original {
    pub fn new(expected: Owner, input: OriginalInput) -> Result<Self, Refused> {
        Self::checked(
            ScopedToken::new(expected, input.token)?,
            OriginalFields {
                request_id: input.request_id,
                poster_instance: input.poster_instance,
                post_nonce: input.post_nonce,
                namespace: input.namespace,
                namespace_generation: input.namespace_generation,
                actions: AllowedActions::checked(input.actions)?,
            },
        )
    }
    fn checked(scoped: ScopedToken, fields: OriginalFields) -> Result<Self, Refused> {
        if !crate::creation::canonical_uuid(&fields.poster_instance)
            || fields.poster_instance == "00000000-0000-0000-0000-000000000000"
            || fields.request_id.is_empty()
            || fields.request_id.len() > 512
            || fields.request_id.chars().any(char::is_control)
        {
            return Err(Refused::InvalidIdentity);
        }
        Ok(Self {
            scoped,
            request_id: fields.request_id,
            poster_instance: fields.poster_instance,
            post_nonce: NonZeroU64::new(fields.post_nonce).ok_or(Refused::InvalidIdentity)?,
            namespace: super::namespace(fields.namespace.bus_id, fields.namespace.owner)?
                .ok_or(Refused::InvalidNamespace)?,
            namespace_generation: NonZeroU64::new(fields.namespace_generation)
                .ok_or(Refused::InvalidIdentity)?,
            actions: fields.actions,
        })
    }
    pub fn scoped(&self) -> &ScopedToken {
        &self.scoped
    }
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    /// Exact validated canonical spelling; no UUID dependency or normalization.
    pub fn poster_instance(&self) -> &str {
        &self.poster_instance
    }
    pub fn post_nonce(&self) -> NonZeroU64 {
        self.post_nonce
    }
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }
    pub fn namespace_generation(&self) -> NonZeroU64 {
        self.namespace_generation
    }
    pub fn actions(&self) -> &AllowedActions {
        &self.actions
    }
}
#[derive(PartialEq, Eq)]
pub struct NativeBinding {
    original: Original,
    native_id: NonZeroU32,
}
impl NativeBinding {
    pub fn original(&self) -> &Original {
        &self.original
    }
    pub fn native_id(&self) -> NonZeroU32 {
        self.native_id
    }
}
#[derive(PartialEq, Eq)]
pub enum RetainedClaim {
    PossiblePost(Original),
    Delivered(NativeBinding),
    WithdrawalPending(NativeBinding),
    UnknownPost {
        original: Original,
        native_id: Option<NonZeroU32>,
    },
    UnknownWithdrawal(NativeBinding),
}
impl RetainedClaim {
    pub fn original(&self) -> &Original {
        match self {
            Self::PossiblePost(o) | Self::UnknownPost { original: o, .. } => o,
            Self::Delivered(b) | Self::WithdrawalPending(b) | Self::UnknownWithdrawal(b) => {
                &b.original
            }
        }
    }
    /// Includes unknown and pending-withdrawal IDs, never just Delivered rows.
    pub fn native_id(&self) -> Option<NonZeroU32> {
        match self {
            Self::PossiblePost(_) => None,
            Self::UnknownPost { native_id, .. } => *native_id,
            Self::Delivered(b) | Self::WithdrawalPending(b) | Self::UnknownWithdrawal(b) => {
                Some(b.native_id)
            }
        }
    }
    pub fn tag(&self) -> &'static str {
        match self {
            Self::PossiblePost(_) => "possible_post",
            Self::Delivered(_) => "delivered",
            Self::WithdrawalPending(_) => "withdrawal_pending",
            Self::UnknownPost { .. } => "unknown_post",
            Self::UnknownWithdrawal(_) => "unknown_withdrawal",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidationReason {
    NativeIdCollision,
    NamespaceChanged,
    WithdrawalConflict,
    UnverifiedResult,
}
impl InvalidationReason {
    pub fn tag(self) -> &'static str {
        match self {
            Self::NativeIdCollision => "native_id_collision",
            Self::NamespaceChanged => "namespace_changed",
            Self::WithdrawalConflict => "withdrawal_conflict",
            Self::UnverifiedResult => "unverified_result",
        }
    }
}
#[derive(PartialEq, Eq)]
pub struct Invalidation {
    target: NativeBinding,
    reason: InvalidationReason,
}
impl Invalidation {
    pub fn target(&self) -> &NativeBinding {
        &self.target
    }
    pub fn reason(&self) -> InvalidationReason {
        self.reason
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Tokens(u32),
    Uncertain,
}
pub enum ClaimData {
    Historical(ScopedToken),
    Retained(RetainedClaim),
}
pub struct ClaimRow {
    data: ClaimData,
    placement: Placement,
    has_unknown: bool,
    raw_bytes: usize,
}
impl ClaimRow {
    pub fn data(&self) -> &ClaimData {
        &self.data
    }
    pub fn placement(&self) -> Placement {
        self.placement
    }
    pub fn has_unknown(&self) -> bool {
        self.has_unknown
    }
    pub fn missing_provenance(&self) -> bool {
        matches!(self.data, ClaimData::Historical(_))
    }
    pub fn encode(&self) -> Result<Vec<u8>, Refused> {
        wire::encode_claim(self)
    }
    pub(super) fn scoped(&self) -> &ScopedToken {
        match &self.data {
            ClaimData::Historical(t) => t,
            ClaimData::Retained(c) => &c.original().scoped,
        }
    }
}
pub struct InvalidationRow {
    data: Invalidation,
    has_unknown: bool,
    raw_bytes: usize,
}
impl InvalidationRow {
    pub fn data(&self) -> &Invalidation {
        &self.data
    }
    pub fn has_unknown(&self) -> bool {
        self.has_unknown
    }
    pub fn encode(&self) -> Result<Vec<u8>, Refused> {
        wire::encode_invalidation(self)
    }
}
/// Caller-selected owner is checked, never selected by a tag or filename.
pub fn decode_claim(
    bytes: &[u8],
    expected: Owner,
    placement: Placement,
) -> Result<ClaimRow, Refused> {
    wire::claim(bytes, expected, placement)
}
/// Only an explicitly selected update envelope may carry agent invalidations.
pub fn decode_invalidation(bytes: &[u8], expected: Owner) -> Result<InvalidationRow, Refused> {
    wire::invalidation(bytes, expected)
}
pub struct Rows {
    claims: Vec<ClaimRow>,
    invalidations: Vec<InvalidationRow>,
}
impl Rows {
    /// Structural row membership only, not full envelope/pair preparation. The
    /// caller must additionally count both envelopes and the single legacy home.
    pub fn new(
        expected: Owner,
        claims: Vec<ClaimRow>,
        invalidations: Vec<InvalidationRow>,
    ) -> Result<Self, Refused> {
        if claims.len().saturating_add(invalidations.len()) > LIMIT
            || claims
                .iter()
                .map(|r| r.raw_bytes)
                .chain(invalidations.iter().map(|r| r.raw_bytes))
                .try_fold(0_usize, |n, len| n.checked_add(len))
                .is_none_or(|n| n as u64 > FILE_LIMIT)
        {
            return Err(Refused::TooLarge);
        }
        if expected != Owner::SoftwareUpdate && !invalidations.is_empty() {
            return Err(Refused::InvalidIdentity);
        }
        let (mut ids, mut originals) = (BTreeSet::new(), BTreeSet::new());
        for row in &claims {
            if row.scoped().owner() != expected {
                return Err(Refused::InvalidIdentity);
            }
            if let Placement::Tokens(id) = row.placement {
                if !ids.insert(id) {
                    return Err(Refused::DuplicateIdentity);
                }
            }
            if let ClaimData::Retained(claim) = &row.data {
                let o = claim.original();
                // A Poster nonce denotes one original post. Changed request,
                // owner, token, state or namespace cannot create another row.
                if !originals.insert((o.poster_instance.as_str(), o.post_nonce)) {
                    return Err(Refused::DuplicateIdentity);
                }
            }
        }
        Ok(Self {
            claims,
            invalidations,
        })
    }
    pub fn claims(&self) -> &[ClaimRow] {
        &self.claims
    }
    pub fn invalidations(&self) -> &[InvalidationRow] {
        &self.invalidations
    }
    pub fn has_unknown(&self) -> bool {
        self.claims.iter().any(ClaimRow::has_unknown)
            || self.invalidations.iter().any(InvalidationRow::has_unknown)
    }
    pub fn missing_provenance(&self) -> bool {
        self.claims.iter().any(ClaimRow::missing_provenance)
    }
    /// Collision accounting only: includes unknown/withdrawal evidence, never
    /// an activation set or current numeric-ID permission.
    pub fn positive_claims(&self) -> impl Iterator<Item = (&Original, NonZeroU32)> {
        self.claims.iter().filter_map(|row| match &row.data {
            ClaimData::Retained(c) => c.native_id().map(|id| (c.original(), id)),
            _ => None,
        })
    }
}
macro_rules! redacted {
    ($($name:ty),+) => { $(impl std::fmt::Debug for $name {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(concat!(stringify!($name), "([retained data])"))
        }
    })+ };
}
redacted!(
    OriginalInput,
    AllowedActions,
    Original,
    NativeBinding,
    RetainedClaim,
    Invalidation,
    ClaimData,
    ClaimRow,
    InvalidationRow,
    Rows
);
