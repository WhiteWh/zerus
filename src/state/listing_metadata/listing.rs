//! Ordinary ls only. Watch and canonical/action readers retain their own paths.
use super::*;
use zerus_client_core::source;

const COMPAT_ENTRIES: usize = 256;
const COMPAT_BYTES: u64 = 16 << 20;
const BODY_BYTES: usize = 64 << 20;
const PAGE_BYTES: usize = 512 << 10;
const OVERLAY_DEADLINE: &str = source!("Selected listing overlay deadline exceeded");

// One original request clock surrounds each public observation. The operating
// system call may overrun cooperatively; no following observation may start.
pub(super) fn observe_until<T>(
    until: Instant,
    observe: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    anyhow::ensure!(Instant::now() < until, OVERLAY_DEADLINE);
    let value = observe()?;
    anyhow::ensure!(Instant::now() < until, OVERLAY_DEADLINE);
    Ok(value)
}

fn public_observation(record: &Value) -> bool {
    if record["agent"] == "claude" {
        return !claude_approval::listing_needs_record(record);
    }
    if record["agent"] != "codex" {
        return false;
    }
    let Ok(screen) = kimi_tui_choice::capture(record) else {
        return false;
    };
    if screen
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r'))
    {
        return false;
    }
    input::composer::codex_public_frame(record, &screen)
}

#[derive(Default)]
pub(crate) struct Query {
    pub paged: bool,
    pub cursor: Option<String>,
    pub page_size: usize,
}

