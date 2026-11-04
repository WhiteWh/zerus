//! Bounded display-only catalog reads. Revisions stay exact decimal strings on the wire.
use super::{valid_computer, CATALOG_ROWS};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CHANGES_ABI: &str = "desktop-catalog-changes-v1";
pub const CHANGES_HELP: &str = "Desktop catalog changes ABI: desktop-catalog-changes-v1";
pub const RELAY_ABI: &str = "desktop-catalog-relay-v1";
pub const RELAY_HELP: &str = "Desktop catalog relay ABI: desktop-catalog-relay-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Revision(u64);
impl TryFrom<String> for Revision {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let number = value
            .parse::<u64>()
            .map_err(|_| "invalid_catalog_revision")?;
        if number > i64::MAX as u64 || number.to_string() != value {
            return Err("invalid_catalog_revision");
        }
        Ok(Self(number))
    }
}
impl From<Revision> for String {
    fn from(value: Revision) -> Self {
        value.0.to_string()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct CatalogIdentity {
    pub incarnation: u64,
    pub generation: u64,
    pub projection: Option<String>,
}
impl CatalogIdentity {
    pub fn valid(&self) -> bool {
        self.projection.as_ref().is_none_or(|p| {
            p.len() == 32
                && p.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct CatalogItem {
    pub key: String,
    pub row: Value,
}

#[derive(Debug, Deserialize)]
pub struct CatalogPage {
    #[serde(flatten)]
    pub identity: CatalogIdentity,
    pub start_revision: Option<Revision>,
    pub revision: Option<Revision>,
    pub online: bool,
    pub last_known: bool,
    pub stale: bool,
    pub complete: bool,
    pub items: Vec<CatalogItem>,
    pub next_cursor: Option<String>,
}
fn envelope(value: &Value, request: &str, epoch: &str, computer: &str) -> bool {
    value["request_id"] == request
        && zerus_desktop_migration::canonical_uuid(request)
        && value["abi"] == "desktop-catalog-v1"
        && value["changes_abi"] == CHANGES_ABI
        && value["accepted"] == true
        && value["durable"] == false
        && value["epoch"] == epoch
        && zerus_desktop_migration::canonical_uuid(epoch)
        && value["computer"] == computer
        && valid_computer(computer)
        && value["display_only"] == true
        && value["authority"] == false
        && value.get("next_cursor").is_some_and(|v| {
            v.is_null()
                || v.as_str()
                    .is_some_and(zerus_desktop_migration::canonical_uuid)
        })
}
fn key_valid(key: &str) -> bool {
    !key.is_empty() && key.len() <= 4096
}
impl CatalogPage {
    pub fn checked(
        value: Value,
        request: &str,
        epoch: &str,
        computer: &str,
    ) -> Result<Self, &'static str> {
        if !envelope(&value, request, epoch, computer) {
            return Err("invalid_catalog_page");
        }
        let page: Self = serde_json::from_value(value).map_err(|_| "invalid_catalog_page")?;
        if !page.identity.valid()
            || page.items.len() > CATALOG_ROWS
            || page
                .items
                .iter()
                .any(|i| !key_valid(&i.key) || !i.row.is_object())
            || page.start_revision.is_some_and(|start| {
                !page.online
                    || page.identity.projection.is_none()
                    || page.revision.is_none_or(|r| r < start)
            })
            || (page.complete
                && (!page.online || page.last_known || page.stale || page.start_revision.is_none()))
        {
            return Err("invalid_catalog_page");
        }
        Ok(page)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ChangeItem {
    pub op: String,
    pub revision: Revision,
    pub key: String,
    pub row: Option<Value>,
}
#[derive(Debug, Deserialize)]
pub struct ChangePage {
    #[serde(flatten)]
    pub identity: CatalogIdentity,
    pub since_revision: Revision,
    pub through_revision: Revision,
    pub revision: Revision,
    pub online: bool,
    pub last_known: bool,
    pub stale: bool,
    pub complete: bool,
    pub items: Vec<ChangeItem>,
    pub next_cursor: Option<String>,
}
impl ChangePage {
    pub fn checked(
        value: Value,
        request: &str,
        epoch: &str,
        computer: &str,
    ) -> Result<Self, &'static str> {
        if !envelope(&value, request, epoch, computer) {
            return Err("catalog_changes_reset");
        }
        let page: Self = serde_json::from_value(value).map_err(|_| "invalid_catalog_changes")?;
        if !page.identity.valid()
            || page.identity.projection.is_none()
            || !page.online
            || page.last_known
            || !page.complete
            || page.since_revision > page.through_revision
            || page.revision < page.through_revision
            || page.stale != (page.revision > page.through_revision)
            || page.items.len() > CATALOG_ROWS
            || page.items.iter().any(|i| {
                !key_valid(&i.key)
                    || i.revision <= page.since_revision
                    || i.revision > page.through_revision
                    || match i.op.as_str() {
                        "upsert" => i.row.as_ref().is_none_or(|r| !r.is_object()),
                        "remove" => i.row.is_some(),
                        _ => true,
                    }
            })
        {
            return Err("invalid_catalog_changes");
        }
        Ok(page)
    }
}
