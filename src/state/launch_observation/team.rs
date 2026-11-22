//! Additive Teams facts using the strict observer's pinned, bounded reads.
//! A label is only a route. A matching launch or run identifies the record.
use super::unix::*;
use super::{TeamObservation, TeamRecord, TeamSession};
use std::{path::Path, time::Instant};

/// Configuration may name the state directory through an ancestor alias
/// (notably macOS /var). Resolve only that trusted root before the existing
/// bounded, pinned no-follow walk. No descendant or record leaf is resolved.
fn configured_root(state: &Path, deadline: Instant) -> Checked<PathBuf> {
    valid_path(state)?;
    if Instant::now() >= deadline {
        return Err(Fault);
    }
    let root = match crate::platform::canonical(state) {
        Ok(root) => root,
        // Preserve the existing bounded absence checks for a missing root.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => state.to_owned(),
        Err(_) => return Err(Fault),
    };
    valid_path(&root)?;
    if Instant::now() >= deadline {
        return Err(Fault);
    }
    Ok(root)
}

struct Reads<'a> {
    counts: &'a mut Counts,
    bytes: ByteBudget,
    files: Vec<(Root, String, Option<Image>)>,
    absent: Vec<PathBuf>,
}
impl<'a> Reads<'a> {
    fn new(counts: &'a mut Counts) -> Self {
        Self {
            counts,
            bytes: ByteBudget::new(),
            files: vec![],
            absent: vec![],
        }
    }
    fn read(&mut self, path: &Path, cap: usize) -> Checked<Option<Value>> {
        valid_path(path)?;
        let parent = path.parent().ok_or(Fault)?;
        if path_stamp(parent, self.counts)?.is_none() {
            self.absent.push(parent.into());
            return Ok(None);
        }
        let root = Root::pin(parent, self.counts)?;
        let leaf = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(Fault)?
            .to_owned();
        let image = capture(&root, &leaf, cap, &mut self.bytes, self.counts)?;
        let value = image.as_ref().map(json).transpose()?;
        self.files.push((root, leaf, image));
        Ok(value)
    }
    fn finish(&mut self) -> Checked<()> {
        let mut bytes = ByteBudget::new();
        for (root, leaf, image) in &self.files {
            recheck(root, leaf, image.as_ref(), &mut bytes, self.counts)?;
            root.finish(self.counts)?;
        }
        for path in &self.absent {
            if path_stamp(path, self.counts)?.is_some() {
                return Err(Fault);
            }
        }
        Ok(())
    }
    fn label(&mut self, state: &Path, label: &str) -> Checked<Option<Value>> {
        if !name(label) {
            return Err(Fault);
        }
        let leaf = format!("{:x}.json", Sha256::digest(label.as_bytes()));
        if let Some(agent @ ("dsh" | "opencode")) = label.split('/').next() {
            return self.read(&state.join(agent).join("bindings").join(&leaf), RAW);
        }
        let route = self.read(&state.join("name_routes").join(&leaf), SMALL)?;
        let path = if route.is_some() {
            state.join("bindings").join(&leaf)
        } else {
            state.join(&leaf)
        };
        self.read(&path, RAW)
    }
    fn by_id(&mut self, state: &Path, id: &str, agent: &str) -> Checked<Option<Value>> {
        if !canonical(id) {
            return Err(Fault);
        }
        let folder = if matches!(agent, "dsh" | "opencode") {
            state.join(agent).join("sessions")
        } else {
            state.join("sessions")
        };
        if let Some(v) = self.read(&folder.join(format!("{id}.json")), RAW)? {
            if v["session_id"] != id || v["layout"] != "id" {
                return Err(Fault);
            }
            return Ok(Some(v));
        }
        let Some(index) = self.read(&state.join("ids").join(format!("{id}.json")), SMALL)? else {
            return Ok(None);
        };
        let value = self.label(state, text(&index, "name")?)?;
        if value.as_ref().is_some_and(|v| v["session_id"] != id) {
            return Err(Fault);
        }
        Ok(value)
    }
}
fn record(value: Value, launch: &str) -> Checked<TeamObservation> {
    let actual = text(&value, "launch_id")?;
    if actual != launch {
        return Ok(TeamObservation::Mismatch {
            field: "launch_id".into(),
        });
    }
    Ok(TeamObservation::Record(team_record(&value)?))
}

