//! Read-only host graph collection. The derived index and the standard machine
//! identity bootstrap may be persisted. Collection never enters a provider
//! adapter, opens a terminal or reads message bodies.
mod index;
mod model;
mod sends;

pub(super) use sends::target_receipt;
pub(crate) use sends::{caller_record, forward_send, send_origin};

use super::*;
use anyhow::{ensure, Result as GraphResult};
use index::Index;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    time::{Duration, Instant, UNIX_EPOCH},
};

const MAX_SOURCES: usize = 10_000;
const SLICE_BYTES: u64 = 2 * 1024 * 1024;
pub(super) const MAX_RECORD: u64 = 16 * 1024 * 1024;
const SLICE_RECORDS: usize = 32;
const SLICE_STAMPS: usize = 512;

type InventorySource = (String, PathBuf, Option<String>);

fn source_order(source: &str) -> (u8, &str) {
    (
        if source.starts_with("archives/") {
            2
        } else if source.starts_with("sends/") {
            1
        } else {
            0
        },
        source,
    )
}

fn candidate(path: &Path) -> bool {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    path.extension().and_then(|s| s.to_str()) == Some("json")
        && stem.len() == 64
        && stem
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn inventory(root: &Path) -> GraphResult<(Vec<InventorySource>, bool)> {
    let mut paths = Vec::new();
    let mut complete = true;
    for directory in [
        "",
        "bindings",
        "dsh/bindings",
        "opencode/bindings",
        "sends",
        "archives",
    ] {
        if directory == "sends" {
            for (id, stamp) in sends::source_ids(root)? {
                if paths.len() == MAX_SOURCES {
                    complete = false;
                    break;
                }
                paths.push((
                    format!("sends/{id}"),
                    root.join("graph/sends.sqlite3"),
                    Some(stamp),
                ));
            }
            if !complete {
                break;
            }
            continue;
        }
        let entries = match fs::read_dir(root.join(directory)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let path = entry?.path();
            let archive = directory == "archives"
                && path.extension().and_then(|s| s.to_str()) == Some("json")
                && path.file_stem().and_then(|s| s.to_str()).is_some_and(|s| {
                    uuid::Uuid::parse_str(s).is_ok_and(|id| !id.is_nil() && id.to_string() == s)
                });
            if !candidate(&path) && !archive {
                continue;
            }
            if paths.len() == MAX_SOURCES {
                complete = false;
                break;
            }
            paths.push((
                format!(
                    "{directory}/{}",
                    path.file_name().unwrap().to_string_lossy()
                ),
                path,
                None,
            ));
        }
        if !complete {
            break;
        }
    }
    paths.sort_by(|a, b| source_order(&a.0).cmp(&source_order(&b.0)));
    Ok((paths, complete))
}

fn collect(index: &mut Index, machine: &str, root: &Path) -> GraphResult<()> {
    let started = Instant::now();
    let (paths, complete) = inventory(root)?;
    // Restart the derived scan once when upgrading the ordering. Keep all
    // prior facts until the new complete inventory has actually been seen.
    if !matches!(index.get("scan_order").as_deref(), Ok("live-first-v1")) {
        index.set("scan_after", "")?;
        index.set("scan_order", "live-first-v1")?;
    }
    let after = index.get("scan_after")?;
    let generation = index.generation()?;
    index.set("complete", "false")?;
    let mut bytes = 0;
    let mut changed = 0;
    let mut seen = Vec::new();
    let mut scanned = after.clone();
    for (count, (source, path, known_stamp)) in paths
        .iter()
        .filter(|(key, _, _)| after.is_empty() || source_order(key) > source_order(&after))
        .enumerate()
    {
        if count >= SLICE_STAMPS
            || changed >= SLICE_RECORDS
            || bytes >= SLICE_BYTES
            || (count > 0 && started.elapsed() > Duration::from_millis(50))
        {
            index.checkpoint(&seen, generation, &scanned)?;
            return Ok(());
        }
        let mut loaded = false;
        let read = (|| -> GraphResult<()> {
            if let Some(id) = source.strip_prefix("sends/") {
                let stamp = known_stamp
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Missing graph send stamp"))?;
                if index.stamp(source)?.as_deref() == Some(stamp) {
                    seen.push(source.clone());
                    return Ok(());
                }
                loaded = true;
                let record = sends::load(id)?;
                let data = serde_json::to_vec(&record)?;
                bytes += data.len() as u64;
                ensure!(data.len() <= 16 * 1024, "Graph send metadata size limit");
                let projection = model::exchange(machine, &record)
                    .ok_or_else(|| anyhow::anyhow!("Invalid graph send identity"))?;
                index.publish(source, stamp, generation, &projection, "")?;
                return Ok(());
            }
            let meta = path.symlink_metadata()?;
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "nonregular_binding"
            );
            ensure!(meta.len() <= MAX_RECORD, "binding_size_limit");
            // Canonical renamed bindings supersede their legacy mirrors. The
            // route is consulted on every slice, even when file stamps match.
            let legacy = path.parent() == Some(root);
            if legacy
                && root
                    .join("name_routes")
                    .join(path.file_name().unwrap())
                    .exists()
            {
                if index.stamp(source)?.as_deref() == Some("superseded") {
                    seen.push(source.clone());
                    return Ok(());
                }
                loaded = true;
                index.publish(
                    source,
                    "superseded",
                    generation,
                    &model::Projection::new(),
                    "",
                )?;
                return Ok(());
            }
            let stamp = format!(
                "{}:{}",
                meta.len(),
                meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos()
            );
            if index.stamp(source)?.as_deref() == Some(&stamp) {
                seen.push(source.clone());
                return Ok(());
            }
            let file = fs::File::open(path)?;
            loaded = true;
            let mut data = Vec::new();
            file.take(MAX_RECORD + 1).read_to_end(&mut data)?;
            bytes += data.len() as u64;
            ensure!(data.len() as u64 <= MAX_RECORD, "binding_size_limit");
            let record: Value = serde_json::from_slice(&data)?;
            let archived = source.starts_with("archives/");
            let expected = if archived {
                format!("{}.json", string(&record, "archive_id"))
            } else {
                format!(
                    "{:x}.json",
                    Sha256::digest(string(&record, "name").as_bytes())
                )
            };
            ensure!(
                path.file_name().and_then(|v| v.to_str()) == Some(&expected),
                "binding_identity_mismatch"
            );
            let native = source.starts_with("dsh/") || source.starts_with("opencode/");
            ensure!(
                !archived || record["state"] == "archived",
                "archive_state_mismatch"
            );
            ensure!(
                if native {
                    record["backend"]
                        == if source.starts_with("dsh/") {
                            "dsh"
                        } else {
                            "opencode"
                        }
                } else {
                    record["version"] == 1
                },
                "binding_schema_mismatch"
            );
            let projection = if record["rename_shadow"] == true {
                model::Projection::new()
            } else {
                model::project(machine, &record)
                    .ok_or_else(|| anyhow::anyhow!("binding_identity_unavailable"))?
            };
            index.publish(source, &stamp, generation, &projection, "")?;
            Ok(())
        })();
        if loaded || read.is_err() {
            changed += 1;
        }
        if read.is_err() {
            // Do not persist parser text: it may contain private source data.
            // Retain last-good facts with an explicit coverage error.
            index.source_error(source, generation, "binding_unavailable")?;
        }
        scanned.clone_from(source);
    }
    index.checkpoint(&seen, generation, &scanned)?;
    index.finish_scan(generation, complete)?;
    Ok(())
}

