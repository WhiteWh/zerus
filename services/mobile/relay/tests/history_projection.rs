//! Additive projection admission and one-hour read retention on private state.
mod common;

use common::Fixture;
use serde_json::{json, Value};
use std::time::Duration;
use uuid::Uuid;
use zerus_relay::{
    args,
    db::{i, now, s},
    json::canonical,
    protocol,
};

fn command(computer: &str) -> Value {
    let id = Uuid::new_v4().to_string();
    json!({"request_id":id,"computer_id":computer,"operation":"history_projection","session":"codex/example/tag",
        "payload":{"request_id":id,"expected_run_id":"run","expected_conversation_id":"conversation",
            "history_epoch":"epoch","projection_after":0,"projection_until":7}})
}

async fn beat(f: &Fixture, operations: &[&str]) {
    f.beat(&json!({"sessions":["codex/example/tag"],"mobile_capabilities":{"protocol_version":1,"operations":operations,"features":[]}})).await;
}

async fn complete(f: &Fixture, body: &Value, state: &str) {
    assert_eq!(f.submit(body).await.unwrap()["state"], "queued");
    let claimed = f
        .store
        .claim(&f.node, false)
        .await
        .unwrap()
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(claimed["requests"][0]["request_id"], body["request_id"]);
    let receipt = json!({"state":state,"result":{"synthetic":"projection-retention"},"error":null});
    f.store
        .result(
            &f.node,
            s(body, "request_id"),
            &receipt,
            Some(&canonical(&receipt["result"]).unwrap()),
        )
        .await
        .unwrap();
}

async fn age(f: &Fixture, body: &Value, seconds: f64) {
    f.store
        .db
        .exec(
            "UPDATE requests SET updated=? WHERE id=?",
            args![now() - seconds, &body["request_id"]],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn projection_needs_its_own_selected_machine_operation_and_uses_the_read_quota() {
    assert!(protocol::OPERATIONS.contains(&"history_projection"));
    assert!(protocol::READS.contains(&"history_projection"));
    assert!(!protocol::READ_SQL.contains("history_projection"));
    let f = Fixture::new().await;
    let body = command(&f.node.id);
    beat(&f, &["history"]).await;
    assert_eq!(f.submit(&body).await.unwrap_err().0.as_u16(), 409);
    assert_eq!(f.count("requests").await, 0);
    beat(&f, &["history_projection"]).await;
    assert_eq!(f.submit(&body).await.unwrap()["state"], "queued");
    assert_eq!(f.submit(&body).await.unwrap()["state"], "queued");
    assert_eq!(f.count("requests").await, 1);
    let usage = f
        .store
        .db
        .one(
            "SELECT reads,mutations FROM workspace_usage WHERE workspace_id=?",
            args![&f.phone.workspace],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!((i(&usage, "reads"), i(&usage, "mutations")), (1, 0));
    let mut changed = body.clone();
    changed["payload"]["projection_until"] = json!(8);
    assert_eq!(f.submit(&changed).await.unwrap_err().0.as_u16(), 409);
    f.store
        .db
        .exec(
            "UPDATE workspace_usage SET reads=5000 WHERE workspace_id=?",
            args![&f.phone.workspace],
        )
        .await
        .unwrap();
    assert_eq!(
        f.submit(&command(&f.node.id)).await.unwrap_err().0.as_u16(),
        429
    );
    assert_eq!(f.submit(&body).await.unwrap()["state"], "queued");
    assert_eq!(
        f.submit(&f.command("send")).await.unwrap()["state"],
        "queued"
    );
    f.store
        .db
        .exec(
            "UPDATE workspace_usage SET reads=1 WHERE workspace_id=?",
            args![&f.phone.workspace],
        )
        .await
        .unwrap();
    f.check_accounting().await;
    f.close().await;
}

#[tokio::test]
async fn projection_terminal_reads_expire_by_own_index_and_keep_active_rows_and_mutations() {
    tokio::time::timeout(Duration::from_secs(60), projection_retention_fixture())
        .await
        .expect("projection retention fixture must finish within its retained-row headroom");
}

async fn projection_retention_fixture() {
    let f = Fixture::new().await;
    beat(&f, &["history_projection"]).await;
    let mut reads = Vec::new();
    for state in ["completed", "failed", "uncertain"] {
        let body = command(&f.node.id);
        complete(&f, &body, state).await;
        // Stay near the one-hour boundary to catch shortened retention.
        // The 100-second headroom exceeds the whole fixture's 60-second bound.
        age(&f, &body, 3500.0).await;
        reads.push(body);
    }
    let mutation = f.command("send");
    complete(&f, &mutation, "completed").await;
    age(&f, &mutation, 3601.0).await;
    let active = command(&f.node.id);
    f.submit(&active).await.unwrap();
    age(&f, &active, 3601.0).await;
    // Reproduce scheduling beyond the former one-second retention headroom.
    tokio::time::sleep(Duration::from_millis(1250)).await;
    f.store.maintain(false).await.unwrap();
    for body in &reads {
        assert!(f
            .store
            .receipt(&f.phone, s(body, "request_id"))
            .await
            .is_ok());
    }
    for body in &reads {
        age(&f, body, 3601.0).await;
    }
    f.store.maintain(false).await.unwrap();
    assert_eq!(f.count("requests WHERE operation='history_projection' AND state IN ('completed','failed','uncertain')").await, 0);
    for body in &reads {
        assert_eq!(
            f.store
                .receipt(&f.phone, s(body, "request_id"))
                .await
                .unwrap_err()
                .0
                .as_u16(),
            404
        );
    }
    // An active queue row is retained; its normal queue TTL is based on expires_at,
    // not the synthetic update age used by this retention control.
    assert_eq!(
        f.store
            .receipt(&f.phone, s(&active, "request_id"))
            .await
            .unwrap()["state"],
        "queued"
    );
    assert_eq!(
        f.submit(&mutation).await.unwrap()["result"],
        json!({"synthetic":"projection-retention"})
    );
    age(&f, &mutation, 8.0 * 86400.0).await;
    f.store.maintain(false).await.unwrap();
    let retained = f.submit(&mutation).await.unwrap();
    assert_eq!(retained["state"], "completed");
    assert!(retained["result"].is_null());
    assert_eq!(
        retained["error"],
        "request history expired; delivery must not be retried"
    );
    f.check_accounting().await;
    f.close().await;
}
