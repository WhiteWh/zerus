//! Durable intent receipts. Called only by the desktop actor, never executors.
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zerus_desktop_ledger::reads::{self, ReadMark};

const REQUESTS: i64 = 100_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Policy {
    pub enabled: bool,
    pub revision: i64,
}

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE desktop_delivery_policy(
           id INTEGER PRIMARY KEY CHECK(id=1), enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),
           revision INTEGER NOT NULL CHECK(revision>=0), requests INTEGER NOT NULL CHECK(requests>=0));
         INSERT INTO desktop_delivery_policy VALUES(1,0,0,0);
         CREATE TABLE desktop_delivery_requests(
           request_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, receipt TEXT NOT NULL CHECK(length(receipt)<=4096));",
    )?;
    Ok(())
}

pub(super) fn policy(db: &Connection) -> Result<Policy> {
    Ok(db.query_row(
        "SELECT enabled,revision FROM desktop_delivery_policy WHERE id=1",
        [],
        |r| {
            Ok(Policy {
                enabled: r.get(0)?,
                revision: r.get(1)?,
            })
        },
    )?)
}

pub(super) fn canonical(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Mark {
    computer: String,
    name: String,
    run_id: String,
    conversation_id: String,
    reply_id: Option<String>,
    attention_signature: Option<String>,
}
impl From<Mark> for ReadMark {
    fn from(mark: Mark) -> Self {
        Self {
            computer: mark.computer,
            name: mark.name,
            run_id: mark.run_id,
            conversation_id: mark.conversation_id,
            reply_id: mark.reply_id,
            attention_signature: mark.attention_signature,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    ReadIntent {
        request_id: String,
        mark: Mark,
    },
    DeliveryResult {
        request_id: String,
        original_request_id: String,
    },
}
impl Request {
    pub fn id(&self) -> &str {
        match self {
            Self::ReadIntent { request_id, .. } | Self::DeliveryResult { request_id, .. } => {
                request_id
            }
        }
    }
}

pub(super) fn enqueue(db: &mut Connection, id: &str, mark: &ReadMark, now: i64) -> Result<Value> {
    ensure!(canonical(id), "invalid delivery request ID");
    let used: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM desktop_delivery_requests WHERE request_id=?1)",
        [id],
        |r| r.get(0),
    )?;
    ensure!(!used, "request ID belongs to another intent");
    // The existing queue commits its value and immutable request receipt before
    // this returns. A timeout query/repeated exact ID never creates a new mark.
    let queued = reads::enqueue(db, id, mark, now)?;
    Ok(
        json!({"request_id":id,"accepted":true,"durable":true,"op":"read_intent",
        "queued":queued,"delivered":false}),
    )
}

fn policy_fingerprint(enabled: bool) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!(["automatic_approvals", enabled]))
                .expect("closed policy fingerprint")
        )
    )
}
/// Returning an original committed receipt grants no new policy mutation.
pub(super) fn policy_receipt(db: &Connection, id: &str, enabled: bool) -> Result<Option<Value>> {
    ensure!(canonical(id), "invalid delivery request ID");
    let old: Option<(String, String)> = db
        .query_row(
            "SELECT substr(fingerprint,1,65),substr(receipt,1,4097) FROM desktop_delivery_requests WHERE request_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((hash, receipt)) = old {
        ensure!(
            hash == policy_fingerprint(enabled),
            "request ID changed its intent"
        );
        ensure!(receipt.len() <= 4096, "invalid delivery receipt");
        return Ok(Some(serde_json::from_str(&receipt)?));
    }
    Ok(None)
}
pub(super) fn set_policy(db: &mut Connection, id: &str, enabled: bool) -> Result<Value> {
    ensure!(canonical(id), "invalid delivery request ID");
    let fingerprint = policy_fingerprint(enabled);
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(receipt) = policy_receipt(&tx, id, enabled)? {
        return Ok(receipt);
    }
    let used: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM desktop_read_requests WHERE request_id=?1)",
        [id],
        |r| r.get(0),
    )?;
    ensure!(!used, "request ID belongs to another intent");
    let (revision, count): (i64, i64) = tx.query_row(
        "SELECT revision,requests FROM desktop_delivery_policy WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(count < REQUESTS, "delivery request history is full");
    let revision = revision
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("policy revision exhausted"))?;
    let receipt = json!({"request_id":id,"accepted":true,"durable":true,
        "op":"automatic_approvals","enabled":enabled,"revision":revision});
    tx.execute(
        "UPDATE desktop_delivery_policy SET enabled=?1,revision=?2,requests=requests+1 WHERE id=1",
        params![enabled, revision],
    )?;
    tx.execute(
        "INSERT INTO desktop_delivery_requests VALUES(?1,?2,?3)",
        params![id, fingerprint, receipt.to_string()],
    )?;
    tx.commit()?;
    // Never read or import old UI/Qt settings here: only an explicit actor
    // intent can change this default-off policy.
    Ok(receipt)
}

pub(super) fn result(db: &Connection, id: &str, original: &str) -> Result<Value> {
    ensure!(
        canonical(id) && canonical(original),
        "invalid delivery request ID"
    );
    let policy: Option<String> = db
        .query_row(
            "SELECT substr(receipt,1,4097) FROM desktop_delivery_requests WHERE request_id=?1",
            [original],
            |r| r.get(0),
        )
        .optional()?;
    let result = if let Some(receipt) = policy {
        {
            ensure!(receipt.len() <= 4096, "invalid delivery receipt");
            Some(serde_json::from_str::<Value>(&receipt)?)
        }
    } else {
        let read: Option<String> = db
            .query_row(
                "SELECT substr(receipt,1,4097) FROM desktop_read_requests WHERE request_id=?1",
                [original],
                |r| r.get(0),
            )
            .optional()?;
        read.map(|raw| -> Result<Value> {
            ensure!(raw.len() <= 4096, "invalid read receipt");
            let queued: reads::Queued = serde_json::from_str(&raw)?;
            Ok(json!({"request_id":original,"accepted":true,"durable":true,
                "op":"read_intent","queued":queued,"delivered":false}))
        })
        .transpose()?
    };
    Ok(
        json!({"request_id":id,"accepted":true,"durable":false,"op":"delivery_result",
        "original_request_id":original,"found":result.is_some(),"result":result}),
    )
}