fn team_record(value: &Value) -> Checked<TeamRecord> {
    if value["rename_shadow"] == true {
        return Err(Fault);
    }
    let get = |key: &str, max: usize| -> Checked<String> {
        let s = match value.get(key) {
            None | Some(Value::Null) => "",
            Some(Value::String(s)) => s,
            _ => return Err(Fault),
        };
        if s.len() > max || s.chars().any(char::is_control) {
            return Err(Fault);
        }
        Ok(s.into())
    };
    let label = get("name", 512)?;
    if !name(&label) {
        return Err(Fault);
    }
    let agent = get("agent", 32)?;
    if !matches!(
        agent.as_str(),
        "claude" | "codex" | "kimi" | "dsh" | "opencode"
    ) {
        return Err(Fault);
    }
    let account = get("account_id", 256)?;
    Ok(TeamRecord {
        launch_id: get("launch_id", 256)?,
        name: label,
        agent: agent.clone(),
        session_id: get("session_id", 256)?,
        run_id: get("run_id", 256)?,
        conversation_id: get("conversation_id", 2048)?,
        cwd: get("cwd", 4096)?,
        provider: None,
        account: if account.is_empty() {
            format!("native-{agent}")
        } else {
            account
        },
    })
}
fn selected_team(state: &Path, launch: &str, r: &mut Reads<'_>) -> Checked<TeamObservation> {
    let start = r.read(
        &state.join("launches").join(format!("{launch}.json")),
        SMALL,
    )?;
    let end = r.read(
        &state.join("launches").join(format!("{launch}.end.json")),
        SMALL,
    )?;
    if let Some(end) = end {
        if end["launch_id"] != launch {
            return Ok(TeamObservation::Mismatch {
                field: "launch_id".into(),
            });
        }
        let code = end["code"]
            .as_i64()
            .and_then(|c| i32::try_from(c).ok())
            .ok_or(Fault)?;
        return Ok(TeamObservation::Ended { code });
    }
    let Some(start) = start else {
        return Ok(TeamObservation::Absent { legacy: true });
    };
    if start["launch_id"] != launch {
        return Ok(TeamObservation::Mismatch {
            field: "launch_id".into(),
        });
    }
    let label = text(&start, "name")?;
    if !name(label) {
        return Err(Fault);
    }
    let agent = label.split('/').next().ok_or(Fault)?;
    if let Some(id) = start["session_id"].as_str() {
        if let Some(v) = r.by_id(state, id, agent)? {
            return record(v, launch);
        }
    }
    let mut candidate = r.label(state, label)?;
    for _ in 0..3 {
        let Some(v) = &candidate else { break };
        if v["launch_id"] != launch || v["rename_shadow"] != true {
            return record(v.clone(), launch);
        }
        let target = text(v, "rename_target")?.to_owned();
        candidate = r.label(state, &target)?;
    }
    if candidate.is_some() {
        return Err(Fault);
    }
    let leaf = format!("{:x}.json", Sha256::digest(label.as_bytes()));
    if let Some(index) = r.read(&state.join("labels").join(leaf), SMALL)? {
        if index["label"] != label {
            return Err(Fault);
        }
        let ids = index["ids"]
            .as_array()
            .filter(|ids| ids.len() <= 8)
            .ok_or(Fault)?;
        let mut found = None;
        for id in ids {
            if let Some(v) = r.by_id(state, id.as_str().ok_or(Fault)?, agent)? {
                if v["launch_id"] == launch {
                    if found.is_some() {
                        return Err(Fault);
                    }
                    found = Some(v);
                }
            }
        }
        if let Some(v) = found {
            return record(v, launch);
        }
    }
    Ok(TeamObservation::Absent {
        legacy: start["session_id"].is_null(),
    })
}
/// The most rename shadows the label hint is followed through.
const HOPS: usize = 3;
/// Whether `v` is the live or saved record of session `id`: it carries the
/// ID, is no rename shadow and no archive.
fn carries(v: &Value, id: &str) -> bool {
    v["session_id"] == id && v["rename_shadow"] != true && unarchived(v)
}

fn unarchived(v: &Value) -> bool {
    v.get("archive_id")
        .is_none_or(|a| a.is_null() || a.as_str() == Some(""))
}
/// Whether `v` lost its launch ID by starting fresh: a fresh start carries
/// no launch ID and marks the record with `fresh_from` until its new
/// conversation starts, then with a `session_clear` whose source is
/// `fresh_start` (`clear_context::carry_fresh_start`, `observe_start`).
fn started_fresh(v: &Value) -> bool {
    let launch = &v["launch_id"];
    (launch.is_null() || launch.as_str() == Some(""))
        && (v["fresh_from"].is_object() || v["session_clear"]["source"] == "fresh_start")
}
/// The handle's own routes: its record in the ID layout, else the ID index's
/// label. An index entry whose label now holds another record is a stale
/// cache entry and a miss, never a fault: the index is not a source of truth.
fn by_handle(state: &Path, id: &str, agent: &str, r: &mut Reads<'_>) -> Checked<Option<Value>> {
    let folder = if matches!(agent, "dsh" | "opencode") {
        state.join(agent).join("sessions")
    } else {
        state.join("sessions")
    };
    if let Some(v) = r.read(&folder.join(format!("{id}.json")), RAW)? {
        if v["session_id"] != id || v["layout"] != "id" {
            return Err(Fault);
        }
        return Ok(carries(&v, id).then_some(v));
    }
    let Some(index) = r.read(&state.join("ids").join(format!("{id}.json")), SMALL)? else {
        return Ok(None);
    };
    let value = r.label(state, text(&index, "name")?)?;
    Ok(value.filter(|v| carries(v, id)))
}
/// The label at binding, through at most [`HOPS`] rename shadows of this
/// same session (a legacy rename leaves a shadow at the old label).
fn by_hint(state: &Path, id: &str, hint: &str, r: &mut Reads<'_>) -> Checked<Option<Value>> {
    if hint.is_empty() {
        return Ok(None);
    }
    let mut label = hint.to_owned();
    for _ in 0..=HOPS {
        let Some(v) = r.label(state, &label)? else {
            return Ok(None);
        };
        if v["session_id"] != id {
            return Ok(None);
        }
        if v["rename_shadow"] != true {
            return Ok(carries(&v, id).then_some(v));
        }
        label = text(&v, "rename_target")?.to_owned();
    }
    Ok(None)
}
fn selected_session(
    state: &Path,
    id: &str,
    launch: &str,
    agent: &str,
    hint: &str,
    r: &mut Reads<'_>,
    provider: bool,
) -> Checked<TeamSession> {
    if !canonical(id) || !canonical(launch) {
        return Err(Fault);
    }
    let found = match by_handle(state, id, agent, r)? {
        Some(v) => Some(v),
        None => by_hint(state, id, hint, r)?,
    };
    let Some(v) = found else {
        return Ok(TeamSession::Gone);
    };
    if v["launch_id"] != launch {
        return Ok(if started_fresh(&v) {
            TeamSession::StartedFresh
        } else {
            TeamSession::LaunchChanged
        });
    }
    observed_record(&v, state, r, provider)
}

