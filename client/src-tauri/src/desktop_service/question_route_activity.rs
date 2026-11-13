//! The read-only Activity lines of submitted question routes
//! (zerus-sa9qn.33): the service's bounded `question_route_activity` read and
//! the exact shape of its reply. Only a reply to this request, about this
//! session, with at most [`LIMIT`] lines of the exact shape is shown; anything
//! else shows nothing. Nothing here sends, settles or grants anything.
use serde_json::{json, Value};

pub(super) const OP: &str = "question_route_activity";
/// The service's fixed bound of one reply.
pub(super) const LIMIT: usize = 20;
/// The route's own bounds (client/core `question_route`).
const PREVIEW_CHARS: usize = 300;
const TEXT_BYTES: usize = 2048;
const REFERENCE_BYTES: usize = 1024;
const NAME_BYTES: usize = 512;
const KINDS: [&str; 4] = [
    "single-choice",
    "multiple-choice",
    "free-text",
    "multi-part",
];

pub(super) fn canonical(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|v| !v.is_nil() && v.hyphenated().to_string() == id)
}

pub(super) fn query(request_id: &str, session_id: &str) -> Value {
    json!({"op": OP, "request_id": request_id, "session_id": session_id})
}

fn keys(value: &Value, expected: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == expected.len() && expected.iter().all(|k| o.contains_key(*k)))
}
fn text(value: &Value, bytes: usize) -> bool {
    value
        .as_str()
        .is_some_and(|v| !v.is_empty() && v.len() <= bytes)
}

/// One line exactly as the service builds it, about `session`.
fn line(value: &Value, session: &str) -> bool {
    let parent = value["role"] == "parent";
    let shape = if parent {
        keys(
            value,
            &[
                "request_id",
                "at",
                "role",
                "child",
                "parent",
                "question",
                "text",
            ],
        )
    } else {
        keys(
            value,
            &["request_id", "at", "role", "child", "parent", "question"],
        )
    };
    let (child, owner, question) = (&value["child"], &value["parent"], &value["question"]);
    let preview = question["preview"].as_str().unwrap_or("\n");
    shape
        && (parent || value["role"] == "child")
        && value["request_id"].as_str().is_some_and(canonical)
        && value["at"].as_i64().is_some_and(|at| at > 0)
        && keys(child, &["name", "reference", "session_id"])
        && text(&child["name"], NAME_BYTES)
        && child["reference"]
            .as_str()
            .is_some_and(|r| r.starts_with("zerus://session?") && r.len() <= REFERENCE_BYTES)
        && child["session_id"].as_str().is_some_and(canonical)
        && keys(owner, &["name", "session_id"])
        && text(&owner["name"], NAME_BYTES)
        && owner["session_id"].as_str().is_some_and(canonical)
        && child["session_id"] != owner["session_id"]
        && (if parent {
            &owner["session_id"]
        } else {
            &child["session_id"]
        }) == session
        && keys(question, &["kind", "questions", "options", "preview"])
        && question["kind"]
            .as_str()
            .is_some_and(|k| KINDS.contains(&k))
        && question["questions"]
            .as_u64()
            .is_some_and(|n| (1..=u32::MAX as u64).contains(&n))
        && question["options"]
            .as_u64()
            .is_some_and(|n| n <= u32::MAX as u64)
        && preview.chars().count() <= PREVIEW_CHARS
        && !preview.chars().any(char::is_control)
        && (!parent || text(&value["text"], TEXT_BYTES))
}

