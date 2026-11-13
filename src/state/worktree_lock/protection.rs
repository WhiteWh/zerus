//! The veto retained before a new ID-layout record is published. These
//! objects cannot authorize removal or a signal. The caller holds the state
//! writer; repository admission is a nonblocking try in that lock order.
use super::*;
use crate::state::repositories::directory::Directory;
use fs2::FileExt;
use std::io::{Seek, SeekFrom, Write};

mod legacy_veto;

#[cfg(all(test, unix))]
thread_local! {
    pub(super) static BEFORE_LEGACY_RETENTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const {std::cell::RefCell::new(None)};
}

pub(in crate::state) struct Protection {
    owner: Value,
    state: Directory,
    receipts: Directory,
    common: Directory,
    checkout: Option<Directory>,
    metadata: Directory,
    _repository: crate::platform::FileLock,
    receipt: String,
    before: Value,
    lock: fs::File,
    reason: String,
}

/// Retain the provider's original exclusion through the whole reason transfer,
/// not just its negative metadata write. A changed name is never a new lock.
struct ProviderLock {
    directories: Vec<Directory>,
    file: crate::platform::FileLock,
}
impl ProviderLock {
    fn verify(&self) -> Result<()> {
        for directory in &self.directories {
            directory.verify()?;
        }
        let current = self.directories[0].read_private("bindings.lock")?;
        if object(&current)? != exclusion_object(&self.file)? {
            return Err("The shared holder's original exclusion file changed".into());
        }
        Ok(())
    }
}

fn object(file: &fs::File) -> Result<Value> {
    let id = crate::platform::FileIdentity::of(file).map_err(|e| e.to_string())?;
    Ok(json!([id.device, id.inode]))
}

fn exclusion_object(lock: &crate::platform::FileLock) -> Result<Value> {
    let id = lock.identity().map_err(|e| e.to_string())?;
    Ok(json!([id.device, id.inode]))
}

fn state() -> Result<Directory> {
    Directory::open(&crate::platform::canonical(root()).map_err(|e| e.to_string())?)
}

fn receipt(owner: &Value) -> String {
    format!("{}.json", storage::label_key(string(owner, "id")))
}

