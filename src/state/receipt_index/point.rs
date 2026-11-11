//! A known UUID is already a bounded locator. Only its original proves delivery;
//! this reader never opens SQLite, discovers files, or repairs missing evidence.
use super::*;
use crate::state::repositories::directory::Directory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::state) struct DeliveryQuery {
    request_id: String,
    message_request_id: String,
    expected_machine_id: String,
    expected_session_id: String,
    expected_run_id: String,
    expected_conversation_id: String,
    provider: String,
    native_digest: String,
    #[serde(default)]
    agent_id: String,
}

fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}
fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn provider(value: &str) -> bool {
    matches!(value, "claude" | "codex" | "kimi" | "dsh" | "opencode")
}
impl DeliveryQuery {
    pub(in crate::state) fn validate(&self, name: &str) -> Result<()> {
        if name.is_empty()
            || !bounded_identity(name, 512)
            || !uuid(&self.request_id)
            || !uuid(&self.message_request_id)
            || !uuid(&self.expected_machine_id)
            || !uuid(&self.expected_session_id)
            || self.expected_run_id.is_empty()
            || !bounded_identity(&self.expected_run_id, 128)
            || !bounded_identity(&self.expected_conversation_id, 512)
            || !bounded_identity(&self.agent_id, 512)
            || self.agent_id.contains(['/', '\\'])
            || !provider(&self.provider)
            || !hex(&self.native_digest)
        {
            return Err("Invalid delivery receipt query".into());
        }
        Ok(())
    }
    pub(in crate::state) fn machine(&self) -> &str {
        &self.expected_machine_id
    }
    fn source(&self) -> Source {
        match self.provider.as_str() {
            "dsh" => Source::Dsh,
            "opencode" => Source::OpenCode,
            _ => Source::Terminal,
        }
    }
    fn response(&self, name: &str, reason: &str) -> Value {
        json!({"schema":1,"request_id":self.request_id,"message_request_id":self.message_request_id,
            "name":name,"machine_id":self.expected_machine_id,"session_id":self.expected_session_id,
            "run_id":self.expected_run_id,"conversation_id":self.expected_conversation_id,
            "provider":self.provider,"agent_id":self.agent_id,"native_digest":self.native_digest,
            "status":"unknown","reason":reason})
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeliveryBinding {
    schema: u8,
    machine_id: String,
    session_id: String,
    provider: String,
    namespace: String,
}
impl DeliveryBinding {
    fn valid(&self) -> bool {
        self.schema == 1
            && uuid(&self.machine_id)
            && uuid(&self.session_id)
            && provider(&self.provider)
            && self.namespace.strip_prefix("account:").is_some_and(hex)
    }
}

/// Capture before the first possible offer, never retrofit a replayed receipt.
/// The existing graph target has already bound its native account and machine.
pub(in crate::state) fn delivery_binding(record: &Value, target: &Value) -> Option<Value> {
    let binding = DeliveryBinding {
        schema: 1,
        machine_id: string(&target["endpoint"], "machine").into(),
        session_id: string(record, "session_id").into(),
        provider: string(record, "agent").into(),
        namespace: string(&target["endpoint"], "namespace").into(),
    };
    (binding.valid()
        && target["schema"] == 1
        && target["run_id"] == record["run_id"]
        && target["endpoint"]["provider"] == record["agent"])
        .then(|| json!(binding))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphTarget {
    schema: u8,
    node: String,
    name: String,
    run_id: String,
    endpoint: GraphEndpoint,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphEndpoint {
    machine: String,
    provider: String,
    namespace: String,
    conversation: String,
    provisional_run: String,
}

#[derive(Deserialize)]
struct Original {
    request_id: String,
    name: String,
    run_id: String,
    conversation_id: String,
    digest: String,
    status: String,
    transport: String,
    submitted_at: f64,
    #[serde(default)]
    first_message: bool,
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    queued: bool,
    native_message_id: Option<String>,
    delivery_binding: Option<DeliveryBinding>,
    graph_target: Option<GraphTarget>,
}

fn original(state: &Path, source: Source, id: &str) -> std::result::Result<Original, &'static str> {
    if !state.is_absolute() {
        return Err("source_unavailable");
    }
    let mut directory = Directory::open(state).map_err(|_| "source_unavailable")?;
    if !controlled_directory(&directory)? {
        return Err("source_unavailable");
    }
    for component in source.directory().split('/') {
        directory = directory
            .child(component)
            .map_err(|_| "source_unavailable")?;
        if !controlled_directory(&directory)? {
            return Err("source_unavailable");
        }
    }
    let file = directory
        .read_private_optional(&format!("{id}.json"))
        .map_err(|_| "unreadable")?
        .ok_or("missing")?;
    let (before, metadata) =
        crate::platform::FileIdentity::with_metadata(&file).map_err(|_| "unreadable")?;
    if metadata.len() > ORIGINAL_BYTES {
        return Err("oversized");
    }
    let mut bytes = Vec::new();
    (&file)
        .take(ORIGINAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unreadable")?;
    let (after, end) =
        crate::platform::FileIdentity::with_metadata(&file).map_err(|_| "unreadable")?;
    if before != after || metadata.len() != end.len() || bytes.len() as u64 != end.len() {
        return Err("changed");
    }
    directory.verify().map_err(|_| "source_unavailable")?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "corrupt")?;
    Binding::parse(source, &value, &format!("{id}.json")).map_err(|_| "binding_changed")?;
    // Deserialize from the original bytes as well: typed identity fields,
    // including nested bindings, reject duplicates before a Value can hide them.
    serde_json::from_slice(&bytes).map_err(|_| "invalid_receipt")
}

fn controlled_directory(directory: &Directory) -> std::result::Result<bool, &'static str> {
    // Legacy state roots can be 0755. Owner-only entry mutation and a pinned
    // walk protect the binding; the receipt itself must remain a private file.
    #[cfg(unix)]
    let result = directory.owner_controls_entries();
    #[cfg(windows)]
    let result = directory.private();
    result.map_err(|_| "source_unavailable")
}

/// Historical proof only: no name resolution, current-session read, SQL,
/// provider reconciliation or write. Unknown never grants replay authority.
pub(in crate::state) fn delivery_receipt(state: &Path, name: &str, query: &DeliveryQuery) -> Value {
    let fail = |reason| query.response(name, reason);
    let receipt = match original(state, query.source(), &query.message_request_id) {
        Ok(value) => value,
        Err(reason) => return fail(reason),
    };
    let Some(binding) = receipt.delivery_binding else {
        return fail("binding_unavailable");
    };
    if let Some(target) = &receipt.graph_target {
        let endpoint = &target.endpoint;
        let provisional: &str = if receipt.conversation_id.is_empty() {
            &receipt.run_id
        } else {
            ""
        };
        if target.schema != 1
            || target.name != receipt.name
            || target.run_id != receipt.run_id
            || target.node != format!("node:{}", hash(&json!(endpoint).to_string()))
            || endpoint.machine != binding.machine_id
            || endpoint.provider != binding.provider
            || endpoint.namespace != binding.namespace
            || endpoint.conversation != receipt.conversation_id
            || endpoint.provisional_run != provisional
        {
            return fail("binding_changed");
        }
    }
    if !binding.valid()
        || receipt.request_id != query.message_request_id
        || receipt.name != name
        || receipt.run_id != query.expected_run_id
        || receipt.conversation_id != query.expected_conversation_id
        || receipt.agent_id != query.agent_id
        || binding.machine_id != query.expected_machine_id
        || binding.session_id != query.expected_session_id
        || binding.provider != query.provider
        || (receipt.conversation_id.is_empty() && !receipt.first_message)
    {
        return fail("binding_changed");
    }
    if !hex(&receipt.digest) || receipt.digest != query.native_digest {
        return fail("digest_mismatch");
    }
    let transport = match query.source() {
        Source::Terminal => "terminal",
        Source::Dsh => "native_dsh",
        Source::OpenCode => "native_opencode",
        Source::Question => return fail("unsupported_receipt"),
    };
    if receipt.status != "submitted" || receipt.transport != transport {
        return fail("not_submitted");
    }
    if !receipt.submitted_at.is_finite() || receipt.submitted_at < 0.0 {
        return fail("invalid_receipt");
    }
    if receipt
        .native_message_id
        .as_deref()
        .is_some_and(|id| id.is_empty() || !bounded_identity(id, 256))
    {
        return fail("invalid_receipt");
    }
    let mut proof = query.response(name, "");
    proof.as_object_mut().unwrap().remove("reason");
    proof["status"] = json!("submitted");
    proof["proof"] = json!("original_receipt");
    proof["submitted_at"] = json!(receipt.submitted_at);
    proof["queued"] = json!(receipt.queued);
    if let Some(id) = receipt.native_message_id {
        proof["native_message_id"] = json!(id);
    }
    proof
}