fn run(args: &[String]) -> GraphResult<Value> {
    if args == ["receipt", "--json"] {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            name: String,
            expected_run_id: String,
            expected_conversation_id: String,
            message_request_id: String,
            #[serde(default)]
            archive_id: String,
        }
        let mut bytes = Vec::new();
        io::stdin().take(8193).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 8192, "Graph receipt scope is too large");
        let request: Request = serde_json::from_slice(crate::state::without_bom(&bytes))?;
        ensure!(
            !request.name.is_empty()
                && request.name.len() <= 512
                && !request.expected_run_id.is_empty()
                && request.expected_run_id.len() <= 128
                && request.expected_conversation_id.len() <= 512
                && [
                    &request.name,
                    &request.expected_run_id,
                    &request.expected_conversation_id
                ]
                .iter()
                .all(|s| !s.chars().any(char::is_control)),
            "Invalid graph receipt scope"
        );
        let record = if !request.archive_id.is_empty() {
            archive::read_archive(&request.name, &request.archive_id)
        } else if dsh::exists(&request.name) {
            dsh::binding(&request.name)
        } else if opencode::exists(&request.name) {
            opencode::binding(&request.name)
        } else {
            read(&request.name)
        }
        .map_err(anyhow::Error::msg)?;
        ensure!(
            record["run_id"] == request.expected_run_id
                && record["conversation_id"] == request.expected_conversation_id,
            "Graph conversation changed; refresh before opening evidence"
        );
        let event = attachments::graph_receipt(&record, &request.message_request_id)
            .map_err(anyhow::Error::msg)?;
        return Ok(
            json!({"schema":1,"machine_id":target_receipt(&record).and_then(|v|v["endpoint"]["machine"].as_str().map(str::to_owned)),"name":request.name,"run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id,
            "archive_id":request.archive_id,"message_request_id":request.message_request_id,"event":event}),
        );
    }
    let (changes, mut at) = match args.first().map(String::as_str) {
        Some("snapshot") => (false, 1),
        Some("changes") => (true, 1),
        _ => anyhow::bail!(
            "usage: hgs graph snapshot [--cursor CURSOR] --json | changes [--after CURSOR] --json"
        ),
    };
    let mut cursor = None;
    let mut limit = 200;
    let mut json_flag = false;
    while at < args.len() {
        match args[at].as_str() {
            "--json" if !json_flag => {
                json_flag = true;
                at += 1;
            }
            flag if flag == if changes { "--after" } else { "--cursor" } && cursor.is_none() => {
                cursor = Some(
                    args.get(at + 1)
                        .ok_or_else(|| anyhow::anyhow!("Graph cursor is required"))?
                        .as_str(),
                );
                at += 2;
            }
            "--limit" => {
                limit = args
                    .get(at + 1)
                    .ok_or_else(|| anyhow::anyhow!("Graph limit is required"))?
                    .parse::<usize>()?;
                at += 2;
            }
            _ => anyhow::bail!("Invalid graph option"),
        }
    }
    ensure!(
        json_flag && (1..=200).contains(&limit),
        "Graph requires --json and a limit of 1..200"
    );
    let config = crate::config::Config::load().map_err(|e| anyhow::anyhow!(e.message))?;
    let machine = crate::swarm::local_node_id(&config)?;
    let root = root();
    let directory = root.join("graph");
    private_dir(&directory).map_err(anyhow::Error::msg)?;
    let _guard = lock_until(
        &directory.join(".lock"),
        Instant::now() + Duration::from_millis(100),
    )
    .map_err(anyhow::Error::msg)?
    .ok_or_else(|| anyhow::anyhow!("Graph cache is updating; retain the last snapshot"))?;
    let path = directory.join("index.sqlite3");
    if !path.exists() {
        crate::platform::private_open(fs::OpenOptions::new().write(true).create_new(true))
            .open(&path)?;
    }
    let mut index = Index::open(&path, &machine)?;
    // Continuation pages do not advance collection underneath their snapshot.
    if changes || cursor.is_none() {
        collect(&mut index, &machine, &root)?;
    }
    let mut result = index.page(cursor, changes, limit)?;
    result["coverage"]["delivery_recording"] = sends::coverage()?;
    result["machine_id"] = json!(machine);
    result["host"] = json!(config.selves.first().cloned().unwrap_or_default());
    Ok(result)
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    println!("{}", run(args).map_err(|e| e.to_string())?);
    Ok(0)
}