fn read_receipt(directory: &Directory, name: &str) -> Result<Option<Value>> {
    directory.verify()?;
    match fs::symlink_metadata(directory.path.join(name)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            directory.verify()?;
            return Ok(None);
        }
        Err(e) => return Err(e.to_string()),
        Ok(_) => (),
    }
    let mut bytes = Vec::new();
    directory
        .read_private(name)?
        .take(16385)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    directory.verify()?;
    if bytes.len() > 16384 {
        return Err("The protection receipt exceeds its bound".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn write_receipt(directory: &Directory, name: &str, value: &Value, replace: bool) -> Result<()> {
    directory.verify()?;
    storage::cut_write()?;
    let bytes = value.to_string();
    if replace {
        directory.replace_file(name, bytes.as_bytes())?;
    } else {
        directory.write_new(name, bytes.as_bytes())?;
    }
    directory.verify()
}

fn owner_matches(owner: &Value) -> bool {
    let path = Path::new(string(owner, "path"));
    let common = Path::new(string(owner, "common_dir"));
    owner["version"] == 1
        && path.is_absolute()
        && common.is_absolute()
        && session_id::canonical(string(owner, "id"))
        && worktree_lifecycle::owned(common, path).as_ref() == Some(owner)
}

#[cfg(unix)]
fn mark_for_move(record: &Value, owner: &Value) -> Result<Value> {
    if record
        .get("worktree_protection")
        .is_some_and(|marker| marker != &owner["id"])
    {
        return Err("The original legacy protection marker is inconsistent".into());
    }
    let mut marked = record.clone();
    marked["worktree_protection"] = owner["id"].clone();
    if marked.to_string().len() as u64 > storage::ID_RECORD_BYTES {
        return Err("The protected legacy binding exceeds its bound".into());
    }
    Ok(marked)
}

fn open_lock(metadata: &Directory) -> Result<fs::File> {
    let file = crate::platform::no_follow(fs::OpenOptions::new().read(true).write(true))
        .open(metadata.path.join("locked"))
        .map_err(|e| e.to_string())?;
    // A hard link could mutate an unrelated object through this descriptor.
    if !crate::platform::verify_private_file(&file).map_err(|e| e.to_string())? {
        return Err("The protective lock is not a private regular file".into());
    }
    Ok(file)
}

fn text(file: &mut fs::File) -> Result<String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(LOCK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LOCK_BYTES {
        return Err("The protective lock exceeds its bound".into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

impl Protection {
    /// Existing immutable protection only. Transaction recovery verifies its
    /// own original before-image; this reader never rewrites a legacy record
    /// or creates a missing proof/lock after the record left its old name.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub(in crate::state) fn retained_for_move(record: &Value) -> Result<Option<Self>> {
        if record["worktree_owner"].is_null() {return Ok(None);}
        let owner=&record["worktree_owner"];
        if record["worktree_protection"]!=owner["id"] {
            return Err("The original transition protection marker is missing".into());
        }
        Self::retained_any(owner,None,false).map(Some)
    }

    /// Dormant metadata-move preparation, under the original state writer.
    /// Unlike new-session publication, this NEVER creates a Git lock: an
    /// existing legacy holder must retain its verified original veto object.
    /// A negative marker is durable before the receipt or an ID record, so a
    /// missing/interrupted receipt cannot restore text-only release authority.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub(in crate::state) fn for_legacy_move(record: &mut Value) -> Result<Option<Self>> {
        if record["worktree_owner"].is_null() {
            return Ok(None);
        }
        let before = record.clone();
        let owner = &before["worktree_owner"];
        let id = string(&before, "session_id");
        let name = string(&before, "name");
        if storage::in_id_layout(&before)
            || before["version"] != 1
            || !AGENTS.contains(&string(&before, "agent"))
            || !name.starts_with(&format!("{}/", string(&before, "agent")))
            || !session_id::canonical(id)
            || before.get("legacy_shadow").is_some()
            || before["rename_shadow"] == true
            || !owner_matches(owner)
            || !Path::new(string(&before, "launch_dir"))
                .starts_with(Path::new(string(owner, "path")))
        {
            return Err("The original legacy checkout binding cannot be moved".into());
        }
        let state = state()?;
        if !state.private()? {
            return Err("The original legacy state root is not private".into());
        }
        // Routed/shadow chains need their own complete before-images in the
        // record transaction. Never silently leave a name-based route behind.
        match fs::symlink_metadata(storage::route_path(name)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            _ => {
                return Err("The legacy rename route requires explicit transition recovery".into())
            }
        }
        let binding = format!("{}.json", storage::label_key(name));
        let read_binding = || -> Result<Value> {
            serde_json::from_slice(&state.read_file(&binding, storage::ID_RECORD_BYTES as usize)?)
                .map_err(|e| e.to_string())
        };
        if read_binding()? != before {
            return Err("The original legacy record changed".into());
        }
        let receipts = state.ensure_child("worktree-protections")?;
        let receipt = receipt(owner);
        if let Some(proof) = read_receipt(&receipts, &receipt)? {
            if proof["version"] == legacy_veto::VERSION
                && before["worktree_protection"] != owner["id"]
            {
                return Err(
                    "The original legacy retention marker is missing; retain it for review".into(),
                );
            }
            // Use ONLY the existing-proof reader, never the create-capable
            // new-publication path (even if a receipt changes after our read).
            let held = Self::retained_any(owner, None, false)?;
            if held.before["version"]==legacy_veto::VERSION && before["worktree_protection"]!=owner["id"] {
                return Err("The original legacy retention marker was lost; keep it for review".into());
            }
            if !worktree_cleanup::bindings()?.iter().any(|holder| {
                holder.owner == string(owner, "id") && holder.session_id == held.reason
            }) {
                return Err("The original protective veto has no verified holder".into());
            }
            let marked = mark_for_move(&before, owner)?;
            if marked != before {
                held.verify()?;
                state.verify()?;
                if read_binding()? != before {
                    return Err("The legacy record changed before retention".into());
                }
                storage::cut_write()?;
                state.replace_file(&binding, marked.to_string().as_bytes())?;
            }
            held.verify()?;
            if read_binding()? != marked {
                return Err("Legacy retention publication is unconfirmed".into());
            }
            *record = marked;
            return Ok(Some(held));
        }
        if before.get("worktree_protection").is_some() {
            return Err(
                "The original legacy protection receipt is missing; retain it for review".into(),
            );
        }
        let common = Directory::open(Path::new(string(owner, "common_dir")))?;
        let checkout = Directory::open(Path::new(string(owner, "path")))?;
        let metadata = Directory::open(Path::new(string(owner, "metadata")))?;
        let repository = worktree_cleanup::repository_lock(&common.path)?;
        repository
            .try_lock_exclusive()
            .map_err(|_| "The original checkout is busy")?;
        let repository = crate::platform::FileLock::held(repository);
        #[cfg(all(test, unix))]
        crate::platform::lock_tests::reached("protection-move");
        let mut lock = legacy_veto::open(&metadata)?;
        let lock_object = object(&lock)?;
        let lock_version = legacy_veto::version(&lock)?;
        if text(&mut lock)? != format!("zerus:{id}\n") {
            return Err("The existing Git lock is not this legacy holder's original veto".into());
        }
        let proof = json!({"version":legacy_veto::VERSION,"kind":legacy_veto::KIND,"status":"ready","owner":owner,"state_object":state.object()?,
            "receipt_object":receipts.object()?,"common_object":common.object()?,
            "checkout_object":checkout.object()?,"metadata_object":metadata.object()?,"lock_object":lock_object,
            "lock_version":lock_version,"lock_mode":lock_version[2]});
        let verify = || -> Result<()> {
            state.verify()?;
            receipts.verify()?;
            common.verify()?;
            checkout.verify()?;
            metadata.verify()?;
            if crate::platform::canonical(root()).map_err(|e| e.to_string())? != state.path
                || !owner_matches(owner)
            {
                return Err("The original legacy checkout scope changed".into());
            }
            let mut current = legacy_veto::open(&metadata)?;
            if object(&current)? != lock_object
                || legacy_veto::version(&current)? != lock_version
                || text(&mut current)? != format!("zerus:{id}\n")
            {
                return Err("The original legacy Git lock changed".into());
            }
            Ok(())
        };
        #[cfg(all(test, unix))]
        if let Some(hook) = BEFORE_LEGACY_RETENTION.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        verify()?;
        let marked = mark_for_move(&before, owner)?;
        if read_binding()? != before {
            return Err("The legacy record changed before retention".into());
        }
        storage::cut_write()?;
        state.replace_file(&binding, marked.to_string().as_bytes())?;
        verify()?;
        if read_binding()? != marked {
            return Err("Legacy retention publication is unconfirmed".into());
        }
        write_receipt(&receipts, &receipt, &proof, false)?;
        let held = Self {
            owner: owner.clone(),
            state,
            receipts,
            common,
            checkout: Some(checkout),
            metadata,
            _repository: repository,
            receipt,
            before: proof,
            lock,
            reason: id.to_owned(),
        };
        held.verify()?;
        *record = marked;
        Ok(Some(held))
    }

    /// Existing protection only. A missing lock with a retained Ready receipt
    /// is a person's unlock or an interrupted action, never a relock request.
    fn retained(owner: &Value, expected: Option<&str>, allow_missing: bool) -> Result<Self> {
        if !owner_matches(owner) {
            return Err("The original checkout ownership changed".into());
        }
        let common = Directory::open(Path::new(string(owner, "common_dir")))?;
        let checkout = match Directory::open(Path::new(string(owner, "path"))) {
            Ok(checkout) => Some(checkout),
            Err(_)
                if allow_missing
                    && fs::symlink_metadata(string(owner, "path"))
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let metadata = Directory::open(Path::new(string(owner, "metadata")))?;
        let repository = worktree_cleanup::repository_lock(&common.path)?;
        repository
            .try_lock_exclusive()
            .map_err(|_| "The worktree is busy; nothing was published")?;
        let repository = crate::platform::FileLock::held(repository);
        #[cfg(all(test, unix))]
        crate::platform::lock_tests::reached("protection-retained");
        let state = state()?;
        let receipts = state.child("worktree-protections")?;
        let receipt = receipt(owner);
        let before = read_receipt(&receipts, &receipt)?
            .ok_or("An existing Git lock is not Zerus publication protection")?;
        if before["version"] != 1
            || before["status"] != "ready"
            || before["owner"] != *owner
            || before["state_object"] != json!(state.object()?)
            || before["receipt_object"] != json!(receipts.object()?)
            || before["common_object"] != json!(common.object()?)
            || match &checkout {
                Some(checkout) => before["checkout_object"] != json!(checkout.object()?),
                None => !before["checkout_object"]
                    .as_array()
                    .is_some_and(|a| a.len() == 2 && a.iter().all(|n| n.as_u64().is_some())),
            }
            || before["metadata_object"] != json!(metadata.object()?)
        {
            return Err("The original protective lock needs manual review".into());
        }
        let mut lock = open_lock(&metadata)?;
        if before["lock_object"] != object(&lock)? {
            return Err("The protective lock was replaced".into());
        }
        let reason = text(&mut lock)?
            .strip_prefix("zerus:")
            .and_then(|s| s.strip_suffix('\n'))
            .filter(|id| session_id::canonical(id))
            .ok_or("The protective lock changed")?
            .to_owned();
        if expected.is_some_and(|id| id != reason) {
            return Err("The protective lock belongs to another session".into());
        }
        let value = Self {
            owner: owner.clone(),
            state,
            receipts,
            common,
            checkout,
            metadata,
            _repository: repository,
            receipt,
            before,
            lock,
            reason,
        };
        value.verify()?;
        Ok(value)
    }

    /// Called before create-only publication, under the existing writer. No
    /// owner means a custom/plain folder: no Git command or lock is attempted.
    pub(in crate::state) fn acquire(record: &Value) -> Result<Option<Self>> {
        let owner = &record["worktree_owner"];
        if owner.is_null() {
            return Ok(None);
        }
        let id = string(record, "session_id");
        if !session_id::canonical(id)
            || !owner_matches(owner)
            || !Path::new(string(record, "launch_dir"))
                .starts_with(Path::new(string(owner, "path")))
        {
            return Err("Cannot protect the original owned checkout for this session".into());
        }
        let refs = worktree_cleanup::bindings()?;
        let path = Path::new(string(owner, "path"));
        let users: Vec<_> = refs
            .iter()
            .filter(|r| {
                r.owner == string(owner, "id")
                    || r.folders.iter().any(|folder| folder.starts_with(path))
            })
            .collect();
        let state = state()?;
        let receipts = state.ensure_child("worktree-protections")?;
        let receipt = receipt(owner);
        let current = read_receipt(&receipts, &receipt)?;
        if current.as_ref().is_some_and(|r| r["status"] == "ready") {
            let held = Self::retained_any(owner, None, false)?;
            // An orphaned lock from interrupted publication is never adopted.
            if !users
                .iter()
                .any(|r| r.owner == string(owner, "id") && r.session_id == held.reason)
            {
                return Err(
                    "The protective lock has no verified live or saved holder; review it manually"
                        .into(),
                );
            }
            return Ok(Some(held));
        }
        let common = Directory::open(Path::new(string(owner, "common_dir")))?;
        let checkout = Directory::open(path)?;
        let metadata = Directory::open(Path::new(string(owner, "metadata")))?;
        let repository = worktree_cleanup::repository_lock(&common.path)?;
        repository
            .try_lock_exclusive()
            .map_err(|_| "The worktree is busy; nothing was published")?;
        let repository = crate::platform::FileLock::held(repository);
        #[cfg(all(test, unix))]
        crate::platform::lock_tests::reached("protection-new");
        match fs::symlink_metadata(metadata.path.join("locked")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            _ => {
                return Err(
                    "This checkout already has a Git lock; Zerus will not replace it".into(),
                )
            }
        }
        let mut before = json!({"version":1,"status":"preparing","owner":owner,"state_object":state.object()?,
            "receipt_object":receipts.object()?,"common_object":common.object()?,"checkout_object":checkout.object()?,"metadata_object":metadata.object()?});
        match current {
            None => write_receipt(&receipts, &receipt, &before, false)?,
            Some(previous)
                if previous["status"] == "released"
                    && previous["owner"] == *owner
                    && previous["state_object"] == before["state_object"]
                    && previous["receipt_object"] == before["receipt_object"]
                    && previous["common_object"] == before["common_object"]
                    && previous["checkout_object"] == before["checkout_object"]
                    && previous["metadata_object"] == before["metadata_object"] =>
            {
                write_receipt(&receipts, &receipt, &before, true)?
            }
            _ => return Err(
                "An interrupted protection or manual unlock needs review; Zerus will not relock it"
                    .into(),
            ),
        }
        common.verify()?;
        checkout.verify()?;
        metadata.verify()?;
        if !owner_matches(owner) {
            return Err("Checkout ownership changed before protection".into());
        }
        storage::cut_write()?;
        let lock = metadata.write_new_file("locked", format!("zerus:{id}\n").as_bytes())?;
        crate::platform::sync_dir(&metadata.path).map_err(|e| e.to_string())?;
        before["lock_object"] = object(&lock)?;
        before["status"] = json!("ready");
        write_receipt(&receipts, &receipt, &before, true)?;
        let held = Self {
            owner: owner.clone(),
            state,
            receipts,
            common,
            checkout: Some(checkout),
            metadata,
            _repository: repository,
            receipt,
            before,
            lock,
            reason: id.into(),
        };
        held.verify()?;
        Ok(Some(held))
    }

    pub(in crate::state) fn verify(&self) -> Result<()> {
        if self.before["version"] == legacy_veto::VERSION {
            return self.verify_legacy();
        }
        self.verify_v1()
    }

    fn verify_v1(&self) -> Result<()> {
        self.state.verify()?;
        self.receipts.verify()?;
        self.common.verify()?;
        self.checkout_current()?;
        self.metadata.verify()?;
        if crate::platform::canonical(root()).map_err(|e| e.to_string())? != self.state.path {
            return Err("The selected state root changed".into());
        }
        if !owner_matches(&self.owner)
            || read_receipt(&self.receipts, &self.receipt)?.as_ref() != Some(&self.before)
        {
            return Err("Checkout ownership or protection changed".into());
        }
        let mut current = open_lock(&self.metadata)?;
        if object(&current)? != object(&self.lock)?
            || object(&current)? != self.before["lock_object"]
            || text(&mut current)? != format!("zerus:{}\n", self.reason)
        {
            return Err("The original protective lock changed; nothing was published".into());
        }
        Ok(())
    }

    pub(super) fn original_object(&self) -> Result<Value> {
        object(&self.lock)
    }

    fn checkout_current(&self) -> Result<()> {
        match &self.checkout {
            Some(checkout) => checkout.verify(),
            // Release after a person removed the checkout still checks the
            // original metadata/nonce/backlink and complete reference census.
            // This branch is unavailable to publication and cannot relock it.
            None if fs::symlink_metadata(string(&self.owner, "path"))
                .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                Ok(())
            }
            None => Err("The missing checkout was replaced".into()),
        }
    }

    /// Persist negative retention evidence before a legacy holder becomes the
    /// reason of a protected lock. Losing the receipt must not restore that
    /// holder's old text-only release path. The exact census snapshot is used;
    /// a label lookup could select a different same-label session.
    fn mark_legacy(&self, holder: &worktree_cleanup::Reference) -> Result<Option<ProviderLock>> {
        use sha2::{Digest, Sha256};
        let binding = holder
            .binding
            .as_ref()
            .ok_or("The shared holder has no saved binding")?;
        let provider = match binding.directory.as_str() {
            "sessions" | "dsh/sessions" | "opencode/sessions" => return Ok(None),
            "" | "bindings" => None,
            "dsh/bindings" => Some("dsh"),
            "opencode/bindings" => Some("opencode"),
            _ => return Err("The shared holder has an unknown binding namespace".into()),
        };
        let mut directories = Vec::new();
        for component in binding.directory.split('/').filter(|s| !s.is_empty()) {
            let parent = directories.last().unwrap_or(&self.state);
            directories.push(parent.child(component)?);
        }
        let directory = directories.last().unwrap_or(&self.state);
        // Native bindings have their own writer. Never wait in reverse lock
        // order: a contended provider keeps the original veto for review.
        let provider = provider
            .map(|_| {
                let directory = &directories[0];
                let file = directory.open_private_lock("bindings.lock")?;
                file.try_lock_exclusive().map_err(|_| {
                    "The shared native holder is busy; the original lock stays".to_owned()
                })?;
                let file = crate::platform::FileLock::held(file);
                #[cfg(all(test, unix))]
                crate::platform::lock_tests::reached("provider-acquired");
                Ok::<_, String>(file)
            })
            .transpose()?;
        let verify = || -> Result<()> {
            self.verify()?;
            for dir in &directories {
                dir.verify()?;
            }
            if let Some(file) = &provider {
                if object(&directories[0].read_private("bindings.lock")?)? != exclusion_object(file)? {
                    return Err("The shared holder's original exclusion file changed".into());
                }
            }
            Ok(())
        };
        verify()?;
        let mut bytes = Vec::new();
        directory
            .read_private(&binding.file)?
            .take(storage::ID_RECORD_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > storage::ID_RECORD_BYTES
            || format!("{:x}", Sha256::digest(&bytes)) != binding.digest
        {
            return Err("The shared holder changed after the reference census".into());
        }
        let mut record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if storage::in_id_layout(&record)
            || record["worktree_owner"] != self.owner
            || string(&record, "session_id") != holder.session_id
            || string(&record, "name") != holder.name
        {
            return Err("The shared holder's original binding changed".into());
        }
        if let Some(marker) = record.get("worktree_protection") {
            if marker != &self.owner["id"] {
                return Err("The shared holder's protection marker is inconsistent".into());
            }
            verify()?;
            return Ok(provider.map(|file| ProviderLock { directories, file }));
        }
        record["worktree_protection"] = self.owner["id"].clone();
        let encoded = record.to_string();
        if encoded.len() as u64 > storage::ID_RECORD_BYTES {
            return Err("The shared binding exceeds its bound".into());
        }
        verify()?;
        storage::cut_write()?;
        directory.replace_file(&binding.file, encoded.as_bytes())?;
        verify()?;
        Ok(provider.map(|file| ProviderLock { directories, file }))
    }

    /// Keep the same regular Git lock present throughout a shared handoff.
    /// Partial/error writes leave a lock for review, never an unlocked gap.
    pub(super) fn transfer(mut self, holder: &worktree_cleanup::Reference) -> Result<()> {
        if self.before["version"] == legacy_veto::VERSION {
            return Err("The original legacy advisory veto is retained unchanged; shared retirement needs review".into());
        }
        let id = holder.session_id.as_str();
        if !session_id::canonical(id) {
            return Err("A shared holder has no stable session identity".into());
        }
        self.verify()?;
        let provider = self.mark_legacy(holder)?;
        self.verify()?;
        if let Some(provider) = &provider {
            provider.verify()?;
        }
        storage::cut_write()?;
        self.lock
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        self.lock
            .write_all(format!("zerus:{id}\n").as_bytes())
            .map_err(|e| e.to_string())?;
        self.lock.sync_all().map_err(|e| e.to_string())?;
        self.reason = id.into();
        self.verify()?;
        #[cfg(all(test, unix))]
        crate::platform::lock_tests::reached("provider-transfer");
        if let Some(provider) = &provider {
            provider.verify()?;
        }
        Ok(())
    }

    pub(super) fn released(&self) -> Result<()> {
        self.state.verify()?;
        self.receipts.verify()?;
        self.common.verify()?;
        self.checkout_current()?;
        self.metadata.verify()?;
        if crate::platform::canonical(root()).map_err(|e| e.to_string())? != self.state.path {
            return Err("The selected state root changed".into());
        }
        if read_receipt(&self.receipts, &self.receipt)?.as_ref() != Some(&self.before)
            || !fs::symlink_metadata(self.metadata.path.join("locked"))
                .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
            || !owner_matches(&self.owner)
        {
            return Err("The protective release is unconfirmed".into());
        }
        let mut released = self.before.clone();
        released["status"] = json!("released");
        write_receipt(&self.receipts, &self.receipt, &released, true)
    }
}

/// None is the old floor's release-only lock, without publication metadata.
/// Present but changed metadata is never silently treated as that old case.
pub(super) fn existing(record: &Value) -> Result<Option<Protection>> {
    let owner = &record["worktree_owner"];
    let state = state()?;
    state.verify()?;
    let present = match fs::symlink_metadata(state.path.join("worktree-protections")) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.to_string()),
        Ok(_) => read_receipt(&state.child("worktree-protections")?, &receipt(owner))?.is_some(),
    };
    state.verify()?;
    if !present {
        // ID publication and a protected legacy handoff require the original
        // native lock proof. Losing it cannot opt into text-only retirement.
        if storage::in_id_layout(record) || record.get("worktree_protection").is_some() {
            return Err(
                "The original protection receipt is missing; review the retained lock".into(),
            );
        }
        return Ok(None);
    }
    if read_receipt(&state.child("worktree-protections")?, &receipt(owner))?.is_some_and(|proof| {
        proof["version"] == legacy_veto::VERSION && record["worktree_protection"] != owner["id"]
    }) {
        return Err("The original legacy retention marker is missing; keep it for review".into());
    }
    Protection::retained_any(owner, Some(string(record, "session_id")), true).map(Some)
}