fn observed_record(
    v: &Value,
    state: &Path,
    r: &mut Reads<'_>,
    provider: bool,
) -> Checked<TeamSession> {
    let progress = provider
        .then(|| {
            ["last_main_progress_at", "turn_started"]
                .iter()
                .filter_map(|field| v[field].as_f64())
                .reduce(f64::max)
                .map(|t| (t * 1000.0).round() as i64)
        })
        .flatten();
    let mut stop = provider
        .then(|| crate::state::team_provider_stop::saved_stop(v))
        .flatten();
    if stop.is_some_and(|stop| {
        stop.kind == crate::state::team_provider_stop::ProviderStopKind::Provider
    }) {
        let owner = crate::state::storage::owner_key(v);
        let job = state.join("recovery/jobs").join(format!("{owner}.json"));
        match r.read(&job, RAW) {
            Ok(job)
                if !job
                    .as_ref()
                    .is_some_and(|job| crate::state::recovery::same_active_failure(v, job)) => {}
            _ => stop = None,
        }
    }
    let mut found = team_record(v)?;
    found.provider = provider.then(|| {
        Box::new(crate::state::team_provider_stop::ProviderObservation {
            stop,
            progress_ms: progress,
        })
    });
    Ok(TeamSession::Record(found))
}
pub(super) fn session(
    state: &Path,
    id: &str,
    launch: &str,
    agent: &str,
    hint: &str,
    deadline: Instant,
    provider: bool,
) -> TeamSession {
    pinned_session_read(state, deadline, |state, r| {
        selected_session(state, id, launch, agent, hint, r, provider)
    })
}

pub(super) fn run(
    state: &Path,
    run: &str,
    id: &str,
    agent: &str,
    hint: &str,
    deadline: Instant,
) -> TeamSession {
    pinned_session_read(state, deadline, |state, r| {
        let row = if id.is_empty() {
            // A legacy record may have only the current catalog label.
            r.label(state, hint)?.filter(unarchived)
        } else if canonical(id) {
            match by_handle(state, id, agent, r)? {
                Some(record) => Some(record),
                None => by_hint(state, id, hint, r)?,
            }
        } else {
            return Err(Fault);
        };
        let Some(row) = row else {
            return Ok(TeamSession::Gone);
        };
        if run.is_empty() || row["run_id"] != run || row["agent"] != agent {
            return Ok(TeamSession::LaunchChanged);
        }
        observed_record(&row, state, r, true)
    })
}

fn pinned_session_read(
    state: &Path,
    deadline: Instant,
    read: impl FnOnce(&Path, &mut Reads<'_>) -> Checked<TeamSession>,
) -> TeamSession {
    let Ok(state) = configured_root(state, deadline) else {
        return TeamSession::Unavailable;
    };
    let mut counts = Counts {
        deadline: Some(deadline),
        ..Counts::default()
    };
    let mut r = Reads::new(&mut counts);
    let result = read(&state, &mut r).and_then(|result| r.finish().map(|()| result));
    if Instant::now() >= deadline {
        return TeamSession::Unavailable;
    }
    result.unwrap_or(TeamSession::Unavailable)
}
pub(super) fn observe(state: &Path, launch: &str, deadline: Instant) -> TeamObservation {
    if !canonical(launch) {
        return TeamObservation::Unavailable;
    }
    let Ok(state) = configured_root(state, deadline) else {
        return TeamObservation::Unavailable;
    };
    let mut counts = Counts {
        deadline: Some(deadline),
        ..Counts::default()
    };
    let mut r = Reads::new(&mut counts);
    let result =
        selected_team(&state, launch, &mut r).and_then(|result| r.finish().map(|()| result));
    if Instant::now() >= deadline {
        return TeamObservation::Unavailable;
    }
    result.unwrap_or(TeamObservation::Unavailable)
}

#[cfg(test)]
mod tests;
