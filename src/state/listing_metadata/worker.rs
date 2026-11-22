//! One opt-in metadata worker per native state root. It owns no session lane,
//! terminal, caller resources or canonical writer lock.
use super::*;
use fs2::FileExt;
use std::{
    process::{Command, Stdio},
    thread,
};

fn directory(root: &Path) -> anyhow::Result<Directory> {
    let parent = checked(Directory::open(&crate::platform::canonical(root)?))?;
    let dir = checked(parent.child("listing-metadata"))?;
    checked(parent.verify())?;
    Ok(dir)
}
fn take(root: &Path, name: &str) -> anyhow::Result<Option<storage::Guard>> {
    let file = checked(directory(root)?.open_private_lock(name))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(storage::Guard::held(file))),
        Err(e) if crate::platform::lock_contended(&e) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub(super) fn running(root: &Path) -> bool {
    let inspect = || -> anyhow::Result<bool> {
        let Some(file) = checked(directory(root)?.read_private_optional("worker.lock"))? else {
            return Ok(false);
        };
        match file.try_lock_exclusive() {
            Ok(()) => Ok(false),
            Err(e) if crate::platform::lock_contended(&e) => Ok(true),
            Err(e) => Err(e.into()),
        }
    };
    inspect().unwrap_or(false)
}
pub(super) fn request(root: &Path) {
    if !changes::qualified() || !enabled(root) {
        return;
    }
    let launch = || -> anyhow::Result<()> {
        let Some(_launch) = take(root, "launch.lock")? else {
            return Ok(());
        };
        let Some(probe) = take(root, "worker.lock")? else {
            return Ok(());
        };
        let dir = directory(root)?;
        let attempt = checked(dir.read_private_optional("attempt.json"))?;
        if let Some(file) = attempt {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes)?;
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                let age = now() - value["at"].as_f64().unwrap_or(0.0);
                if (0.0..2.0).contains(&age) {
                    return Ok(());
                }
            }
        }
        checked(dir.replace_file("attempt.json", format!("{{\"at\":{}}}\n", now()).as_bytes()))?;
        let root = crate::platform::canonical(root)?;
        let mut command = Command::new(std::env::current_exe()?);
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("HGS_"))
        {
            command.env_remove(key);
        }
        command
            .args(["__state", "listing-metadata-worker"])
            .env("HGS_STATE_DIR", &root)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        drop(probe);
        let mut child = crate::platform::spawn_detached(&mut command)?;
        thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    };
    let _ = launch();
}
fn status(root: &Path, state: &str, reason: &str) {
    let write = || -> anyhow::Result<()> {
        let dir = directory(root)?;
        let value = json!({"version":1,"state":state,"reason":reason.chars().take(512).collect::<String>(),"at":now()});
        checked(dir.replace_file("worker-status.json", format!("{value}\n").as_bytes()))
    };
    let _ = write();
}

pub(in crate::state) fn entry() -> Result<i32> {
    let run = || -> anyhow::Result<i32> {
        anyhow::ensure!(
            changes::qualified(),
            "Indexed session listing is not qualified on this platform"
        );
        let selected = root();
        let root = checked(Directory::open(&crate::platform::canonical(&selected)?))?;
        let Some(_lease) = take(&root.path, "worker.lock")? else {
            return Ok(0);
        };
        let executable = std::env::current_exe()?;
        let binary = crate::platform::FileIdentity::at(&executable)
            .ok()
            .map(|v| v.0);
        if binary.is_none() {
            return Ok(0);
        }
        let mut work = None;
        let mut reconciliation = reconcile::Reconcile::default();
        let mut last_state = String::new();
        loop {
            checked(root.verify())?;
            if crate::platform::canonical(&selected).ok().as_ref() != Some(&root.path)
                || !enabled(&root.path)
                || crate::platform::FileIdentity::at(&executable)
                    .ok()
                    .map(|v| v.0)
                    != binary
            {
                return Ok(0);
            }
            match background_at(&root.path, &mut work) {
                Ok(stats) => {
                    anyhow::ensure!(
                        stats.entries <= 256
                            && stats.rows <= 128
                            && stats.bytes <= 16 << 20
                            && stats.seed_bytes <= 512 << 10,
                        "Metadata worker step exceeded its bound"
                    );
                    let state = if stats.complete {
                        "ready"
                    } else if stats.done {
                        "unavailable"
                    } else {
                        "building"
                    };
                    if last_state != state {
                        status(&root.path, state, "");
                        last_state = state.into();
                    }
                    if stats.complete {
                        // This optional work owns no display transaction/gate.
                        // Failure retains the canonical binding for later review.
                        let _ = reconciliation.step(&root.path);
                    }
                    // A resident stream closes its gap before each new cut.
                    // Idle polling is fixed-size native metadata, never a census.
                    thread::sleep(Duration::from_millis(if stats.complete {
                        200
                    } else if stats.done {
                        2000
                    } else {
                        1
                    }));
                }
                Err(error) => {
                    gap(&root.path);
                    status(&root.path, "unavailable", &public_index_error(&error));
                    last_state = "unavailable".into();
                    work = None;
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
    };
    run().map_err(|e| e.to_string())
}
