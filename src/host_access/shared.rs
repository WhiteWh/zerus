//! Shared project history and memory (phase 5): a typed, read-only projection
//! that members of one project publish explicitly. It is not a view of native
//! session stores. A publication holds only what its author approved:
//! messages (role and text), a title and a provenance digest. It never holds
//! an agent home, account or credential material, native conversation or run
//! IDs, or any other record that could resume the author's session. The
//! strict schemas below refuse every other field.
//!
//! Layout under the operator's shared root (provisioned with exact ACLs by the
//! privileged launcher; Zerus never widens them):
//!
//! ```text
//! shared/<project>/<person>/index.jsonl          append-only feed
//! shared/<project>/<person>/history/<id>.json    publications
//! shared/<project>/<person>/memory/<id>.json     memory notes
//! shared/<project>/<person>/receipts/<rid>.json  request receipts
//! ```
//!
//! Each person writes only their own partition, which must belong to their OS
//! identity; the policy's project membership is checked before every read,
//! list, write and handoff, whatever the file ACLs allow, so revocation takes
//! effect on the next call. Feeds are read in bounded pages from byte offsets;
//! a cursor is bound to the project, kind and policy revision.
//!
//! Personal memory and swarms are never merged in: a memory note exists only
//! because its author added that text to this project.
use super::gate::Refusal;
use super::runtime::{owned_by, OsIdentity, Principal};
use super::Policy;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Read, Seek, Write};
use std::path::{Path, PathBuf};

pub const PUBLICATION_SCHEMA: &str = "zerus-shared-history-v1";
pub const MEMORY_SCHEMA: &str = "zerus-shared-memory-v1";
pub const HANDOFF_SCHEMA: &str = "zerus-handoff-v1";
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 2000;
const MAX_TEXT: usize = 64 * 1024;
const MAX_MEMORY_TEXT: usize = 16 * 1024;
const MAX_TITLE: usize = 200;
const MAX_LINE: usize = 2048;
const MAX_PAGE: usize = 100;
const MAX_PARTITIONS: usize = 512;
const MAX_FILE: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    History,
    Memory,
}
impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::Memory => "memory",
        }
    }
}

/// One approved message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub request_id: String,
    pub project: String,
    /// Republishing names its ID and the generation it replaces.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub expected_generation: Option<u64>,
    pub title: String,
    /// The caller's own session, for provenance only.
    pub session: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRequest {
    pub request_id: String,
    pub project: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub expected_generation: Option<u64>,
    pub title: String,
    pub note_kind: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    pub project: String,
    pub kind: Kind,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub project: String,
    pub kind: Kind,
    pub person: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinueRequest {
    pub request_id: String,
    pub project: String,
    pub person: String,
    pub id: String,
    pub generation: u64,
}

/// A stored publication.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    pub schema: String,
    pub project: String,
    pub person: String,
    pub id: String,
    pub generation: u64,
    pub title: String,
    pub published_at: u64,
    pub source_digest: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryNote {
    pub schema: String,
    pub project: String,
    pub person: String,
    pub id: String,
    pub generation: u64,
    pub title: String,
    pub note_kind: String,
    pub published_at: u64,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct IndexLine {
    seq: u64,
    kind: Kind,
    id: String,
    generation: u64,
    title: String,
    at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    request_id: String,
    digest: String,
    result: Value,
}

/// The store: the shared root and the caller's state folder.
pub struct Store<'a> {
    pub shared_root: PathBuf,
    pub state_root: PathBuf,
    pub policy: &'a Policy,
    pub principal: &'a Principal,
    pub now: u64,
}

fn refuse(code: &'static str, detail: &str) -> Refusal {
    Refusal::new(code, detail)
}

fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.hyphenated().to_string() == value)
}

fn publication_id(value: &str) -> bool {
    uuid(value)
}

/// The shared contract decision (`person::authorize_person`, mirrored by the
/// Go executor's `authorizePerson`), as a typed refusal.
fn authorize(policy: &Policy, person: &str, operation: &str, project: &str) -> Result<(), Refusal> {
    super::person::authorize_person(policy, person, operation, project).map(|_| ()).map_err(|code| {
        let detail = match code {
            "person-revoked" => "this person's access to the host is revoked",
            "project-unknown" => "no such shared project",
            "not-a-member" => "you are not a member of this project",
            "role-insufficient" => "project readers cannot publish, add memory or continue",
            "person-unknown" => "this person is not in the host's policy",
            _ => "the operation is not authorized",
        };
        refuse(code, detail)
    })
}

