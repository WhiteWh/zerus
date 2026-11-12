//! zerus-a4mub.9: a main inspection carries the native `current_read` only
//! when the CLI offers it, it names exactly the checked session, and the
//! newest rows a phone binds it to survive the relay bound.
#![cfg(unix)]
mod common;

use common::{Scratch, EPOCH};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use zerus_mobile_connector::execute::{Outcome, MAX_RESULT};
use zerus_mobile_connector::journal::Journal;
use zerus_mobile_connector::native::{Call, Native, NativeError, NativeResult, Output};
use zerus_mobile_connector::runtime::{Connector, Settings};

const LOCAL: &str = "11111111-1111-4111-8111-111111111111";
const SESSION: &str = "claude/example/session";
const SIGNATURE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const HELP: &str = "hgs [@host] read <session> --json\n";
const MARKER: &str =
    "hgs [@host] inspect <session> --current-read   with the reply and attention\n";

fn current() -> Value {
    json!({"schema":1,"run_id":"run","conversation_id":"conv","reply_id":"7:100.5",
        "attention_signature":SIGNATURE,"attention_complete":true})
}

struct Fixture {
    calls: RefCell<Vec<Vec<String>>>,
    help: Cell<bool>,
    /// What the native inspection answers when asked with `--current-read`.
    current: RefCell<Value>,
    /// History rows of the native inspection.
    rows: RefCell<Value>,
}

impl Native for Fixture {
    async fn call(&self, call: Call) -> NativeResult {
        self.calls.borrow_mut().push(call.argv.clone());
        let argv: Vec<&str> = call.argv.iter().map(String::as_str).collect();
        if argv == ["--help"] {
            let help = if self.help.get() {
                format!("{HELP}{MARKER}")
            } else {
                HELP.into()
            };
            return Ok(Output::Text(help));
        }
        let row = json!({"name":SESSION,"cmd":"claude","agent":"claude","run_id":"run","conversation_id":"conv",
            "activity":"idle","phase":"idle"});
        let result = match argv.as_slice() {
            ["ls", "--json", "--local"] => json!({"sessions":[row]}),
            ["inspect", SESSION, rest @ ..] => {
                let mut inspection = row.clone();
                inspection["events"] = self.rows.borrow().clone();
                if rest.contains(&"--current-read") {
                    inspection["current_read"] = self.current.borrow().clone();
                }
                inspection
            }
            ["account", "ls"] => json!({"profiles":[]}),
            ["swarm", "get"] => json!({"projects":[]}),
            _ => return Err(NativeError::Failed("native command failed")),
        };
        Ok(Output::Json(result))
    }
}

struct Harness {
    connector: Rc<Connector<Fixture>>,
    next: Cell<u64>,
    _scratch: Scratch,
}

impl Harness {
    async fn new(label: &str, help: bool) -> Self {
        let scratch = Scratch::new(label);
        let journal = Journal::open(&scratch.path("state"), "fixture-binding", EPOCH).unwrap();
        let connector = Rc::new(Connector::new(
            Fixture {
                calls: RefCell::default(),
                help: Cell::new(help),
                current: RefCell::new(current()),
                rows: RefCell::new(json!([{"type":"AgentMessage","detail":"reply","at":1}])),
            },
            journal,
            Settings {
                interval: 5.0,
                binding: "fixture-binding".into(),
                default_timeout: 3.0,
                clock_scale: 1.0,
            },
            Box::new(|| EPOCH),
        ));
        *connector.fleet.as_ref().unwrap().local_id.borrow_mut() = Some(LOCAL.into());
        connector.snapshot().await.unwrap();
        Harness {
            connector,
            next: Cell::new(0),
            _scratch: scratch,
        }
    }