struct Canonical {
    root: Directory,
    dirs: String,
    witnesses: Vec<(Key, String)>,
    rows: Vec<(String, Value)>,
}
impl Canonical {
    fn capture(path: &Path) -> anyhow::Result<Self> {
        let root = checked(Directory::open(&crate::platform::canonical(path)?))?;
        let dirs = repair::directories_at(&root)?;
        let until = Instant::now() + Duration::from_millis(250);
        let mut entries = 0;
        let mut bytes = 0u64;
        let mut witnesses = Vec::new();
        // Enumerate and prove the COMPLETE small roster before reading a body.
        for namespace in storage::BINDING_DIRS {
            let paths = match std::fs::read_dir(root.path.join(namespace)) {
                Ok(paths) => paths,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in paths {
                entries += 1;
                anyhow::ensure!(
                    entries <= COMPAT_ENTRIES && Instant::now() < until,
                    "Session listing exceeds the bounded default-off inventory; enable listing-index on this computer"
                );
                let path = entry?.path();
                let Some(key) = Key::at(&root.path, &path) else {
                    continue;
                };
                let metadata = std::fs::symlink_metadata(&path)?;
                anyhow::ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "Session listing source is unavailable"
                );
                bytes = bytes.saturating_add(metadata.len());
                anyhow::ensure!(
                    bytes <= COMPAT_BYTES,
                    "Session listing exceeds the bounded default-off byte budget; enable listing-index on this computer"
                );
                let file = crate::platform::no_follow(std::fs::OpenOptions::new().read(true))
                    .open(&path)?;
                witnesses.push((key, store::version(&file)?));
            }
        }
        anyhow::ensure!(
            repair::directories_at(&root)? == dirs,
            "Session listing inventory changed during preflight"
        );
        match std::fs::read_dir(root.path.join("renames")) {
            Ok(mut entries) => anyhow::ensure!(
                entries.next().transpose()?.is_none(),
                "Session listing is waiting for canonical rename recovery"
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let mut rows = Vec::new();
        let mut read = 0;
        let mut files = 0;
        for (key, stamp) in &witnesses {
            let record = selected_record(
                &root.path,
                &key.0,
                &json!({"native_stamp":stamp}),
                &mut read,
                &mut files,
                COMPAT_ENTRIES,
                until,
            )?;
            // Unsupported private/public shapes refuse before native overlays.
            let _projection = Seed::encode(key, &record)?;
            if !key.0.contains('/') || key.0.starts_with("bindings/") {
                anyhow::ensure!(
                    record["rename_shadow"] == true
                        || !root
                            .path
                            .join("name_routes")
                            .join(format!(
                                "{}.json",
                                storage::label_key(string(&record, "name"))
                            ))
                            .try_exists()?,
                    "Legacy alias needs canonical reconciliation before listing"
                );
            }
            if record["rename_shadow"] != true {
                rows.push((key.0.clone(), json!({"record":record,"native_stamp":stamp})));
            }
        }
        let result = Self {
            root,
            dirs,
            witnesses,
            rows,
        };
        result.verify()?;
        Ok(result)
    }
    fn verify(&self) -> anyhow::Result<()> {
        let until = Instant::now() + Duration::from_millis(250);
        for (key, stamp) in &self.witnesses {
            anyhow::ensure!(
                Instant::now() < until,
                "Session listing verification timed out"
            );
            let file = crate::platform::no_follow(std::fs::OpenOptions::new().read(true))
                .open(self.root.path.join(&key.0))?;
            anyhow::ensure!(
                store::version(&file)? == *stamp,
                "Session listing changed before publication"
            );
        }
        anyhow::ensure!(
            repair::directories_at(&self.root)? == self.dirs,
            "Session listing membership changed before publication"
        );
        checked(self.root.verify())
    }
}

pub(super) fn selected_record(
    root: &Path,
    source: &str,
    seed: &Value,
    bytes: &mut usize,
    files: &mut usize,
    max_files: usize,
    until: Instant,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        *files < max_files,
        source!("Selected listing overlays exceeded their file limit")
    );
    anyhow::ensure!(Instant::now() < until, OVERLAY_DEADLINE);
    let key = Key(source.into());
    key.validate()?;
    let parent = checked(Directory::open(root))?;
    let mut directory = parent;
    for part in Path::new(source).parent().unwrap().components() {
        directory = checked(
            directory.child(
                part.as_os_str()
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid display source"))?,
            ),
        )?;
    }
    let path = directory.path.join(Path::new(source).file_name().unwrap());
    let mut file =
        crate::platform::no_follow(std::fs::OpenOptions::new().read(true)).open(&path)?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "Selected listing source is not regular"
    );
    let stamp = store::version(&file)?;
    anyhow::ensure!(
        seed["native_stamp"].as_str() == Some(stamp.as_str()),
        "Selected listing source changed"
    );
    let length = file.metadata()?.len() as usize;
    anyhow::ensure!(
        length <= COMPAT_BYTES as usize - *bytes,
        "Selected listing overlay exceeds its byte budget"
    );
    use std::io::Read;
    let mut raw = Vec::new();
    for _ in 0..length.div_ceil(64 << 10) {
        anyhow::ensure!(Instant::now() < until, "Selected listing overlay timed out");
        let mut chunk = [0; 64 << 10];
        let remaining = (length - raw.len()).min(chunk.len());
        let n = file.read(&mut chunk[..remaining])?;
        raw.extend_from_slice(&chunk[..n]);
        if n == 0 {
            break;
        }
    }
    *bytes += raw.len();
    *files += 1;
    anyhow::ensure!(
        raw.len() == length && store::version(&file)? == stamp,
        "Selected listing overlay changed"
    );
    checked(directory.verify())?;
    let current = crate::platform::no_follow(std::fs::OpenOptions::new().read(true)).open(path)?;
    anyhow::ensure!(
        store::version(&current)? == stamp,
        "Selected listing source route changed"
    );
    let record: Value = serde_json::from_slice(&raw)?;
    anyhow::ensure!(key.matches(&record), "Selected listing identity changed");
    let record = if source.starts_with("dsh/") || source.starts_with("opencode/") {
        record
    } else {
        let _route =
            storage::listing_scope::Route::verified(root.join(source), string(&record, "name"));
        checked(storage::verify_at(
            &root.join(source),
            string(&record, "name"),
            &raw,
        ))?
    };
    anyhow::ensure!(
        Instant::now() < until,
        "Selected native listing observation exceeded its deadline"
    );
    Ok(record)
}

