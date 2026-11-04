//! Separate Qt clone checkpoints. The caller already read QtStore; this
//! adapter decodes evidence and never constructs a live Clone or runs a call.
use super::{
    pages, qt_json_bounded, QtSnapshot, QtValue, SourceError, SourceResult, SourceStream,
    READ_BYTES, READ_ENTRIES,
};
use crate::{
    creation,
    paths::{self, Style},
    repository,
};
use serde_json::{json, Value};
use zerus_desktop_migration::{SourceRecord, MAX_SNAPSHOT_BYTES};

const PREFIX: &str = "workspace/repositoryClone";

fn text(text: &str, empty: bool) -> bool {
    (empty || !text.is_empty()) && text.len() <= 8192 && !text.chars().any(char::is_control)
}

fn entry(key: String, value: QtValue) -> SourceResult<Value> {
    let suffix = key
        .strip_prefix(PREFIX)
        .and_then(|s| s.strip_prefix('/'))
        .ok_or(SourceError::UnsupportedSource)?;
    let (scope, kind) = suffix
        .split_once('/')
        .ok_or(SourceError::UnsupportedSource)?;
    if scope.is_empty()
        || scope.len() > 32768
        || scope.len() % 2 != 0
        || !scope
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(SourceError::UnsupportedSource);
    }
    // RepositoryClone::settingsKey stores toHex(compact JSON [executable,host]),
    // not a path/name lookup. Decode that exact scope; never choose an endpoint.
    let bytes = (0..scope.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&scope[at..at + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let [executable, host]: [String; 2] =
        serde_json::from_slice(&bytes).map_err(|_| SourceError::UnsupportedSource)?;
    if !text(&executable, false) || !text(&host, true) {
        return Err(SourceError::UnsupportedSource);
    }
    let record = match kind {
        "parent" => {
            let value = match value {
                QtValue::Text(value) => value,
                _ => return Err(SourceError::UnsupportedSource),
            };
            if !text(&value, false) {
                return Err(SourceError::UnsupportedSource);
            }
            Value::String(value)
        }
        "pending" | "previous" => {
            let value = qt_json_bounded(&value, MAX_SNAPSHOT_BYTES as usize)?;
            let pending: repository::Pending = serde_json::from_value(value.clone())
                .map_err(|_| SourceError::UnsupportedSource)?;
            let style =
                if paths::windows_shaped(&pending.parent) || paths::windows_shaped(&pending.path) {
                    Style::Windows
                } else {
                    Style::Posix
                };
            if !creation::uuid(&pending.request_id)
                || !repository::valid_name(&pending.name)
                || [&pending.url, &pending.parent, &pending.name, &pending.path]
                    .iter()
                    .any(|s| !text(s, false))
                || !creation::safe_repository_url(&pending.url)
                || !paths::absolute(&pending.parent, style)
                || !paths::absolute(&pending.path, style)
                || !paths::same(
                    &pending.path,
                    &repository::destination(&pending.parent, &pending.name),
                    style,
                )
            {
                return Err(SourceError::UnsupportedSource);
            }
            value
        }
        _ => return Err(SourceError::UnsupportedSource),
    };
    Ok(json!({"key":key,"executable":executable,"host":host,"kind":kind,"record":record}))
}

/// Qt pending/previous checkpoints and remembered parent, preserving original
/// private fields without status, retirement, clone, checkout or launch effects.
/// Next's later Settings extension is retained by `next_repository_clones`.
pub fn qt_repository_clones(snapshot: &QtSnapshot) -> SourceResult<SourceStream> {
    if !["qt-ini", "qt-plist", "qt-defaults"].contains(&snapshot.source) {
        return Err(SourceError::UnsupportedSource);
    }
    let mut records = Vec::new();
    let mut total = 0;
    for (key, value) in snapshot.values.iter().filter(|(key, _)| {
        key.as_str() == PREFIX || key.strip_prefix(PREFIX).is_some_and(|s| s.starts_with('/'))
    }) {
        let length = match value {
            QtValue::Text(text) => text.len(),
            QtValue::Bytes(bytes) => bytes.len(),
            _ => return Err(SourceError::UnsupportedSource),
        };
        total += key.len() + length;
        if length as u64 > MAX_SNAPSHOT_BYTES || total > READ_BYTES || records.len() >= READ_ENTRIES
        {
            return Err(SourceError::TooLarge);
        }
        records.push((key.clone(), value.clone()));
    }
    let source = SourceRecord {
        family: "qt-repository-clones".into(),
        schema: 1,
        format: snapshot.source.into(),
        sha256: snapshot.sha256.clone(),
        build: None,
    };
    source
        .validate()
        .map_err(|_| SourceError::InvalidMetadata)?;
    let header = json!({"kind":"clone-header","present":!records.is_empty()});
    Ok(SourceStream::new(
        source,
        std::iter::once(Ok(header)).chain(pages(
            records.into_iter().map(|(key, value)| entry(key, value)),
        )),
    ))
}

/// Next Projects Clone's native-only Settings extension. Only the original
/// `repository_clones` JSON crosses this boundary; unrelated settings do not.
/// Opaque scope hashes, pending/previous requests, whitespace and order remain
/// byte-exact, without choosing an endpoint or constructing a live Clone.
pub fn next_repository_clones(bytes: &[u8]) -> SourceResult<SourceStream> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let raw = crate::settings::repository::snapshot(bytes).map_err(|reason| match reason {
        crate::settings::SnapshotError::TooLarge => SourceError::TooLarge,
        crate::settings::SnapshotError::Unsupported => SourceError::UnsupportedSource,
    })?;
    let source = SourceRecord {
        family: "next-repository-clones".into(),
        schema: 1,
        format: "settings-json-v1".into(),
        sha256: zerus_desktop_migration::sha256(bytes),
        build: None,
    };
    let header = json!({"kind":"clone-header","present":raw.is_some(),
        "field":"repository_clones","bytes":raw.as_ref().map(|r|r.get().len()),"encoding":"base64"});
    let mut offset = 0;
    let chunks = std::iter::from_fn(move || {
        let bytes = raw.as_ref()?.get().as_bytes();
        if offset == bytes.len() {
            return None;
        }
        // Half a MiB before base64 leaves room inside the existing 1 MiB
        // private snapshot cap, even when a UTF-8 codepoint spans chunks.
        let end = (offset + (512 << 10)).min(bytes.len());
        let chunk = json!({"kind":"clone-checkpoint-bytes","offset":offset,
            "base64":STANDARD.encode(&bytes[offset..end])});
        offset = end;
        Some(Ok(chunk))
    });
    Ok(SourceStream::new(
        source,
        std::iter::once(Ok(header)).chain(chunks),
    ))
}

#[cfg(test)]
mod tests;
