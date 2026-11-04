//! Native-only settings mutations and ephemeral app-wide focus. Read-only
//! status/lookup data never authorizes posting, focus or an ownership handoff.
use super::{store::Store, ui_authority};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Instant;
use zerus_client_core::desktop::notification_control::{
    self as wire, Command, FocusReply, FocusState, Lookup, NativeFocus, Outcome, Preferences,
    Query, Receipt, Scope, Status,
};

const RECEIPTS: i64 = 4096;
pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE desktop_notification_preferences(
        id INTEGER PRIMARY KEY CHECK(id=1),journal TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>=0),
        configured INTEGER NOT NULL CHECK(configured IN(0,1)),optional_questions INTEGER NOT NULL CHECK(optional_questions IN(0,1)));
        CREATE TABLE desktop_notification_preferences_requests(
        ordinal INTEGER PRIMARY KEY AUTOINCREMENT,request_id TEXT NOT NULL UNIQUE,
        fingerprint TEXT NOT NULL,receipt TEXT NOT NULL CHECK(length(receipt)<=8192));")?;
    tx.execute(
        "INSERT INTO desktop_notification_preferences VALUES(1,?1,0,0,0)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    Ok(())
}
fn status(db: &Connection, scope: &Scope) -> Result<Status> {
    let (journal,revision,configured,optional_questions):(String,u64,bool,bool)=db.query_row(
        "SELECT substr(journal,1,37),revision,configured,optional_questions FROM desktop_notification_preferences WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let value = Status {
        abi: wire::ABI.into(),
        scope: scope.clone(),
        journal: Some(journal),
        revision,
        configured,
        preferences: Preferences { optional_questions },
    };
    ensure!(value.valid(), "invalid notification preferences state");
    Ok(value)
}
fn original(db: &Connection, id: &str) -> Result<Option<(String, Receipt)>> {
    let row:Option<(String,String)>=db.query_row("SELECT substr(fingerprint,1,65),substr(receipt,1,8193) FROM desktop_notification_preferences_requests WHERE request_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    row.map(|(fingerprint, receipt)| Ok((fingerprint, serde_json::from_str(&receipt)?)))
        .transpose()
}
struct Focus {
    observation: NativeFocus,
    peer: ui_authority::FocusPeer,
}
pub(super) struct Runtime {
    scope: Scope,
    preferences: Preferences,
    focus: Option<Focus>,
    last_observation: Option<NativeFocus>,
}
impl Runtime {
    pub fn new(store: &Store, configuration: &str, epoch: &str) -> Result<Self> {
        let scope = Scope {
            configuration: configuration.into(),
            service_epoch: epoch.into(),
        };
        ensure!(scope.valid(), "invalid notification control scope");
        Ok(Self {
            preferences: status(&store.db, &scope)?.preferences,
            scope,
            focus: None,
            last_observation: None,
        })
    }
    pub fn preferences(&self) -> Preferences {
        self.preferences
    }
    pub fn foreground_until(&mut self) -> Option<Instant> {
        if self
            .focus
            .as_ref()
            .is_some_and(|focus| !focus.peer.current())
        {
            self.focus = None;
        }
        self.focus.as_ref().map(|focus| focus.peer.until())
    }
    pub fn frontend(&mut self, store: &mut Store, authority: ui_authority::Validated) -> Value {
        let result = (|| -> Result<Value> {
            let (scope, control) = authority.notification_parts()?;
            ensure!(scope == self.scope, "notification control scope changed");
            match control {
                Command::Configure { .. } => {
                    Ok(serde_json::to_value(self.configure(store, authority)?)?)
                }
                Command::Focus { .. } => Ok(serde_json::to_value(self.observe(authority)?)?),
            }
        })();
        result.unwrap_or_else(
            |_| json!({"accepted":false,"durable":false,"code":"notification_control_refused"}),
        )
    }
    fn configure(
        &mut self,
        store: &mut Store,
        authority: ui_authority::Validated,
    ) -> Result<Receipt> {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // The original two-second permit is checked again AFTER writer lock.
        let (scope, control) = authority.notification_parts()?;
        ensure!(
            scope == self.scope,
            "notification preferences scope changed"
        );
        let Command::Configure {
            request_id,
            journal,
            revision,
            preferences,
        } = &control
        else {
            anyhow::bail!("not preferences")
        };
        let fingerprint = control
            .fingerprint(&scope.configuration)
            .context("invalid preferences command")?;
        if let Some((old, receipt)) = original(&tx, request_id)? {
            ensure!(
                old == fingerprint && receipt.matches(&control, &scope.configuration),
                "notification preferences request conflict"
            );
            tx.commit()?;
            return Ok(receipt); // Historical receipt never overwrites current settings.
        }
        let current = status(&tx, &scope)?;
        let matches = *revision == current.revision
            && if current.configured {
                *journal == current.journal
            } else {
                journal.is_none() || *journal == current.journal
            };
        let receipt = Receipt {
            abi: wire::ABI.into(),
            request_id: request_id.clone(),
            scope: scope.clone(),
            fingerprint: fingerprint.clone(),
            journal: current.journal.clone(),
            revision: current.revision + u64::from(matches),
            preferences: *preferences,
            durable: true,
            outcome: if matches {
                Outcome::Saved
            } else {
                Outcome::Refused {
                    code: "notification_preferences_changed".into(),
                }
            },
        };
        ensure!(
            receipt.matches(&control, &scope.configuration),
            "invalid preferences receipt"
        );
        if matches {
            tx.execute("UPDATE desktop_notification_preferences SET revision=?1,configured=1,optional_questions=?2 WHERE id=1",params![receipt.revision,preferences.optional_questions])?;
        }
        tx.execute("INSERT INTO desktop_notification_preferences_requests(request_id,fingerprint,receipt) VALUES(?1,?2,?3)",params![request_id,fingerprint,serde_json::to_string(&receipt)?])?;
        tx.execute("DELETE FROM desktop_notification_preferences_requests WHERE ordinal<COALESCE((SELECT ordinal FROM desktop_notification_preferences_requests ORDER BY ordinal DESC LIMIT 1 OFFSET ?1),0)",[RECEIPTS-1])?;
        tx.commit()?;
        if matches {
            self.preferences = *preferences;
        }
        Ok(receipt)
    }
    fn observe(&mut self, authority: ui_authority::Validated) -> Result<FocusReply> {
        let (scope, request_id, observation, peer) = authority.notification_focus()?;
        ensure!(scope == self.scope, "native focus scope changed");
        if let Some(previous) = self
            .last_observation
            .as_ref()
            .filter(|v| v.connection_id == observation.connection_id)
        {
            ensure!(
                observation.revision > previous.revision || observation == *previous,
                "stale native focus revision"
            );
        }
        self.last_observation = Some(observation.clone());
        let ttl_ms = if observation.state == FocusState::Foreground {
            let remaining = peer
                .until()
                .saturating_duration_since(Instant::now())
                .as_millis() as u64;
            self.focus = Some(Focus {
                observation: observation.clone(),
                peer,
            });
            remaining.min(wire::FOCUS_TTL_MS)
        } else {
            self.focus = None;
            0
        };
        Ok(FocusReply {
            abi: wire::ABI.into(),
            request_id,
            scope,
            observation,
            ttl_ms,
        })
    }
    pub fn request(&mut self, store: &Store, value: &Value) -> Value {
        let result = (|| -> Result<Value> {
            let encoded = serde_json::to_vec(value)?;
            ensure!(
                encoded.len() <= wire::BYTES,
                "notification read query too large"
            );
            match value["op"].as_str() {
                Some("notification_preferences_status") => {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Read {
                        op: String,
                        request_id: String,
                        scope: Scope,
                    }
                    let request: Read = serde_json::from_value(value.clone())?;
                    ensure!(
                        request.op == "notification_preferences_status"
                            && zerus_desktop_migration::canonical_uuid(&request.request_id)
                            && request.scope == self.scope,
                        "notification status scope changed"
                    );
                    Ok(serde_json::to_value(status(&store.db, &self.scope)?)?)
                }
                Some("notification_preferences_result") => {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Read {
                        op: String,
                        query: Query,
                    }
                    let request: Read = serde_json::from_value(value.clone())?;
                    ensure!(
                        request.op == "notification_preferences_result"
                            && request.query.valid()
                            && request.query.scope == self.scope,
                        "notification result scope changed"
                    );
                    let query = request.query;
                    let fingerprint = query
                        .original
                        .fingerprint(&self.scope.configuration)
                        .context("invalid original preferences command")?;
                    let saved = original(&store.db, query.original.request_id())?;
                    if let Some((old, receipt)) = &saved {
                        ensure!(
                            *old == fingerprint
                                && receipt.matches(&query.original, &self.scope.configuration),
                            "original preferences conflict"
                        );
                    }
                    let current = status(&store.db, &self.scope)?;
                    let definitive_not_applied = saved.is_none()
                        && matches!(&query.original,Command::Configure {journal:Some(journal),revision,..} if Some(journal)==current.journal.as_ref() && *revision==current.revision);
                    Ok(serde_json::to_value(Lookup {
                        abi: wire::ABI.into(),
                        request_id: query.request_id,
                        scope: self.scope.clone(),
                        original_request_id: query.original.request_id().into(),
                        fingerprint,
                        receipt: saved.map(|(_, receipt)| receipt),
                        definitive_not_applied,
                    })?)
                }
                _ => anyhow::bail!("not notification read controls"),
            }
        })();
        result.unwrap_or_else(
            |_| json!({"accepted":false,"code":"notification_control_query_refused"}),
        )
    }
    pub fn projection(&mut self, store: &Store) -> Value {
        let until = self.foreground_until();
        json!({"preferences":status(&store.db,&self.scope).ok(),"foreground_ttl_ms":until.map(|at|at.saturating_duration_since(Instant::now()).as_millis() as u64).unwrap_or(0),
            "focus":self.focus.as_ref().map(|v|&v.observation),"app_wide":true,"authority":false})
    }
}

#[cfg(test)]
mod tests;
