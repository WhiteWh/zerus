//! Read-only native reservation data. No lease, dispatch or new-send authority.
//! A reserved row may already have reached the OS. Re-reading it NEVER replaces
//! the original fresh reservation plus the actor's live profile/lease proof.
use super::{hash, json, uuid, Identity, Kind, Reservation, Reserve, State, PAYLOAD_BYTES};
use anyhow::{anyhow, ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

const IDENTITY_BYTES: usize = 4096;

// The unique request index selects one attempt. SQL checks large/corrupt stored
// fields before materializing either private JSON string in the host process.
const LOOKUP: &str = "SELECT a.seq,
    CASE WHEN length(CAST(i.identity AS BLOB)) BETWEEN 1 AND ?10 THEN i.identity END,
    CASE WHEN length(CAST(i.payload AS BLOB)) BETWEEN 1 AND ?11 THEN i.payload END
    FROM desktop_attempts a
    JOIN desktop_intents i ON i.id=a.intent_id
    JOIN desktop_writer w ON w.id=1
    WHERE a.request_id=?1 AND a.fingerprint=?2
    AND a.intent_id=?3 AND a.revision=?4 AND a.epoch=?5 AND a.service_epoch=?6
    AND a.adapter=?7 AND a.lease=?8 AND a.retry_of IS ?9
    AND a.state='reserved' AND a.receipt IS NULL
    AND (w.revoked=0 OR ?12) AND w.epoch=a.epoch AND w.service_epoch=a.service_epoch
    AND i.revision=a.revision AND i.withdrawn=0 AND i.blocked=0";

/// Immutable local data, deliberately neither Clone nor serde-Serialize.
/// This observes only one committed database snapshot. A future actor must
/// recheck live authority before dispatch and retain its actual worker charge.
pub struct Snapshot {
    request: Reserve,
    seq: i64,
    identity: Identity,
    payload: String,
    payload_sha256: String,
    fingerprint: String,
}
impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeReservationSnapshot { .. }")
    }
}
impl Snapshot {
    pub fn request(&self) -> &Reserve {
        &self.request
    }
    pub fn seq(&self) -> i64 {
        self.seq
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    /// Exact saved JSON at the original intent revision, not original source or
    /// native wire bytes. Existing put_intent forbids changing a revision's data.
    pub fn payload_json(&self) -> &str {
        &self.payload
    }
    pub fn payload_sha256(&self) -> &str {
        &self.payload_sha256
    }
    pub fn reservation_fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// A snapshot obtained by consuming the original fresh commit response.
/// This retains origin, not current permission: it owns no database, clock,
/// configuration, live lease, role proof, worker charge or native connection.
pub struct OriginalSnapshot(Snapshot);
impl std::fmt::Debug for OriginalSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OriginalNativeReservationSnapshot { .. }")
    }
}
impl OriginalSnapshot {
    pub fn snapshot(&self) -> &Snapshot {
        &self.0
    }
}

/// Consume the original reserve response once, then recheck its exact row.
/// Duplicate/coalesced responses have no private origin, even if their public
/// `fresh`/`attempt` fields are changed. Errors consume the response too; they
/// never authorize replay or restore the original commit result after restart.
/// The actor chooses the correct store and rechecks its original live lease and
/// actual resources. Only the private fresh withdrawal origin admits cleanup
/// after revocation; an ordinary read cannot reconstruct it.
pub fn inspect_original(db: &mut Connection, reservation: Reservation) -> Result<OriginalSnapshot> {
    let origin = reservation
        .origin
        .ok_or_else(|| anyhow!("original fresh reservation response is unavailable"))?;
    ensure!(
        reservation.fresh
            && reservation.attempt.seq == origin.seq
            && reservation.attempt.request_id == origin.request.request_id
            && reservation.attempt.intent_id == origin.request.intent_id
            && reservation.attempt.revision == origin.request.revision
            && reservation.attempt.state == State::Reserved,
        "original reservation response changed"
    );
    let snapshot = inspect_scoped(db, &origin.request, origin.withdrawal)?;
    ensure!(
        snapshot.seq == origin.seq,
        "original reservation sequence changed"
    );
    Ok(OriginalSnapshot(snapshot))
}

fn fingerprint(request: &Reserve) -> Result<String> {
    for value in [
        &request.request_id,
        &request.epoch,
        &request.service_epoch,
        &request.adapter,
        &request.lease,
    ] {
        uuid(value)?;
    }
    if let Some(value) = &request.retry_of {
        uuid(value)?;
    }
    ensure!(
        request.intent_id.len() == 64
            && request
                .intent_id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            && request.revision > 0
            && request.revision <= i64::MAX as u64,
        "invalid native reservation identity"
    );
    Ok(hash(json(request, IDENTITY_BYTES)?.as_bytes()))
}

/// Read one exact still-reserved Post/Withdrawal under one read transaction.
/// No schema change, reservation, receipt settlement or other write occurs.
/// Current database epochs are checked; lease freshness, canonical frontend,
/// profile drain, original fresh=true, native owner/category and tombstone proof
/// remain the future actor's separate requirements. There is no production caller.
pub fn inspect(db: &mut Connection, request: &Reserve) -> Result<Snapshot> {
    inspect_scoped(db, request, false)
}
fn inspect_scoped(db: &mut Connection, request: &Reserve, withdrawal: bool) -> Result<Snapshot> {
    let fingerprint = fingerprint(request)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let row: Option<(i64, Option<String>, Option<String>)> = tx
        .query_row(
            LOOKUP,
            params![
                request.request_id,
                fingerprint,
                request.intent_id,
                request.revision as i64,
                request.epoch,
                request.service_epoch,
                request.adapter,
                request.lease,
                request.retry_of,
                IDENTITY_BYTES as i64,
                PAYLOAD_BYTES as i64,
                withdrawal,
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (seq, identity, payload) =
        row.ok_or_else(|| anyhow!("native reservation is not current"))?;
    ensure!(seq > 0, "invalid native reservation sequence");
    let (identity, payload) = identity
        .zip(payload)
        .ok_or_else(|| anyhow!("native reservation data exceeds its bounds"))?;
    let parsed: Identity = serde_json::from_str(&identity)
        .map_err(|_| anyhow!("invalid native reservation identity data"))?;
    ensure!(
        matches!(parsed.kind, Kind::Post | Kind::Withdrawal),
        "unsupported native reservation kind"
    );
    ensure!(
        parsed.encoded()? == identity && hash(identity.as_bytes()) == request.intent_id,
        "native reservation identity binding changed"
    );
    serde_json::from_str::<serde::de::IgnoredAny>(&payload)
        .map_err(|_| anyhow!("invalid native reservation payload data"))?;
    if withdrawal {
        super::withdrawal_source(&tx, &parsed, &payload)?;
    }
    let payload_sha256 = hash(payload.as_bytes());
    tx.commit()?;
    Ok(Snapshot {
        request: request.clone(),
        seq,
        identity: parsed,
        payload,
        payload_sha256,
        fingerprint,
    })
}

#[cfg(test)]
mod tests;