    async fn inspect(&self, payload: Value) -> Value {
        let next = self.next.get() + 1;
        self.next.set(next);
        let request = json!({"request_id":format!("44444444-4444-4444-8444-{next:012x}"),"operation":"inspect",
            "session":SESSION,"payload":payload});
        match self.connector.execute(request).await.unwrap() {
            Outcome::Finished(value) => {
                assert_eq!(value["state"], "completed", "{value}");
                value["result"].clone()
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    fn last_inspect(&self) -> Vec<String> {
        self.connector
            .native
            .calls
            .borrow()
            .iter()
            .rev()
            .find(|argv| argv.first().map(String::as_str) == Some("inspect"))
            .unwrap()
            .clone()
    }
}

fn expected() -> Value {
    json!({"expected_run_id":"run","expected_conversation_id":"conv"})
}

#[tokio::test(flavor = "current_thread")]
async fn only_a_cli_that_offers_it_is_asked_and_its_bound_value_passes_through() {
    let h = Harness::new("current-read-offered", true).await;
    let after = json!({"expected_run_id":"run","expected_conversation_id":"conv","after":3});
    for payload in [expected(), after] {
        let result = h.inspect(payload.clone()).await;
        assert!(
            h.last_inspect().contains(&"--current-read".to_owned()),
            "{payload}"
        );
        assert_eq!(result["current_read"], current(), "{payload}");
    }
    let older = Harness::new("current-read-older", false).await;
    let result = older.inspect(expected()).await;
    assert!(!older.last_inspect().contains(&"--current-read".to_owned()));
    assert!(result.get("current_read").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_value_that_does_not_name_exactly_the_checked_session_is_dropped() {
    let h = Harness::new("current-read-binding", true).await;
    let changed = |key: &str, value: Value| {
        let mut current = current();
        current[key] = value;
        current
    };
    let mut extra = current();
    extra["read"] = json!(null);
    let mut missing = current();
    missing
        .as_object_mut()
        .unwrap()
        .remove("attention_complete");
    for (label, value) in [
        ("other run", changed("run_id", json!("other-run"))),
        (
            "other conversation",
            changed("conversation_id", json!("other-conversation")),
        ),
        ("empty run", changed("run_id", json!(""))),
        ("schema 2", changed("schema", json!(2))),
        (
            "uppercase signature",
            changed("attention_signature", json!(SIGNATURE.to_uppercase())),
        ),
        (
            "short signature",
            changed("attention_signature", json!("abc")),
        ),
        ("numeric reply", changed("reply_id", json!(7))),
        (
            "oversized reply",
            changed("reply_id", json!("r".repeat(257))),
        ),
        (
            "string completeness",
            changed("attention_complete", json!("true")),
        ),
        ("extra field", extra),
        ("missing field", missing),
        ("not an object", json!("current")),
    ] {
        *h.connector.native.current.borrow_mut() = value;
        // With and without requested identities: the inspection's own ones bind it.
        for payload in [json!({}), expected()] {
            let result = h.inspect(payload).await;
            assert!(result.get("current_read").is_none(), "{label}: {result}");
            assert_eq!(result["run_id"], "run", "{label}");
        }
    }
    // Null reply and attention are values a mark may carry.
    let mut nulls = current();
    nulls["reply_id"] = json!(null);
    nulls["attention_signature"] = json!(null);
    nulls["attention_complete"] = json!(false);
    *h.connector.native.current.borrow_mut() = nulls.clone();
    assert_eq!(h.inspect(expected()).await["current_read"], nulls);
}

/// zerus-a4mub.24: DeepSeek and OpenCode inspections carry their own reply
/// identities; the unchanged gate passes them through as values a mark may
/// carry, including unknown attention.
#[tokio::test(flavor = "current_thread")]
async fn deepseek_and_opencode_values_pass_the_unchanged_gate() {
    let h = Harness::new("current-read-providers", true).await;
    for (label, reply, signature, complete) in [
        ("DeepSeek turn", json!("dsh-turn-3"), json!(SIGNATURE), true),
        (
            "DeepSeek unknown requests",
            json!("dsh-turn-3"),
            json!(null),
            false,
        ),
        (
            "OpenCode reply",
            json!("msg_14bb2d63e001NhfRTKy4frROM8"),
            json!(null),
            true,
        ),
        (
            "OpenCode without an observation",
            json!(null),
            json!(null),
            false,
        ),
    ] {
        let mut value = current();
        value["reply_id"] = reply;
        value["attention_signature"] = signature;
        value["attention_complete"] = json!(complete);
        *h.connector.native.current.borrow_mut() = value.clone();
        let result = h.inspect(expected()).await;
        assert!(
            h.last_inspect().contains(&"--current-read".to_owned()),
            "{label}"
        );
        assert_eq!(result["current_read"], value, "{label}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn bounding_keeps_it_only_while_the_newest_rows_survive() {
    let h = Harness::new("current-read-bound", true).await;
    let big = "x".repeat(MAX_RESULT / 2);
    // Old rows are dropped first; the newest rows survive and so does the value.
    *h.connector.native.rows.borrow_mut() = json!([
        {"type":"AgentMessage","detail":big,"at":1},
        {"type":"AgentMessage","detail":big,"at":2},
        {"type":"AgentMessage","detail":"newest","at":9},
        {"type":"AgentMessage","detail":"newest too","at":9},
    ]);
    let result = h.inspect(expected()).await;
    assert_eq!(result["mobile_history_truncated"], true);
    assert_eq!(result["events"].as_array().unwrap().len(), 3);
    assert_eq!(result["mobile_history_omitted"]["events"], 1);
    assert_eq!(result["current_read"], current());
    // A newest row too large to keep is dropped with an older one: the phone
    // cannot display the row the value is bound to, so the value goes too.
    *h.connector.native.rows.borrow_mut() = json!([
        {"type":"AgentMessage","detail":"old","at":1},
        {"type":"AgentMessage","detail":big.repeat(3),"at":9},
        {"type":"UserMessage","detail":"own","at":5},
    ]);
    let result = h.inspect(expected()).await;
    assert_eq!(result["mobile_history_truncated"], true);
    assert!(
        result["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["at"] != 9),
        "{result}"
    );
    assert!(result.get("current_read").is_none(), "{result}");
}