fn read_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::History => "history.read",
        Kind::Memory => "memory.read",
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, Refusal> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(refuse("store-unreadable", "the shared store cannot be read")),
        Ok(meta) if !meta.is_file() || meta.len() > MAX_FILE => {
            return Err(refuse("store-corrupt", "a shared record is not a bounded file"))
        }
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_FILE + 1).read_to_end(&mut bytes))
        .map_err(|_| refuse("store-unreadable", "the shared store cannot be read"))?;
    Ok(Some(bytes))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Refusal> {
    let write = || -> std::io::Result<()> {
        let dir = path.parent().expect("record has a parent");
        std::fs::create_dir_all(dir)?;
        let temp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    };
    write().map_err(|_| refuse("store-write", "the shared store could not be written"))
}

impl<'a> Store<'a> {
    fn project_dir(&self, project: &str) -> Result<PathBuf, Refusal> {
        if !super::name(project) {
            return Err(refuse("project-unknown", "no such shared project"));
        }
        Ok(self.shared_root.join(project))
    }

    fn partition(&self, project: &str, person: &str) -> Result<PathBuf, Refusal> {
        if !super::host_id(person) {
            return Err(refuse("person-unknown", "no such person"));
        }
        Ok(self.project_dir(project)?.join(person))
    }

    /// The caller's own partition, which the launcher provisioned for their
    /// OS identity. Zerus never creates or re-owns it.
    fn own_partition(&self, project: &str) -> Result<PathBuf, Refusal> {
        let dir = self.partition(project, &self.principal.person)?;
        let meta = std::fs::symlink_metadata(&dir)
            .map_err(|_| refuse("partition-missing", "the operator has not provisioned your partition of this project"))?;
        if !meta.is_dir() || !owned_by(&dir, &self.principal.os) {
            return Err(refuse("partition-owner", "your partition of this project is not owned by your OS account"));
        }
        Ok(dir)
    }

    fn allowed(&self, operation: &str, project: &str) -> Result<(), Refusal> {
        authorize(self.policy, &self.principal.person, operation, project)
    }

