//! a message to an agent that has not confirmed the previous input waits
//! in hgs instead of being refused. Only a request that asks for it
//! (`queue_when_unconfirmed`, sent by clients that read `input_hold_supported`
//! from the inspection) is held: a bounded per-session FIFO, each message
//! under its own request ID and receipt. One deliverer per session submits the
//! oldest through the guarded `input::submit` once the previous input is
//! confirmed. A cancelled message is never sent; after an uncertain delivery
//! nothing more is sent automatically, and nothing is ever replayed.
use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Messages one session may hold at once.
pub(super) const LIMIT: usize = 8;
/// How long a deliverer waits for a confirmation without delivering anything.
const PATIENCE: Duration = Duration::from_secs(30 * 60);
const POLL: Duration = Duration::from_millis(500);
/// The deliverer's reason to drop a message the owner cancelled.
pub(super) const CANCELLED: &str = "this queued message was cancelled; nothing was sent";
const STOPPED: &str = "an earlier queued message may already be in the agent's input; nothing more was sent automatically";

fn directory(name: &str) -> PathBuf {
    root().join("input_hold").join(format!("{:x}", Sha256::digest(name.as_bytes())))
}

fn deliverer_lock(name: &str) -> PathBuf {
    directory(name).join(".deliver.lock")
}

fn receipt_path(request_id: &str) -> PathBuf {
    root().join("input_receipts").join(format!("{request_id}.json"))
}

/// The held messages of `name`, oldest first: (item file, item).
pub(super) fn items(name: &str) -> Vec<(PathBuf, Value)> {
    let Ok(entries) = fs::read_dir(directory(name)) else { return vec![] };
    let mut items: Vec<(PathBuf, Value)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let item: Value = serde_json::from_str(&fs::read_to_string(&path).ok()?).ok()?;
            (string(&item, "name") == name).then_some((path, item))
        })
        .collect();
    items.sort_by_key(|(_, item)| item["seq"].as_u64().unwrap_or(u64::MAX));
    items
}

/// where the latest held messages that were not delivered stay
/// listed (failed, or uncertain), so the composer that sent one gets its text
/// back and says so; at most `LIMIT`, the oldest leave first.
fn settled_dir(name: &str) -> PathBuf {
    directory(name).join("settled")
}

fn keep_settled(name: &str, item: &Value, status: &str, error: &str) {
    let dir = settled_dir(name);
    if private_dir(&dir).is_err() {
        return;
    }
    // Named by when it settled: a queue's own numbers start over once it empties.
    let settled_at = now();
    let request_id = string(item, "request_id");
    let record = json!({"request_id":request_id,"name":name,"text":item["text"],"queued_at":item["queued_at"],
        "status":status,"error":error,"settled_at":settled_at});
    let _ = atomic(&dir.join(format!("{:020}-{request_id}.json", (settled_at * 1_000_000.0) as u64)), &record.to_string());
    let mut kept: Vec<PathBuf> = fs::read_dir(&dir).into_iter().flatten().flatten().map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json")).collect();
    kept.sort();
    let excess = kept.len().saturating_sub(LIMIT);
    for old in kept.drain(..excess) {
        let _ = fs::remove_file(old);
    }
}

/// the held messages of `name` that were not delivered, oldest first.
pub(super) fn settled(name: &str) -> Value {
    let mut kept: Vec<PathBuf> = fs::read_dir(settled_dir(name)).into_iter().flatten().flatten().map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json")).collect();
    kept.sort();
    json!(kept.iter().rev().take(LIMIT).rev()
        .filter_map(|path| serde_json::from_str::<Value>(&fs::read_to_string(path).ok()?).ok())
        .filter(|record| string(record, "name") == name)
        .map(|record| json!({"request_id":record["request_id"],"text":record["text"],"queued_at":record["queued_at"],
            "status":record["status"],"error":record["error"],"settled_at":record["settled_at"]}))
        .collect::<Vec<_>>())
}

fn remove(path: &Path) {
    let _ = fs::remove_file(path.with_extension("payload"));
    let _ = fs::remove_file(path);
}

