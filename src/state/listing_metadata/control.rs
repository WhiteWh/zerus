//! Explicit per-computer activation of display acceleration, never authority.
use super::*;
use std::io::Read;

pub(crate) const MARKER: &str = "session-listing-metadata-v1";

/// Ordinary supported Linux use initializes its internal cache. Existing
/// policy, including malformed or unreadable policy, is never overwritten.
pub(super) fn automatic(root: &Path) {
    if !changes::qualified() || enabled(root) {
        return;
    }
    let initialize = || -> anyhow::Result<()> {
        let parent = checked(Directory::open(&crate::platform::canonical(root)?))?;
        let path = parent.path.join("listing-metadata");
        for marker in ["enabled", "disabled"] {
            match std::fs::symlink_metadata(path.join(marker)) {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let dir = checked(parent.ensure_child("listing-metadata"))?;
        // Atomic no-replace publication through the pinned private directory.
        // A concurrent disable still wins in enabled(), including malformed ones.
        checked(dir.write_new("enabled", b"1\n"))?;
        checked(dir.verify())?;
        checked(parent.verify())
    };
    let _ = initialize(); // Advisory failure never changes canonical state.
}

fn directory(root: &Path) -> anyhow::Result<Directory> {
    let root = checked(Directory::open(&crate::platform::canonical(root)?))?;
    let dir = checked(root.child("listing-metadata"))?;
    checked(root.verify())?;
    Ok(dir)
}
fn remove_marker(dir: &Directory, name: &str) -> anyhow::Result<()> {
    let Some(mut file) = checked(dir.read_private_optional(name))? else {
        return Ok(());
    };
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(3).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.is_empty() || bytes == b"1\n",
        "Unrecognized display kill switch; keep the index disabled"
    );
    let expected = crate::platform::FileIdentity::of(&file)?;
    let current = checked(dir.read_private(name))?;
    anyhow::ensure!(
        expected == crate::platform::FileIdentity::of(&current)?,
        "Display activation marker changed"
    );
    drop(current);
    drop(file);
    checked(dir.verify())?;
    std::fs::remove_file(dir.path.join(name))?;
    crate::platform::sync_dir(&dir.path)?;
    Ok(())
}
pub(super) fn status_at(root: &Path) -> Value {
    let active = enabled(root);
    let qualified = changes::qualified();
    let mut value = json!({"version":1,"enabled":active,"qualified":qualified,"state":if active {"building"}else{"disabled"}});
    if active && !qualified {
        value["state"] = json!("unsupported");
        return value;
    }
    if !active {
        return value;
    }
    if !worker::running(root) {
        value["reason"] = json!("The metadata worker is starting or unavailable");
        return value;
    }
    let read = || -> anyhow::Result<Value> {
        let store = Store::read(root)?;
        let (ready, generation): (bool, i64) =
            store
                .db
                .query_row("SELECT ready,generation FROM meta WHERE id=1", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
        if ready && generation > 0 {
            let (dirs, gap): (String, String) = store.db.query_row(
                "SELECT dirs,gap FROM generations WHERE generation=?1 AND complete=1",
                [generation],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if repair::directories(&store)? == dirs && repair::gap_stamp(&store)? == gap {
                return Ok(json!({"state":"ready","generation":generation}));
            }
        }
        let dir = directory(root)?;
        if let Some(file) = checked(dir.read_private_optional("worker-status.json"))? {
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes)?;
            if bytes.len() <= 4096 {
                let status: Value = serde_json::from_slice(&bytes)?;
                if status["state"] == "unavailable" {
                    return Ok(json!({"state":"unavailable","reason":status["reason"]}));
                }
            }
        }
        Ok(json!({"state":"building"}))
    };
    match read() {
        Ok(status) => value
            .as_object_mut()
            .unwrap()
            .extend(status.as_object().unwrap().clone()),
        Err(error) => {
            value["state"] = json!("unavailable");
            value["reason"] = json!(public_index_error(&error));
        }
    }
    value
}
pub(in crate::state) fn dispatch(args: &[String]) -> Result<Value> {
    let action = match args {
        [action] => action.as_str(),
        [action, json] if json == "--json" => action.as_str(),
        _ => return Err("usage: hgs listing-index <enable|disable|status> [--json]".into()),
    };
    if !["enable", "disable", "status"].contains(&action) {
        return Err("usage: hgs listing-index <enable|disable|status> [--json]".into());
    }
    local_sessions()?;
    let root = root();
    if action == "status" {
        return Ok(status_at(&root));
    }
    if action == "enable" && !changes::qualified() {
        return Err(
            "Indexed session listing is not qualified on this platform; activation stays off"
                .into(),
        );
    }
    let run = || -> anyhow::Result<()> {
        // Qualified Linux initializes lazily: an explicit early disable must
        // remain durable even before the first cache exists.
        if action == "enable" || changes::qualified() {
            checked(private_dir(&root))?;
            let root = checked(Directory::open(&crate::platform::canonical(&root)?))?;
            checked(root.ensure_child("listing-metadata"))?;
        } else if !root.join("listing-metadata").exists() {
            return Ok(());
        }
        let dir = directory(&root)?;
        if action == "disable" {
            checked(dir.replace_file("disabled", b"1\n"))?;
            return Ok(());
        }
        checked(dir.replace_file("enabled", b"1\n"))?;
        remove_marker(&dir, "disabled")?;
        checked(dir.verify())
    };
    run().map_err(|e| e.to_string())?;
    if action == "enable" {
        worker::request(&root);
        return Ok(json!({"version":1,"enabled":true,"qualified":true,"state":"building"}));
    }
    Ok(status_at(&root))
}