    fn lock(dir: &Path) -> Result<std::fs::File, Refusal> {
        use fs2::FileExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))
            .map_err(|_| refuse("store-write", "the shared store could not be locked"))?;
        file.lock_exclusive().map_err(|_| refuse("store-write", "the shared store could not be locked"))?;
        Ok(file)
    }

    /// Runs `effect` once per request ID: the same request returns its first
    /// result; a different body under the same ID is a conflict.
    fn once(&self, dir: &Path, request_id: &str, body: &[u8], effect: impl FnOnce() -> Result<Value, Refusal>) -> Result<Value, Refusal> {
        if !uuid(request_id) {
            return Err(refuse("request-id", "request_id must be a lowercase UUID"));
        }
        let path = dir.join("receipts").join(format!("{request_id}.json"));
        let body_digest = digest(body);
        if let Some(bytes) = read_bounded(&path)? {
            let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| refuse("store-corrupt", "a receipt is malformed"))?;
            if receipt.digest != body_digest {
                return Err(refuse("request-conflict", "this request ID was used for a different request"));
            }
            return Ok(receipt.result);
        }
        let result = effect()?;
        let receipt = Receipt { request_id: request_id.into(), digest: body_digest, result: result.clone() };
        write_atomic(&path, &serde_json::to_vec(&receipt).expect("receipt serializes"))?;
        Ok(result)
    }

    fn next_seq(dir: &Path) -> Result<u64, Refusal> {
        let path = dir.join("seq");
        let current = match read_bounded(&path)? {
            Some(bytes) => std::str::from_utf8(&bytes).ok().and_then(|s| s.trim().parse::<u64>().ok())
                .ok_or_else(|| refuse("store-corrupt", "the feed sequence is malformed"))?,
            None => 0,
        };
        write_atomic(&path, (current + 1).to_string().as_bytes())?;
        Ok(current + 1)
    }

    fn append_index(dir: &Path, line: &IndexLine) -> Result<(), Refusal> {
        let mut text = serde_json::to_string(line).expect("index line serializes");
        text.push('\n');
        if text.len() > MAX_LINE {
            return Err(refuse("too-large", "a feed line exceeds its bound"));
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("index.jsonl"))
            .map_err(|_| refuse("store-write", "the feed could not be written"))?;
        file.write_all(text.as_bytes()).and_then(|()| file.sync_all())
            .map_err(|_| refuse("store-write", "the feed could not be written"))
    }

    /// Generation compare-and-swap for a republish.
    fn next_generation(&self, path: &Path, id_given: bool, expected: Option<u64>) -> Result<u64, Refusal> {
        let current = match read_bounded(path)? {
            None if id_given => return Err(refuse("not-found", "no such record to update")),
            None => return Ok(1),
            Some(bytes) => serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|value| value["generation"].as_u64())
                .ok_or_else(|| refuse("store-corrupt", "a shared record is malformed"))?,
        };
        if expected != Some(current) {
            return Err(refuse("conflict", "the record changed since you read it; reload and retry"));
        }
        Ok(current + 1)
    }

    fn source_digest(&self, session_id: &str) -> String {
        digest(format!("zerus-share-source-v1\n{}\n{}\n{session_id}", self.policy.host, self.principal.person).as_bytes())
    }

    /// Publishes approved messages of the caller's own session `session_id`
    /// (already resolved in the caller's state folder).
    pub fn publish(&self, request: &PublishRequest, body: &[u8], session_id: &str) -> Result<Value, Refusal> {
        self.allowed("history.publish", &request.project)?;
        validate_title(&request.title)?;
        if request.entries.is_empty() || request.entries.len() > MAX_ENTRIES {
            return Err(refuse("too-large", "a publication needs 1 to 2000 entries"));
        }
        for entry in &request.entries {
            if !matches!(entry.role.as_str(), "person" | "agent" | "note") || entry.text.is_empty() || entry.text.len() > MAX_TEXT {
                return Err(refuse("entry-invalid", "entries are person, agent or note text up to 64 KiB"));
            }
        }
        let dir = self.own_partition(&request.project)?;
        let _lock = Self::lock(&dir)?;
        self.once(&dir, &request.request_id, body, || {
            let id = match &request.id {
                Some(id) if publication_id(id) => id.clone(),
                Some(_) => return Err(refuse("id-invalid", "a publication ID is a lowercase UUID")),
                None => uuid::Uuid::new_v4().hyphenated().to_string(),
            };
            let path = dir.join("history").join(format!("{id}.json"));
            let generation = self.next_generation(&path, request.id.is_some(), request.expected_generation)?;
            let publication = Publication {
                schema: PUBLICATION_SCHEMA.into(),
                project: request.project.clone(),
                person: self.principal.person.clone(),
                id: id.clone(),
                generation,
                title: request.title.clone(),
                published_at: self.now,
                source_digest: self.source_digest(session_id),
                entries: request.entries.clone(),
            };
            write_atomic(&path, &serde_json::to_vec(&publication).expect("publication serializes"))?;
            let seq = Self::next_seq(&dir)?;
            Self::append_index(&dir, &IndexLine { seq, kind: Kind::History, id: id.clone(), generation, title: request.title.clone(), at: self.now })?;
            Ok(json!({"project":request.project,"person":self.principal.person,"id":id,"generation":generation}))
        })
    }

    /// Adds or updates one memory note of the caller in this project.
    pub fn add_memory(&self, request: &MemoryRequest, body: &[u8]) -> Result<Value, Refusal> {
        self.allowed("memory.write", &request.project)?;
        validate_title(&request.title)?;
        if !matches!(request.note_kind.as_str(), "decision" | "fact" | "recipe" | "note") {
            return Err(refuse("entry-invalid", "a memory note is a decision, fact, recipe or note"));
        }
        if request.text.is_empty() || request.text.len() > MAX_MEMORY_TEXT {
            return Err(refuse("entry-invalid", "a memory note holds 1 byte to 16 KiB of text"));
        }
        let dir = self.own_partition(&request.project)?;
        let _lock = Self::lock(&dir)?;
        self.once(&dir, &request.request_id, body, || {
            let id = match &request.id {
                Some(id) if publication_id(id) => id.clone(),
                Some(_) => return Err(refuse("id-invalid", "a memory ID is a lowercase UUID")),
                None => uuid::Uuid::new_v4().hyphenated().to_string(),
            };
            let path = dir.join("memory").join(format!("{id}.json"));
            let generation = self.next_generation(&path, request.id.is_some(), request.expected_generation)?;
            let note = MemoryNote {
                schema: MEMORY_SCHEMA.into(),
                project: request.project.clone(),
                person: self.principal.person.clone(),
                id: id.clone(),
                generation,
                title: request.title.clone(),
                note_kind: request.note_kind.clone(),
                published_at: self.now,
                text: request.text.clone(),
            };
            write_atomic(&path, &serde_json::to_vec(&note).expect("note serializes"))?;
            let seq = Self::next_seq(&dir)?;
            Self::append_index(&dir, &IndexLine { seq, kind: Kind::Memory, id: id.clone(), generation, title: request.title.clone(), at: self.now })?;
            Ok(json!({"project":request.project,"person":self.principal.person,"id":id,"generation":generation}))
        })
    }

    /// One bounded page of a project's feed of `kind`, across its partitions.
    pub fn list(&self, request: &ListRequest) -> Result<Value, Refusal> {
        self.allowed(read_operation(request.kind), &request.project)?;
        let limit = request.limit.unwrap_or(50).clamp(1, MAX_PAGE);
        let mut offsets = match &request.cursor {
            None => std::collections::BTreeMap::new(),
            Some(cursor) => decode_cursor(cursor, self.policy.revision, &request.project, request.kind)?,
        };
        let project_dir = self.project_dir(&request.project)?;
        let mut people = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&project_dir) {
            for entry in entries.flatten().take(MAX_PARTITIONS) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if super::host_id(&name) {
                    people.push(name);
                }
            }
        }
        people.sort();
        let mut items = Vec::new();
        let labels = &self.policy.labels;
        'people: for person in &people {
            let path = project_dir.join(person).join("index.jsonl");
            let Ok(mut file) = std::fs::File::open(&path) else { continue };
            let offset = offsets.get(person).copied().unwrap_or(0);
            if file.seek(std::io::SeekFrom::Start(offset)).is_err() {
                continue;
            }
            let mut reader = std::io::BufReader::new(file);
            let mut position = offset;
            loop {
                if items.len() >= limit {
                    offsets.insert(person.clone(), position);
                    break 'people;
                }
                let mut line = Vec::new();
                let read = reader.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line)
                    .map_err(|_| refuse("store-unreadable", "the feed cannot be read"))?;
                if read == 0 || !line.ends_with(b"\n") {
                    // End of feed, or a line still being appended.
                    break;
                }
                position += read as u64;
                let Ok(entry) = serde_json::from_slice::<IndexLine>(&line) else {
                    return Err(refuse("store-corrupt", "a feed line is malformed"));
                };
                if entry.kind == request.kind {
                    items.push(json!({
                        "person":person,"label":labels.get(person),"id":entry.id,
                        "generation":entry.generation,"title":entry.title,"at":entry.at,"seq":entry.seq,
                    }));
                }
            }
            offsets.insert(person.clone(), position);
        }
        let cursor = encode_cursor(self.policy.revision, &request.project, request.kind, &offsets);
        Ok(json!({"project":request.project,"kind":request.kind,"items":items,"cursor":cursor,
            "policy_revision":self.policy.revision}))
    }

    /// Reads one record after checking membership and that its body belongs
    /// to its partition.
    pub fn read(&self, request: &ReadRequest) -> Result<Value, Refusal> {
        self.allowed(read_operation(request.kind), &request.project)?;
        if !publication_id(&request.id) {
            return Err(refuse("id-invalid", "a record ID is a lowercase UUID"));
        }
        let path = self.partition(&request.project, &request.person)?.join(request.kind.dir()).join(format!("{}.json", request.id));
        let bytes = read_bounded(&path)?.ok_or_else(|| refuse("not-found", "no such record"))?;
        let value = match request.kind {
            Kind::History => {
                let record: Publication = serde_json::from_slice(&bytes).map_err(|_| refuse("store-corrupt", "a shared record is malformed"))?;
                if record.schema != PUBLICATION_SCHEMA || record.person != request.person || record.project != request.project || record.id != request.id {
                    return Err(refuse("record-owner-mismatch", "this record does not belong to the partition it is in"));
                }
                serde_json::to_value(record).expect("publication serializes")
            }
            Kind::Memory => {
                let record: MemoryNote = serde_json::from_slice(&bytes).map_err(|_| refuse("store-corrupt", "a shared record is malformed"))?;
                if record.schema != MEMORY_SCHEMA || record.person != request.person || record.project != request.project || record.id != request.id {
                    return Err(refuse("record-owner-mismatch", "this record does not belong to the partition it is in"));
                }
                serde_json::to_value(record).expect("note serializes")
            }
        };
        Ok(json!({"record":value,"label":self.policy.labels.get(&request.person),"read_only":true}))
    }

    /// Continue with my account: a private handoff in the caller's own state
    /// folder. It carries only the published text and provenance; the new
    /// session the desktop starts from it runs under the caller's own account.
    /// The author's session, account and native resume identity are never
    /// named, so this can never become a resume, fork or send on the author's
    /// grant. The same request ID returns the same handoff.
    pub fn continue_with_own_account(&self, request: &ContinueRequest, body: &[u8]) -> Result<Value, Refusal> {
        self.allowed("handoff.continue", &request.project)?;
        let read = self.read(&ReadRequest {
            project: request.project.clone(),
            kind: Kind::History,
            person: request.person.clone(),
            id: request.id.clone(),
        })?;
        let record: Publication = serde_json::from_value(read["record"].clone()).expect("read returns a publication");
        if record.generation != request.generation {
            return Err(refuse("conflict", "the publication changed since you read it; reload and retry"));
        }
        let dir = self.state_root.join("host-access").join("handoffs");
        std::fs::create_dir_all(&dir).map_err(|_| refuse("store-write", "the handoff could not be saved"))?;
        let _lock = Self::lock(&dir)?;
        self.once(&dir, &request.request_id, body, || {
            let handoff = json!({
                "schema":HANDOFF_SCHEMA,
                "handoff_id":request.request_id,
                "project":record.project,
                "from":{"person":record.person,"label":self.policy.labels.get(&record.person),"id":record.id,"generation":record.generation},
                "owner":{"person":self.principal.person,"context":self.principal.context},
                "account":"own",
                "title":record.title,
                "entries":record.entries,
                "created_at":self.now,
            });
            let path = dir.join(format!("{}.handoff.json", request.request_id));
            write_atomic(&path, &serde_json::to_vec(&handoff).expect("handoff serializes"))?;
            Ok(json!({"handoff_id":request.request_id,"path":path,"account":"own","project":record.project}))
        })
    }
}

