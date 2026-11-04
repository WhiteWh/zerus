//! Historical Linux native identity evidence over G4's shared policy decoder.
//! No namespace adoption, bus lookup or notification effect occurs here.
use super::{pages, QtSnapshot, QtValue, SourceError, SourceResult, SourceStream};
use crate::linux_notify::{self, Saved, FILE_LIMIT, LIMIT};
use serde_json::{json, Map, Value};
use zerus_desktop_migration::{sha256, SourceRecord};

mod owned;
pub use owned::next_linux_owned_notifications;

const OWNER: &str = "attention/linuxOwner";
const TOKENS: &str = "attention/linuxTokens";

fn text(value: &QtValue) -> SourceResult<&str> {
    match value {
        QtValue::Text(text) => Ok(text),
        QtValue::Bytes(bytes) => {
            std::str::from_utf8(bytes).map_err(|_| SourceError::UnsupportedSource)
        }
        _ => Err(SourceError::UnsupportedSource),
    }
}

fn stream(
    source: SourceRecord,
    mut root: Map<String, Value>,
    fields: Value,
) -> SourceResult<SourceStream> {
    source
        .validate()
        .map_err(|_| SourceError::InvalidMetadata)?;
    let Some(Value::Object(tokens)) = root.remove("tokens") else {
        return Err(SourceError::UnsupportedSource);
    };
    let uncertain = match root.remove("uncertain") {
        None => Vec::new(),
        Some(Value::Array(values)) => values,
        _ => return Err(SourceError::UnsupportedSource),
    };
    // Bound original entries too, before a typed map/set could coalesce them.
    if tokens.len() + uncertain.len() > LIMIT {
        return Err(SourceError::TooLarge);
    }
    let header = json!({"kind":"linux-notification-header","fields":fields,"content":root});
    let entries = tokens
        .into_iter()
        .map(|(id, token)| Ok(json!({"kind":"token","id":id,"token":token})))
        .chain(
            uncertain
                .into_iter()
                .enumerate()
                .map(|(index, token)| Ok(json!({"kind":"uncertain","index":index,"token":token}))),
        );
    Ok(SourceStream::new(
        source,
        std::iter::once(Ok(header)).chain(pages(entries)),
    ))
}

/// Qt's owner-only lineage is retained as historical evidence, including when
/// the owner is absent. Empty token text is kept distinct from an absent key.
pub fn qt_linux_notifications(snapshot: &QtSnapshot) -> SourceResult<SourceStream> {
    if !["qt-ini", "qt-plist", "qt-defaults"].contains(&snapshot.source) {
        return Err(SourceError::UnsupportedSource);
    }
    let owner = snapshot.values.get(OWNER).map(text).transpose()?;
    let raw = snapshot.values.get(TOKENS).map(text).transpose()?;
    if owner.map_or(0, str::len) as u64 + raw.map_or(0, str::len) as u64 > FILE_LIMIT {
        return Err(SourceError::TooLarge);
    }
    let tokens: Map<String, Value> = match raw {
        None | Some("") => Map::new(),
        Some(text) => serde_json::from_str(text).map_err(|_| SourceError::UnsupportedSource)?,
    };
    if tokens.len() > LIMIT {
        return Err(SourceError::TooLarge);
    }
    // Reuse the same map serde and Saved validator as the existing Qt seed.
    // This projection is never serialized as proof of the original Qt source.
    let projection = Saved {
        version: 1,
        owner: owner.unwrap_or_default().into(),
        tokens: serde_json::from_value(Value::Object(tokens.clone()))
            .map_err(|_| SourceError::UnsupportedSource)?,
        ..Saved::default()
    };
    if !projection.valid() {
        return Err(SourceError::UnsupportedSource);
    }
    let fields = json!({"owner":owner.is_some(),"tokens":if raw.is_none(){"absent"}else if raw==Some(""){"empty-text"}else{"object"}});
    let mut root = Map::new();
    if let Some(owner) = owner {
        root.insert("owner".into(), json!(owner));
    }
    root.insert("tokens".into(), Value::Object(tokens));
    stream(
        SourceRecord {
            family: "qt-linux-notification-identities".into(),
            schema: 1,
            format: snapshot.source.into(),
            sha256: snapshot.sha256.clone(),
            build: None,
        },
        root,
        fields,
    )
}

/// Next's saved file, with original unknown fields and optional-field absence
/// retained separately from the typed Saved projection. Nothing becomes live.
pub fn next_linux_notifications(bytes: &[u8]) -> SourceResult<SourceStream> {
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(SourceError::TooLarge);
    }
    let root: Map<String, Value> =
        serde_json::from_slice(bytes).map_err(|_| SourceError::UnsupportedSource)?;
    linux_notify::decode_saved(bytes).map_err(|_| SourceError::UnsupportedSource)?;
    let fields =
        json!({"bus_id":root.contains_key("bus_id"),"uncertain":root.contains_key("uncertain")});
    stream(
        SourceRecord {
            family: "next-linux-notification-identities".into(),
            schema: 1,
            format: "linux-notifications-json-v1".into(),
            sha256: sha256(bytes),
            build: None,
        },
        root,
        fields,
    )
}

#[cfg(test)]
mod tests;