// Read a locator from the already observed pane, with the existing native
// observation clock. It is no membership proof and starts no server/agent.
fn native_locator(
    terminal: &str,
    pane: &(String, String, String),
    names: &BTreeMap<String, String>,
    until: Instant,
) -> anyhow::Result<(String, String)> {
    anyhow::ensure!(
        Instant::now() < until,
        "Native display lookup deadline exceeded"
    );
    let mut command = std::process::Command::new("tmux");
    command.args(["display-message","-p","-t",&pane.0,"#{pane_id}\t#{session_id}\t#{@hgs_run}\t#{HGS_SESSION_ID}\t#{HGS_RESUME_SESSION_ID}\t#{HGS_SESSION}"]);
    let (status, stdout, _) =
        crate::platform::run_bounded(&mut command, [8192, 4096], until, &|| false).map_err(
            |_| anyhow::anyhow!("Native display locator is unavailable or exceeded its bound"),
        )?;
    anyhow::ensure!(status.success(), "Native display locator is unavailable");
    let text = std::str::from_utf8(&stdout)?;
    let fields = text.trim_end_matches('\n').split('\t').collect::<Vec<_>>();
    anyhow::ensure!(
        fields.len() == 6
            && fields[0] == pane.0
            && fields[2] == pane.2
            && names.get(fields[1]).is_some_and(|name| name == terminal),
        "Native display identity changed"
    );
    let sid = if fields[3].is_empty() {
        fields[4]
    } else {
        fields[3]
    };
    anyhow::ensure!(
        sid.is_empty() || session_id::canonical(sid),
        "Native display session ID is invalid"
    );
    anyhow::ensure!(
        fields[5].len() <= 2048,
        "Native display label exceeds its bound"
    );
    Ok((
        sid.to_owned(),
        if fields[5].is_empty() {
            terminal.to_owned()
        } else {
            fields[5].to_owned()
        },
    ))
}