fn validate_title(title: &str) -> Result<(), Refusal> {
    let count = title.chars().count();
    if count == 0 || count > MAX_TITLE || title.chars().any(char::is_control) {
        return Err(refuse("title-invalid", "a title has 1 to 200 characters and no control characters"));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    revision: u64,
    project: String,
    kind: Kind,
    offsets: std::collections::BTreeMap<String, u64>,
}

fn encode_cursor(revision: u64, project: &str, kind: Kind, offsets: &std::collections::BTreeMap<String, u64>) -> String {
    let cursor = Cursor { revision, project: project.into(), kind, offsets: offsets.clone() };
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cursor).expect("cursor serializes"))
}

/// A cursor is only a position: every page re-checks membership. It is bound
/// to the policy revision so a page never mixes two permission states.
fn decode_cursor(text: &str, revision: u64, project: &str, kind: Kind) -> Result<std::collections::BTreeMap<String, u64>, Refusal> {
    let bad = || refuse("cursor-invalid", "the cursor is malformed");
    if text.len() > 64 * 1024 {
        return Err(bad());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text).map_err(|_| bad())?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    if cursor.project != project || cursor.kind != kind || cursor.offsets.len() > MAX_PARTITIONS
        || cursor.offsets.keys().any(|person| !super::host_id(person))
    {
        return Err(bad());
    }
    if cursor.revision != revision {
        return Err(refuse("cursor-stale", "the project's access changed; start the listing again"));
    }
    Ok(cursor.offsets)
}

/// Whether an OS identity may write a partition (exposed for diagnostics).
pub fn partition_writable_by(dir: &Path, os: &OsIdentity) -> bool {
    owned_by(dir, os)
}
