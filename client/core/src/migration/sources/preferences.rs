//! Original view-preference evidence, independent of the Settings family
//! imports. This stream retains opaque Qt values without interpreting splitters
//! or changing the cleanup policy; compatible UI seeds use Settings instead.
use super::{bounded_json, QtSnapshot, QtValue, SourceError, SourceResult, SourceStream};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use zerus_desktop_migration::{SourceRecord, MAX_SNAPSHOT_BYTES};

fn typed(value: &QtValue, list: bool, left: &mut usize) -> SourceResult<Value> {
    let bytes = match value {
        QtValue::Text(s) => s.len(),
        QtValue::Bytes(b) => b.len(),
        _ => 16,
    };
    *left = left.checked_sub(bytes).ok_or(SourceError::TooLarge)?;
    Ok(match value {
        QtValue::Bool(v) => json!({"type":"bool","value":v}),
        QtValue::Int(v) => json!({"type":"int","value":v}),
        QtValue::Double(v) if v.is_finite() => json!({"type":"double","value":v}),
        QtValue::Text(v) => json!({"type":"text","value":v}),
        QtValue::Bytes(v) => json!({"type":"bytes","base64":STANDARD.encode(v)}),
        QtValue::List(v) if list => {
            if v.len() > 64 {
                return Err(SourceError::TooLarge);
            }
            let values = v
                .iter()
                .map(|item| typed(item, false, left))
                .collect::<SourceResult<Vec<_>>>()?;
            json!({"type":"list","value":values})
        }
        _ => return Err(SourceError::UnsupportedSource),
    })
}

/// One explicitly retained field, never a seed or current action policy.
/// The caller's existing source reader supplies a fresh identity/hash recheck.
pub fn qt_retained_view_preference(snapshot: &QtSnapshot, key: &str) -> SourceResult<SourceStream> {
    let family = match key {
        "workspace/projectFolderSplit" => "qt-retained-project-folder-split",
        "tasks/inspectorSplit" => "qt-retained-task-inspector-split",
        "workspace/removeWorktreeAfterTermination" => "qt-retained-termination-remove-default",
        _ => return Err(SourceError::UnsupportedSource),
    };
    if !["qt-ini", "qt-plist", "qt-defaults"].contains(&snapshot.source) {
        return Err(SourceError::UnsupportedSource);
    }
    let source = SourceRecord {
        family: family.into(),
        schema: 1,
        format: snapshot.source.into(),
        sha256: snapshot.sha256.clone(),
        build: None,
    };
    source
        .validate()
        .map_err(|_| SourceError::InvalidMetadata)?;
    let value = snapshot.values.get(key);
    let mut left = MAX_SNAPSHOT_BYTES as usize;
    let original = value.map(|v| typed(v, true, &mut left)).transpose()?;
    let body = json!({"kind":"retained-view-preference","key":key,"present":value.is_some(),"value":original});
    bounded_json(&body, MAX_SNAPSHOT_BYTES as usize)?;
    Ok(SourceStream::new(source, std::iter::once(Ok(body))))
}

#[cfg(test)]
mod tests;
