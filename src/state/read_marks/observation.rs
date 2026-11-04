//! Explicit inspection only. Never used by the listing or Activity watch loop.
use super::*;
use serde::de::{DeserializeSeed, Error, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::io::Read;

const MAX_BYTES: u64 = 16 << 20;
const BUDGET: Duration = Duration::from_secs(1);

#[derive(Deserialize, PartialEq)]
struct Binding {
    version: u64,
    name: String,
    run_id: String,
    conversation_id: String,
    #[serde(default)]
    journal_name: String,
}
impl Binding {
    fn owner(&self) -> &str {
        if self.journal_name.is_empty() {
            &self.name
        } else {
            &self.journal_name
        }
    }
}

/// A bounded snapshot of an original private file. Replacement or mutation
/// during the read is unavailable, never an empty mark. No lock file is created.
struct Snapshot {
    data: Vec<u8>,
    file: fs::File,
    stamp: (crate::platform::FileIdentity, u64),
}
fn stamp(file: &fs::File) -> Result<(crate::platform::FileIdentity, u64)> {
    if !crate::platform::verify_private_file(file).map_err(|_| "source_unavailable")? {
        return Err("source_not_private_regular_file".into());
    }
    let (identity, metadata) =
        crate::platform::FileIdentity::with_metadata(file).map_err(|_| "source_unavailable")?;
    Ok((identity, metadata.len()))
}
impl Snapshot {
    fn unchanged(&self, path: &Path) -> Result<()> {
        let current = crate::platform::no_follow(fs::OpenOptions::new().read(true))
            .open(path)
            .map_err(|_| "source_changed")?;
        if stamp(&self.file).map_err(|_| "source_changed")? != self.stamp
            || stamp(&current).map_err(|_| "source_changed")? != self.stamp
        {
            return Err("source_changed".into());
        }
        Ok(())
    }
}
fn bytes(path: &Path, deadline: Instant) -> Result<Option<Snapshot>> {
    if Instant::now() >= deadline {
        return Err("deadline".into());
    }
    let mut file = match crate::platform::no_follow(fs::OpenOptions::new().read(true)).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("source_unavailable".into()),
    };
    let before = stamp(&file)?;
    if before.1 > MAX_BYTES {
        return Err("source_limit".into());
    }
    let mut data = Vec::new();
    let mut chunk = [0; 65536];
    loop {
        if Instant::now() >= deadline {
            return Err("deadline".into());
        }
        let count = file.read(&mut chunk).map_err(|_| "source_unavailable")?;
        if count == 0 {
            break;
        }
        if data.len() as u64 + count as u64 > MAX_BYTES {
            return Err("source_limit".into());
        }
        data.extend_from_slice(&chunk[..count]);
    }
    if data.len() as u64 != before.1 {
        return Err("source_changed".into());
    }
    let snapshot = Snapshot {
        data,
        file,
        stamp: before,
    };
    snapshot.unchanged(path)?;
    Ok(Some(snapshot))
}

