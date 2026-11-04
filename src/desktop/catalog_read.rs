//! Bounded display reads. Internal numeric cursors never cross private IPC.
use super::{feed, protocol, store::Store};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use zerus_desktop_catalog::{self as catalog, changes};

pub const ABI: &str = "desktop-catalog-v1";
pub const CHANGES_ABI: &str = "desktop-catalog-changes-v1";
pub const WALKS: usize = 64;
const IDLE: Duration = Duration::from_secs(120);
const LIFETIME: Duration = Duration::from_secs(600);

#[derive(Debug)]
struct WalkCapacity;
impl std::fmt::Display for WalkCapacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("catalog walk capacity")
    }
}
impl std::error::Error for WalkCapacity {}

#[derive(Clone)]
enum Position {
    Baseline {
        cursor: catalog::Cursor,
        start: changes::Watermark,
    },
    Changes(changes::Cursor),
}
impl Position {
    fn computer(&self) -> &str {
        match self {
            Self::Baseline { cursor, .. } => &cursor.computer,
            Self::Changes(cursor) => &cursor.scope.source.computer,
        }
    }
}
#[derive(Clone)]
struct Walk {
    epoch: String,
    position: Position,
    idle: Instant,
    until: Instant,
}
#[derive(Default)]
pub struct Reader {
    // Shared cap across both kinds of walk. Never session rows or prior pages.
    walks: BTreeMap<String, Walk>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request_id: String,
    op: String,
    computer: String,
    cursor: Option<String>,
    epoch: Option<String>,
    incarnation: Option<u64>,
    generation: Option<u64>,
    projection: Option<String>,
    since_revision: Option<String>,
}
impl Reader {
    pub fn status(&self) -> Value {
        json!({"abi":ABI,"changes_abi":CHANGES_ABI,
            "change_item_limit":changes::MAX_ITEMS,"change_byte_limit":changes::MAX_BYTES,"retained_walks":self.walks.len(),"limit":WALKS,
            "idle_ms":IDLE.as_millis(),"lifetime_ms":LIFETIME.as_millis()})
    }
    pub fn request(
        &mut self,
        store: &Store,
        feed: &feed::Broker,
        epoch: &str,
        value: &Value,
    ) -> Value {
        self.at(store, feed, epoch, value, Instant::now())
    }
    fn at(
        &mut self,
        store: &Store,
        feed: &feed::Broker,
        epoch: &str,
        value: &Value,
        now: Instant,
    ) -> Value {
        self.walks
            .retain(|_, walk| now < walk.idle && now < walk.until);
        let id = value
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let refused = |code| json!({"request_id":id,"accepted":false,"durable":false,"abi":ABI,"code":code,"reset_required":true});
        let Ok(request) = serde_json::from_value::<Request>(value.clone()) else {
            return refused("invalid_catalog_request");
        };
        if !uuid::Uuid::parse_str(&request.request_id)
            .is_ok_and(|u| u.to_string() == request.request_id)
            || request.computer.len() > 255
            || (!request.computer.is_empty()
                && (request.computer.starts_with(['-', '@'])
                    || request
                        .computer
                        .chars()
                        .any(|c| c.is_control() || c.is_whitespace())))
            || !matches!(
                request.op.as_str(),
                "catalog_page" | "catalog_changes" | "catalog_close"
            )
        {
            return refused("invalid_catalog_request");
        }
        let old = match request.cursor.as_ref() {
            None => None,
            Some(token) => {
                if !uuid::Uuid::parse_str(token).is_ok_and(|u| u.to_string() == *token) {
                    return refused("invalid_catalog_cursor");
                }
                let Some(walk) = self.walks.get(token) else {
                    return refused("catalog_cursor_expired");
                };
                if walk.position.computer() != request.computer {
                    return refused("catalog_cursor_scope");
                }
                if walk.epoch != epoch {
                    self.walks.remove(token);
                    return refused("catalog_cursor_expired");
                }
                // A continuation is one-use even if a later read/encoding fails.
                // Unknown outcomes restart a display walk; no rows are cached.
                self.walks.remove(token)
            }
        };
        if request.op == "catalog_close" {
            return if old.is_some() {
                json!({"request_id":id,"accepted":true,"durable":false,"abi":ABI,"closed":true})
            } else {
                refused("invalid_catalog_cursor")
            };
        }
        let result = if request.op == "catalog_changes" {
            self.changes(store, feed, epoch, &request, old, now)
        } else {
            self.baseline(store, feed, epoch, &request, old, now)
        };
        match result {
            Ok(value) => value,
            Err(e) if e.is::<WalkCapacity>() => refused("catalog_walk_capacity"),
            Err(e) if e.is::<changes::RetentionGap>() => refused("catalog_changes_gap"),
            Err(_) if request.op == "catalog_changes" => refused("catalog_changes_reset"),
            Err(_) => refused("catalog_read_refused"),
        }
    }
    fn baseline(
        &mut self,
        store: &Store,
        feed: &feed::Broker,
        epoch: &str,
        request: &Request,
        old: Option<Walk>,
        now: Instant,
    ) -> Result<Value> {
        ensure!(
            request.epoch.is_none()
                && request.incarnation.is_none()
                && request.generation.is_none()
                && request.projection.is_none()
                && request.since_revision.is_none(),
            "unexpected baseline scope"
        );
        let mut current = changes::watermark(&store.db, &request.computer)?;
        current.online &=
            current.scope.source.service_epoch == epoch && feed.available(&request.computer);
        current.complete &= current.online && feed.coverage(&request.computer);
        let (cursor, start) = match old.as_ref().map(|w| &w.position) {
            Some(Position::Baseline { cursor, start }) => {
                ensure!(
                    start.scope == current.scope
                        && start.online == current.online
                        && (!start.complete || current.complete),
                    "baseline source reset"
                );
                (Some(cursor), start.clone())
            }
            Some(_) => anyhow::bail!("wrong catalog cursor kind"),
            None => {
                let mut start = current.clone();
                if !start.complete {
                    start.revision = None;
                }
                (None, start)
            }
        };
        let page = catalog::page(&store.db, &request.computer, cursor)?;
        let position = page.next.map(|cursor| Position::Baseline {
            cursor,
            start: start.clone(),
        });
        let rows = page
            .rows
            .into_iter()
            .map(|r| {
                let row: Value =
                    serde_json::from_str(&r.json).context("invalid private catalog row")?;
                Ok(json!({"key":r.key,"row":row}))
            })
            .collect::<Result<Vec<_>>>()?;
        let stale = start.revision != current.revision;
        let value = json!({"request_id":request.request_id,"accepted":true,"durable":false,"abi":ABI,
            "changes_abi":CHANGES_ABI,"epoch":epoch,"computer":request.computer,
            "incarnation":current.scope.source.incarnation,"generation":current.scope.source.generation,
            "projection":current.scope.projection,"start_revision":start.revision.map(|r|r.to_string()),
            "revision":current.revision.map(|r|r.to_string()),"stale":stale,"online":current.online,
            "last_known":!current.complete,"display_only":true,"authority":false,
            "complete":page.complete && current.complete && !stale,"items":rows});
        self.finish(epoch, value, position, old.as_ref(), now)
    }
    fn changes(
        &mut self,
        store: &Store,
        feed: &feed::Broker,
        epoch: &str,
        request: &Request,
        old: Option<Walk>,
        now: Instant,
    ) -> Result<Value> {
        ensure!(
            request.epoch.as_deref() == Some(epoch) && feed.coverage(&request.computer),
            "catalog source unavailable"
        );
        let projection = request.projection.clone().context("missing projection")?;
        ensure!(
            projection.len() == 32
                && projection
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid projection"
        );
        let revision = request
            .since_revision
            .as_deref()
            .context("missing revision")?;
        let since: i64 = revision.parse()?;
        ensure!(
            since >= 0 && since.to_string() == revision,
            "invalid revision"
        );
        let scope = changes::Scope {
            source: catalog::Source {
                computer: request.computer.clone(),
                service_epoch: epoch.into(),
                incarnation: request.incarnation.context("missing incarnation")?,
                generation: request.generation.context("missing generation")?,
            },
            projection: Some(projection),
        };
        let cursor = match old.as_ref().map(|w| &w.position) {
            Some(Position::Changes(cursor)) => Some(cursor),
            Some(_) => anyhow::bail!("wrong catalog cursor kind"),
            None => None,
        };
        let page = changes::page(&store.db, &scope, since, cursor)?;
        let items=page.items.into_iter().map(|r| {
            let row:Option<Value>=r.json.as_deref().map(serde_json::from_str).transpose()?;
            Ok(json!({"op":if row.is_some(){"upsert"}else{"remove"},"revision":r.revision.to_string(),"key":r.key,"row":row}))
        }).collect::<Result<Vec<_>>>()?;
        let current = changes::watermark(&store.db, &request.computer)?;
        let value = json!({"request_id":request.request_id,"accepted":true,"durable":false,"abi":ABI,
            "changes_abi":CHANGES_ABI,"epoch":epoch,"computer":request.computer,
            "incarnation":scope.source.incarnation,"generation":scope.source.generation,"projection":scope.projection,
            "since_revision":revision,"through_revision":page.through.to_string(),
            "revision":current.revision.map(|r|r.to_string()),"stale":current.revision!=Some(page.through),
            "online":true,"last_known":false,"complete":true,"display_only":true,"authority":false,"items":items});
        self.finish(
            epoch,
            value,
            page.next.map(Position::Changes),
            old.as_ref(),
            now,
        )
    }
    fn finish(
        &mut self,
        epoch: &str,
        mut value: Value,
        position: Option<Position>,
        old: Option<&Walk>,
        now: Instant,
    ) -> Result<Value> {
        if position.is_some() && self.walks.len() >= WALKS {
            return Err(WalkCapacity.into());
        }
        let token = position.as_ref().map(|_| uuid::Uuid::new_v4().to_string());
        value["next_cursor"] = json!(token);
        // Include the exact outer envelope before retaining any continuation.
        let mut framed = super::frame(epoch, u64::MAX, "response", value);
        protocol::encode(&framed)?;
        if let (Some(token), Some(position)) = (token, position) {
            self.walks.insert(
                token,
                Walk {
                    epoch: epoch.into(),
                    position,
                    idle: now + IDLE,
                    until: old.map_or(now + LIFETIME, |w| w.until),
                },
            );
        }
        Ok(framed["value"].take())
    }
}

#[cfg(test)]
mod tests;
