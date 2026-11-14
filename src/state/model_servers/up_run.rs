//! A detached `up`: the template runs in its own `hgs model-server __up-run`
//! process, so Zerus never waits minutes for a model to load. Its record under
//! the state root (`model-servers/runs/<request_id>.json`) is written before the
//! run starts and again with its outcome: exit code, time limit and the last
//! characters of its output. Only `up-result` reads it back.
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The output a record keeps, from its end.
const TAIL_BYTES: usize = 16 * 1024;

std::thread_local! {
    /// Tests run the template in this process instead of a detached one.
    pub(crate) static INLINE_RUNNER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn path(id: &str) -> Option<PathBuf> {
    let parsed = uuid::Uuid::parse_str(id).ok()?;
    (parsed.hyphenated().to_string() == id).then(|| super::super::root().join("model-servers").join("runs").join(format!("{id}.json")))
}

fn write(id: &str, record: &Value) -> Result<(), String> {
    let path = path(id).ok_or("Invalid request id")?;
    let parent = path.parent().ok_or("Invalid run path")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    super::super::storage::atomic_private(&path, &record.to_string())
}

/// The record of the detached up `id`, if there is one.
pub(crate) fn record(id: &str) -> Option<Value> {
    let bytes = std::fs::read(path(id)?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Records the run, then starts its runner detached (or inline in tests).
pub(crate) fn start(id: &str, label: &str, argv: &[String], minutes: u64) -> Result<(), String> {
    if argv.is_empty() {
        return Err("The up command is empty".into());
    }
    let record = json!({"label": label, "argv": argv, "status": "running", "started": super::super::now(),
        "timeout_secs": minutes.saturating_mul(60)});
    write(id, &record)?;
    if INLINE_RUNNER.with(std::cell::Cell::get) {
        run(id);
        return Ok(());
    }
    let mut command = Command::new(std::env::current_exe().map_err(|error| error.to_string())?);
    command.args(["model-server", "__up-run", id]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::platform::spawn_detached(&mut command).map(drop).map_err(|error| error.to_string())
}

/// `hgs model-server __up-run ID`: runs the recorded argv within its time
/// limit and records the outcome.
pub(crate) fn run(id: &str) -> i32 {
    let Some(mut record) = record(id) else { return 1 };
    let argv: Vec<String> = record["argv"].as_array().into_iter().flatten().filter_map(|word| word.as_str().map(str::to_owned)).collect();
    let limit = Duration::from_secs(record["timeout_secs"].as_u64().unwrap_or(0).max(1));
    let outcome = execute(&argv, limit);
    let (exit, timed_out, error, tail) = match outcome {
        Ok((exit, timed_out, tail)) => (exit.map_or(Value::Null, |code| json!(code)), timed_out, Value::Null, tail),
        Err(error) => (Value::Null, false, json!(error), String::new()),
    };
    record["status"] = json!("ran");
    record["exit"] = exit;
    record["timed_out"] = json!(timed_out);
    record["error"] = error;
    record["tail"] = json!(tail);
    record["ended"] = json!(super::super::now());
    i32::from(write(id, &record).is_err())
}

fn execute(argv: &[String], limit: Duration) -> Result<(Option<i32>, bool, String), String> {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let mut readers = Vec::new();
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten() {
        readers.push(std::thread::spawn(move || {
            let mut stream = stream;
            let mut kept = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(count) = stream.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                kept.extend_from_slice(&buffer[..count]);
                if kept.len() > TAIL_BYTES {
                    kept.drain(..kept.len() - TAIL_BYTES);
                }
            }
            kept
        }));
    }
    let deadline = Instant::now() + limit;
    let (code, timed_out) = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break (status.code(), false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut output = Vec::new();
    for reader in readers {
        output.extend(reader.join().unwrap_or_default());
    }
    let start = output.len().saturating_sub(TAIL_BYTES);
    Ok((code, timed_out, String::from_utf8_lossy(&output[start..]).into_owned()))
}
