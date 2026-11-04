//! Read-only approval history through the selected desktop service.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ABI: &str = "desktop-approval-audit-v1";
pub const HELP_MARKER: &str = "Desktop approval audit ABI: desktop-approval-audit-v1";
pub const PAGE: usize = 50;
pub const REQUEST_BYTES: usize = 8 << 10;
pub const REPLY_BYTES: usize = 256 << 10;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum Request {
    #[serde(rename = "approval_audit_list")]
    List {
        request_id: String,
        configuration: String,
        epoch: String,
        journal: Option<String>,
        through: Option<i64>,
        #[serde(default)]
        host: String,
        #[serde(default)]
        session: String,
        before: Option<i64>,
        after: Option<i64>,
    },
    #[serde(rename = "approval_audit_detail")]
    Detail {
        request_id: String,
        configuration: String,
        epoch: String,
        journal: String,
        id: i64,
    },
}
impl Request {
    pub fn id(&self) -> &str {
        match self {
            Self::List { request_id, .. } | Self::Detail { request_id, .. } => request_id,
        }
    }
    pub fn configuration(&self) -> &str {
        match self {
            Self::List { configuration, .. } | Self::Detail { configuration, .. } => configuration,
        }
    }
    pub fn epoch(&self) -> &str {
        match self {
            Self::List { epoch, .. } | Self::Detail { epoch, .. } => epoch,
        }
    }
    pub fn journal(&self) -> Option<&str> {
        match self {
            Self::List { journal, .. } => journal.as_deref(),
            Self::Detail { journal, .. } => Some(journal),
        }
    }
    pub fn valid(&self) -> bool {
        let uuid = zerus_desktop_migration::canonical_uuid;
        uuid(self.id())
            && super::digest(self.configuration())
            && uuid(self.epoch())
            && self.journal().is_none_or(uuid)
            && serde_json::to_vec(self).is_ok_and(|v| v.len() <= REQUEST_BYTES)
            && match self {
                Self::Detail { id, .. } => *id > 0,
                Self::List {
                    journal,
                    through,
                    host,
                    session,
                    before,
                    after,
                    ..
                } => {
                    let text = |s: &str, max| s.len() <= max && !s.chars().any(char::is_control);
                    text(host, 256)
                        && text(session, 1024)
                        && before.is_none_or(|n| n > 0)
                        && after.is_none_or(|n| n > 0)
                        && !(before.is_some() && after.is_some())
                        && through.is_none_or(|n| n >= 0)
                        && (journal.is_some() == through.is_some())
                        && (before.is_none() && after.is_none() || journal.is_some())
                }
            }
    }
    pub fn matches(&self, reply: &Value) -> bool {
        self.valid()
            && reply["abi"] == ABI
            && reply["request_id"] == self.id()
            && reply["configuration"] == self.configuration()
            && reply["service_epoch"] == self.epoch()
            && reply["read_only"] == true
            && reply["accepted"].is_boolean()
            && (reply["accepted"] != true
                || (self.journal().is_none_or(|id| reply["journal"] == id)
                    && self.valid_result(reply)))
            && serde_json::to_vec(reply).is_ok_and(|v| v.len() <= REPLY_BYTES)
    }
    fn valid_result(&self, reply: &Value) -> bool {
        let journal = reply["journal"].as_str();
        if !reply["journal"].is_null()
            && !journal.is_some_and(zerus_desktop_migration::canonical_uuid)
        {
            return false;
        }
        let result = &reply["result"];
        match self {
            Self::List { through, .. } => {
                let Some(high) = result["through"].as_i64().filter(|n| *n >= 0) else {
                    return false;
                };
                let Some(rows) = result["entries"].as_array().filter(|v| v.len() <= PAGE) else {
                    return false;
                };
                through.is_none_or(|n| n == high)
                    && (journal.is_some() || (high == 0 && rows.is_empty()))
                    && ["next", "previous"].iter().all(|name| {
                        result[*name].is_null()
                            || result[*name].as_i64().is_some_and(|n| n > 0 && n <= high)
                    })
                    && rows.iter().all(|row| {
                        row["id"].as_i64().is_some_and(|n| n > 0 && n <= high)
                            && row["at"].is_i64()
                            && [
                                ("host", 256),
                                ("session", 1024),
                                ("provider", 64),
                                ("prompt", 1280),
                                ("status", 32),
                            ]
                            .iter()
                            .all(|(name, max)| row[*name].as_str().is_some_and(|s| s.len() <= *max))
                    })
            }
            Self::Detail { id, .. } => {
                journal.is_some()
                    && result["id"] == *id
                    && result["question"].is_object()
                    && result["answers"].is_array()
                    && result["status"].as_str().is_some_and(|s| s.len() <= 32)
                    && result["detail"].as_str().is_some_and(|s| s.len() <= 8192)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn query() -> Value {
        json!({"op":"approval_audit_list","request_id":"00000000-0000-4000-8000-000000000001",
            "configuration":"a".repeat(64),"epoch":"00000000-0000-4000-8000-000000000002"})
    }
    #[test]
    fn only_closed_bounded_read_queries_are_supported() {
        let value = query();
        assert!(serde_json::from_value::<Request>(value.clone())
            .unwrap()
            .valid());
        for (key, wrong) in [
            ("op", json!("approval_audit_begin")),
            ("path", json!("/not/selected")),
            ("role", json!("writer")),
        ] {
            let mut changed = value.clone();
            changed[key] = wrong;
            assert!(serde_json::from_value::<Request>(changed).is_err());
        }
        for (key, wrong) in [
            ("epoch", json!("unknown")),
            ("host", json!("x".repeat(257))),
            ("before", json!(1)),
            ("through", json!(5)),
        ] {
            let mut changed = value.clone();
            changed[key] = wrong;
            assert!(!serde_json::from_value::<Request>(changed).unwrap().valid());
        }
    }
    #[test]
    fn read_replies_are_bound_to_request_scope_journal_and_limits() {
        let value = query();
        let request: Request = serde_json::from_value(value).unwrap();
        let reply = json!({"abi":ABI,"request_id":request.id(),"configuration":request.configuration(),
            "service_epoch":request.epoch(),"accepted":true,"read_only":true,"journal":null,
            "result":{"entries":[],"next":null,"previous":null,"through":0}});
        assert!(request.matches(&reply));
        for (key, wrong) in [
            ("configuration", json!("b".repeat(64))),
            ("service_epoch", json!("other")),
            ("request_id", json!("other")),
            ("read_only", json!(false)),
            ("journal", json!("unknown")),
            ("result", Value::Null),
        ] {
            let mut changed = reply.clone();
            changed[key] = wrong;
            assert!(!request.matches(&changed));
        }
        let mut changed = reply;
        changed["oversize"] = json!("x".repeat(REPLY_BYTES));
        assert!(!request.matches(&changed));
    }
}
