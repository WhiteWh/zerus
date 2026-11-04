//! Original AgentAttention sibling evidence only, never a live owner pair,
//! a Model, current namespace, permission, lease or source discovery.
use super::*;
use crate::{linux_notify::owned as policy, native_notice::Owner};
use base64::{engine::general_purpose::STANDARD, Engine as _};

/// Supplied bounded original `linux-notifications-owned.json` bytes, checked
/// by G4's explicitly selected AgentAttention row decoder. Schema-one sources
/// stay separate. No file, bus, policy encoding, import caller or effect API.
pub fn next_linux_owned_notifications(bytes: &[u8]) -> SourceResult<SourceStream> {
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(SourceError::TooLarge);
    }
    let saved =
        policy::decode_owned_for(bytes, Owner::AgentAttention).map_err(|error| match error {
            policy::Refused::TooLarge | policy::Refused::LegacyCapacity => SourceError::TooLarge,
            _ => SourceError::UnsupportedSource,
        })?;
    let header = json!({"kind":"linux-owned-notification-header","file":policy::FILE,
        "owner":"agent_attention","bytes":bytes.len(),"raw_entries":saved.raw_entries(),
        "unknown_fields":saved.has_unknown(),"encoding":"base64"});
    drop(saved);
    let source = SourceRecord {
        family: "next-linux-owned-notification-identities".into(),
        schema: 2,
        format: "linux-notifications-owned-json-v2".into(),
        sha256: sha256(bytes),
        build: None,
    };
    // Keep original private unknown fields, order and whitespace. Never
    // serialize a decoded policy projection as evidence of this source.
    let original = bytes.to_vec();
    let mut offset = 0;
    let chunks = std::iter::from_fn(move || {
        if offset == original.len() {
            return None;
        }
        let end = (offset + (512 << 10)).min(original.len());
        let chunk = json!({"kind":"linux-owned-notification-bytes","offset":offset,
            "base64":STANDARD.encode(&original[offset..end])});
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