/// Holds a checked request under the state lock the caller already owns and
/// returns its receipt, status "queued".
pub(super) fn hold(name: &str, request_id: &str, digest: &str, payload: &[u8], record: &Value, text: &str) -> Result<Value> {
    let held = items(name);
    if held.len() >= LIMIT {
        return Err(format!("{LIMIT} messages already wait for this agent; wait for them or cancel one"));
    }
    let seq = held.last().and_then(|(_, item)| item["seq"].as_u64()).unwrap_or(0) + 1;
    let dir = directory(name);
    private_dir(&dir)?;
    let item_path = dir.join(format!("{seq:020}-{request_id}.json"));
    let queued_at = now();
    let receipt = json!({"request_id":request_id,"name":name,"run_id":record["run_id"],
        "conversation_id":string(record,"conversation_id"),"first_message":false,"digest":digest,"status":"queued",
        "queued_at":queued_at,"text":text,"transport":"hgs_queue","position":held.len() + 1});
    // The receipt last: once it says "queued", the message is in the queue.
    crate::platform::private_open(fs::OpenOptions::new().create_new(true).write(true))
        .open(item_path.with_extension("payload"))
        .and_then(|mut file| { use std::io::Write; file.write_all(payload)?; file.sync_all() })
        .map_err(|e| e.to_string())?;
    atomic(&item_path, &json!({"request_id":request_id,"name":name,"seq":seq,"queued_at":queued_at,"text":text}).to_string())?;
    if let Err(error) = atomic(&receipt_path(request_id), &receipt.to_string()) {
        remove(&item_path);
        return Err(error);
    }
    start_deliverer(name);
    Ok(receipt)
}

/// The held messages as the inspection reports them; a deliverer resumes if none runs.
pub(super) fn inspection(name: &str) -> Value {
    let held = items(name);
    if !held.is_empty() {
        start_deliverer(name);
    }
    json!(held.iter().map(|(_, item)| json!({"request_id":item["request_id"],"text":item["text"],"queued_at":item["queued_at"]}))
        .collect::<Vec<_>>())
}

