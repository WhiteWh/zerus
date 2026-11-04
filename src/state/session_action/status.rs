//! Observation of an original lifecycle receipt. This never calls `attempt`,
//! acquires a writer, creates a store, or turns missing evidence into a retry.
use super::*;
use crate::state::repositories::directory::Directory;
use std::fs;
use std::io::Read;

const LIMIT: u64 = 64 * 1024;
const INVALID: &str = "The original action receipt could not be verified. Check the session.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    request_id: String,
    name: String,
    run_id: String,
    conversation_id: String,
    request_hash: String,
    status: String,
    at: f64,
    archive_id: Option<String>,
    result_target: Option<ResultTarget>,
    error: Option<String>,
    error_source: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResultTarget {
    name: String,
    run_id: String,
    conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_id: Option<String>,
}

fn bounded(text: &str, limit: usize, required: bool) -> bool {
    (!required || !text.is_empty()) && text.len() <= limit && !text.chars().any(char::is_control)
}

fn uuid(text: &str) -> bool {
    uuid::Uuid::parse_str(text).is_ok_and(|id| !id.is_nil() && id.to_string() == text)
}

impl ResultTarget {
    fn valid(&self, request: &Request, name: &str) -> bool {
        if !bounded(&self.name, 512, true)
            || !bounded(&self.run_id, 128, true)
            || !bounded(&self.conversation_id, 256, false)
            || self.archive_id.as_deref().is_some_and(|id| !uuid(id))
        {
            return false;
        }
        match request.action.as_str() {
            "fork" => {
                self.name != name
                    && self.run_id != request.expected_run_id
                    && !self.conversation_id.is_empty()
                    && self.archive_id.is_none()
            }
            "resume" | "restore" => {
                self.name == name
                    && self.conversation_id == request.expected_conversation_id
                    && self.archive_id.is_none()
            }
            "rename" => {
                Some(&self.name) == request.new_name.as_ref()
                    && self.run_id == request.expected_run_id
                    && self.conversation_id == request.expected_conversation_id
                    && self.archive_id == request.archive_id
            }
            _ => {
                self.name == name
                    && self.run_id == request.expected_run_id
                    && self.conversation_id == request.expected_conversation_id
                    && (if matches!(request.action.as_str(), "archive" | "terminate") {
                        self.archive_id.is_some()
                    } else {
                        self.archive_id == request.archive_id
                    })
            }
        }
    }
}

fn project(bytes: &[u8], request: &Request, name: &str) -> Result<Value> {
    let old: Receipt = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
    if old.request_id != request.request_id
        || old.name != name
        || old.run_id != request.expected_run_id
        || old.conversation_id != request.expected_conversation_id
        || old.archive_id != request.archive_id
        || old.request_hash != request_hash(request, name)
        || !old.at.is_finite()
        || old.at < 0.0
        || !matches!(
            old.status.as_str(),
            "claimed" | "completed" | "failed" | "uncertain"
        )
        || (old.status == "completed"
            && !matches!(request.action.as_str(), "forget" | "remove_queued")
            && old.result_target.is_none())
        || old.error.as_ref().is_some_and(|error| error.len() > 8192)
        || old
            .error_source
            .as_deref()
            .is_some_and(|source| source != "opencode")
        || old
            .result_target
            .as_ref()
            .is_some_and(|target| old.status != "completed" || !target.valid(request, name))
    {
        return Err(INVALID.into());
    }
    let mut answer = json!({"request_id":old.request_id,"name":old.name,"run_id":old.run_id,
        "conversation_id":old.conversation_id,"status":if old.status=="claimed" {"uncertain"} else {&old.status},"at":old.at});
    if let Some(id) = old.archive_id {
        answer["archive_id"] = json!(id);
    }
    if let Some(target) = old.result_target {
        answer["result_target"] = json!(target);
    }
    // Private native diagnostics are not part of this observation protocol.
    Ok(answer)
}

fn unavailable(request: &Request, name: &str) -> Value {
    let mut answer = json!({"request_id":request.request_id,"name":name,
        "run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id,
        "status":"unavailable"});
    if let Some(id) = &request.archive_id {
        answer["archive_id"] = json!(id);
    }
    answer
}

fn missing(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(INVALID.into()),
        Ok(_) => Ok(false),
    }
}

fn observe(root: &Path, request: &Request, name: &str) -> Result<Value> {
    validate(request, name)?;
    // No absolute_root(): that writer helper creates a missing state root.
    if !root.is_absolute() {
        return Err(INVALID.into());
    }
    if missing(root)? {
        return Ok(unavailable(request, name));
    }
    let state = Directory::open(root).map_err(|_| INVALID)?;
    if !state.private().map_err(|_| INVALID)? {
        return Err(INVALID.into());
    }
    let directory = root.join("action_receipts");
    let leaf = format!("{}.json", request.request_id);
    let Some(bytes) = read_existing(&directory, &leaf, LIMIT)? else {
        return Ok(unavailable(request, name));
    };
    state.verify().map_err(|_| INVALID)?;
    project(&bytes, request, name)
}

fn read_existing(directory: &Path, leaf: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    if !directory.is_absolute() {
        return Err(INVALID.into());
    }
    if missing(directory)? {
        return Ok(None);
    }
    let pinned = Directory::open(directory).map_err(|_| INVALID)?;
    if !pinned.private().map_err(|_| INVALID)? {
        return Err(INVALID.into());
    }
    if missing(&directory.join(leaf))? {
        return Ok(None);
    }
    let file = pinned.read_private(leaf).map_err(|_| INVALID)?;
    if file.metadata().map_err(|_| INVALID)?.len() > limit {
        return Err(INVALID.into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 > limit {
        return Err(INVALID.into());
    }
    Ok(Some(bytes))
}

/// Unlike local_node_id, this guard never initializes, locks or saves Swarm.
/// The catalog is the same existing identity source, not imported evidence.
pub(crate) fn machine(config: &crate::config::Config) -> Result<String> {
    let bytes = read_existing(&config.dir.join("swarm"), "catalog.json", 128 * 1024 * 1024)?
        .ok_or(INVALID)?;
    crate::swarm::existing_node_id(&bytes).map_err(|_| INVALID.into())
}

pub(in crate::state) fn dispatch(args: &[String]) -> Result<i32> {
    let request = request_input(args, "session-action-status")?;
    println!("{}", observe(&root(), &request, &args[0])?);
    Ok(0)
}

#[cfg(test)]
mod tests;