/// Retain only one selected nested value. Unrelated owners/conversations are
/// consumed by serde's bounded-depth parser, not assembled into a session map.
struct Select<'a>(&'a [&'a str]);
impl<'de> DeserializeSeed<'de> for Select<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Value, D::Error> {
        if self.0.is_empty() {
            return Mark::deserialize(d)
                .and_then(|mark| serde_json::to_value(mark).map_err(D::Error::custom));
        }
        d.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Select<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a read marks object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut selected = None;
        while let Some(key) = map.next_key::<String>()? {
            if key == self.0[0] {
                if selected.is_some() {
                    return Err(A::Error::custom("duplicate selected key"));
                }
                selected = Some(map.next_value_seed(Select(&self.0[1..]))?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(selected.unwrap_or(Value::Null))
    }
}
struct Marks<'a> {
    owner: &'a str,
    conversation: &'a str,
}
impl<'de> DeserializeSeed<'de> for Marks<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Value, D::Error> {
        d.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Marks<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a versioned read marks store")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut version = None;
        let mut mark = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "version" if version.is_none() => {
                    version = Some(map.next_value::<u64>()?);
                }
                "sessions" if mark.is_none() => {
                    mark = Some(map.next_value_seed(Select(&[self.owner, self.conversation]))?);
                }
                "version" | "sessions" => return Err(A::Error::custom("duplicate store field")),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if version != Some(1) || mark.is_none() {
            return Err(A::Error::custom("invalid store"));
        }
        Ok(mark.unwrap())
    }
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Mark {
    conversation_id: String,
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    reply_id: Option<String>,
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    attention_signature: Option<String>,
    at: f64,
}

fn selected(data: &[u8], owner: &str, conversation: &str) -> Result<Value> {
    let mut parser = serde_json::Deserializer::from_slice(data);
    let value = Marks {
        owner,
        conversation,
    }
    .deserialize(&mut parser)
    .map_err(|_| "invalid_source")?;
    parser.end().map_err(|_| "invalid_source")?;
    if value.is_null() {
        return Ok(value);
    }
    // Same identity/field limits as the shared-read writer, without applying its
    // presentation enrichment or replacing an old reply with a current one.
    let mark: Mark = serde_json::from_value(value.clone()).map_err(|_| "invalid_mark")?;
    let text = |s: &str| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control);
    if mark.conversation_id != conversation
        || !text(&mark.conversation_id)
        || mark.reply_id.as_deref().is_some_and(|s| !text(s))
        || mark.attention_signature.as_deref().is_some_and(|s| {
            s.len() != 64
                || !s
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
        || !mark.at.is_finite()
        || mark.at < 0.0
    {
        return Err("invalid_mark".into());
    }
    Ok(value)
}

fn binding(path: &Path, deadline: Instant) -> Result<Binding> {
    let data = bytes(path, deadline)?.ok_or("target_unavailable")?;
    serde_json::from_slice(&data.data).map_err(|_| "target_unavailable".into())
}

fn observe_in(
    path: &Path,
    record: &Path,
    view: &Value,
    deadline: Instant,
) -> Result<(Value, Value)> {
    let before = binding(record, deadline)?;
    if before.version != 1
        || before.name != string(view, "name")
        || before.run_id.is_empty()
        || before.run_id != string(view, "run_id")
        || before.conversation_id.is_empty()
        || before.conversation_id != string(view, "conversation_id")
    {
        return Err("target_changed".into());
    }
    let data = bytes(path, deadline)?;
    let mark = match &data {
        Some(data) => selected(&data.data, before.owner(), &before.conversation_id)?,
        None => Value::Null,
    };
    if before != binding(record, deadline)? {
        return Err("target_changed".into());
    }
    if Instant::now() >= deadline {
        return Err("deadline".into());
    }
    if let Some(data) = &data {
        data.unchanged(path)?;
    } else {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err("source_changed".into()),
        }
    }
    let digest = data.as_ref().map(|data| Sha256::digest(&data.data));
    if Instant::now() >= deadline {
        return Err("deadline".into());
    }
    Ok((
        mark,
        json!({"complete":true,"freshness":"observed","observed_at":now(),
        "source":"read-marks.json","generation":digest.map(|v| format!("sha256:{v:x}")),
        "source_state":if data.is_some() {"present"} else {"absent"},
        "target":{"name":before.name,"run_id":before.run_id,"conversation_id":before.conversation_id},
        "byte_limit":MAX_BYTES}),
    ))
}

pub(in crate::state) fn enrich(view: &mut Value) {
    let record = record_path(string(view, "name"));
    let result = observe_in(&path(), &record, view, Instant::now() + BUDGET).and_then(|observed| {
        if record_path(string(view, "name")) != record {
            return Err("target_changed".into());
        }
        Ok(observed)
    });
    match result {
        Ok((mark, observation)) => {
            view["read"] = mark;
            view["read_observation"] = observation;
        }
        Err(code) => {
            view["read"] = Value::Null;
            view["read_observation"] = json!({"complete":false,"freshness":"unknown","code":code});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mark() -> Value {
        json!({"conversation_id":"conversation","reply_id":"old-reply","attention_signature":"a".repeat(64),"at":1.0})
    }
    fn store() -> Value {
        json!({"version":1,"sessions":{"owner":{"conversation":mark()}}})
    }
    fn write(path: &Path, data: &[u8]) {
        use std::io::Write;
        let mut file = crate::platform::private_open(
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true),
        )
        .open(path)
        .unwrap();
        file.write_all(data).unwrap();
        crate::platform::make_private(&file).unwrap();
    }
    fn deadline() -> Instant {
        Instant::now() + BUDGET
    }
    #[test]
    fn retains_only_one_of_twenty_thousand_owners_and_the_raw_old_part() {
        let mut value = store();
        for n in 0..20000 {
            value["sessions"][format!("unrelated-{n}")] = json!({"other":{"private":"unrelated"}});
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(selected(&bytes, "owner", "conversation").unwrap(), mark());
        assert_eq!(
            selected(&bytes, "missing", "conversation").unwrap(),
            Value::Null
        );
    }
    #[test]
    fn rejects_ambiguous_malformed_and_wrong_identity_without_echoing_content() {
        for text in [
            r#"{"version":1,"version":1,"sessions":{}}"#,
            r#"{"version":1,"sessions":{},"sessions":{}}"#,
            r#"{"version":1,"sessions":{"owner":{},"owner":{}}}"#,
            r#"{"version":1,"sessions":{"owner":{"conversation":{},"conversation":{}}}}"#,
            r#"{"version":2,"sessions":{}}"#,
            r#"{"version":1,"sessions":null}"#,
            r#"{"version":1,"sessions":{"owner":{"conversation":null}}}"#,
            r#"{"version":1,"sessions":{}} trailing"#,
        ] {
            assert_eq!(
                selected(text.as_bytes(), "owner", "conversation"),
                Err("invalid_source".into())
            );
        }
        for (field, value) in [
            ("conversation_id", json!("wrong")),
            ("reply_id", json!("x".repeat(257))),
            ("attention_signature", json!("bad")),
            ("at", json!(-1)),
        ] {
            let mut data = store();
            data["sessions"]["owner"]["conversation"][field] = value;
            assert_eq!(
                selected(&serde_json::to_vec(&data).unwrap(), "owner", "conversation"),
                Err("invalid_mark".into())
            );
        }
        let duplicate = serde_json::to_string(&store())
            .unwrap()
            .replace("\"at\":1.0", "\"at\":1.0,\"at\":2.0");
        assert_eq!(
            selected(duplicate.as_bytes(), "owner", "conversation"),
            Err("invalid_source".into())
        );
        let mut missing = store();
        missing["sessions"]["owner"]["conversation"]
            .as_object_mut()
            .unwrap()
            .remove("reply_id");
        assert_eq!(
            selected(
                &serde_json::to_vec(&missing).unwrap(),
                "owner",
                "conversation"
            ),
            Err("invalid_source".into())
        );
    }
    #[test]
    fn exact_binding_missing_source_and_current_observation_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record.json");
        let path = dir.path().join("marks.json");
        let view = json!({"name":"renamed","run_id":"run","conversation_id":"conversation"});
        let mut binding = view.clone();
        binding["version"] = json!(1);
        binding["journal_name"] = json!("owner");
        write(&record, &serde_json::to_vec(&binding).unwrap());
        let (read, absent) = observe_in(&path, &record, &view, deadline()).unwrap();
        assert!(read.is_null());
        assert_eq!(absent["source_state"], "absent");
        assert_eq!(absent["complete"], true);
        let data = serde_json::to_vec(&store()).unwrap();
        write(&path, &data);
        let before = fs::read(&path).unwrap();
        let (read, observed) = observe_in(&path, &record, &view, deadline()).unwrap();
        assert_eq!(read, mark());
        assert_eq!(observed["target"], view);
        assert_eq!(
            observed["generation"],
            format!("sha256:{:x}", Sha256::digest(&data))
        );
        assert_eq!(before, fs::read(&path).unwrap());
        assert!(!dir.path().join("read-marks.lock").exists());
        let mut wrong = view.clone();
        wrong["run_id"] = json!("other-run");
        assert_eq!(
            observe_in(&path, &record, &wrong, deadline()).unwrap_err(),
            "target_changed"
        );
        assert_eq!(
            observe_in(&path, &record, &view, Instant::now()).unwrap_err(),
            "deadline"
        );
    }
    #[test]
    fn oversized_or_replaced_source_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marks.json");
        write(&path, b"{}");
        let snapshot = bytes(&path, deadline()).unwrap().unwrap();
        let replacement = dir.path().join("new");
        write(&replacement, b"[]");
        fs::remove_file(&path).unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert_eq!(snapshot.unchanged(&path), Err("source_changed".into()));
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();
        assert!(matches!(bytes(&path,deadline()),Err(e) if e == "source_limit"));
    }
    #[cfg(unix)]
    #[test]
    fn rejects_links_shared_files_and_fifo_without_blocking_or_writing() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marks.json");
        write(&path, b"{}");
        let link = dir.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(bytes(&link, deadline()).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&path, &link).unwrap();
        assert!(bytes(&path, deadline()).is_err());
        fs::remove_file(&link).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(bytes(&path, deadline()).is_err());
        fs::remove_file(&path).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());
        assert!(bytes(&path, deadline()).is_err());
    }
}