/// Starts `hgs __state deliver-held` unless one runs for `name`. Tests step
/// the queue themselves.
#[cfg(not(test))]
fn start_deliverer(name: &str) {
    use std::process::{Command, Stdio};
    if crate::status::locked(&deliverer_lock(name)) {
        return;
    }
    let Ok(executable) = std::env::current_exe() else { return };
    let mut command = Command::new(executable);
    command.args(["__state", "deliver-held", name]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let _ = crate::platform::spawn_detached(&mut command);
}
#[cfg(test)]
fn start_deliverer(_: &str) {}

/// Whether a refusal only waits for something that passes: the previous input,
/// a dialog or draft the owner resolves, a busy agent. Identity and process
/// changes never pass, and neither do invalid requests.
pub(super) fn waits(error: &str) -> bool {
    ![
        "session identity changed",
        "no longer running",
        "identity is unconfirmed",
        "no saved binding",
        "invalid saved binding",
        "invalid send payload",
        "request ID was already used",
        "delivery uncertain",
    ]
    .iter()
    .any(|permanent| error.contains(permanent))
}

fn settle(request_id: &str, status: &str, error: &str) {
    let path = receipt_path(request_id);
    let Ok(contents) = fs::read_to_string(&path) else { return };
    let Ok(mut receipt) = serde_json::from_str::<Value>(&contents) else { return };
    if receipt["status"] != "queued" {
        return;
    }
    receipt["status"] = json!(status);
    receipt["error"] = json!(error);
    receipt["settled_at"] = json!(now());
    let _ = atomic(&path, &receipt.to_string());
}

/// What one delivery attempt of the oldest message did.
#[derive(Debug, PartialEq)]
pub(super) enum Step { Delivered, Dropped, Wait, Stop }

/// One attempt for the oldest held message of `name`, through `submit`
/// (`input::submit_held` outside tests).
pub(super) fn step(name: &str, submit: &dyn Fn(&str, &[u8]) -> Result<Value>) -> Option<Step> {
    let (path, item) = items(name).into_iter().next()?;
    let request_id = string(&item, "request_id").to_owned();
    let Ok(payload) = fs::read(path.with_extension("payload")) else {
        let _guard = lock(None).ok()?;
        settle(&request_id, "failed", "the queued message could not be read; nothing was sent");
        keep_settled(name, &item, "failed", "the queued message could not be read; nothing was sent");
        remove(&path);
        return Some(Step::Dropped);
    };
    Some(match submit(name, &payload) {
        Ok(_) => { remove(&path); Step::Delivered }
        Err(error) if error == CANCELLED => { remove(&path); Step::Dropped }
        Err(error) if error.starts_with("delivery uncertain") => {
            // Its receipt stays in progress; the rest are never sent automatically.
            keep_settled(name, &item, "uncertain", &error);
            remove(&path);
            let _guard = lock(None).ok()?;
            for (rest, item) in items(name) {
                settle(string(&item, "request_id"), "failed", STOPPED);
                keep_settled(name, &item, "failed", STOPPED);
                remove(&rest);
            }
            Step::Stop
        }
        Err(error) if waits(&error) => Step::Wait,
        Err(error) => {
            let _guard = lock(None).ok()?;
            settle(&request_id, "failed", &error);
            keep_settled(name, &item, "failed", &error);
            remove(&path);
            Step::Dropped
        }
    })
}

/// `hgs __state deliver-held <session>`: delivers the held messages in order,
/// one deliverer per session, until none is left or none could be delivered
/// for PATIENCE; messages left then wait for the next send or inspection.
pub(super) fn deliver(args: &[String]) -> Result<i32> {
    let [name] = args else { return Err("usage: hgs __state deliver-held <session>".into()) };
    private_dir(&directory(name))?;
    let Some(_owner) = lock_until(&deliverer_lock(name), Instant::now())? else { return Ok(0) };
    let mut progress = Instant::now();
    while let Some(step) = step(name, &|name, payload| input::submit_held(name, payload)) {
        match step {
            Step::Delivered | Step::Dropped => progress = Instant::now(),
            Step::Stop => break,
            Step::Wait if progress.elapsed() > PATIENCE => break,
            Step::Wait => std::thread::sleep(POLL),
        }
    }
    Ok(0)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancel { request_id: String }

/// `hgs send-cancel <session> --json`: cancels a message still held. Under the
/// state lock, which delivery holds from its last check to Enter, so a
/// cancelled message is never sent and a sent one is never reported cancelled.
pub(super) fn cancel(name: &str, input: &[u8]) -> Result<Value> {
    let request: Cancel = serde_json::from_slice(input).map_err(|e| format!("invalid cancel payload: {e}"))?;
    let request_id = uuid::Uuid::parse_str(&request.request_id).map_err(|_| "request_id must be a UUID")?.to_string();
    let _guard = lock(None)?;
    let path = receipt_path(&request_id);
    let mut receipt: Value = fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok())
        .ok_or("no queued message has this request ID")?;
    if string(&receipt, "name") != name {
        return Err("no queued message has this request ID".into());
    }
    match string(&receipt, "status") {
        "queued" => {}
        "cancelled" => return Ok(receipt),
        "submitted" => return Err("the message was already delivered; it cannot be cancelled".into()),
        "failed" => return Err(format!("the message was not delivered: {}", string(&receipt, "error"))),
        _ => return Err("delivery uncertain: the message may already be in the agent's input; check Terminal".into()),
    }
    receipt["status"] = json!("cancelled");
    receipt["settled_at"] = json!(now());
    atomic(&path, &receipt.to_string())?;
    for (item_path, item) in items(name) {
        if string(&item, "request_id") == request_id {
            remove(&item_path);
        }
    }
    Ok(receipt)
}

pub(super) fn dispatch_cancel(args: &[String]) -> Result<i32> {
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs send-cancel <session> --json (JSON from stdin)".into());
    }
    let mut input = Vec::new();
    io::stdin().take(8193).read_to_end(&mut input).map_err(|e| e.to_string())?;
    if input.len() > 8192 {
        return Err("cancel request is too large".into());
    }
    println!("{}", cancel(&args[0], &input)?);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn with_root(run: impl FnOnce()) {
        let dir = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(dir.path(), run);
    }
    fn record() -> Value {
        json!({"name":"claude/x/lead","run_id":"run","conversation_id":"conv","agent":"claude"})
    }
    fn hold_one(text: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let _guard = lock(None).unwrap();
        let payload = json!({"request_id":id,"text":text,"expected_run_id":"run"}).to_string();
        let receipt = hold("claude/x/lead", &id, "digest", payload.as_bytes(), &record(), text).unwrap();
        assert_eq!(receipt["status"], "queued");
        id
    }
    fn status(id: &str) -> String {
        let receipt: Value = serde_json::from_str(&fs::read_to_string(receipt_path(id)).unwrap()).unwrap();
        string(&receipt, "status").to_owned()
    }

    #[test]
    fn held_messages_are_bounded_and_delivered_once_in_order() {
        with_root(|| {
            let first = hold_one("first");
            let second = hold_one("second");
            assert_eq!(inspection("claude/x/lead").as_array().unwrap().len(), 2);
            let sent = RefCell::new(Vec::<String>::new());
            let confirmed = RefCell::new(false);
            let submit = |_: &str, payload: &[u8]| -> Result<Value> {
                // The previous input is unconfirmed until the agent shows it.
                if !*confirmed.borrow() {
                    return Err(input::UNCONFIRMED.into());
                }
                let request: Value = serde_json::from_slice(payload).unwrap();
                sent.borrow_mut().push(string(&request, "text").to_owned());
                Ok(json!({"status":"submitted"}))
            };
            assert_eq!(step("claude/x/lead", &submit), Some(Step::Wait));
            assert!(sent.borrow().is_empty());
            *confirmed.borrow_mut() = true;
            assert_eq!(step("claude/x/lead", &submit), Some(Step::Delivered));
            assert_eq!(step("claude/x/lead", &submit), Some(Step::Delivered));
            assert_eq!(step("claude/x/lead", &submit), None);
            assert_eq!(*sent.borrow(), vec!["first", "second"]);
            let _ = (first, second);
            // At most LIMIT wait at once.
            for _ in 0..LIMIT { hold_one("more"); }
            let _guard = lock(None).unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let error = hold("claude/x/lead", &id, "d", b"{}", &record(), "one too many").unwrap_err();
            assert!(error.contains("wait for them or cancel one"), "{error}");
        });
    }

    #[test]
    fn a_cancelled_message_is_never_sent() {
        with_root(|| {
            let first = hold_one("first");
            let second = hold_one("second");
            let receipt = cancel("claude/x/lead", json!({"request_id":first}).to_string().as_bytes()).unwrap();
            assert_eq!(receipt["status"], "cancelled");
            // Cancelling again is the same answer; another session's name is not.
            assert_eq!(cancel("claude/x/lead", json!({"request_id":first}).to_string().as_bytes()).unwrap()["status"], "cancelled");
            assert!(cancel("claude/x/other", json!({"request_id":second}).to_string().as_bytes()).is_err());
            let sent = RefCell::new(Vec::<String>::new());
            let submit = |_: &str, payload: &[u8]| -> Result<Value> {
                let request: Value = serde_json::from_slice(payload).unwrap();
                sent.borrow_mut().push(string(&request, "text").to_owned());
                Ok(json!({"status":"submitted"}))
            };
            while step("claude/x/lead", &submit).is_some() {}
            assert_eq!(*sent.borrow(), vec!["second"]);
            assert_eq!(status(&first), "cancelled");
            // A delivered message cannot be cancelled afterwards.
            fs::write(receipt_path(&second), json!({"name":"claude/x/lead","status":"submitted"}).to_string()).unwrap();
            assert!(cancel("claude/x/lead", json!({"request_id":second}).to_string().as_bytes()).unwrap_err().contains("already delivered"));
        });
    }

    #[test]
    fn uncertain_delivery_stops_the_queue_and_permanent_refusals_fail_one() {
        with_root(|| {
            let first = hold_one("first");
            let second = hold_one("second");
            let third = hold_one("third");
            let attempts = RefCell::new(0);
            let submit = |_: &str, _: &[u8]| -> Result<Value> {
                *attempts.borrow_mut() += 1;
                Err("session identity changed; refresh before sending".into())
            };
            assert_eq!(step("claude/x/lead", &submit), Some(Step::Dropped));
            assert_eq!(status(&first), "failed");
            let uncertain = |_: &str, _: &[u8]| -> Result<Value> { Err("delivery uncertain: agent changed after paste; inspect Terminal before retrying".into()) };
            assert_eq!(step("claude/x/lead", &uncertain), Some(Step::Stop));
            // The uncertain one keeps its receipt; the rest are never sent.
            assert_eq!(status(&second), "queued");
            assert_eq!(status(&third), "failed");
            assert!(items("claude/x/lead").is_empty());
            // Dialogs, drafts and busy agents wait; identity changes do not.
            assert!(waits(input::UNCONFIRMED) && waits("terminal has a draft, dialog, or unrecognized prompt; open Terminal"));
            assert!(!waits("the exact agent process is no longer running; resume it explicitly"));
        });
    }

    #[test]
    fn a_held_message_that_is_not_delivered_stays_listed_with_its_text() {
        // a failed or uncertain held message leaves the queue but not
        // the inspection: its text and outcome stay listed, bounded.
        with_root(|| {
            let failed = hold_one("the session restarted under me");
            let refuse = |_: &str, _: &[u8]| -> Result<Value> { Err("session identity changed; nothing was sent".into()) };
            assert_eq!(step("claude/x/lead", &refuse), Some(Step::Dropped));
            assert_eq!(status(&failed), "failed");
            assert!(inspection("claude/x/lead").as_array().unwrap().is_empty());
            let listed = settled("claude/x/lead");
            assert_eq!(listed.as_array().unwrap().len(), 1);
            assert_eq!((listed[0]["request_id"].as_str(), listed[0]["status"].as_str()), (Some(failed.as_str()), Some("failed")));
            assert_eq!(listed[0]["text"], "the session restarted under me");
            assert!(listed[0]["error"].as_str().unwrap().contains("session identity changed"));
            // An uncertain head is listed as uncertain; the ones behind it fail.
            let head = hold_one("head");
            let behind = hold_one("behind");
            let unsure = |_: &str, _: &[u8]| -> Result<Value> { Err("delivery uncertain: the terminal did not confirm".into()) };
            assert_eq!(step("claude/x/lead", &unsure), Some(Step::Stop));
            let listed = settled("claude/x/lead");
            let outcome = |id: &str| listed.as_array().unwrap().iter().find(|item| item["request_id"] == id).map(|item| item["status"].as_str().unwrap().to_owned());
            assert_eq!((outcome(&head).as_deref(), outcome(&behind).as_deref()), (Some("uncertain"), Some("failed")));
            // A delivered or cancelled message is not listed; the list is bounded.
            for n in 0..LIMIT + 2 {
                hold_one(&format!("lost {n}"));
                assert_eq!(step("claude/x/lead", &refuse), Some(Step::Dropped));
            }
            let listed = settled("claude/x/lead");
            assert_eq!(listed.as_array().unwrap().len(), LIMIT);
            assert_eq!(listed[LIMIT - 1]["text"], format!("lost {}", LIMIT + 1), "the newest last");
            let cancelled = hold_one("cancel me");
            cancel("claude/x/lead", json!({"request_id":cancelled}).to_string().as_bytes()).unwrap();
            assert!(!settled("claude/x/lead").as_array().unwrap().iter().any(|item| item["request_id"] == cancelled.as_str()));
            // Another session's list is its own.
            assert!(settled("claude/x/other").as_array().unwrap().is_empty());
        });
    }
}
