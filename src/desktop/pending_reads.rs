//! Authenticated safe-passive materialization into the one existing read queue.
//! No worker, writer, policy, host acknowledgement or handoff is created here.
use super::{store::Store, ui_authority::Validated};
use anyhow::{ensure, Result};
use rusqlite::TransactionBehavior;
use serde::Deserialize;
use serde_json::{json, Value};
use zerus_client_core::desktop::{
    pending_reads::{self, Page},
    ui_requests::Request,
};
use zerus_desktop_ledger::reads;
use zerus_desktop_migration::canonical_uuid;

fn response(id: &str, page: &Page, superseded: usize) -> Value {
    json!({"op":"import_pending_reads","request_id":id,"accepted":true,
        "durable":true,"source":page.source,"after":page.after,
        "through":page.through(),"superseded":superseded,"delivered":false})
}

pub(super) fn frontend(store: &mut Store, authority: Validated) -> Value {
    let id = authority.id().to_owned();
    let refused = |code| json!({"request_id":id,"accepted":false,"durable":false,"code":code});
    let Ok(Request::ImportPendingReads { request_id, page }) = authority.into_request() else {
        return refused("frontend_identity_expired");
    };
    materialize(store, &request_id, &page)
        .unwrap_or_else(|_| refused("pending_read_import_unconfirmed"))
}

fn materialize(store: &mut Store, id: &str, page: &Page) -> Result<Value> {
    ensure!(
        canonical_uuid(id) && page.valid(),
        "invalid pending read page"
    );
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let configuration: String =
        tx.query_row("SELECT configuration FROM service WHERE id=1", [], |r| {
            r.get(0)
        })?;
    let mut superseded = 0;
    for mark in &page.marks {
        let id = pending_reads::intent_id(&configuration, mark).map_err(anyhow::Error::msg)?;
        let occupied: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM desktop_delivery_requests WHERE request_id=?1)",
            [&id],
            |r| r.get(0),
        )?;
        ensure!(!occupied, "read ID belongs to another intent");
        let receipt = reads::adopt_in(&tx, &id, mark, chrono::Utc::now().timestamp_millis())?;
        superseded += usize::from(receipt.superseded);
    }
    tx.commit()?;
    Ok(response(id, page, superseded))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    op: String,
    request_id: String,
    original_request_id: String,
    page: Page,
}
pub(super) fn result_request(store: &Store, value: &Value) -> Value {
    let id = value["request_id"].as_str().unwrap_or("");
    lookup(store, value).unwrap_or_else(|_| {
        json!({"request_id":id,
        "accepted":false,"durable":false,"code":"invalid_pending_read_lookup"})
    })
}
fn lookup(store: &Store, value: &Value) -> Result<Value> {
    let request: Lookup = serde_json::from_value(value.clone())?;
    ensure!(
        request.op == "pending_reads_import_result"
            && canonical_uuid(&request.request_id)
            && canonical_uuid(&request.original_request_id)
            && request.page.valid(),
        "invalid pending read lookup"
    );
    let configuration: String =
        store
            .db
            .query_row("SELECT configuration FROM service WHERE id=1", [], |r| {
                r.get(0)
            })?;
    let mut found = true;
    let mut superseded = 0;
    for mark in &request.page.marks {
        let id = pending_reads::intent_id(&configuration, mark).map_err(anyhow::Error::msg)?;
        match reads::receipt(&store.db, &id, mark)? {
            Some(receipt) => superseded += usize::from(receipt.superseded),
            None => found = false,
        }
    }
    Ok(
        json!({"op":"pending_reads_import_result","request_id":request.request_id,
        "original_request_id":request.original_request_id,"accepted":true,
        "durable":false,"found":found,"result":found.then(|| response(&request.original_request_id,&request.page,superseded))}),
    )
}

#[cfg(test)]
mod tests;
