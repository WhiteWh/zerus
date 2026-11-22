//! Ordinary dispatch serialization. Native identity survives a display rename
//! and a new run; unrelated conversations never wait for this guard.
use super::*;

fn key(record: &Value) -> Result<String> {
    let home = string(record, "agent_home");
    let conversation = string(record, "conversation_id");
    if home.is_empty() || !conversation.starts_with("session-") || conversation.len() > 2048 {
        return Err("DeepSeek binding has no stable native identity".into());
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(json!([home, conversation]).to_string().as_bytes())
    ))
}

/// The first atomic metadata read only chooses a guard. Never wait for that
/// guard under the machine-wide binding lock. Once held, re-read the binding
/// under the short file lock and refuse a replaced native identity.
pub(in crate::state) fn acquire(name: &str) -> Result<(Guard, Value)> {
    let observed = binding(name)?;
    let identity = key(&observed)?;
    let guard = lock_record(&observed)?;
    let record = {
        let _bindings = lock(Some(&root().join("dsh/bindings.lock")))?;
        let current = binding(name)?;
        if key(&current)? != identity {
            return Err("DeepSeek binding changed while waiting for its session".into());
        }
        current
    };
    Ok((guard, record))
}

pub(super) fn lock_record(record: &Value) -> Result<Guard> {
    let identity = key(record)?;
    let path = root()
        .join("dsh/session-locks")
        .join(format!("{identity}.lock"));
    lock(Some(&path))
}

/// Only local binding I/O belongs in `write`. Other, not-yet-migrated metadata
/// writers still use the machine lock, so recheck the full saved record before
/// publishing an update rather than overwriting their intervening changes.
pub(in crate::state) fn transaction<T>(
    name: &str,
    expected: &Value,
    write: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _bindings = lock(Some(&root().join("dsh/bindings.lock")))?;
    if binding(name)? != *expected {
        return Err(
            "DeepSeek binding changed during the native operation; inspect its result".into(),
        );
    }
    write()
}

pub(super) fn persist(name: &str, expected: &mut Value, record: &mut Value) -> Result<()> {
    transaction(name, expected, || save(record))?;
    *expected = record.clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Value {
        json!({"name":"dsh/example/old", "run_id":"old-run",
               "agent_home":"/fixture/native", "conversation_id":"session-one"})
    }

    #[test]
    fn rename_and_resume_share_the_original_native_guard() {
        let original = record();
        let mut changed = original.clone();
        changed["name"] = json!("dsh/example/renamed");
        changed["run_id"] = json!("new-run");
        assert_eq!(key(&original).unwrap(), key(&changed).unwrap());
    }

    #[test]
    fn a_reused_label_or_another_native_home_is_independent() {
        let original = record();
        for (field, value) in [
            ("conversation_id", "session-other"),
            ("agent_home", "/fixture/other"),
        ] {
            let mut changed = original.clone();
            changed[field] = json!(value);
            assert_ne!(key(&original).unwrap(), key(&changed).unwrap());
        }
    }

    #[test]
    fn missing_or_invalid_native_identity_never_selects_a_shared_fallback() {
        for (field, value) in [
            ("conversation_id", ""),
            ("conversation_id", "other"),
            ("agent_home", ""),
        ] {
            let mut changed = record();
            changed[field] = json!(value);
            assert!(key(&changed).is_err());
        }
    }
}
