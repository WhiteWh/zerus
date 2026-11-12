//! Optional Codex questions are asynchronous messages, not blocking tool calls.
//! Tool acceptance is not an answer. Native turn completion or a new ordinary
//! user input closes the TUI's questions; neither fabricates a user answer.
//! Actual replies still acknowledge exact native questionItemIds.
use super::question_terminal::{Native, Transport};
use super::*;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::time::{Duration, Instant};

fn cache_path(record: &Value) -> Result<PathBuf> {
    Ok(absolute_root()?.join("codex_questions").join(format!(
        "{:x}.json",
        Sha256::digest(json!([record["agent_home"], record["conversation_id"]]).to_string())
    )))
}
fn parse_reply(text: &str) -> Vec<Value> {
    let text = text.trim();
    let text = if text.starts_with("# Context from my IDE setup:\n") {
        let Some((_, request)) = text.rsplit_once("\n## My request for Codex:\n") else {
            return vec![];
        };
        request.trim()
    } else {
        text
    };
    let Some(body) = text
        .strip_prefix("<send_user_message_question_reply>")
        .and_then(|s| s.strip_suffix("</send_user_message_question_reply>"))
    else {
        return vec![];
    };
    match serde_json::from_str::<Value>(body.trim()) {
        Ok(Value::Array(a)) => a,
        Ok(v @ Value::Object(_)) => vec![v],
        _ => vec![],
    }
}
fn observe(record: &Value, cache: &mut Value, event: &Value) {
    let payload = &event["payload"];
    if event["type"] == "event_msg" {
        let turn = string(payload, "turn_id");
        if !turn.is_empty() && turn.len() <= 200 && !turn.chars().any(char::is_control) {
            match string(payload, "type") {
                "task_started" | "turn_started" => cache["active_turn"] = json!(turn),
                "task_complete" | "turn_complete" | "turn_aborted" => {
                    if let Some(items) = cache["items"].as_object_mut() {
                        for card in items.values_mut().filter(|card| card["native_turn_id"] == turn) {
                            card["closed"] = json!(string(payload, "type"));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if event["type"] == "response_item"
        && payload["type"] == "function_call"
        && matches!(
            string(payload, "name"),
            "request_user_input_async" | "functions.request_user_input_async"
        )
    {
        // A same-named tool in another namespace is not a user question.
        if !["", "functions"].contains(&string(payload, "namespace")) {
            return;
        }
        let call = string(payload, "call_id");
        if call.is_empty() || call.len() > 200 {
            return;
        }
        let Ok(args) = serde_json::from_str::<Value>(string(payload, "arguments")) else {
            return;
        };
        let Some(items) = args["questions"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 16)
        else {
            return;
        };
        for (index, item) in items.iter().enumerate() {
            let title = string(item, "title");
            if title.trim().is_empty()
                || title.len() > 8192
                || title
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                continue;
            }
            let options = match item.get("options") {
                None | Some(Value::Null) => vec![],
                Some(Value::Array(a)) if a.len() <= 20 => a.clone(),
                _ => continue,
            };
            if options.iter().any(|o| {
                o.as_str().is_none_or(|s| {
                    s.trim().is_empty() || s.len() > 1024 || s.chars().any(char::is_control)
                })
            }) {
                continue;
            }
            let native_id = json!(["request_user_input_async", call, index]).to_string();
            let id = format!("codex-async:{call}:{index}");
            if !cache["items"][&id].is_null() {
                cache["items"][&id]["native_turn_id"] = cache["active_turn"].as_str().unwrap_or("").into();
                continue;
            }
            let choices: Vec<_> = options
                .iter()
                .enumerate()
                .map(|(i, o)| json!({"id":format!("opt_{i}"),"label":o,"description":""}))
                .collect();
            let mut card = json!({"question_id":id,"tool_call_id":call,"native_question_id":native_id,
                "run_id":record["run_id"],"conversation_id":record["conversation_id"],"source":"codex_async",
                "created_at":search::timestamp(&event["timestamp"]),"optional":true,"can_skip":true,"can_answer":false,
                "native_turn_id":cache["active_turn"].as_str().unwrap_or(""),
                "questions":[{"id":"q_0","question":title,"options":choices,"allow_other":true,"required":false,"multi_select":false}]});
            card["question_hash"] = json!(questions::fingerprint(&card));
            cache["items"][id] = card;
        }
    }
    if event["type"] == "response_item" && payload["type"] == "function_call_output" {
        if let Some(items) = cache["items"].as_object_mut() {
            let accepted = serde_json::from_str::<Value>(string(payload, "output"))
                .ok()
                .is_some_and(|v| v["accepted"] == true);
            for card in items
                .values_mut()
                .filter(|c| c["tool_call_id"] == payload["call_id"] && c.get("native_turn_id").is_some())
            {
                card["accepted"] = json!(accepted);
            }
        }
    }
    let texts: Vec<&str> = if event["type"] == "event_msg" && payload["type"] == "user_message" {
        vec![string(payload, "message")]
    } else if event["type"] == "response_item"
        && payload["type"] == "message"
        && payload["role"] == "user"
    {
        payload["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["text"].as_str())
            .collect()
    } else {
        vec![]
    };
    // Only a committed native user-message event establishes new input. User-role
    // response items can be harness context; mere assistant progress ends nothing.
    if event["type"] == "event_msg" && payload["type"] == "user_message"
        && texts.iter().any(|text| !text.trim().is_empty() && parse_reply(text).is_empty())
    {
        if let Some(items) = cache["items"].as_object_mut() {
            for card in items.values_mut() { card["closed"] = json!("follow_up"); }
        }
    }
    for reply in texts.into_iter().flat_map(parse_reply) {
        if let Some(items) = cache["items"].as_object_mut() {
            for card in items.values_mut() {
                if reply["questionItemId"] == card["native_question_id"]
                    && reply["question"] == card["questions"][0]["question"]
                    && reply["answer"].is_string()
                {
                    card["response"] = json!({"type":"interaction.resolved","time":search::timestamp(&event["timestamp"])*1000.,
                    "response":{"answers":{string(&reply,"question"):reply["answer"]}}});
                }
            }
        }
    }
}
fn read_cache(record: &Value) -> Result<Value> {
    index(record, &cache_path(record)?, None)?.ok_or_else(|| "question index incomplete".into())
}
/// The transcript's questions and replies alone, without answer authority:
/// for reading a native acknowledgement or recording this request's own
/// submission. Receipts are not recovered here, and nothing here decides
/// whether a question may be answered.
fn read_transcript(record: &Value) -> Result<Value> {
    index_with(
        record,
        &cache_path(record)?,
        None,
        false,
        absolute_root,
        |_| Box::new(Locator),
    )?
    .ok_or_else(|| "question index incomplete".into())
}
/// One page of question receipts: a bounded call reads at most one, an
/// unbounded call (an answer) reads every page (zerus-a4mub.8).
const RECEIPTS_PER_CALL: usize = 64;
// Work done by the calls of this test thread: transcript lines and receipt
// files read.
#[cfg(test)]
thread_local! {
    static WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}
#[cfg(test)]
fn worked(lines: usize, receipts: usize) {
    WORK.with(|work| {
        let (l, r) = work.get();
        work.set((l + lines, r + receipts));
    });
}
#[cfg(not(test))]
fn worked(_: usize, _: usize) {}
/// Bring the question cache up to date with the transcript, then recover
/// submissions from this run's question receipts. With a deadline, both
/// phases stop once it passes (after at least one line or receipt) and save
/// their progress, so the next call resumes; the cache is returned only when
/// both are complete, and an incomplete one is unknown, never empty. None
/// also when another process holds the cache past the deadline.
fn index(record: &Value, cache_path: &Path, deadline: Option<Instant>) -> Result<Option<Value>> {
    index_with(record, cache_path, deadline, true, absolute_root, |_| {
        Box::new(Locator)
    })
}
fn index_with(
    record: &Value,
    cache_path: &Path,
    deadline: Option<Instant>,
    recover: bool,
    root: impl FnOnce() -> Result<PathBuf>,
    pages: impl for<'a> FnOnce(&'a Path) -> Box<dyn ReceiptPages + 'a>,
) -> Result<Option<Value>> {
    let path = PathBuf::from(string(record, "transcript"));
    let id = string(record, "conversation_id");
    if uuid::Uuid::parse_str(id).is_err()
        || string(record, "run_id").is_empty()
        || !path.is_absolute()
        || !path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains(id))
    {
        return Ok(Some(json!({"items":{}})));
    }
    let lock_path = cache_path.with_extension("lock");
    let _guard = match deadline {
        None => lock(Some(&lock_path))?,
        Some(deadline) => match lock_until(&lock_path, deadline)? {
            Some(guard) => guard,
            None => return Ok(None),
        },
    };
    let file = File::open(&path).map_err(|e| e.to_string())?;
    let (identity, meta) =
        crate::platform::FileIdentity::with_metadata(&file).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader
        .by_ref()
        .take(256 * 1024)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let header: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if header["type"] != "session_meta" || header["payload"]["id"] != id {
        return Err("Conversation transcript identity mismatch".into());
    }
    // A native child is owned by its parent, not by the person at this terminal.
    // Do not infer this from forked_from_id: an interactive fork is still user-owned.
    if !header["payload"]["source"]["subagent"].is_null() {
        return Ok(Some(json!({"items":{}})));
    }
    let saved: Option<Value> = fs::read_to_string(cache_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    // Only a changed cache is serialized and written again.
    let mut changed = saved.is_none();
    let mut cache = saved.unwrap_or(json!({"items":{}}));
    let inode = json!([identity.device, identity.inode]);
    let offset = cache["offset"].as_u64().unwrap_or(0);
    if cache["inode"] != inode || offset > meta.len() {
        cache = json!({"items":{},"offset":0,"inode":inode});
        changed = true;
    }
    // Questions can stay unanswered long after they leave the recent-message
    // tail. Index the entire exact conversation once, then only appended bytes.
    // Reindex old tail-only caches while retaining explicit local dismissals.
    if cache["scan_version"] != 3 {
        changed = true;
        cache["offset"] = json!(0);
        cache["scan_version"] = json!(3);
        cache["active_turn"] = Value::Null;
        // Re-establish native call scope and turn binding. Keep exact local skips,
        // delivery receipts and recorded answers; reindexing cannot resend them.
        if let Some(items) = cache["items"].as_object_mut() {
            for card in items.values_mut() {
                card["accepted"] = json!(false);
                card.as_object_mut().unwrap().remove("native_turn_id");
            }
        }
    }
    let offset = cache["offset"].as_u64().unwrap_or(0);
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    changed |= cache["initialized"] != true;
    // Initial tail can begin inside a JSON line; never interpret that fragment.
    if cache.get("initialized").is_none() && offset > 0 {
        line.clear();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
    }
    let mut complete = true;
    loop {
        let before = reader.stream_position().map_err(|e| e.to_string())?;
        line.clear();
        let count = reader
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        worked(1, 0);
        if !line.ends_with('\n') {
            if count <= 1024 * 1024 {
                cache["offset"] = json!(before);
                break;
            }
            // An oversized line is skipped unparsed; it still counts against the deadline.
            reader.skip_until(b'\n').map_err(|e| e.to_string())?;
        }
        cache["offset"] = json!(reader.stream_position().map_err(|e| e.to_string())?);
        if count <= 1024 * 1024
            && (line.contains("request_user_input_async")
                || line.contains("function_call_output")
                || line.contains("\"user_message\"")
                || line.contains("\"task_started\"") || line.contains("\"turn_started\"")
                || line.contains("\"task_complete\"") || line.contains("\"turn_complete\"")
                || line.contains("\"turn_aborted\"")
                || line.contains("send_user_message_question_reply"))
        {
            if let Ok(event) = serde_json::from_str(&line) {
                observe(record, &mut cache, &event);
            }
        }
        // Lines appended while indexing do not count against the deadline.
        if deadline.is_some_and(|deadline| Instant::now() >= deadline)
            && cache["offset"].as_u64().unwrap_or(0) < meta.len()
        {
            complete = false;
            break;
        }
    }
    changed |= cache["offset"].as_u64().unwrap_or(0) != offset;
    let mut refused = None;
    if complete && recover {
        let root = root()?;
        let source = pages(&root);
        let recovered = recover_submissions(record, &mut cache, &root, source.as_ref(), deadline);
        match recovered {
            Ok((recovered, progressed)) => {
                complete = recovered;
                changed |= progressed;
            }
            // The transcript's progress is still kept.
            Err(error) => refused = Some(error),
        }
    }
    // The offset is at a line boundary, also after a partial scan.
    cache["initialized"] = json!(true);
    if changed {
        atomic(cache_path, &cache.to_string())?;
    }
    if let Some(error) = refused {
        return Err(error);
    }
    Ok(complete.then_some(cache))
}
pub(super) fn current(record: &Value) -> Result<Vec<Value>> {
    if string(record, "agent") != "codex" {
        return Ok(vec![]);
    }
    Ok(pending(record, &read_cache(record)?))
}
/// Pending questions, or None when the index is not complete by `deadline`.
pub(super) fn current_until(record: &Value, deadline: Instant) -> Result<Option<Vec<Value>>> {
    if string(record, "agent") != "codex" {
        return Ok(Some(vec![]));
    }
    Ok(index(record, &cache_path(record)?, Some(deadline))?.map(|cache| pending(record, &cache)))
}
fn pending(record: &Value, cache: &Value) -> Vec<Value> {
    cache["items"]
        .as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .filter(|c| c["accepted"] == true && c["response"].is_null() && c["skipped"] != true
            && (c["closed"].is_null() || matching_submission(record, c, &c["delivery"])))
        .cloned()
        .map(|mut card| {
            card["run_id"] = record["run_id"].clone();
            card["question_hash"] = json!(questions::fingerprint(&card));
            if matching_submission(record, &card, &card["delivery"]) {
                card["answer_delivery"] = card["delivery"].clone();
            }
            card.as_object_mut().unwrap().remove("delivery");
            card
        })
        .collect()
}
/// The question rows of an inspection, or None (unknown, never no rows) when
/// the index is not complete by `deadline`; its progress is kept.
pub(super) fn messages(record: &Value, deadline: Instant) -> Result<Option<Vec<Value>>> {
    if string(record, "agent") != "codex" {
        return Ok(Some(vec![]));
    }
    let Some(cache) = index(record, &cache_path(record)?, Some(deadline))? else {
        return Ok(None);
    };
    Ok(Some(cache["items"].as_object().into_iter().flat_map(|m|m.values()).filter(|c|c["accepted"]==true).map(|card|{
        let question=&card["questions"][0];
        let choices=question["options"].as_array().unwrap().iter().flat_map(|o|["\n- ",string(o,"label")]).collect::<String>();
        let status=if card["skipped"]==true {"Skipped"}else if !card["response"].is_null(){"Answered"}else if !card["closed"].is_null(){"Closed"}else{"Optional"};
        json!({"type":"AgentMessage","source":"codex_transcript","message_id":card["question_id"],"agent_id":"",
            "at":card["created_at"],"message_phase":"commentary","detail":format!("{}\n{}{}",status,string(question,"question"),choices)})
    }).collect()))
}
fn unambiguous(record: &Value, card: &Value) -> Result<bool> {
    Ok(unique(card, &current(record)?))
}
fn unique(card: &Value, pending: &[Value]) -> bool {
    pending
        .iter()
        .filter(|c| c["questions"] == card["questions"])
        .count()
        == 1
}
pub(super) fn response(record: &Value, card: &Value) -> Result<Option<Value>> {
    let cache = read_transcript(record)?;
    let response = &cache["items"][string(card, "question_id")]["response"];
    Ok((!response.is_null()).then(|| response.clone()))
}
fn panel_body(screen: &str, card: &Value) -> Result<Option<String>> {
    Ok(panel(screen, card)?.map(|(_, body)| body))
}
/// Codex animates its working status above a waiting question. Before input,
/// only this card's complete panel must be unchanged.
pub(super) fn same_panel(first: &str, second: &str, card: &Value) -> bool {
    matches!((panel(first, card), panel(second, card)), (Ok(Some((a, _))), Ok(Some((b, _)))) if a == b)
}
/// This card's visible panel rows, from its progress line or title through the
/// key hints, and the answer rows within them.
fn panel(screen: &str, card: &Value) -> Result<Option<(String, String)>> {
    let plain = question_terminal::plain(screen)?;
    let lines: Vec<_> = plain.lines().collect();
    let Some(footer) = lines.iter().rposition(|line| {
        // Codex 0.162 abbreviates the skip key as "^]".
        line.contains("enter submit") && (line.contains("ctrl+] skip") || line.contains("^] skip"))
    }) else {
        return Ok(None);
    };
    let Some(body_end) = (0..footer).rev().find(|i| !lines[*i].trim().is_empty()) else {
        return Ok(None);
    };
    let Some(separator) = (0..body_end).rev().find(|i| lines[*i].trim().is_empty()) else {
        return Ok(None);
    };
    let Some(title_end) = (0..separator).rev().find(|i| !lines[*i].trim().is_empty()) else {
        return Ok(None);
    };
    let mut title_start = (0..title_end)
        .rev()
        .find(|i| lines[*i].trim().is_empty())
        .map_or(0, |i| i + 1);
    let start = title_start;
    // Codex 0.162 numbers the questions of one call above the title ("1 of 2").
    // Only this card's own position may stand there.
    if let Some(shown) = progress(lines[title_start]) {
        if title_start == title_end || Some(shown) != position(card) {
            return Ok(None);
        }
        title_start += 1;
    }
    let compact = |s: &str| s.split_whitespace().collect::<String>();
    if compact(&lines[title_start..=title_end].join("\n"))
        != compact(string(&card["questions"][0], "question"))
    {
        return Ok(None);
    }
    let body = lines[separator + 1..=body_end].join("\n");
    let options = card["questions"][0]["options"]
        .as_array()
        .ok_or("missing options")?;
    if !options
        .iter()
        .all(|o| compact(&body).contains(&compact(string(o, "label"))))
    {
        return Ok(None);
    }
    Ok(Some((lines[start..=footer].join("\n"), body)))
}
fn progress(line: &str) -> Option<usize> {
    let (shown, total) = line.trim().split_once(" of ")?;
    let shown: usize = shown.parse().ok()?;
    (total.parse::<usize>().ok()? >= shown).then_some(shown)
}
fn position(card: &Value) -> Option<usize> {
    let index: usize = string(card, "question_id")
        .rsplit(':')
        .next()?
        .parse()
        .ok()?;
    Some(index + 1)
}
fn visible_panel(screen: &str, card: &Value) -> Result<bool> {
    Ok(panel_body(screen, card)?.is_some())
}
fn panel_empty(screen: &str, card: &Value) -> Result<bool> {
    let Some(body) = panel_body(screen, card)? else {
        return Ok(false);
    };
    let options = card["questions"][0]["options"].as_array().unwrap();
    if options.is_empty() {
        return Ok(body.trim() == "Type your answer");
    }
    let index = options.len() + 1;
    // An unselected Other row displays its saved draft instead of the label.
    // Never overwrite either a visible draft or one stored behind a selection.
    Ok(body.lines().last().is_some_and(|line| {
        let row = line.trim().trim_start_matches('›').trim();
        row == format!("{index}. Other") || row == format!("{index}. Other (write an answer)")
    }))
}
/// What a paste left in this card's panel, wrapped rows joined: the selected
/// Other row, or a free-form question's answer field. None while another row
/// is selected; an option label with the same text is not a draft.
fn other_draft(screen: &str, card: &Value) -> Result<Option<String>> {
    let Some(body) = panel_body(screen, card)? else {
        return Ok(None);
    };
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let options = card["questions"][0]["options"].as_array().map_or(0, Vec::len);
    if options == 0 {
        return Ok(Some(words(&body)));
    }
    let label = format!("{}. ", options + 1);
    let rows: Vec<_> = body.lines().collect();
    let Some(start) = rows.iter().rposition(|row| {
        row.trim_start()
            .strip_prefix('›')
            .is_some_and(|rest| rest.trim_start().starts_with(&label))
    }) else {
        return Ok(None);
    };
    let draft = words(&rows[start..].join("\n"));
    Ok(draft.strip_prefix('›').map(str::trim_start).and_then(|row| row.strip_prefix(&label)).map(str::to_owned))
}
/// The key hints below an open question panel: its last row offering submit and skip.
fn hints(screen: &str) -> Option<String> {
    let plain = question_terminal::plain(screen).ok()?;
    plain
        .lines()
        .rev()
        .find(|line| {
            line.contains("enter submit") && (line.contains("ctrl+] skip") || line.contains("^] skip"))
        })
        .map(str::to_owned)
}
/// Whether key hints name the visible row of a scrolled answer list ("option 1/6").
fn scrolled(hints: &str) -> bool {
    let words: Vec<_> = hints.split_whitespace().collect();
    words.windows(2).any(|pair| {
        pair[0] == "option"
            && pair[1].split_once('/').is_some_and(|(row, rows)| {
                !row.is_empty()
                    && !rows.is_empty()
                    && row.chars().chain(rows.chars()).all(|c| c.is_ascii_digit())
            })
    })
}
/// Codex 0.160.1 and 0.162.0 fit an open question panel taller than the pane
/// by scrolling its answer list below the whole title, and name the visible
/// row in the key hints. Such a panel is clipped; once the pane holds every
/// answer, the hints drop that name and the panel is complete.
fn clipped(screen: &str) -> bool {
    hints(screen).is_some_and(|row| scrolled(&row))
}
fn complete(screen: &str) -> bool {
    hints(screen).is_some_and(|row| !scrolled(&row))
}
/// Grow the pane no client views until this question's scrolled panel shows
/// every answer (`question_terminal::fit_unviewed`); the exact match in
/// `available` still decides. Only a question listing calls it. A title taller
/// than the pane pushes the key hints off screen, so that panel stays in Terminal.
pub(super) fn fit(record: &Value, card: &Value) {
    if card["answer_delivery"]["status"] == "submitted" {
        return;
    }
    let Ok(current) = Native::new(record, card).identity() else {
        return;
    };
    if let Ok(screen) = Native::capture(&current) {
        question_terminal::fit_unviewed(&current, screen, Native::capture, clipped, complete);
    }
}
pub(super) fn available(record: &Value, card: &Value) -> Result<()> {
    available_with(record, card, || current(record))
}
/// `available` against the pending Codex questions an inspection already
/// read from a complete index, without indexing again.
pub(super) fn available_among(record: &Value, card: &Value, pending: &[Value]) -> Result<()> {
    available_with(record, card, || Ok(pending.to_vec()))
}
fn available_with(
    record: &Value,
    card: &Value,
    pending: impl FnOnce() -> Result<Vec<Value>>,
) -> Result<()> {
    if card["answer_delivery"]["status"] == "submitted" {
        return Err("An answer was already submitted. Wait for Codex to record it.".into());
    }
    let mut terminal = Native::new(record, card);
    let screen = terminal.screen()?;
    if visible_panel(&screen, card)? {
        if !unique(card, &pending()?) {
            return Err(
                "Several pending questions have the same text. Choose one in Terminal.".into(),
            );
        }
        if !panel_empty(&screen, card)? {
            return Err("This question has a draft in Terminal. Finish or clear it there.".into());
        }
        return Ok(());
    }
    input::checked_terminal(record, true)
}
fn answer_text(card: &Value, answer: &Value) -> Result<String> {
    if let Some(id) = answer["selected_option_ids"]
        .as_array()
        .and_then(|a| a.first())
    {
        card["questions"][0]["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == *id)
            .map(|o| string(o, "label").to_owned())
            .ok_or("unknown answer option".into())
    } else {
        Ok(string(answer, "text").to_owned())
    }
}
fn envelope(card: &Value, text: &str) -> String {
    format!(
        "<send_user_message_question_reply>\n{}\n</send_user_message_question_reply>",
        json!([{"questionItemId":card["native_question_id"],"question":card["questions"][0]["question"],"answer":text}])
    )
}

fn matching_submission(record: &Value, card: &Value, delivery: &Value) -> bool {
    delivery["status"] == "submitted"
        && delivery["run_id"] == record["run_id"]
        && delivery["conversation_id"] == record["conversation_id"]
        && delivery["question_id"] == card["question_id"]
        && delivery["question_hash"] == questions::fingerprint(card)
}

fn submission(record: &Value, card: &Value, answers: &Value, request_id: &str) -> Value {
    json!({"status":"submitted","request_id":request_id,"run_id":record["run_id"],
        "conversation_id":record["conversation_id"],"question_id":card["question_id"],
        "question_hash":questions::fingerprint(card),"answers":answers,"submitted_at":now()})
}

fn save_submission(record: &Value, card: &Value, answers: &Value, request_id: &str) -> Result<()> {
    read_transcript(record)?;
    let path = cache_path(record)?;
    let _guard = lock(Some(&path.with_extension("lock")))?;
    let mut cache: Value =
        serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let saved = &mut cache["items"][string(card, "question_id")];
    if saved["questions"] != card["questions"]
        || saved["native_question_id"] != card["native_question_id"]
    {
        return Err("question changed after submission".into());
    }
    saved["delivery"] = submission(record, card, answers, request_id);
    atomic(&path, &cache.to_string())
}

/// One page of a session's question receipts (zerus-a4mub.8): their request
/// IDs and original files, which recovery reads and validates itself. Only a
/// complete page without a next cursor, in a chain that began at the head and
/// never had to restart, covers every receipt; anything else is unknown.
pub(super) struct ReceiptPage {
    pub(super) rows: Vec<(String, PathBuf)>,
    pub(super) next: Option<String>,
    pub(super) complete: bool,
    /// The source knows receipts it cannot attribute: nothing it covers is
    /// complete until they are read again.
    pub(super) gap: bool,
    pub(super) restart: bool,
}
/// The question receipts of exactly `record`'s name, run and conversation, in
/// pages of at most `limit`, after `after`. An expired deadline returns no
/// rows and keeps the cursor.
pub(super) trait ReceiptPages {
    fn page(
        &self,
        record: &Value,
        after: Option<&str>,
        limit: usize,
        deadline: Option<Instant>,
    ) -> Result<ReceiptPage>;
}
/// The receipt locator (zerus-a4mub.11): exact identity pages that never
/// enumerate the receipt folder; without a deadline it first reconciles the
/// question receipts, then answers Complete or fails.
struct Locator;
impl ReceiptPages for Locator {
    fn page(
        &self,
        record: &Value,
        after: Option<&str>,
        limit: usize,
        deadline: Option<Instant>,
    ) -> Result<ReceiptPage> {
        receipt_index::question_page(
            string(record, "name"),
            string(record, "run_id"),
            string(record, "conversation_id"),
            after,
            limit,
            deadline,
        )
        .map(from_locator)
    }
}
/// Only complete coverage can complete recovery, and a restart always
/// reaches it.
fn from_locator(page: receipt_index::QuestionReceiptPage) -> ReceiptPage {
    ReceiptPage {
        rows: page
            .rows
            .into_iter()
            .map(|row| (row.request_id, row.path))
            .collect(),
        next: page.next_cursor,
        complete: page.coverage == receipt_index::Coverage::Complete,
        gap: page.coverage == receipt_index::Coverage::Gap,
        restart: page.restart_required,
    }
}

// Older adapters left an in-progress question receipt after successfully
// entering its envelope. Recover only a confirmed question submission or the
// exact durable terminal acknowledgement; a timeout or a queue preview alone
// never proves submission. A replaced question cache also recovers its
// submitted answers here, so pending questions stay unknown until recovery
// is complete.
//
// A completed recovery is never trusted later: every call validates the
// receipt locator's current coverage from the head, so a receipt or a gap
// recorded since is seen (zerus-a4mub.8 review). A bounded call (with a
// deadline) reads one page of at most RECEIPTS_PER_CALL receipts, which never
// enumerates the receipt folder, and keeps an unfinished chain's cursor in
// the cache (`submission_scan`), so the next call resumes it. An unbounded
// call (an answer) starts at the head, reads every page, and fails rather
// than act when the pages cannot cover this run's receipts. A gap fails both.
// Returns whether recovery is complete and whether the cache changed.
fn recover_submissions(
    record: &Value,
    cache: &mut Value,
    root: &Path,
    pages: &dyn ReceiptPages,
    deadline: Option<Instant>,
) -> Result<(bool, bool)> {
    // An older cache's completion marker grants nothing.
    let mut changed = cache
        .as_object_mut()
        .ok_or("Invalid question cache")?
        .remove("submission_scan_run")
        .is_some();
    // Another run's unfinished chain starts again for this one, and an
    // answer always starts at the head, where its source seals every receipt.
    if cache["submission_scan"]["run_id"] != record["run_id"]
        || deadline.is_none() && !cache["submission_scan"]["after"].is_null()
    {
        cache["submission_scan"] = json!({"run_id": record["run_id"], "after": null});
        changed = true;
    }
    let mut restarts = 0;
    loop {
        let after = cache["submission_scan"]["after"]
            .as_str()
            .map(str::to_owned);
        let page = pages.page(record, after.as_deref(), RECEIPTS_PER_CALL, deadline)?;
        if page.gap {
            return Err(INCOMPLETE_RECEIPTS.into());
        }
        if page.restart {
            // A cursor its source no longer accepts: start again at the head.
            cache["submission_scan"]["after"] = Value::Null;
            changed = true;
            restarts += 1;
            if deadline.is_some() {
                return Ok((false, changed));
            }
            if restarts > 3 {
                return Err(INCOMPLETE_RECEIPTS.into());
            }
            continue;
        }
        for (request_id, path) in &page.rows {
            recover_receipt(record, cache, root, request_id, path);
        }
        changed |= !page.rows.is_empty();
        if page.complete && page.next.is_none() {
            // Complete now; the next call validates again from the head.
            if after.is_some() {
                cache["submission_scan"]["after"] = Value::Null;
                changed = true;
            }
            return Ok((true, changed));
        }
        if let Some(next) = page
            .next
            .as_deref()
            .filter(|next| after.as_deref() != Some(*next))
        {
            cache["submission_scan"]["after"] = json!(next);
            changed = true;
            if deadline.is_none() {
                continue;
            }
        } else if deadline.is_none() {
            return Err(INCOMPLETE_RECEIPTS.into());
        }
        return Ok((false, changed));
    }
}
/// An answer (an unbounded call) never acts on incomplete receipt evidence.
const INCOMPLETE_RECEIPTS: &str =
    "Earlier answers to this session's questions cannot all be checked yet; refresh before answering";
/// One question receipt: a confirmed submission of this exact run and
/// question, or its exact durable terminal acknowledgement, becomes the
/// question's delivery. Anything else is left alone.
fn recover_receipt(record: &Value, cache: &mut Value, root: &Path, row: &str, path: &Path) {
    worked(0, 1);
    // A regular file only, as its directory entry describes it.
    if !fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() <= 256 * 1024) {
        return;
    }
    let Some(receipt): Option<Value> = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    else {
        return;
    };
    let request_id = string(&receipt, "request_id");
    if uuid::Uuid::parse_str(request_id).is_err()
        || request_id != row
        || receipt["run_id"] != record["run_id"]
        || receipt["conversation_id"] != record["conversation_id"]
        || receipt["name"] != record["name"]
        || !["in_progress", "submitted"].contains(&string(&receipt, "status"))
    {
        return;
    }
    let mut card = cache["items"][string(&receipt, "question_id")].clone();
    card["run_id"] = record["run_id"].clone();
    if card["source"] != "codex_async"
        || receipt["question_hash"] != questions::fingerprint(&card)
        || receipt["answers"].as_array().is_none_or(|a| a.len() != 1)
        || receipt["answers"][0]["skip"] == true
        || receipt["answers"][0]["question_id"] != card["questions"][0]["id"]
    {
        return;
    }
    if receipt["status"] == "submitted" {
        if !matching_submission(record, &card, &card["delivery"]) {
            cache["items"][string(&receipt, "question_id")]["delivery"] =
                submission(record, &card, &receipt["answers"], request_id);
        }
        return;
    }
    let input_path = root
        .join("input_receipts")
        .join(format!("{request_id}.json"));
    if !fs::metadata(&input_path).is_ok_and(|m| m.is_file() && m.len() <= 256 * 1024) {
        return;
    }
    let Some(input): Option<Value> = fs::read_to_string(input_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    else {
        return;
    };
    let Ok(text) = answer_text(&card, &receipt["answers"][0]) else {
        return;
    };
    if input["status"] == "submitted"
        && input["request_id"] == request_id
        && input["name"] == record["name"]
        && input["run_id"] == record["run_id"]
        && input["conversation_id"] == record["conversation_id"]
        && input["text"] == envelope(&card, &text)
        && !matching_submission(record, &card, &card["delivery"])
    {
        cache["items"][string(&receipt, "question_id")]["delivery"] =
            submission(record, &card, &receipt["answers"], request_id);
    }
}

/// A failure once input may have reached Codex (`touched`) is never a
/// refusal: the answer, or its Skip, may already be there.
fn after_input(touched: bool, error: String) -> String {
    if touched && !error.contains("delivery uncertain") {
        format!("delivery uncertain: {error}")
    } else {
        error
    }
}

/// Codex's own Skip of `card`: its key while the panel is `visible`, then
/// the skip recorded in the question cache at `path`. Once the key may have
/// reached Codex (`touched`), every later failure, the key's own included,
/// is uncertain: it may already have dismissed the panel.
fn skip<T: Transport>(
    terminal: &mut T,
    touched: fn(&T) -> bool,
    visible: bool,
    path: &Path,
    card: &Value,
) -> Result<()> {
    let skipped = (|| -> Result<()> {
        if visible {
            terminal.key("C-]")?;
        }
        let _guard = lock(Some(&path.with_extension("lock")))?;
        let mut cache: Value =
            serde_json::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        cache["items"][string(card, "question_id")]["skipped"] = json!(true);
        atomic(path, &cache.to_string())
    })();
    skipped.map_err(|error| after_input(touched(terminal), error))
}

pub(super) fn answer(
    record: &Value,
    card: &Value,
    answers: &Value,
    request_id: &str,
) -> Result<&'static str> {
    let mut terminal = Native::new(record, card);
    let screen = terminal.screen()?;
    if answers[0]["skip"] == true {
        let visible = visible_panel(&screen, card)? && unambiguous(record, card)?;
        skip(
            &mut terminal,
            Native::touched,
            visible,
            &cache_path(record)?,
            card,
        )?;
        return Ok("answered");
    }
    available(record, card)?;
    let text = answer_text(card, &answers[0])?;
    if visible_panel(&screen, card)? {
        // Native paste selects Other and submits its own exact question envelope.
        let result: Result<()> = (|| {
            terminal.paste(&text)?;
            std::thread::sleep(Duration::from_millis(180));
            let screen = terminal.screen()?;
            // Codex wraps a long answer onto indented rows of its Other field.
            let pasted = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if other_draft(&screen, card)? != Some(pasted) {
                return Err("question or answer draft changed before submission".into());
            }
            terminal.key("Enter")
        })();
        if let Err(error) = result {
            return Err(after_input(terminal.touched(), error));
        }
    } else {
        // Its own submission refuses pending settings, whose keys would
        // reach the terminal before this answer: only its paste may.
        let payload = json!({"request_id":request_id,"text":envelope(card,&text),"expected_run_id":record["run_id"],"expected_conversation_id":record["conversation_id"]});
        input::submit_answer(string(record, "name"), payload.to_string().as_bytes())?;
    }
    save_submission(record, card, answers, request_id)
        .map_err(|e| format!("delivery uncertain: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(response) =
            response(record, card).map_err(|e| format!("delivery uncertain: {e}"))?
        {
            return if response["response"]["answers"][string(&card["questions"][0], "question")]
                == text
            {
                Ok("answered")
            } else {
                Err("delivery uncertain: question received a different answer".into())
            };
        }
        if Instant::now() >= deadline {
            return Ok("submitted");
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Value {
        json!({"type":"response_item","timestamp":"2026-10-07T15:00:00Z","payload":{"type":"function_call","name":"request_user_input_async","call_id":"call_one","arguments":json!({"questions":[{"title":"Which option?","options":["One","Two"]},{"title":"Any notes?"}]}).to_string()}})
    }
    #[test]
    fn accepted_is_not_answered_and_each_optional_question_has_an_exact_identity() {
        let record = json!({"run_id":"run","conversation_id":"conv"});
        let mut cache = json!({"items":{}});
        observe(&record, &mut cache, &request());
        assert_eq!(cache["items"].as_object().unwrap().len(), 2);
        observe(
            &record,
            &mut cache,
            &json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call_one","output":"{\"accepted\":true}"}}),
        );
        let card = cache["items"]["codex-async:call_one:0"].clone();
        assert!(card["response"].is_null());
        assert_eq!(card["optional"], true);
        let reply = envelope(&card, "One");
        assert_eq!(
            parse_reply(&reply)[0]["questionItemId"],
            "[\"request_user_input_async\",\"call_one\",0]"
        );
        let wrong = reply.replace("call_one", "call_other");
        observe(
            &record,
            &mut cache,
            &json!({"type":"event_msg","payload":{"type":"user_message","message":wrong}}),
        );
        assert!(cache["items"]["codex-async:call_one:0"]["response"].is_null());
        observe(
            &record,
            &mut cache,
            &json!({"type":"event_msg","payload":{"type":"user_message","message":reply}}),
        );
        assert!(!cache["items"]["codex-async:call_one:0"]["response"].is_null());
        assert!(cache["items"]["codex-async:call_one:1"]["response"].is_null());
        observe(&record, &mut cache, &request());
        assert!(!cache["items"]["codex-async:call_one:0"]["response"].is_null());
        assert_eq!(
            cache["items"]["codex-async:call_one:1"]["questions"][0]["options"],
            json!([])
        );
    }
    fn boundary(kind: &str, turn: &str) -> Value {
        json!({"type":"event_msg","payload":{"type":kind,"turn_id":turn}})
    }
    fn accepted_questions() -> (Value, Value) {
        let record = json!({"run_id":"run","conversation_id":"conv"});
        let mut cache = json!({"items":{}});
        observe(&record, &mut cache, &boundary("task_started", "original-turn"));
        observe(&record, &mut cache, &request());
        observe(&record, &mut cache, &json!({"type":"response_item","payload":{
            "type":"function_call_output","call_id":"call_one","output":"{\"accepted\":true}"}}));
        (record, cache)
    }
    #[test]
    fn native_turn_completion_closes_without_inventing_answers_or_reopening_on_acceptance() {
        for kind in ["task_complete", "turn_complete", "turn_aborted"] {
            let (record, mut cache) = accepted_questions();
            assert_eq!(pending(&record, &cache).len(), 2);
            observe(&record, &mut cache, &boundary(kind, "other-turn"));
            assert_eq!(pending(&record, &cache).len(), 2);
            observe(&record, &mut cache, &boundary(kind, ""));
            assert_eq!(pending(&record, &cache).len(), 2);
            observe(&record, &mut cache, &boundary(kind, "original-turn"));
            assert!(pending(&record, &cache).is_empty());
            assert!(cache["items"]["codex-async:call_one:0"]["response"].is_null());
            observe(&record, &mut cache, &json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"call_one","output":"{\"accepted\":true}"}}));
            assert!(pending(&record, &cache).is_empty());
            observe(&record, &mut cache, &boundary("task_started", "next-turn"));
            let mut next = request(); next["payload"]["call_id"] = json!("call_next");
            observe(&record, &mut cache, &next);
            observe(&record, &mut cache, &json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"call_next","output":"{\"accepted\":true}"}}));
            observe(&record, &mut cache, &boundary(kind, "original-turn"));
            let cards = pending(&record, &cache);
            assert_eq!(cards.len(), 2);
            assert!(cards.iter().all(|card| card["tool_call_id"] == "call_next"));
        }
    }
    #[test]
    fn only_committed_follow_up_closes_the_remaining_questions() {
        let (record, mut cache) = accepted_questions();
        let card = cache["items"]["codex-async:call_one:0"].clone();
        observe(&record, &mut cache, &json!({"type":"event_msg","payload":{
            "type":"user_message","message":envelope(&card,"One")}}));
        assert_eq!(pending(&record, &cache).len(), 1);
        assert!(!cache["items"]["codex-async:call_one:0"]["response"].is_null());
        for event in [json!({"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":"Injected context"}]}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"Continuing work"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":""}})] {
            observe(&record, &mut cache, &event);
            assert_eq!(pending(&record, &cache).len(), 1);
        }
        observe(&record, &mut cache, &json!({"type":"event_msg","payload":{
            "type":"user_message","message":"Continue with the defaults."}}));
        assert!(pending(&record, &cache).is_empty());
        assert!(cache["items"]["codex-async:call_one:1"]["response"].is_null());
    }
    #[test]
    fn closing_a_question_keeps_its_exact_submitted_answer_until_native_acknowledgement() {
        let (record, mut cache) = accepted_questions();
        let card = cache["items"]["codex-async:call_one:0"].clone();
        cache["items"]["codex-async:call_one:0"]["delivery"] = submission(&record, &card,
            &json!([{"question_id":"q_0","text":"One"}]), "original-request");
        observe(&record, &mut cache, &boundary("task_complete", "original-turn"));
        let cards = pending(&record, &cache);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["answer_delivery"]["request_id"], "original-request");
        assert!(cache["items"]["codex-async:call_one:0"]["response"].is_null());
        let mut other_run = record.clone(); other_run["run_id"] = json!("another-run");
        assert!(pending(&other_run, &cache).is_empty());
        observe(&record, &mut cache, &json!({"type":"event_msg","payload":{
            "type":"user_message","message":envelope(&card,"One")}}));
        assert!(pending(&record, &cache).is_empty());
        assert_eq!(cache["items"]["codex-async:call_one:0"]["response"]["type"], "interaction.resolved");
    }
    #[test]
    fn child_sources_and_foreign_namespaces_never_become_user_questions() {
        let (record, mut cache) = accepted_questions();
        let mut foreign = request();
        foreign["payload"]["namespace"] = json!("collaboration");
        foreign["payload"]["call_id"] = json!("child_question");
        observe(&record, &mut cache, &foreign);
        observe(&record, &mut cache, &json!({"type":"response_item","payload":{
            "type":"function_call_output","call_id":"child_question","output":"{\"accepted\":true}"}}));
        assert_eq!(pending(&record, &cache).len(), 2);
        for child in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let path = dir.path().join(format!("rollout-{id}.jsonl"));
            let source = if child { json!({"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}) } else { json!("cli") };
            let rows = [json!({"type":"session_meta","payload":{"id":id,"source":source,"forked_from_id":"parent"}}),
                request(), json!({"type":"response_item","payload":{"type":"function_call_output",
                    "call_id":"call_one","output":"{\"accepted\":true}"}})];
            fs::write(&path, rows.iter().map(|row| format!("{row}\n")).collect::<String>()).unwrap();
            let record = json!({"run_id":"run","conversation_id":id,"transcript":path});
            // A private root: recovery never reaches this machine's receipts.
            let root = dir.path().join("state");
            let cache = bounded(
                &record,
                &dir.path().join("cache.json"),
                &root,
                &Rows::default(),
                None,
            )
            .unwrap();
            assert_eq!(pending(&record, &cache).len(), if child { 0 } else { 2 });
        }
    }
    #[test]
    fn version_two_cache_rebuilds_turn_closures_and_preserves_local_delivery_and_skip() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = dir.path().join(format!("rollout-{id}.jsonl"));
        let rows = [json!({"type":"session_meta","payload":{"id":id}}),
            boundary("task_started", "original-turn"), request(),
            json!({"type":"response_item","payload":{"type":"function_call_output",
                "call_id":"call_one","output":"{\"accepted\":true}"}}),
            boundary("task_complete", "original-turn")];
        fs::write(&path, rows.iter().map(|row| format!("{row}\n")).collect::<String>()).unwrap();
        let (_, mut cache) = accepted_questions();
        cache["items"]["codex-async:call_one:0"]["skipped"] = json!(true);
        cache["items"]["codex-async:call_one:1"]["delivery"] = json!({"request_id":"original-delivery"});
        let file = File::open(&path).unwrap();
        let (identity, meta) = crate::platform::FileIdentity::with_metadata(&file).unwrap();
        cache["inode"] = json!([identity.device, identity.inode]);
        cache["offset"] = json!(meta.len()); cache["scan_version"] = json!(2);
        cache["submission_scan_run"] = json!("run");
        let cache_path = dir.path().join("cache.json");
        fs::write(&cache_path, cache.to_string()).unwrap();
        let record = json!({"run_id":"run","conversation_id":id,"transcript":path});
        // A private root: recovery never reaches this machine's receipts.
        let root = dir.path().join("state");
        let cache = bounded(&record, &cache_path, &root, &Rows::default(), None).unwrap();
        assert_eq!(cache["scan_version"], 3);
        assert!(pending(&record, &cache).is_empty());
        assert_eq!(cache["items"]["codex-async:call_one:0"]["skipped"], true);
        assert_eq!(cache["items"]["codex-async:call_one:1"]["delivery"]["request_id"], "original-delivery");
    }
    #[test]
    fn bounded_index_saves_its_progress_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let transcript = dir.path().join(format!("rollout-{id}.jsonl"));
        let accepted = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call_one","output":"{\"accepted\":true}"}});
        let mut lines = vec![json!({"type":"session_meta","payload":{"id":id}}), request(), accepted];
        lines.extend((0..5).map(|i| json!({"type":"response_item","payload":{"type":"function_call_output","call_id":format!("call-{i}"),"output":"x"}})));
        fs::write(&transcript, lines.iter().map(|line| line.to_string() + "\n").collect::<String>()).unwrap();
        let record = json!({"agent":"codex","run_id":"run","conversation_id":id,"transcript":transcript});
        let cache = dir.path().join("cache.json");
        let meta = fs::metadata(&transcript).unwrap();
        let source = crate::platform::file_identity(&transcript).unwrap();
        // An older cache's completion marker grants nothing; recovery uses
        // a private root and never reaches this machine's receipts.
        fs::write(&cache, json!({"items":{},"offset":0,"inode":[source.device,source.inode],"scan_version":2,
            "initialized":true,"submission_scan_run":"run"}).to_string()).unwrap();
        let root = dir.path().join("state");
        let none = Rows::default();
        // An expired deadline still indexes one line per call and keeps it,
        // and stays unknown: it recovers nothing.
        let expired = Instant::now();
        let mut offsets = Vec::new();
        while offsets.last() != Some(&meta.len()) {
            assert!(offsets.len() < lines.len(), "indexing made no progress");
            assert_eq!(bounded(&record, &cache, &root, &none, Some(expired)), None);
            let saved: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
            offsets.push(saved["offset"].as_u64().unwrap());
        }
        // Every line ended a call, the header included.
        assert_eq!(offsets.len(), lines.len());
        assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]));
        let budget = Some(Instant::now() + Duration::from_secs(30));
        let complete = bounded(&record, &cache, &root, &none, budget).unwrap();
        assert_eq!(complete["offset"], json!(meta.len()));
        assert_eq!(pending(&record, &complete).len(), 2);
        assert_eq!(bounded(&record, &cache, &root, &none, None), Some(complete));
        // A cache held by another process past the deadline is unknown, not empty.
        let _held = lock(Some(&cache.with_extension("lock"))).unwrap();
        let held = Some(Instant::now() + Duration::from_millis(30));
        assert_eq!(bounded(&record, &cache, &root, &none, held), None);
    }
    /// The work this thread did while `f` ran: transcript lines, receipts.
    fn work<T>(f: impl FnOnce() -> T) -> (T, (usize, usize)) {
        let before = WORK.with(|work| work.get());
        let value = f();
        let after = WORK.with(|work| work.get());
        (value, (after.0 - before.0, after.1 - before.1))
    }
    /// zerus-a4mub.8: an in-memory receipt locator over exact rows, in
    /// request-ID order, honouring the page contract.
    #[derive(Clone, Default)]
    struct Rows {
        rows: Vec<(String, PathBuf)>,
        /// Report partial coverage instead of the last page.
        partial: bool,
        /// Report a gap (receipts the source cannot attribute) on every page.
        gap: bool,
        /// Refuse this cursor once, as a page begun before a new head.
        restart: std::rc::Rc<std::cell::RefCell<Option<String>>>,
        pages: std::rc::Rc<std::cell::Cell<usize>>,
        /// The cursor of every page requested without a deadline.
        unbounded: std::rc::Rc<std::cell::RefCell<Vec<Option<String>>>>,
    }
    impl ReceiptPages for Rows {
        fn page(
            &self,
            _: &Value,
            after: Option<&str>,
            limit: usize,
            deadline: Option<Instant>,
        ) -> Result<ReceiptPage> {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(ReceiptPage {
                    rows: vec![],
                    next: after.map(str::to_owned),
                    complete: false,
                    gap: false,
                    restart: false,
                });
            }
            self.pages.set(self.pages.get() + 1);
            if deadline.is_none() {
                self.unbounded.borrow_mut().push(after.map(str::to_owned));
            }
            if after.is_some() && self.restart.borrow().as_deref() == after {
                self.restart.replace(None);
                return Ok(ReceiptPage {
                    rows: vec![],
                    next: None,
                    complete: false,
                    gap: false,
                    restart: true,
                });
            }
            let rest: Vec<_> = self
                .rows
                .iter()
                .filter(|(id, _)| after.is_none_or(|after| id.as_str() > after))
                .cloned()
                .collect();
            let rows: Vec<_> = rest.iter().take(limit).cloned().collect();
            let more = rest.len() > rows.len();
            Ok(ReceiptPage {
                next: rows.last().filter(|_| more).map(|(id, _)| id.clone()),
                complete: !more && !self.partial,
                gap: self.gap,
                rows,
                restart: false,
            })
        }
    }
    /// The items without their recovery times.
    fn settled(cache: &Value) -> Value {
        let mut items = cache["items"].clone();
        for card in items.as_object_mut().unwrap().values_mut() {
            if let Some(delivery) = card["delivery"].as_object_mut() {
                delivery.remove("submitted_at");
            }
        }
        items
    }
    fn bounded(
        record: &Value,
        cache: &Path,
        root: &Path,
        rows: &Rows,
        deadline: Option<Instant>,
    ) -> Option<Value> {
        try_bounded(record, cache, root, rows, deadline).unwrap()
    }
    fn try_bounded(
        record: &Value,
        cache: &Path,
        root: &Path,
        rows: &Rows,
        deadline: Option<Instant>,
    ) -> Result<Option<Value>> {
        let rows = rows.clone();
        index_with(
            record,
            cache,
            deadline,
            true,
            || Ok(root.to_path_buf()),
            move |_| Box::new(rows),
        )
    }
    fn second_request() -> Value {
        json!({"type":"response_item","timestamp":"2026-10-07T15:02:00Z","payload":{"type":"function_call","name":"request_user_input_async","call_id":"call_two","arguments":json!({"questions":[{"title":"Second?","options":["Yes","No"]}]}).to_string()}})
    }
    fn accepted(call: &str) -> Value {
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":call,"output":"{\"accepted\":true}"}})
    }
    /// A session whose first question is answered and second closed by its
    /// turn, then `filler` lines, then a third question that stays pending.
    fn closing_session(dir: &Path, filler: usize) -> Value {
        let id = uuid::Uuid::new_v4().to_string();
        let transcript = dir.join(format!("rollout-{id}.jsonl"));
        let (_, cache) = accepted_questions();
        let first = &cache["items"]["codex-async:call_one:0"];
        let mut lines = vec![
            json!({"type":"session_meta","payload":{"id":id}}),
            boundary("task_started", "t1"),
            request(),
            accepted("call_one"),
        ];
        lines.extend((0..filler).map(|i| json!({"type":"response_item","payload":{"type":"function_call_output","call_id":format!("other-{i}"),"output":"x"}})));
        lines.extend([json!({"type":"event_msg","timestamp":"2026-10-07T15:01:00Z","payload":{"type":"user_message","message":envelope(first,"One")}}),
            boundary("task_complete", "t1"), boundary("task_started", "t2"), second_request(), accepted("call_two")]);
        fs::write(&transcript, lines.iter().map(|line| line.to_string() + "\n").collect::<String>()).unwrap();
        json!({"agent":"codex","name":"codex/p/one","run_id":"run","conversation_id":id,"transcript":transcript})
    }
    /// An exact submitted receipt for `question_id`, and `others` of other runs.
    fn receipts(
        root: &Path,
        record: &Value,
        card: &Value,
        others: usize,
    ) -> (String, Vec<(String, PathBuf)>) {
        let folder = root.join("question_receipts");
        fs::create_dir_all(&folder).unwrap();
        let mut rows = Vec::new();
        let mut write = |receipt: Value| {
            let id = string(&receipt, "request_id").to_owned();
            let path = folder.join(format!("{id}.json"));
            fs::write(&path, receipt.to_string()).unwrap();
            rows.push((id, path));
        };
        let exact = uuid::Uuid::new_v4().to_string();
        let mut card = card.clone();
        card["run_id"] = record["run_id"].clone();
        write(
            json!({"status":"submitted","request_id":exact,"name":record["name"],"run_id":record["run_id"],
            "conversation_id":record["conversation_id"],"question_id":card["question_id"],
            "question_hash":questions::fingerprint(&card),"answers":[{"question_id":"q_0","selected_option_ids":["opt_0"]}]}),
        );
        for _ in 0..others {
            write(
                json!({"status":"submitted","request_id":uuid::Uuid::new_v4().to_string(),"name":record["name"],
                "run_id":"another-run","conversation_id":record["conversation_id"],"question_id":card["question_id"],
                "question_hash":questions::fingerprint(&card),"answers":[{"question_id":"q_0","selected_option_ids":["opt_0"]}]}),
            );
        }
        rows.sort();
        (exact, rows)
    }
    /// zerus-a4mub.8: a cold index of a large transcript reads one line per
    /// expired call and stays unknown; an expired call never reads a
    /// receipt; a bounded call reads at most one page. The result is exactly
    /// the unbounded one: the answer, the turn's closure and the submission.
    #[test]
    fn a_cold_index_and_receipt_recovery_are_bounded_and_close_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let record = closing_session(dir.path(), 2000);
        // The pending third question's identity, from a complete index elsewhere.
        let reference = dir.path().join("reference.json");
        let empty = Rows::default();
        let full = bounded(&record, &reference, &root, &empty, None).unwrap();
        let third = full["items"]["codex-async:call_two:0"].clone();
        let (exact, rows) = receipts(&root, &record, &third, 300);
        let rows = Rows {
            rows,
            ..Rows::default()
        };
        let cache = dir.path().join("cache.json");
        let lines = fs::read_to_string(string(&record, "transcript"))
            .unwrap()
            .lines()
            .count();
        // Cold: one line per expired call, unknown (never an empty list) until the end.
        let mut calls = 0;
        loop {
            let (result, (read, opened)) =
                work(|| bounded(&record, &cache, &root, &rows, Some(Instant::now())));
            assert!(
                result.is_none(),
                "an expired call completed with receipts unread"
            );
            assert!(
                read <= 1 && opened == 0,
                "call {calls} read {read} lines and {opened} receipts"
            );
            calls += 1;
            let saved: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
            if saved["offset"] == json!(fs::metadata(string(&record, "transcript")).unwrap().len())
            {
                break;
            }
            assert!(calls <= lines, "indexing made no progress");
        }
        // Every line ended a call, the header included.
        assert_eq!(calls, lines);
        // Expired calls make no recovery progress and read nothing.
        for _ in 0..3 {
            let (result, (read, opened)) =
                work(|| bounded(&record, &cache, &root, &rows, Some(Instant::now())));
            assert_eq!((result, read, opened), (None, 0, 0));
        }
        assert_eq!(rows.pages.get(), 0);
        // A budget reads at most one page per call until this run's recovery is complete.
        let budget = || Some(Instant::now() + Duration::from_secs(30));
        let mut pages = 0;
        let complete = loop {
            let (result, (read, opened)) =
                work(|| bounded(&record, &cache, &root, &rows, budget()));
            assert_eq!(read, 0);
            assert!(
                opened <= RECEIPTS_PER_CALL,
                "a bounded call read {opened} receipts"
            );
            pages += 1;
            if let Some(cache) = result {
                break cache;
            }
            assert!(
                pages <= 301_usize.div_ceil(RECEIPTS_PER_CALL),
                "recovery made no progress"
            );
        };
        assert_eq!(pages, 301_usize.div_ceil(RECEIPTS_PER_CALL));
        // Exactly the unbounded result, with the same submission.
        let unbounded =
            bounded(&record, &dir.path().join("fresh.json"), &root, &rows, None).unwrap();
        assert_eq!(settled(&complete), settled(&unbounded));
        let items = &complete["items"];
        assert_eq!(
            items["codex-async:call_one:0"]["response"]["response"]["answers"]["Which option?"],
            "One"
        );
        assert_eq!(items["codex-async:call_one:1"]["closed"], "task_complete");
        assert_eq!(
            items["codex-async:call_two:0"]["delivery"]["request_id"],
            exact.as_str()
        );
        let pending = pending(&record, &complete);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["answer_delivery"]["request_id"], exact.as_str());
        // A completed recovery is validated again from the head: this
        // source's 301 rows take the same five bounded pages, and the
        // result is the same.
        let mut again = 0;
        let validated = loop {
            let (result, (read, opened)) =
                work(|| bounded(&record, &cache, &root, &rows, budget()));
            assert_eq!(read, 0);
            assert!(opened <= RECEIPTS_PER_CALL);
            again += 1;
            if let Some(cache) = result {
                break cache;
            }
            assert!(again <= 301_usize.div_ceil(RECEIPTS_PER_CALL));
        };
        assert_eq!(again, 301_usize.div_ceil(RECEIPTS_PER_CALL));
        assert_eq!(settled(&validated), settled(&complete));
    }
    /// zerus-a4mub.8: a page that must restart, or a gap, is never complete
    /// for a bounded call, and an answer (unbounded) refuses to act on a gap.
    /// Large receipt counts are read one page per call.
    #[test]
    fn recovery_never_completes_from_restarted_or_gapped_pages() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let record = closing_session(dir.path(), 0);
        let full = bounded(
            &record,
            &dir.path().join("reference.json"),
            &root,
            &Rows::default(),
            None,
        )
        .unwrap();
        let third = full["items"]["codex-async:call_two:0"].clone();
        let (exact, mut rows) = receipts(&root, &record, &third, 200);
        // Large counts: ids beyond the files are rows whose originals are gone.
        rows.extend((0..5000).map(|i| {
            (
                format!("zz-{i:05}"),
                root.join("question_receipts")
                    .join(format!("gone-{i}.json")),
            )
        }));
        let budget = || Some(Instant::now() + Duration::from_secs(30));
        let cache = dir.path().join("cache.json");
        // A restart sends the cursor back to the head; nothing completes from it.
        let restart = Rows {
            rows: rows.clone(),
            ..Rows::default()
        };
        restart
            .restart
            .replace(Some(rows[RECEIPTS_PER_CALL - 1].0.clone()));
        assert_eq!(bounded(&record, &cache, &root, &restart, budget()), None);
        let saved: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!(
            saved["submission_scan"]["after"],
            rows[RECEIPTS_PER_CALL - 1].0.as_str()
        );
        assert_eq!(bounded(&record, &cache, &root, &restart, budget()), None);
        let saved: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!(saved["submission_scan"]["after"], Value::Null);
        let mut calls = 0;
        let complete = loop {
            let (result, (_, opened)) =
                work(|| bounded(&record, &cache, &root, &restart, budget()));
            assert!(opened <= RECEIPTS_PER_CALL);
            calls += 1;
            if let Some(cache) = result {
                break cache;
            }
            assert!(
                calls <= rows.len().div_ceil(RECEIPTS_PER_CALL),
                "recovery made no progress"
            );
        };
        assert_eq!(calls, rows.len().div_ceil(RECEIPTS_PER_CALL));
        assert_eq!(
            complete["items"]["codex-async:call_two:0"]["delivery"]["request_id"],
            exact.as_str()
        );
        // Partial coverage stays unknown for every bounded call.
        let partial = Rows {
            rows: rows.clone(),
            partial: true,
            ..Rows::default()
        };
        let cache = dir.path().join("partial.json");
        for _ in 0..rows.len().div_ceil(RECEIPTS_PER_CALL) + 3 {
            assert_eq!(bounded(&record, &cache, &root, &partial, budget()), None);
        }
        // An answer never acts on it: it fails instead, and the cursor stays.
        let refused = try_bounded(&record, &cache, &root, &partial, None);
        assert_eq!(refused, Err(INCOMPLETE_RECEIPTS.to_owned()));
        assert_eq!(bounded(&record, &cache, &root, &partial, budget()), None);
        // A gap fails a bounded call and an answer alike, and a cold
        // transcript's progress is still kept.
        let gap = Rows {
            rows: rows.clone(),
            gap: true,
            ..Rows::default()
        };
        let cold = dir.path().join("gap.json");
        assert_eq!(
            try_bounded(&record, &cold, &root, &gap, budget()),
            Err(INCOMPLETE_RECEIPTS.to_owned())
        );
        let saved: Value = serde_json::from_str(&fs::read_to_string(&cold).unwrap()).unwrap();
        assert_eq!(
            saved["offset"],
            json!(fs::metadata(string(&record, "transcript")).unwrap().len())
        );
        assert_eq!(
            try_bounded(&record, &cold, &root, &gap, None),
            Err(INCOMPLETE_RECEIPTS.to_owned())
        );
        // Without the gap, the same answer reads every page and completes,
        // starting at the head rather than at the bounded cursor.
        let whole = Rows {
            rows,
            ..Rows::default()
        };
        let saved: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
        assert!(saved["submission_scan"]["after"].is_string());
        let answered = bounded(&record, &cache, &root, &whole, None).unwrap();
        assert_eq!(whole.unbounded.borrow().first(), Some(&None));
        assert_eq!(
            answered["items"]["codex-async:call_two:0"]["delivery"]["request_id"],
            exact.as_str()
        );
        assert_eq!(settled(&answered), settled(&complete));
    }
    /// zerus-a4mub.8 review: a completed recovery grants nothing later. A
    /// submitted receipt indexed since is recovered by the next bounded call
    /// and by an answer, and a gap recorded since fails both; the warm
    /// cache's completion is never trusted.
    #[test]
    fn a_completed_recovery_is_validated_again_from_the_head() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let record = closing_session(dir.path(), 0);
        let cache = dir.path().join("cache.json");
        let budget = || Some(Instant::now() + Duration::from_secs(30));
        let warm = bounded(&record, &cache, &root, &Rows::default(), budget()).unwrap();
        let third = warm["items"]["codex-async:call_two:0"].clone();
        assert!(third["delivery"].is_null());
        // An older cache's completion marker for this run is ignored too.
        let mut saved: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
        saved["submission_scan_run"] = json!("run");
        fs::write(&cache, saved.to_string()).unwrap();
        let (exact, rows) = receipts(&root, &record, &third, 0);
        let indexed = Rows {
            rows,
            ..Rows::default()
        };
        let current = bounded(&record, &cache, &root, &indexed, budget()).unwrap();
        let pending = pending(&record, &current);
        assert_eq!(pending[0]["answer_delivery"]["request_id"], exact.as_str());
        // An answer reads it from the head as well, from a fresh warm cache.
        let answer_cache = dir.path().join("answer.json");
        bounded(&record, &answer_cache, &root, &Rows::default(), budget()).unwrap();
        let answered = bounded(&record, &answer_cache, &root, &indexed, None).unwrap();
        assert_eq!(indexed.unbounded.borrow().first(), Some(&None));
        assert_eq!(
            answered["items"]["codex-async:call_two:0"]["delivery"]["request_id"],
            exact.as_str()
        );
        // A gap recorded after completion: unknown for both, never complete.
        let gap = Rows {
            gap: true,
            ..Rows::default()
        };
        for deadline in [budget(), None] {
            assert_eq!(
                try_bounded(&record, &cache, &root, &gap, deadline),
                Err(INCOMPLETE_RECEIPTS.to_owned())
            );
        }
    }
    /// zerus-a4mub.8: a locator page completes recovery only with complete
    /// coverage, and every restart signal reaches recovery.
    #[test]
    fn locator_pages_complete_only_with_complete_coverage_and_carry_restarts() {
        use receipt_index::{Coverage, QuestionReceiptPage, QuestionReceiptRow};
        let row = QuestionReceiptRow {
            request_id: "request".into(),
            path: PathBuf::from("question_receipts/request.json"),
        };
        for (coverage, complete) in [
            (Coverage::Complete, true),
            (Coverage::Partial, false),
            (Coverage::Gap, false),
        ] {
            for restart in [false, true] {
                let page = from_locator(QuestionReceiptPage {
                    rows: vec![row.clone()],
                    next_cursor: Some("cursor".into()),
                    coverage,
                    restart_required: restart,
                });
                assert_eq!(
                    (page.complete, page.restart, page.next.as_deref()),
                    (complete, restart, Some("cursor")),
                    "{coverage:?} restart {restart}"
                );
                assert_eq!(page.gap, coverage == Coverage::Gap);
                assert_eq!(page.rows, vec![("request".to_owned(), row.path.clone())]);
            }
        }
    }
    /// zerus-a4mub.8: a receipt counts only under its own request ID, never
    /// under another row that points at its file.
    #[test]
    fn a_receipt_is_recovered_only_under_its_own_request_id() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let record = closing_session(dir.path(), 0);
        let full = bounded(
            &record,
            &dir.path().join("reference.json"),
            &root,
            &Rows::default(),
            None,
        )
        .unwrap();
        let third = full["items"]["codex-async:call_two:0"].clone();
        let (exact, rows) = receipts(&root, &record, &third, 0);
        let path = rows[0].1.clone();
        let foreign = Rows {
            rows: vec![(uuid::Uuid::new_v4().to_string(), path.clone())],
            ..Rows::default()
        };
        let cache = bounded(
            &record,
            &dir.path().join("cache.json"),
            &root,
            &foreign,
            None,
        )
        .unwrap();
        assert!(cache["items"]["codex-async:call_two:0"]["delivery"].is_null());
        let own = Rows {
            rows: vec![(exact.clone(), path)],
            ..Rows::default()
        };
        let cache = bounded(&record, &dir.path().join("own.json"), &root, &own, None).unwrap();
        assert_eq!(
            cache["items"]["codex-async:call_two:0"]["delivery"]["request_id"],
            exact.as_str()
        );
    }
    /// zerus-a4mub.8: through the production index and its state directory,
    /// an expired bounded call never reads this run's question receipts (on
    /// the old code its first complete transcript scan read all 301), and
    /// the questions stay unknown until an answer's unbounded call recovers
    /// the exact submission. Runs in its own process with a private state.
    #[test]
    fn an_expired_call_reads_no_question_receipt() {
        private_probe("expired");
    }
    /// zerus-a4mub.8: an inspection whose Codex question index is held
    /// reports its questions incomplete and lists none of them; once the
    /// index is free the same inspection is complete with the question.
    #[test]
    fn an_inspection_of_a_held_index_is_incomplete() {
        private_probe("inspection");
    }
    fn private_probe(case: &str) {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::codex_questions::tests::expired_recovery_probe",
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path())
            .env("HGS_TEST_QUESTION_RECOVERY", case)
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
    }
    #[test]
    #[ignore = "runs in its own process with a private HGS_STATE_DIR"]
    fn expired_recovery_probe() {
        let Ok(case) = std::env::var("HGS_TEST_QUESTION_RECOVERY") else {
            return;
        };
        if case == "inspection" {
            return held_inspection_probe();
        }
        let root = absolute_root().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let record = closing_session(dir.path(), 0);
        let full = index(&record, &dir.path().join("reference.json"), None)
            .unwrap()
            .unwrap();
        let third = full["items"]["codex-async:call_two:0"].clone();
        let (exact, _) = receipts(&root, &record, &third, 300);
        let cache = dir.path().join("cache.json");
        let lines = fs::read_to_string(string(&record, "transcript"))
            .unwrap()
            .lines()
            .count();
        for call in 0..lines + 3 {
            let result = index(&record, &cache, Some(Instant::now())).unwrap();
            assert!(
                result.is_none(),
                "expired call {call} completed: it read every question receipt"
            );
        }
        let saved: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!(
            saved["offset"],
            json!(fs::metadata(string(&record, "transcript")).unwrap().len())
        );
        assert!(saved["items"]["codex-async:call_two:0"]["delivery"].is_null());
        assert_ne!(saved["submission_scan_run"], "run");
        // An answer's unbounded call recovers the exact submission.
        let complete = index(&record, &cache, None).unwrap().unwrap();
        assert_eq!(
            complete["items"]["codex-async:call_two:0"]["delivery"]["request_id"],
            exact.as_str()
        );
    }
    fn held_inspection_probe() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = closing_session(dir.path(), 0);
        // This test process stands in for the live agent.
        record["pid"] = json!(std::process::id());
        record["process_start"] = json!(process_start(std::process::id()));
        let budget = || Instant::now() + Duration::from_millis(200);
        // An answer's unbounded index reconciles the locator and recovers.
        assert_eq!(questions::current(&record).unwrap().len(), 1);
        let (pending, complete) = questions::inspection(&record, true, budget()).unwrap();
        assert!(complete);
        assert_eq!(pending.as_array().unwrap().len(), 1, "{pending}");
        let held = lock(Some(&cache_path(&record).unwrap().with_extension("lock"))).unwrap();
        let (pending, complete) = questions::inspection(&record, true, budget()).unwrap();
        assert!(!complete, "a held index was reported complete");
        assert!(
            pending
                .as_array()
                .unwrap()
                .iter()
                .all(|card| card["source"] != "codex_async"),
            "{pending}"
        );
        assert_eq!(questions::current_until(&record, budget()).unwrap(), None);
        drop(held);
        let (pending, complete) = questions::inspection(&record, true, budget()).unwrap();
        assert!(complete);
        assert_eq!(pending[0]["question_id"], "codex-async:call_two:0");
    }
    #[test]
    fn an_expired_deadline_stops_after_each_oversized_line() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let transcript = dir.path().join(format!("rollout-{id}.jsonl"));
        // Lines over 1 MiB are skipped unparsed, but each still counts against the deadline.
        let oversized = json!({"type":"event_msg","payload":{"type":"agent_message","message":"x".repeat(1024 * 1024)}});
        let accepted = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call_one","output":"{\"accepted\":true}"}});
        let mut lines = vec![json!({"type":"session_meta","payload":{"id":id}})];
        lines.extend(std::iter::repeat_n(oversized, 3));
        lines.extend([request(), accepted]);
        fs::write(&transcript, lines.iter().map(|line| line.to_string() + "\n").collect::<String>()).unwrap();
        let record = json!({"agent":"codex","run_id":"run","conversation_id":id,"transcript":transcript});
        let cache = dir.path().join("cache.json");
        let source = crate::platform::file_identity(&transcript).unwrap();
        fs::write(&cache, json!({"items":{},"offset":0,"inode":[source.device,source.inode],"scan_version":2,
            "initialized":true,"submission_scan_run":"run"}).to_string()).unwrap();
        // An older completion marker grants nothing, and recovery uses a
        // private root: it never reaches this machine's receipts.
        let root = dir.path().join("state");
        let none = Rows::default();
        let expired = Instant::now();
        let end = json!(fs::metadata(&transcript).unwrap().len());
        let mut calls = 0;
        loop {
            calls += 1;
            assert!(calls <= lines.len(), "indexing made no progress");
            assert_eq!(bounded(&record, &cache, &root, &none, Some(expired)), None);
            let saved: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
            if saved["offset"] == end {
                break;
            }
        }
        // Every line ended a call, each oversized line included.
        assert_eq!(calls, lines.len());
        let budget = Some(Instant::now() + Duration::from_secs(30));
        let complete = bounded(&record, &cache, &root, &none, budget).unwrap();
        assert_eq!(pending(&record, &complete).len(), 2);
    }
    /// zerus-cqkoh: a Skip whose key may have reached Codex never fails as
    /// a refusal; one that sent nothing does.
    #[test]
    fn a_skip_after_its_key_never_fails_as_a_refusal() {
        struct Keys {
            touched: bool,
            fail: bool,
        }
        impl Transport for Keys {
            fn screen(&mut self) -> Result<String> {
                Ok(String::new())
            }
            fn key(&mut self, _: &str) -> Result<()> {
                self.touched = true;
                if self.fail {
                    return Err("key not sent".into());
                }
                Ok(())
            }
            fn paste(&mut self, _: &str) -> Result<()> {
                Ok(())
            }
            fn resolved(&mut self) -> Result<Option<Value>> {
                Ok(None)
            }
        }
        let touched = |keys: &Keys| keys.touched;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let card = json!({"question_id": "codex-async:call_one:0"});
        // The key's own failure, after it may have gone out.
        let mut keys = Keys {
            touched: false,
            fail: true,
        };
        let error = skip(&mut keys, touched, true, &path, &card).unwrap_err();
        assert!(
            error.starts_with("delivery uncertain: key not sent"),
            "{error}"
        );
        // The cache after a sent key.
        let mut keys = Keys {
            touched: false,
            fail: false,
        };
        let error = skip(&mut keys, touched, true, &path, &card).unwrap_err();
        assert!(error.starts_with("delivery uncertain: "), "{error}");
        // Nothing sent: the same failure is a refusal.
        let mut keys = Keys {
            touched: false,
            fail: false,
        };
        let error = skip(&mut keys, touched, false, &path, &card).unwrap_err();
        assert!(!error.contains("delivery uncertain"), "{error}");
        assert!(!keys.touched);
        // A sent key and a readable cache record the skip.
        fs::write(&path, json!({"items": {}}).to_string()).unwrap();
        let mut keys = Keys {
            touched: false,
            fail: false,
        };
        skip(&mut keys, touched, true, &path, &card).unwrap();
        let cache: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cache["items"]["codex-async:call_one:0"]["skipped"], true);
    }
    #[test]
    fn a_failure_after_possible_input_is_uncertain() {
        assert_eq!(
            after_input(false, "cache unreadable".into()),
            "cache unreadable"
        );
        assert_eq!(
            after_input(true, "cache unreadable".into()),
            "delivery uncertain: cache unreadable"
        );
        assert_eq!(
            after_input(true, "delivery uncertain: late".into()),
            "delivery uncertain: late"
        );
    }
    #[test]
    fn only_complete_native_question_panels_accept_input() {
        let mut cache = json!({"items":{}});
        observe(&json!({}), &mut cache, &request());
        let card = &cache["items"]["codex-async:call_one:0"];
        let screen="Which option?\n\n› 1. One\n  2. Two\n  3. Other (write an answer)\nenter submit   ctrl+] skip\n";
        assert!(visible_panel(screen, card).unwrap());
        assert!(panel_empty(screen, card).unwrap());
        assert!(!panel_empty(
            &screen.replace("Other (write an answer)", "existing draft"),
            card
        )
        .unwrap());
        let freeform = &cache["items"]["codex-async:call_one:1"];
        assert!(panel_empty(
            "Any notes?\n\nType your answer\n\nenter submit   ctrl+] skip",
            freeform
        )
        .unwrap());
        assert!(!panel_empty(
            "Any notes?\n\nMy existing draft\n\nenter submit   ctrl+] skip",
            freeform
        )
        .unwrap());
        for wrong in [
            screen.replace("Which option?", "Different?"),
            screen.replace("Two", "Hidden"),
            screen.replace("ctrl+] skip", "esc cancel"),
        ] {
            assert!(!visible_panel(&wrong, card).unwrap());
        }
    }
    #[test]
    fn codex_0_162_panels_with_progress_and_short_skip_key_accept_input() {
        let mut cache = json!({"items":{}});
        observe(&json!({}), &mut cache, &request());
        let card = &cache["items"]["codex-async:call_one:0"];
        // Captured from Codex 0.162.1 (trailing spaces trimmed).
        let screen = "• Queued follow-up inputs\n\n\n  1 of 2\n  Which option?\n\n  › 1. One\n    2. Two\n    3. Other\n\n  enter submit   ^] skip   shift+→ main prompt   shift+← next question\n\n";
        assert!(visible_panel(screen, card).unwrap());
        assert!(panel_empty(screen, card).unwrap());
        let freeform = &cache["items"]["codex-async:call_one:1"];
        let notes = "• Queued follow-up inputs\n\n\n  Any notes?\n\n  Type your answer\n\n  enter submit   ^] skip   shift+→ main prompt\n\n";
        assert!(panel_empty(notes, freeform).unwrap());
        assert!(!panel_empty(&notes.replace("Type your answer", "Free notes"), freeform).unwrap());
        // Another question's position, or a progress line alone, is never this card.
        for wrong in [
            screen.replace("1 of 2", "2 of 2"),
            screen.replace("  Which option?\n", ""),
            screen.replace("^] skip", "esc cancel"),
        ] {
            assert!(!visible_panel(&wrong, card).unwrap());
        }
        assert!(visible_panel(&notes.replace("  Any notes?", "  2 of 2\n  Any notes?"), freeform).unwrap());
    }
    #[test]
    fn a_scrolled_answer_list_grows_until_the_whole_panel_shows() {
        // Captured from Codex 0.160.1 and 0.162.0 at 80x24, then grown to 80x48.
        let scrolled = [
            include_str!("../../tests/fixtures/codex-question-scrolled-0.160.1.txt"),
            include_str!("../../tests/fixtures/codex-question-scrolled-0.162.0.txt"),
        ];
        let grown = [
            include_str!("../../tests/fixtures/codex-question-complete-0.160.1.txt"),
            include_str!("../../tests/fixtures/codex-question-complete-0.162.0.txt"),
        ];
        let title = ["Before I continue with the release preparation, which distribution channel should this build target, considering that the stable channel requires the full CI matrix, signed artifacts and the owner-triggered publication step, while nightly and dev channels publish automatically after verification?"; 3].join(" ");
        let options: Vec<_> = (1..=5).map(|i| format!("Option {i}: publish to this channel after the complete matrix, package checks and signed asset verification succeed on the exact candidate")).collect();
        let mut cache = json!({"items":{}});
        let arguments = json!({"questions":[{"title":title,"options":options}]}).to_string();
        observe(&json!({}), &mut cache, &json!({"type":"response_item","payload":{"type":"function_call","name":"request_user_input_async","call_id":"call_tall","arguments":arguments}}));
        let card = &cache["items"]["codex-async:call_tall:0"];
        for screen in scrolled {
            assert!(clipped(screen) && !complete(screen));
            assert!(!visible_panel(screen, card).unwrap());
        }
        for screen in grown {
            assert!(complete(screen) && !clipped(screen));
            assert!(visible_panel(screen, card).unwrap());
        }
        // A pasted answer is the selected Other row, its wrapped rows joined;
        // the same text as an option label elsewhere in the panel is not.
        let answer = &options[1];
        for screen in [
            include_str!("../../tests/fixtures/codex-question-other-draft-0.160.1.txt"),
            include_str!("../../tests/fixtures/codex-question-other-draft-0.162.0.txt"),
        ] {
            assert!(!question_terminal::plain(screen).unwrap().contains(answer.as_str()));
            assert_eq!(other_draft(screen, card).unwrap().as_deref(), Some(answer.as_str()));
        }
        for screen in grown {
            assert_eq!(other_draft(screen, card).unwrap(), None);
        }
        // Only Codex's key hints name the scrolled row; elsewhere it is text.
        for other in [
            "› Ask Codex to do anything\n",
            "• Working (3s • esc to interrupt)\n  option 1/6\n",
            "  Press enter to confirm or esc to cancel   option 1/6\n",
        ] {
            assert!(!clipped(other) && !complete(other), "{other}");
        }
    }
    #[test]
    fn working_status_above_a_codex_panel_does_not_change_it() {
        let mut cache = json!({"items":{}});
        observe(&json!({}), &mut cache, &request());
        let card = &cache["items"]["codex-async:call_one:0"];
        // Captured from Codex 0.162.0 with animations, one second apart.
        let panel = "• Queued follow-up inputs\n\n\n  1 of 2\n  Which option?\n\n  › 1. One\n    2. Two\n    3. Other\n\n  enter submit   ^] skip   shift+→ main prompt   shift+← next question\n\n";
        let first = format!("• \x1b[2mWorking\x1b[0m \x1b[2m(2s • \x1b[0;1mesc\x1b[0;2m to interrupt)\x1b[0m\n\n{panel}");
        let second = format!("\x1b[2m◦\x1b[0m \x1b[2mWorking\x1b[0m \x1b[2m(3s • \x1b[0;1mesc\x1b[0;2m to interrupt)\x1b[0m\n\n{panel}");
        assert_ne!(first, second);
        assert!(same_panel(&first, &second, card));
        for changed in [
            second.replace("  › 1. One\n    2. Two", "    1. One\n  › 2. Two"),
            second.replace("3. Other", "3. Draft"),
            second.replace("1 of 2", "2 of 2"),
            second.replace("^] skip", "esc cancel"),
        ] {
            assert!(!same_panel(&first, &changed, card));
        }
    }
}
