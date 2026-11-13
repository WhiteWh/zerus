//! The read-only latch of one local child (zerus-sa9qn.50): the service's
//! `question_route_child_status` reply, exactly, for the person's next
//! compare-and-swap. It changes nothing.
use serde_json::{json, Value};

pub(super) const STATUS_OP: &str = "question_route_child_status";

pub(super) fn query(request_id: &str, session_id: &str) -> Value {
    json!({"op": STATUS_OP, "request_id": request_id, "session_id": session_id})
}

/// The latch in the service's reply to exactly this request, or `None`.
pub(super) fn status(receipt: &Value, request_id: &str, session_id: &str) -> Option<Value> {
    let keys = [
        "request_id",
        "accepted",
        "durable",
        "op",
        "session_id",
        "person_off",
        "revision",
    ];
    let exact = receipt
        .as_object()
        .is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)));
    (exact
        && receipt["request_id"] == request_id
        && receipt["accepted"] == true
        && receipt["durable"] == false
        && receipt["op"] == STATUS_OP
        && receipt["session_id"] == session_id
        && receipt["person_off"].is_boolean()
        && receipt["revision"]
            .as_u64()
            .is_some_and(|r| r < i64::MAX as u64))
    .then(|| json!({"person_off": receipt["person_off"], "revision": receipt["revision"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "00000000-0000-4000-8000-0000000000aa";
    const SESSION: &str = "00000000-0000-4000-8000-000000000001";

    #[test]
    fn only_the_exact_reply_to_this_request_names_the_latch() {
        let reply = json!({"request_id": ID, "accepted": true, "durable": false, "op": STATUS_OP,
            "session_id": SESSION, "person_off": true, "revision": 3});
        assert_eq!(
            status(&reply, ID, SESSION),
            Some(json!({"person_off": true, "revision": 3}))
        );
        assert_eq!(
            query(ID, SESSION),
            json!({"op": STATUS_OP, "request_id": ID, "session_id": SESSION})
        );
        for (field, wrong) in [
            ("request_id", json!(SESSION)),
            ("session_id", json!(ID)),
            ("accepted", json!(false)),
            ("durable", json!(true)),
            ("op", json!("question_route_child_result")),
            ("person_off", json!("true")),
            ("revision", json!(-1)),
            ("revision", json!(1.5)),
            ("code", json!("")),
        ] {
            let mut bad = reply.clone();
            bad[field] = wrong;
            assert_eq!(status(&bad, ID, SESSION), None, "{field}");
        }
    }
}