/// The lines of the service's reply to exactly this request, or `None`.
pub(super) fn routes(receipt: &Value, request_id: &str, session_id: &str) -> Option<Vec<Value>> {
    let routes = receipt["routes"].as_array()?;
    (keys(
        receipt,
        &[
            "request_id",
            "accepted",
            "durable",
            "op",
            "session_id",
            "routes",
        ],
    ) && receipt["request_id"] == request_id
        && receipt["accepted"] == true
        && receipt["durable"] == false
        && receipt["op"] == OP
        && receipt["session_id"] == session_id
        && routes.len() <= LIMIT
        && routes.iter().all(|route| line(route, session_id)))
    .then(|| routes.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "00000000-0000-4000-8000-000000000001";
    const PARENT: &str = "00000000-0000-4000-8000-000000000002";
    const ID: &str = "00000000-0000-4000-8000-0000000000aa";

    fn child_line() -> Value {
        json!({"request_id": "00000000-0000-8000-8000-000000000003", "at": 5, "role": "child",
            "child": {"name": "claude/project/child", "session_id": SESSION,
                "reference": "zerus://session?machine=x&name=claude%2Fproject%2Fchild"},
            "parent": {"name": "codex/project/lead", "session_id": PARENT},
            "question": {"kind": "single-choice", "questions": 1, "options": 2, "preview": "Which database?"}})
    }
    fn reply(routes: Vec<Value>) -> Value {
        json!({"request_id": ID, "accepted": true, "durable": false, "op": OP,
            "session_id": SESSION, "routes": routes})
    }

    #[test]
    fn only_the_exact_reply_to_this_request_is_shown() {
        assert_eq!(
            query(ID, SESSION),
            json!({"op": OP, "request_id": ID, "session_id": SESSION})
        );
        assert_eq!(
            routes(&reply(vec![child_line()]), ID, SESSION),
            Some(vec![child_line()])
        );
        assert_eq!(routes(&reply(vec![]), ID, SESSION), Some(vec![]));
        let mut parent = child_line();
        parent["role"] = json!("parent");
        parent["child"]["session_id"] = json!("00000000-0000-4000-8000-000000000004");
        parent["parent"]["session_id"] = json!(SESSION);
        parent["text"] = json!("[Zerus question route]");
        assert!(routes(&reply(vec![parent.clone()]), ID, SESSION).is_some());
        // A parent line's text is the route's own, bounded and present.
        for text in [json!(""), json!("x".repeat(2049)), json!(5), Value::Null] {
            let mut bad = parent.clone();
            bad["text"] = text;
            assert_eq!(routes(&reply(vec![bad]), ID, SESSION), None);
        }
        type Change = (&'static str, Box<dyn Fn(&mut Value)>);
        let changes: Vec<Change> = vec![
            (
                "another request",
                Box::new(|r| r["request_id"] = json!(PARENT)),
            ),
            (
                "another session",
                Box::new(|r| r["session_id"] = json!(PARENT)),
            ),
            ("not accepted", Box::new(|r| r["accepted"] = json!(false))),
            ("durable", Box::new(|r| r["durable"] = json!(true))),
            (
                "another op",
                Box::new(|r| r["op"] = json!("question_routes_result")),
            ),
            ("an extra key", Box::new(|r| r["code"] = json!(""))),
            (
                "too many",
                Box::new(|r| r["routes"] = json!(vec![child_line(); LIMIT + 1])),
            ),
            (
                "a child line with text",
                Box::new(|r| r["routes"][0]["text"] = json!("x")),
            ),
            (
                "a parent line without text",
                Box::new(|r| r["routes"][0]["role"] = json!("parent")),
            ),
            (
                "another role",
                Box::new(|r| r["routes"][0]["role"] = json!("peer")),
            ),
            (
                "a line about a third session",
                Box::new(|r| {
                    r["routes"][0]["child"]["session_id"] =
                        json!("00000000-0000-4000-8000-000000000004")
                }),
            ),
            (
                "another child",
                Box::new(|r| r["routes"][0]["child"]["session_id"] = json!(PARENT)),
            ),
            (
                "a long preview",
                Box::new(|r| r["routes"][0]["question"]["preview"] = json!("x".repeat(301))),
            ),
            (
                "a two-line preview",
                Box::new(|r| r["routes"][0]["question"]["preview"] = json!("a\nb")),
            ),
            (
                "no questions",
                Box::new(|r| r["routes"][0]["question"]["questions"] = json!(0)),
            ),
            (
                "another kind",
                Box::new(|r| r["routes"][0]["question"]["kind"] = json!("approval")),
            ),
            (
                "another reference",
                Box::new(|r| {
                    r["routes"][0]["child"]["reference"] = json!("https://example.invalid")
                }),
            ),
            ("no time", Box::new(|r| r["routes"][0]["at"] = json!(0))),
            (
                "an extra field",
                Box::new(|r| r["routes"][0]["parent"]["run_id"] = json!("x")),
            ),
        ];
        for (name, change) in changes {
            let mut bad = reply(vec![child_line()]);
            change(&mut bad);
            assert_eq!(routes(&bad, ID, SESSION), None, "{name}");
        }
        assert_eq!(
            routes(&reply(vec![child_line(); LIMIT]), ID, SESSION).map(|r| r.len()),
            Some(LIMIT)
        );
        assert!(routes(
            &json!({"accepted": false, "code": "unsupported_operation"}),
            ID,
            SESSION
        )
        .is_none());
    }

    #[test]
    fn a_session_id_must_be_canonical() {
        assert!(canonical(SESSION));
        for bad in [
            "",
            "x",
            "0000000A-0000-4000-8000-000000000001",
            "00000000-0000-0000-0000-000000000000",
            "{00000000-0000-4000-8000-000000000001}",
        ] {
            assert!(!canonical(bad), "{bad}");
        }
    }
}