pub(in crate::state) fn merge(
    mut value: Value,
    ids: &BTreeMap<String, String>,
    archives: bool,
    query: &Query,
) -> Result<Value> {
    let run = || -> anyhow::Result<Value> {
        let root = root();
        let until = Instant::now() + Duration::from_secs(10);
        let observation = storage::listing_scope::Scope::enter(until);
        control::automatic(&root);
        let indexed = changes::qualified() && enabled(&root);
        anyhow::ensure!(
            indexed || !query.paged,
            "Paged session listing needs an enabled qualified index on this computer"
        );
        let _capture = checked(storage::lock_until(
            &root.join(".lock"),
            Instant::now() + Duration::from_millis(250),
        ))?
        .ok_or_else(|| anyhow::anyhow!("Session listing is waiting for the canonical writer"))?;
        let mut canonical = if indexed {
            None
        } else {
            Some(Canonical::capture(&root)?)
        };
        drop(_capture);
        if indexed {
            worker::request(&root);
        }
        let reader = if indexed {
            Some(pages::Reader::open(&root)?)
        } else {
            None
        };
        let mut cursor = query
            .cursor
            .as_deref()
            .map(pages::Cursor::decode)
            .transpose()?;
        let ((snapshot, mut names), mut live) = {
            let _guard = checked(storage::lock_until(
                &root.join(".lock"),
                Instant::now() + Duration::from_millis(250),
            ))?
            .ok_or_else(|| {
                anyhow::anyhow!("Session listing is waiting for the canonical writer")
            })?;
            let observation = checked(live_sessions())?;
            let rows = value["sessions"]
                .as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("Invalid session listing"))?;
            (observation, std::mem::take(rows))
        };
        let current = |listed: &str| ids.get(listed).and_then(|id| names.get(id)).cloned();
        let held: std::collections::BTreeSet<_> = live
            .iter()
            .filter_map(|row| current(string(row, "name")))
            .collect();
        live.retain_mut(|row| {
            let listed = string(row, "name").to_owned();
            match current(&listed) {
                Some(name) => {
                    if name != listed {
                        let rec = json!({"name":name});
                        label_row(row, &rec);
                    }
                    true
                }
                None => snapshot.contains_key(&listed) && !held.contains(&listed),
            }
        });
        for row in &mut live {
            row.as_object_mut()
                .unwrap()
                .extend(telemetry::unavailable().as_object().unwrap().clone());
        }
        let marks = read_marks::load();
        let preferences = notifications::load();
        let warm = keep_warm::load();
        let routing = question_routing::load();
        let outcomes = question_route_outcomes::Reader::load();
        let shared = Shared {
            marks: &marks,
            preferences: preferences.as_ref(),
            keep_warm: warm.as_ref(),
            question_routing: routing.as_ref(),
            route_outcomes: outcomes.as_ref(),
            budget: read_marks::question_budget(),
        };
        let mut archive_page = if archives {
            // Reuse this observation for the archive's advisory Restore hint.
            // Looking up an absent live record would invalidate the listing.
            let name_taken = |name: &str| {
                snapshot.contains_key(name)
                    || match &reader {
                        Some(reader) => reader.has_label(name).unwrap_or(true),
                        None => canonical
                            .as_ref()
                            .unwrap()
                            .rows
                            .iter()
                            .any(|(_, seed)| seed["record"]["name"] == name),
                    }
            };
            Some(checked(archive_catalog::preview(&marks, Some(&name_taken)))?)
        } else {
            None
        };
        let archived = archive_page
            .as_ref()
            .and_then(|page| page["sessions"].as_array())
            .cloned()
            .unwrap_or_default();
        let mut rows = Vec::new();
        let mut process_work = processes::WorkSample::default();
        let mut total_bytes = 0;
        let mut canonical_bytes = 0;
        let mut canonical_files = 0;
        // Full/mobile snapshots retain original canonical semantics for the
        // bounded live inventory. Explicit page/deferred proof stays at 16.
        let overlay_files = if query.paged {
            16
        } else {
            storage::listing_scope::ENTRY_LIMIT
        };
        let mut public_witnesses = Vec::new();
        let mut witness_bytes = 0usize;
        let native_until = until.min(Instant::now() + Duration::from_secs(1));
        loop {
            anyhow::ensure!(
                Instant::now() < until,
                "Session listing deadline exceeded; use bounded pages"
            );
            let (batch, next) = if let Some(reader) = &reader {
                let page = reader.page_limit(
                    cursor.as_ref(),
                    if query.paged { query.page_size } else { 128 },
                )?;
                anyhow::ensure!(
                    page.fresh,
                    "Session listing is building, stale or unavailable; no complete body returned"
                );
                (page.rows, page.next)
            } else {
                (std::mem::take(&mut canonical.as_mut().unwrap().rows), None)
            };
            let mut deepseek = Vec::new();
            let mut opencode = Vec::new();
            let start_count = rows.len();
            for (source, seed) in batch {
                anyhow::ensure!(Instant::now() < until, OVERLAY_DEADLINE);
                let mut record = seed["record"].clone();
                if source.starts_with("dsh/") {
                    deepseek.push(record);
                    continue;
                }
                if source.starts_with("opencode/") {
                    opencode.push(record);
                    continue;
                }
                if archive::equivalent(&record, &archived) {
                    continue;
                }
                if let Some(index) = live
                    .iter()
                    .position(|row| string(row, "name") == terminal_name(&record))
                {
                    if matches(&record, panes_of(&snapshot, &record)) {
                        // One bounded screen observation covers the negative
                        // display precheck and the public row. A positive panel
                        // still takes the original full canonical/identity path.
                        kimi_tui_choice::observing(|| -> anyhow::Result<()> {
                            let public = indexed
                                && record["_listing"]["live_public"] == true
                                && observe_until(until, || Ok(public_observation(&record)))?;
                            if indexed && !public {
                                record = selected_record(
                                    &root,
                                    &source,
                                    &seed,
                                    &mut canonical_bytes,
                                    &mut canonical_files,
                                    overlay_files,
                                    until,
                                )?;
                            }
                            let _route = storage::listing_scope::Route::verified(
                                root.join(&source),
                                string(&record, "name"),
                            );
                            let mut row = live.remove(index);
                            label_row(&mut row, &record);
                            workspace::relabel(
                                &mut names,
                                terminal_name(&record),
                                string(&record, "name"),
                            );
                            if public {
                                let stamp = seed["native_stamp"].as_str().ok_or_else(|| {
                                    anyhow::anyhow!(source!("Missing public native version"))
                                })?;
                                reader.as_ref().unwrap().verify_source(&source, stamp)?;
                                witness_bytes += source.len() + stamp.len();
                                anyhow::ensure!(
                                    witness_bytes
                                        <= if query.paged { PAGE_BYTES } else { BODY_BYTES },
                                    source!("Public listing witnesses exceeded their bound")
                                );
                                public_witnesses.push((source.clone(), stamp.to_owned()));
                                let _public = storage::listing_scope::Public::enter(&record);
                                observe_until(until, || {
                                    checked(live_display_row(
                                        &mut row,
                                        record,
                                        &shared,
                                        &mut process_work,
                                    ))
                                })?;
                            } else {
                                checked(live_row(&mut row, record, &shared, &mut process_work))?;
                            }
                            rows.push(row);
                            Ok(())
                        })?;
                        continue;
                    }
                }
                if string(&record, "conversation_id").is_empty()
                    || terminal_live(&snapshot, &record).unwrap_or(false)
                {
                    continue;
                }
                if indexed && record["_listing"]["requires_canonical"] == true {
                    record = selected_record(
                        &root,
                        &source,
                        &seed,
                        &mut canonical_bytes,
                        &mut canonical_files,
                        overlay_files,
                        until,
                    )?;
                }
                let _route = if indexed && seed["record"]["_listing"]["requires_canonical"] == true
                {
                    Some(storage::listing_scope::Route::verified(
                        root.join(&source),
                        string(&record, "name"),
                    ))
                } else {
                    None
                };
                let name = string(&record, "name");
                let parts: Vec<_> = name.splitn(3, '/').collect();
                let mut row = json!({"name":name,"cmd":record["agent"],"project":parts.get(1),"tag":parts.get(2),"attached":0,"clients":[],"created":record["created"],"state":if record["paused"]==true {"paused"}else{"stopped"},"resumable":true});
                row.as_object_mut().unwrap().extend(
                    journal::summary(&record, false)
                        .as_object()
                        .unwrap()
                        .clone(),
                );
                read_marks::enrich_listing(
                    &mut row,
                    &marks,
                    journal::journal_name(&record),
                    Ok(Some(Vec::new())),
                );
                notifications::enrich(
                    &mut row,
                    preferences.as_ref(),
                    journal::journal_name(&record),
                );
                keep_warm::enrich(&mut row, warm.as_ref(), &record);
                question_routing::enrich(&mut row, routing.as_ref());
                rows.push(row);
            }
            // Resolve deferred terminal membership before a slow provider can
            // consume the shared native observation clock.
            if query.paged && query.cursor.is_none() {
                let mut untracked = Vec::new();
                let reader = reader.as_ref().unwrap();
                let mut probes = 0;
                for row in std::mem::take(&mut live) {
                    let terminal = string(&row, "name");
                    let panes = snapshot.get(terminal);
                    let mut bound = false;
                    if let Some(panes) = panes.filter(|panes| {
                        panes.len() == 1 && panes[0].1 == "0" && !panes[0].2.is_empty()
                    }) {
                        probes += 1;
                        anyhow::ensure!(
                            probes <= 128,
                            "Native display lookup count exceeds its bound"
                        );
                        let (sid, label) =
                            native_locator(terminal, &panes[0], &names, native_until)?;
                        let candidates = if sid.is_empty() {
                            reader.terminal_candidates(&label)?
                        } else {
                            reader
                                .parent(&json!({"session_id":sid}))?
                                .into_iter()
                                .collect()
                        };
                        for (source, seed) in candidates {
                            let record = &seed["record"];
                            if source.starts_with("dsh/")
                                || source.starts_with("opencode/")
                                || terminal_name(record) != terminal
                                || !matches(record, Some(panes))
                            {
                                continue;
                            }
                            let record = selected_record(
                                &root,
                                &source,
                                &seed,
                                &mut canonical_bytes,
                                &mut canonical_files,
                                16,
                                until,
                            )?;
                            // Suppression uses the same exact owner predicate as the
                            // normal page projection, on fresh canonical bytes.
                            if terminal_name(&record) == terminal && matches(&record, Some(panes)) {
                                bound = true;
                                break;
                            }
                        }
                    }
                    if !bound {
                        untracked.push(row);
                    }
                }
                live = untracked;
            }
            rows.extend(checked(dsh::listing_records(&deepseek, native_until))?);
            rows.extend(checked(opencode::listing_records(&opencode, native_until))?);
            for row in &rows[start_count..] {
                total_bytes += serde_json::to_vec(row)?.len();
            }
            anyhow::ensure!(
                total_bytes <= if query.paged { PAGE_BYTES } else { BODY_BYTES },
                "Session listing response exceeds its bound; choose a smaller --page-size"
            );
            cursor = next;
            if query.paged || cursor.is_none() {
                break;
            }
        }
        // A late publisher/gap must invalidate the retained transaction's body.

        if !query.paged || query.cursor.is_none() {
            rows.extend(live);
        }
        if let Some(page) = archive_page.as_mut() {
            rows.append(
                page["sessions"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow::anyhow!("Invalid archive preview"))?,
            );
            page.as_object_mut().unwrap().remove("sessions");
        }
        finish_rows(&mut rows, &marks, &names);
        if query.paged {
            for row in &mut rows {
                let Some(link) = row
                    .get("parent")
                    .filter(|parent| parent.is_object())
                    .cloned()
                else {
                    continue;
                };
                let parent = reader.as_ref().unwrap().parent(&link)?;
                let mut pair = Vec::new();
                if let Some((source, seed)) = parent {
                    let parent = if seed["record"]["_listing"]["requires_canonical"] == true {
                        selected_record(
                            &root,
                            &source,
                            &seed,
                            &mut canonical_bytes,
                            &mut canonical_files,
                            16,
                            until,
                        )?
                    } else {
                        seed["record"].clone()
                    };
                    let observed = match string(&parent, "backend") {
                        "dsh" => checked(dsh::listing_records(&[parent], native_until))?.remove(0),
                        "opencode" => {
                            checked(opencode::listing_records(&[parent], native_until))?.remove(0)
                        }
                        _ => {
                            let live = matches(&parent, panes_of(&snapshot, &parent));
                            let mut observed = journal::normalized_activity(&parent, live);
                            observed["session_id"] = parent["session_id"].clone();
                            observed["conversation_id"] = parent["conversation_id"].clone();
                            observed["state"] = json!(if live { "running" } else { "stopped" });
                            observed
                        }
                    };
                    pair.push(observed);
                } else if let Some(archived) = archived.iter().find(|candidate| {
                    !string(&link, "session_id").is_empty()
                        && candidate["session_id"] == link["session_id"]
                        || string(&link, "session_id").is_empty()
                            && string(candidate, "session_id").is_empty()
                            && candidate["conversation_id"] == link["conversation_id"]
                }) {
                    pair.push(archived.clone());
                }
                pair.push(row.clone());
                parent_link::decorate(&mut pair);
                row["parent"] = pair.pop().unwrap()["parent"].clone();
            }
        }
        value["sessions"] = Value::Array(rows);
        if let Some(page) = archive_page {
            value["archive_page"] = page;
        }
        if query.paged {
            value["listing_page"] = json!({"abi":control::MARKER,"complete":cursor.is_none(),"coverage":"ready","generation":reader.as_ref().unwrap().generation()?,"next_cursor":cursor.as_ref().map(pages::Cursor::encode).transpose()?});
        }
        anyhow::ensure!(
            serde_json::to_vec(&value)?.len() <= if query.paged { PAGE_BYTES } else { BODY_BYTES },
            "Complete session listing exceeds its response bound"
        );
        observation.verify().map_err(anyhow::Error::msg)?;
        if let Some(reader) = &reader {
            for (source, stamp) in public_witnesses {
                anyhow::ensure!(
                    Instant::now() < until,
                    source!("Public listing verification deadline exceeded")
                );
                reader.verify_source(&source, &stamp)?;
            }
            anyhow::ensure!(
                reader.page_limit(cursor.as_ref(), 1)?.fresh,
                "Session listing changed before publication; no complete body returned"
            );
        } else {
            canonical.as_ref().unwrap().verify()?;
        }
        Ok(value)
    };
    run().map_err(|error| public_index_error(&error))
}
