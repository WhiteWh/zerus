//! Read-only native session accounting. Cached offsets avoid replaying large
//! transcripts on every inspector poll. This cache contains counters, not text.
//! A complete read describes the file snapshot opened at its start: a path
//! atomically replaced while the reader waits for the usage-cache lock can
//! still answer the previous snapshot and its expiry once. The keep-warm round
//! confirms a cold candidate with a fresh read, which opens the replacement,
//! so a replacement never turns the setting cold falsely (zerus-ryeca.6).
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};

const MAX_READ: u64 = 32 * 1024 * 1024;

/// Test seam for the lock race: signals when a scan of this exact cache is
/// about to take the usage-cache lock, after opening the transcript and before
/// its metadata. The cache path keeps another parallel scan out of the signal.
#[cfg(test)]
static BEFORE_CACHE_LOCK: std::sync::Mutex<Option<(PathBuf, std::sync::mpsc::Sender<()>)>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn note_before_cache_lock(cache_path: &Path) {
    let mut hook = BEFORE_CACHE_LOCK.lock().unwrap();
    if hook.as_ref().is_some_and(|(path, _)| path == cache_path) {
        if let Some((_, sender)) = hook.take() {
            let _ = sender.send(());
        }
    }
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= 9_007_199_254_740_991)
}
fn copy(out: &mut Value, source: &Value, fields: &[(&str, &str)]) {
    for (from, to) in fields {
        if let Some(n) = number(&source[*from]) {
            out[*to] = json!(n);
        }
    }
}
fn sum(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .try_fold(0u64, |total, key| total.checked_add(number(&value[*key])?))
}
pub(super) fn buckets(source: &Value, provider: &str) -> Value {
    let mut out = json!({});
    match provider {
        "codex" => copy(
            &mut out,
            source,
            &[
                ("input_tokens", "input"),
                ("cached_input_tokens", "cache_read"),
                ("cache_write_input_tokens", "cache_write"),
                ("output_tokens", "output"),
                ("reasoning_output_tokens", "reasoning"),
                ("total_tokens", "total"),
            ],
        ),
        "claude" => {
            copy(
                &mut out,
                source,
                &[
                    ("input_tokens", "uncached_input"),
                    ("output_tokens", "output"),
                    ("cache_read_input_tokens", "cache_read"),
                    ("cache_creation_input_tokens", "cache_write"),
                ],
            );
            copy(
                &mut out,
                &source["output_tokens_details"],
                &[("thinking_tokens", "reasoning")],
            );
            copy(
                &mut out,
                &source["server_tool_use"],
                &[
                    ("web_search_requests", "web_searches"),
                    ("web_fetch_requests", "web_fetches"),
                ],
            );
        }
        "kimi" => copy(
            &mut out,
            source,
            &[
                ("inputOther", "uncached_input"),
                ("output", "output"),
                ("inputCacheRead", "cache_read"),
                ("inputCacheCreation", "cache_write"),
            ],
        ),
        "dsh" => copy(
            &mut out,
            source,
            &[
                ("uncachedInputTokens", "uncached_input"),
                ("outputTokens", "output"),
                ("cacheReadTokens", "cache_read"),
                ("cacheWriteTokens", "cache_write"),
            ],
        ),
        _ => {}
    }
    if provider != "codex" {
        if let Some(input) = sum(&out, &["uncached_input", "cache_read", "cache_write"]) {
            out["input"] = json!(input);
        }
        if let Some(total) = sum(&out, &["input", "output"]) {
            out["total"] = json!(total);
        }
    }
    out
}

fn accumulate(state: &mut Value, sample: Value, id: Option<&str>) {
    if sample.as_object().is_none_or(|o| o.is_empty()) {
        return;
    }
    let previous = id
        .map(|id| state["seen"][id].clone())
        .unwrap_or(Value::Null);
    for (key, value) in sample.as_object().unwrap() {
        let total = number(&state["totals"][key]).unwrap_or(0);
        let before = number(&previous[key]).unwrap_or(0);
        state["totals"][key] = json!(total
            .saturating_sub(before)
            .saturating_add(number(value).unwrap_or(0)));
    }
    if let Some(id) = id {
        state["seen"][id] = sample.clone();
    }
    if previous.is_null() {
        state["requests"] = json!(number(&state["requests"]).unwrap_or(0) + 1);
    }
    state["last"] = sample;
}

// TTL is read from native usage, never guessed from provider/account names.
// A mixed prefix can contain both TTLs; the shorter one is the conservative
// deadline. This remains an estimate: prompt/model changes can invalidate it.
fn cache_observation(state: &mut Value, event: &Value, usage: &Value, provider: &str) {
    let mut at=search::timestamp(&event["timestamp"]);
    if at==0.0 { at=search::timestamp(&event["time"]); if at>10_000_000_000.0 {at/=1000.0;} }
    let sample=buckets(usage,provider);
    if sample.as_object().is_none_or(|v|v.is_empty()) {return;}
    let mut cache=json!({"status":"unknown","source":"native_usage","observed_at":at,
        "cache_read":sample["cache_read"],"cache_write":sample["cache_write"],"input":sample["input"]});
    if provider=="claude" {
        let writes=&usage["cache_creation"];
        let ttl=if number(&writes["ephemeral_5m_input_tokens"]).unwrap_or(0)>0 {300}
            else if number(&writes["ephemeral_1h_input_tokens"]).unwrap_or(0)>0 {3600} else {0};
        // Do not extend a deadline for duplicate streaming snapshots of the
        // same request. It started caching by the first observed response.
        let same=state["cache_request_id"]==event["message"]["id"] && !event["message"]["id"].is_null();
        let previous=&state["prompt_cache"];
        let model=string(&event["message"],"model");
        let ttl=if ttl>0 {ttl} else if state["cache_model"]==model {previous["ttl_seconds"].as_u64().unwrap_or(0)} else {0};
        if ttl>0 && at>0.0 && number(&sample["cache_read"]).unwrap_or(0)+number(&sample["cache_write"]).unwrap_or(0)>0 {
            let deadline=if same && previous["expires_at"].is_number() {previous["expires_at"].as_f64().unwrap().min(at+ttl as f64)}
                else {at+ttl as f64};
            cache["status"]=json!("warm");cache["expires_at"]=json!(deadline);
            cache["ttl_seconds"]=json!(ttl);cache["estimated"]=json!(true);
        }
        state["cache_request_id"]=event["message"]["id"].clone();state["cache_model"]=json!(model);
    }
    state["prompt_cache"]=cache;
}

/// U38: a native event's moment in epoch seconds (numerical milliseconds are
/// normalized), for the usage history a span reads.
fn moment(value: &Value) -> f64 {
    let at = search::timestamp(value);
    if at > 10_000_000_000.0 {
        at / 1000.0
    } else {
        at
    }
}

/// U38: the cached usage history a span is summed from: bounded samples with
/// their moment, provider, model and either the request's counters or the
/// cumulative cost and work a cost-state reported. Counters only, never text.
fn push_history(state: &mut Value, sample: Value) {
    if !state["history"].is_array() {
        state["history"] = json!([]);
    }
    let mut truncated = false;
    {
        let history = state["history"].as_array_mut().unwrap();
        history.push(sample);
        // The newest FINE samples stay as they came; older ones fold into one
        // sample per minute, provider and model, so a long session (a day of
        // work, thousands of requests) still answers a span over its first
        // hours. Folded samples lead the list; once the unfolded ones reach
        // twice FINE, all but the newest FINE fold. Only past MINUTES folded
        // samples does the oldest go.
        let folded_count = history.iter().take_while(|sample| sample["minute"] == true).count();
        if history.len() - folded_count > 2 * FINE {
            let split = history.len() - FINE;
            let mut kept = fold_minutes(&history[..split]);
            if kept.len() > MINUTES {
                let drop = kept.len() - MINUTES;
                kept.drain(0..drop);
                truncated = true;
            }
            kept.extend(history.drain(split..));
            *history = kept;
        }
    }
    if truncated {
        state["history_truncated"] = json!(true);
    }
}

/// The newest usage samples kept one per request.
const FINE: usize = 1000;
/// Older samples folded per minute (about four weeks of one model's minutes).
const MINUTES: usize = 40_000;

/// Samples folded per minute, provider and model: request counters summed,
/// a cumulative reading (cost, work) kept as the minute's last, the moment
/// the minute's end. Already folded samples fold again unchanged.
fn fold_minutes(samples: &[Value]) -> Vec<Value> {
    let mut folded: BTreeMap<(i64, String, String), Value> = BTreeMap::new();
    for sample in samples {
        let at = sample["at"].as_f64().unwrap_or(0.0);
        let minute = (at / 60.0).ceil() as i64;
        let key = (minute, sample["provider"].as_str().unwrap_or_default().to_owned(), sample["model"].as_str().unwrap_or_default().to_owned());
        let entry = folded.entry(key).or_insert_with(|| {
            json!({"at":(minute * 60) as f64,"provider":sample["provider"],"model":sample["model"],"minute":true})
        });
        if let Some(tokens) = sample["tokens"].as_object() {
            if !entry["tokens"].is_object() {
                entry["tokens"] = json!({});
            }
            for (bucket, value) in tokens {
                if let Some(count) = number(value) {
                    entry["tokens"][bucket] = json!(number(&entry["tokens"][bucket]).unwrap_or(0).saturating_add(count));
                }
            }
        }
        for key in ["cost_usd", "work_ms"] {
            if !sample[key].is_null() {
                entry[key] = sample[key].clone();
            }
        }
    }
    let mut out: Vec<Value> = folded.into_values().collect();
    out.sort_by(|a, b| a["at"].as_f64().unwrap_or(0.0).total_cmp(&b["at"].as_f64().unwrap_or(0.0)));
    out
}

/// U38: a request sample minus the same request's previous snapshot, so a
/// repeated streaming usage is counted once.
fn delta(sample: &Value, previous: &Value) -> Value {
    let mut out = json!({});
    for (key, value) in sample.as_object().into_iter().flatten() {
        if let Some(count) = number(value) {
            out[key] = json!(count.saturating_sub(number(&previous[key]).unwrap_or(0)));
        }
    }
    out
}

fn has_numbers(value: &Value) -> bool {
    value.as_object().is_some_and(|object| object.values().any(|value| number(value).is_some()))
}

fn observe(state: &mut Value, event: &Value, provider: &str, conversation: &str) {
    match provider {
        "codex" if event["type"] == "event_msg" && event["payload"]["type"] == "token_count" => {
            let info = &event["payload"]["info"];
            let at = moment(&event["timestamp"]);
            let totals = buckets(&info["total_token_usage"], provider);
            if has_numbers(&totals) {
                // U38: the snapshot is cumulative; its history sample is the
                // difference from the previous snapshot.
                let previous = state["codex_seen"].clone();
                let delta = delta(&totals, &previous);
                if at > 0.0 && has_numbers(&delta) {
                    let model = string(&event["payload"], "model");
                    let model = if model.is_empty() { string(info, "model") } else { model };
                    push_history(state, json!({"at":at,"provider":"codex","model":model,"tokens":delta}));
                }
                state["codex_seen"] = totals.clone();
                state["totals"] = totals;
            }
            let last = buckets(&info["last_token_usage"], provider);
            if !last.as_object().unwrap().is_empty() {
                state["last"] = last;
                cache_observation(state,event,&info["last_token_usage"],provider);
            }
            if let Some(n) = number(&info["model_context_window"]) {
                state["context"]["limit"] = json!(n);
            }
            if let Some(n) = number(&state["last"]["total"]) {
                state["context"]["used"] = json!(n);
            }
        }
        "claude" if event["sessionId"] == conversation && event["type"] == "assistant" => {
            let message = &event["message"];
            let id = string(message, "id");
            if id.is_empty() || id.len() > 256 {
                return;
            }
            // U38: the request's counters as a history sample; a repeated
            // streaming snapshot counts only its delta.
            let at = moment(&event["timestamp"]);
            let sample = buckets(&message["usage"], provider);
            if at > 0.0 && has_numbers(&sample) {
                let delta = delta(&sample, state["seen"].get(id).unwrap_or(&Value::Null));
                if has_numbers(&delta) {
                    push_history(state, json!({"at":at,"provider":"claude","model":string(message,"model"),"tokens":delta}));
                }
            }
            accumulate(state, sample, Some(id));
            if search::timestamp(&event["timestamp"]) >= state["compaction_at"].as_f64().unwrap_or(0.0) {
                cache_observation(state,event,&message["usage"],provider);
                if let Some(n) = number(&state["last"]["input"]) {
                    state["context"]["used"] = json!(n);
                }
            }
            if state["cost_usd"].is_number() {
                state["cost_stale"] = json!(true);
            }
        }
        "claude" if event["sessionId"] == conversation && event["type"] == "cost-state" => {
            let at = moment(&event["timestamp"]);
            let mut cost = None;
            if let Some(total) = event["totalCostUSD"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
            {
                state["cost_usd"] = json!(total);
                state["cost_stale"] = json!(false);
                state["cost_incomplete"] = json!(event["hasUnknownModelCost"] == true);
                cost = Some(total);
            }
            copy(
                &mut state["extras"],
                event,
                &[
                    ("totalAPIDuration", "api_ms"),
                    ("totalToolDuration", "tool_ms"),
                    ("totalLinesAdded", "lines_added"),
                    ("totalLinesRemoved", "lines_removed"),
                ],
            );
            // U38: cost and work are cumulative here; a span differences them.
            let work = match (number(&event["totalAPIDuration"]), number(&event["totalToolDuration"])) {
                (None, None) => None,
                (api, tool) => Some(api.unwrap_or(0).saturating_add(tool.unwrap_or(0))),
            };
            if at > 0.0 && (cost.is_some() || work.is_some()) {
                let mut sample = json!({"at":at,"provider":"claude"});
                if let Some(cost) = cost {
                    sample["cost_usd"] = json!(cost);
                }
                if let Some(work) = work {
                    sample["work_ms"] = json!(work);
                }
                push_history(state, sample);
            }
        }
        "claude" if event["sessionId"] == conversation && event["type"] == "system"
            && event["subtype"] == "compact_boundary" => {
            // A new summary replaces the prompt measured by earlier requests.
            // Keep cumulative billing, but neither its old size nor cache TTL
            // describes the compacted prompt. Do not invent a new warm cache.
            state["context"].as_object_mut().unwrap().remove("used");
            if let Some(tokens) = number(&event["compactMetadata"]["postTokens"]) {
                state["context"]["used"] = json!(tokens);
            }
            state["prompt_cache"] = json!({"status":"unknown","source":"native_compaction",
                "observed_at":search::timestamp(&event["timestamp"])});
            state["compaction_at"] = json!(search::timestamp(&event["timestamp"]));
            for key in ["cache_request_id", "cache_model"] {
                state.as_object_mut().unwrap().remove(key);
            }
        }
        "claude" if event["sessionId"] == conversation && event["type"] == "user" => {
            // H7: a prompt after the last cache observation makes that
            // observation older than the session's last turn.
            let at = search::timestamp(&event["timestamp"]);
            if at > 0.0 {
                state["last_prompt_at"] = json!(at.max(state["last_prompt_at"].as_f64().unwrap_or(0.0)));
            }
        }
        "kimi" if ["", "main"].contains(&string(event, "agentId")) => {
            if ["profile.bind", "llm.request"].contains(&string(event, "type")) {
                state["model"] = event["modelAlias"].clone();
            }
            if event["type"] == "usage.record" {
                // The matching message and step.end repeat this usage. Count
                // only its authoritative ledger entry (also includes compaction).
                let at = search::timestamp(&event["timestamp"]);
                let sample = buckets(&event["usage"], provider);
                if at > 0.0 && has_numbers(&sample) {
                    push_history(state, json!({"at":at,"provider":"kimi","model":state["model"],"tokens":sample.clone()}));
                }
                accumulate(state, sample, None);
                cache_observation(state,event,&event["usage"],provider);
            } else if event["type"] == "token_counting.measured" {
                if let Some(n) = number(&event["tokens"]) {
                    state["context"]["used"] = json!(n);
                }
            }
        }
        _ => {}
    }
}

fn output(state: &Value, provider: &str, partial: bool) -> Value {
    let available = state["totals"].as_object().is_some_and(|o| !o.is_empty())
        || state["cost_usd"].is_number()
        || state["context"]["used"].is_number();
    json!({"status":if available {"ok"} else {"unavailable"},"source":provider,
        "scope":"conversation","includes_subagents":false,"partial":partial,
        "totals":state["totals"],"last_request":state["last"],"requests":state["requests"],
        "prompt_cache":settled_cache(state),"context":state["context"],"model":state["model"],"cost_usd":state["cost_usd"],"cost_stale":state["cost_stale"],
        "cost_incomplete":state["cost_incomplete"],"extras":state["extras"]})
}

/// The cache claim an observation still supports (H7): a warm cache whose
/// deadline passed, or one observed before the session's last prompt, is
/// unknown - never a warm deadline in the past, which the desktop reads as a
/// cold cache. An estimate that ran out with nothing newer observed still
/// says which lifetime lapsed and when (`ttl_seconds`, `lapsed_at`): without
/// them keep-warm could not tell a cache that went cold from a lifetime the
/// agent never reported, and waited for a report instead of saying so.
fn settled_cache(state: &Value) -> Value {
    let cache = &state["prompt_cache"];
    if cache["status"] != "warm" {
        return cache.clone();
    }
    let observed = cache["observed_at"].as_f64().unwrap_or(0.0);
    let expires = cache["expires_at"].as_f64().unwrap_or(0.0);
    let prompt = state["last_prompt_at"].as_f64().unwrap_or(0.0);
    if observed <= 0.0 || prompt > observed {
        return json!({"status":"unknown","source":"native_usage","observed_at":observed});
    }
    if expires <= now() {
        let mut lapsed = json!({"status":"unknown","source":"native_usage","observed_at":observed});
        if expires > 0.0 && cache["ttl_seconds"].as_u64().is_some_and(|ttl| ttl > 0) {
            lapsed["ttl_seconds"] = cache["ttl_seconds"].clone();
            lapsed["lapsed_at"] = json!(expires);
        }
        return lapsed;
    }
    cache.clone()
}

pub(super) fn scan(path: &Path, cache_path: &Path, provider: &str, conversation: &str) -> Result<Value> {
    let (state, partial) = scanned(path, cache_path, provider, conversation)?;
    Ok(output(&state, provider, partial))
}

/// U38: the cached scan state and whether the transcript is only partly read,
/// so a span can read the usage history the cache keeps.
fn scanned(path: &Path, cache_path: &Path, provider: &str, conversation: &str) -> Result<(Value, bool)> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    // The file's identity and metadata belong under the cache lock. A scan that
    // waited here must not answer from a size and stamp read before the lock:
    // another reader may have appended and written the cache meanwhile.
    #[cfg(test)]
    note_before_cache_lock(cache_path);
    let _guard = lock(Some(&cache_path.with_extension("lock")))?;
    let (identity, meta) =
        crate::platform::FileIdentity::with_metadata(&file).map_err(|e| e.to_string())?;
    let mut prefix = vec![0; meta.len().min(256) as usize];
    file.read_exact(&mut prefix).map_err(|e| e.to_string())?;
    let fingerprint = format!(
        "{}:{}:{:x}",
        identity.device,
        identity.inode,
        Sha256::digest(&prefix)
    );
    if provider == "codex" {
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut first = String::new();
        BufReader::new((&mut file).take(256 * 1024))
            .read_line(&mut first)
            .map_err(|e| e.to_string())?;
        let first: Value = serde_json::from_str(&first).map_err(|e| e.to_string())?;
        if first["type"] != "session_meta" || first["payload"]["id"] != conversation {
            return Err("Native conversation identity mismatch".into());
        }
    }
    let mut state: Value = fs::read(cache_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let mut offset = number(&state["offset"]).unwrap_or(0);
    let stamp = json!([identity.modified, identity.modified_nanos]);
    // Version 6 (2026-10-31): older samples fold per minute instead of being
    // dropped; a version-5 cache lost them, so it is read again from the start.
    let rescan = state["version"] != 6;
    if state["version"] != 6
        || state["fingerprint"] != fingerprint
        || offset > meta.len()
        || (state["size"] == meta.len() && state["stamp"] != stamp)
    {
        // U38: a restarted session's history survives its old transcript, so a
        // span across the restart still reads both runs.
        let carried = state["history"].as_array().filter(|history| !history.is_empty() && !rescan).cloned();
        let truncated = state["history_truncated"] == true && !rescan;
        state = json!({"version":6,"fingerprint":fingerprint,"totals":{},"seen":{},"context":{},"history":[],"codex_seen":{},"skipping":false});
        if let Some(history) = carried {
            state["history"] = Value::Array(history);
        }
        if truncated {
            state["history_truncated"] = json!(true);
        }
        offset = 0;
        // Codex's last snapshot is cumulative; no need to replay its full log.
        if provider == "codex" && meta.len() > 8 * 1024 * 1024 {
            offset = meta.len() - 8 * 1024 * 1024;
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| e.to_string())?;
            let mut skip = Vec::new();
            offset += BufReader::new((&mut file).take(MAX_READ))
                .read_until(b'\n', &mut skip)
                .map_err(|e| e.to_string())? as u64;
            // U38: the skipped part is not in the usage history.
            state["history_truncated"] = json!(true);
        }
    }
    let mut skipping = state["skipping"].as_bool() == Some(true);
    if !skipping && state["offset"] == meta.len() && state["stamp"] == stamp {
        return Ok((state, false));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    // Reads and discards share one per-scan byte budget. An oversized record is
    // discarded across scans: the reached offset and a skipping flag persist,
    // so the next scan continues the discard before reading records again.
    let mut used = 0u64;
    loop {
        let remaining = MAX_READ.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        if skipping {
            match skip_to_newline(&mut reader, remaining)? {
                Skipped::Found(consumed) => {
                    used += consumed;
                    offset += consumed;
                    skipping = false;
                }
                Skipped::Budget(consumed) | Skipped::Eof(consumed) => {
                    offset += consumed;
                    break;
                }
            }
            continue;
        }
        line.clear();
        let count = (&mut reader)
            .take(remaining)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if line.ends_with(b"\n") {
            used += count as u64;
            offset += count as u64;
            if let Ok(event) = serde_json::from_slice::<Value>(&line) {
                observe(&mut state, &event, provider, conversation);
            }
            continue;
        }
        // No newline within the remaining budget. A record is oversized only
        // when a full MAX_READ read from its own start finds no newline. If
        // only this scan's remaining budget ran out inside it, stop before the
        // record: the next scan reads it from its start with a full budget.
        if count as u64 == remaining && remaining == MAX_READ {
            offset += count as u64;
            skipping = true;
        }
        break;
    }
    state["offset"] = json!(offset);
    state["size"] = json!(meta.len());
    state["stamp"] = stamp;
    state["skipping"] = json!(skipping);
    atomic(cache_path, &state.to_string())?;
    Ok((state, offset < meta.len() || skipping))
}

/// The outcome of discarding the rest of an oversized record within a byte
/// budget.
enum Skipped {
    /// The newline was consumed; the record ended.
    Found(u64),
    /// The budget ran out still inside the record.
    Budget(u64),
    /// The file ended before a newline.
    Eof(u64),
}

/// Skip the rest of an oversized record up to and including its newline
/// without buffering it, consuming at most `budget` bytes.
fn skip_to_newline(reader: &mut BufReader<File>, budget: u64) -> Result<Skipped> {
    let mut consumed = 0u64;
    while consumed < budget {
        let available = reader.fill_buf().map_err(|e| e.to_string())?;
        if available.is_empty() {
            return Ok(Skipped::Eof(consumed));
        }
        let limit = (budget - consumed) as usize;
        let slice = &available[..available.len().min(limit)];
        if let Some(at) = slice.iter().position(|byte| *byte == b'\n') {
            let count = at as u64 + 1;
            reader.consume(count as usize);
            return Ok(Skipped::Found(consumed + count));
        }
        let count = slice.len();
        reader.consume(count);
        consumed += count as u64;
    }
    Ok(Skipped::Budget(consumed))
}

fn kimi_capacity(config: &str, model: &str) -> Option<u64> {
    if model.is_empty() {
        return None;
    }
    let config = config.parse::<toml_edit::DocumentMut>().ok()?;
    let value = config
        .get("models")?
        .get(model)?
        .get("max_context_size")?
        .as_integer()?;
    (value > 0 && value <= 9_007_199_254_740_991).then_some(value as u64)
}

pub(super) fn read(record: &Value) -> Value {
    if let Some(value) = super::watch_index::usage(record) {
        return value;
    }
    let provider = string(record, "agent");
    let id = string(record, "conversation_id");
    let read = || -> Result<Value> {
        let path = if provider == "kimi" {
            questions::wire_paths(record)?
                .into_iter()
                .next()
                .ok_or("No native usage yet")?
        } else {
            if !["codex", "claude"].contains(&provider) || uuid::Uuid::parse_str(id).is_err() {
                return Err("Native usage unavailable".into());
            }
            let p = PathBuf::from(string(record, "transcript"));
            if !p.is_absolute()
                || !p
                    .file_name()
                    .is_some_and(|f| f.to_string_lossy().contains(id))
            {
                return Err("Native transcript unavailable".into());
            }
            p
        };
        let key = format!(
            "cache-v2-{:x}",
            Sha256::digest(json!([provider, id, path]).to_string().as_bytes())
        );
        let mut result = scan(
            &path,
            &root().join("usage-cache").join(format!("{key}.json")),
            provider,
            id,
        )?;
        if provider == "kimi" {
            // Native alias configuration declares context capacity explicitly.
            // llm.request.maxTokens is an output limit and must not be used.
            let mut config = String::new();
            if File::open(questions::kimi_home(record).join("config.toml"))
                .and_then(|file| file.take(1024 * 1024).read_to_string(&mut config))
                .is_ok()
            {
                if let Some(limit) = kimi_capacity(&config, string(&result, "model")) {
                    result["context"]["limit"] = json!(limit);
                    result["context"]["limit_source"] = json!("model_config");
                }
            }
        }
        Ok(result)
    };
    read().unwrap_or_else(|_| json!({"status":"unavailable","source":provider}))
}

pub(super) fn watch_observe(state: &mut Value, event: &Value, provider: &str, conversation: &str) {
    observe(state,event,provider,conversation);
}

pub(super) fn watch_output(state: &Value, provider: &str, partial: bool) -> Value {
    output(state,provider,partial)
}

/// Seed only from a complete canonical counter cache for this exact opened
/// snapshot. An incomplete/cold cache keeps the normal bounded usage reader
/// until it catches up; no partial totals are promoted to complete.
pub(super) fn watch_seed(record: &Value, file: &mut File, identity: crate::platform::FileIdentity, size: u64) -> Option<Value> {
    let (provider,id,path,_) = transcript(record).ok()?;
    let key = format!("cache-v2-{:x}", Sha256::digest(json!([provider,id,path]).to_string().as_bytes()));
    let cache = root().join("usage-cache").join(format!("{key}.json"));
    if fs::metadata(&cache).ok()?.len() > 2*1024*1024 { return None; }
    let bytes = fs::read(cache).ok()?;
    let state: Value = serde_json::from_slice(&bytes).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut prefix = vec![0;size.min(256) as usize];
    file.read_exact(&mut prefix).ok()?;
    let fingerprint = format!("{}:{}:{:x}",identity.device,identity.inode,Sha256::digest(&prefix));
    (state["version"] == 6 && state["fingerprint"] == fingerprint && state["offset"] == size
        && state["size"] == size && state["stamp"] == json!([identity.modified,identity.modified_nanos])
        && state["skipping"] != true).then_some(state)
}

#[cfg(test)]
pub(super) fn watch_reference(record: &Value, boundary: u64) -> Result<Value> {
    let file = File::open(string(record,"transcript")).map_err(|error|error.to_string())?;
    let mut reader = BufReader::new(file.take(boundary));
    let mut state = json!({"totals":{},"seen":{},"context":{},"history":[],"codex_seen":{}});
    let mut line = Vec::new();
    let mut partial = false;
    loop {
        line.clear();
        let count = (&mut reader).take(MAX_READ+1).read_until(b'\n',&mut line).map_err(|error|error.to_string())?;
        if count == 0 { break; }
        if !line.ends_with(b"\n") {
            partial = true;
            if count as u64 > MAX_READ { reader.skip_until(b'\n').map_err(|error|error.to_string())?; continue; }
            break;
        }
        if count as u64 <= MAX_READ {
            if let Ok(event) = serde_json::from_slice::<Value>(&line) {
                observe(&mut state,&event,string(record,"agent"),string(record,"conversation_id"));
            }
        }
    }
    Ok(output(&state,string(record,"agent"),partial))
}

/// U38: the native transcript and cache a Claude or Codex session reads, with
/// the same validation and cache key as `read`.
fn transcript(record: &Value) -> Result<(String, String, PathBuf, PathBuf)> {
    let provider = string(record, "agent");
    let id = string(record, "conversation_id");
    if provider == "kimi" {
        // Kimi's wire log timestamps every usage.record, so its history is the
        // one `read` scans, in the same cache.
        let path = questions::wire_paths(record)?
            .into_iter()
            .next()
            .ok_or("No native usage yet")?;
        let key = format!(
            "cache-v2-{:x}",
            Sha256::digest(json!([provider, id, path]).to_string().as_bytes())
        );
        return Ok((provider.to_owned(), id.to_owned(), path, root().join("usage-cache").join(format!("{key}.json"))));
    }
    if !["codex", "claude"].contains(&provider) || uuid::Uuid::parse_str(id).is_err() {
        return Err("Native usage unavailable".into());
    }
    let path = PathBuf::from(string(record, "transcript"));
    if !path.is_absolute()
        || !path
            .file_name()
            .is_some_and(|f| f.to_string_lossy().contains(id))
    {
        return Err("Native transcript unavailable".into());
    }
    let key = format!(
        "span-v1-{:x}",
        Sha256::digest(json!([provider, id]).to_string().as_bytes())
    );
    Ok((
        provider.to_owned(),
        id.to_owned(),
        path,
        root().join("usage-cache").join(format!("{key}.json")),
    ))
}

/// U38: the tokens and cost the native session spent between `from` and `to`
/// (epoch seconds), per provider and model, from the usage history the cached
/// transcript scan keeps. It reads only the bytes after the cached offset; a
/// provider or a window without timestamped counters is unknown, never zero.
#[cfg(test)]
pub(super) fn span(record: &Value, from: f64, to: f64) -> Value {
    span_many(record, &[(from, to)])
}

/// #736: the tokens and cost spent inside any of `windows` (disjoint, ordered),
/// so a task's own attempt and stage spans count and its member's other work in
/// the same hours does not. The span reaches from the first to the last window;
/// tokens are the request counters of the samples inside the windows, cost and
/// work the sum of each window's two-end readings. A window without timestamped
/// counters makes the whole span unknown, never zero.
pub(super) fn span_many(record: &Value, windows: &[(f64, f64)]) -> Value {
    let provider = string(record, "agent");
    let (from, to) = match (windows.first(), windows.last()) {
        (Some(first), Some(last)) => (first.0, last.1),
        _ => (0.0, 0.0),
    };
    let unknown = |reason: &str| {
        json!({"status":"unknown","scope":"span","source":provider,"from":from,"to":to,"reason":reason})
    };
    if windows.is_empty() || windows.iter().any(|(low, high)| !(low.is_finite() && high.is_finite() && *low > 0.0 && high > low)) {
        return unknown("A span needs two ordered moments");
    }
    if provider == "dsh" {
        // dsh keeps no history: the span is the difference of the counter
        // samples Zerus recorded at the window's ends.
        return usage_samples::span(record, windows);
    }
    let (provider, id, path, cache) = match transcript(record) {
        Ok(found) => found,
        Err(error) => return unknown(&format!("{error:#}")),
    };
    let (state, partial) = match scanned(&path, &cache, &provider, &id) {
        Ok(found) => found,
        Err(error) => return unknown(&format!("{error:#}")),
    };
    let history = state["history"].as_array().cloned().unwrap_or_default();
    let oldest = history
        .iter()
        .filter_map(|sample| sample["at"].as_f64())
        .fold(f64::INFINITY, f64::min);
    if history.is_empty() {
        return unknown("No timestamped counters");
    }
    if state["history_truncated"] == true && oldest > windows[0].0 {
        return unknown("The cached history does not reach back to the window");
    }
    // Request counters per provider and model, the samples wholly in a window.
    let mut models: BTreeMap<(String, String), (Value, u64)> = BTreeMap::new();
    for sample in &history {
        let at = sample["at"].as_f64().unwrap_or(0.0);
        if !windows.iter().any(|(low, high)| at > *low && at <= *high) {
            continue;
        }
        let Some(tokens) = sample["tokens"].as_object() else { continue };
        let key = (
            sample["provider"].as_str().unwrap_or(provider.as_str()).to_owned(),
            sample["model"].as_str().unwrap_or_default().to_owned(),
        );
        let entry = models.entry(key).or_insert_with(|| (json!({}), 0));
        for (bucket, value) in tokens {
            if let Some(count) = number(value) {
                entry.0[bucket] = json!(number(&entry.0[bucket]).unwrap_or(0).saturating_add(count));
            }
        }
        entry.1 += 1;
    }
    if models.is_empty() {
        return unknown("No timestamped counters in the window");
    }
    // Cost and work are cumulative readings on a cost-state; each window is the
    // difference of the readings at its two ends.
    let reading = |key: &str, moment: f64| -> Option<f64> {
        history
            .iter()
            .filter_map(|sample| {
                let at = sample["at"].as_f64()?;
                (at <= moment).then(|| sample[key].as_f64()).flatten()
            })
            .next_back()
    };
    let difference = |key: &str| -> Option<f64> {
        windows.iter().try_fold(0.0, |total, (low, high)| {
            let (low, high) = (reading(key, *low)?, reading(key, *high)?);
            (high >= low).then_some(total + high - low)
        })
    };
    let cost = difference("cost_usd").map_or(Value::Null, |cost| json!(cost));
    let work = difference("work_ms").map_or(Value::Null, |work| json!(work));
    let mut per_provider: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for ((name, model), (tokens, requests)) in models {
        per_provider.entry(name).or_default().push(json!({
            "model": if model.is_empty() { Value::Null } else { json!(model) },
            "tokens": tokens,
            "requests": requests,
        }));
    }
    let providers: Vec<Value> = per_provider
        .into_iter()
        .map(|(name, models)| {
            json!({"provider":name,"models":models,"cost_usd":cost,"work_ms":work})
        })
        .collect();
    json!({"status":"ok","scope":"span","source":provider,"from":from,"to":to,
        "partial":partial,"providers":providers})
}

pub(super) fn dsh(projections: &Value) -> Value {
    let totals = buckets(&projections["tokenUsage"], "dsh");
    let mut state = json!({"totals":totals,"context":{"estimated":true}});
    copy(
        &mut state["context"],
        &projections["contextPressure"],
        &[("projectedTokens", "used"), ("contextWindow", "limit")],
    );
    if state["context"]["used"].is_null() {
        copy(
            &mut state["context"],
            &projections["contextPressure"],
            &[("pressureTokens", "used")],
        );
    }
    output(&state, "dsh", false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stale_cache_observation_is_unknown_never_warm_or_cold() {
        let reply = json!({"type":"assistant","sessionId":"one","timestamp":"2026-10-11T05:00:00Z",
            "message":{"id":"one","model":"fixture","usage":{"input_tokens":10,"cache_read_input_tokens":500,
                "cache_creation_input_tokens":100,"cache_creation":{"ephemeral_1h_input_tokens":100}}}});
        // An observation whose deadline passed is unknown: not warm, and not the
        // cold cache the desktop reads from a past deadline (H7).
        let mut expired = json!({"totals":{},"context":{}});
        observe(&mut expired, &reply, "claude", "one");
        assert_eq!(expired["prompt_cache"]["status"], "warm");
        let lapsed = settled_cache(&expired);
        assert_eq!(lapsed["status"], "unknown");
        assert!(lapsed["expires_at"].is_null());
        // It still names the lifetime that ran out and when: keep-warm reads
        // a cache that went cold, not a lifetime never reported.
        assert_eq!(lapsed["ttl_seconds"], 3600);
        assert_eq!(lapsed["lapsed_at"], expired["prompt_cache"]["expires_at"]);
        // A prompt after the observation makes its deadline untrustworthy.
        let mut later = json!({"totals":{},"context":{}});
        observe(&mut later, &reply, "claude", "one");
        observe(&mut later, &json!({"type":"user","sessionId":"one","timestamp":"2026-10-11T05:30:00Z"}), "claude", "one");
        assert_eq!(settled_cache(&later)["status"], "unknown");
        assert!(settled_cache(&later)["lapsed_at"].is_null());
        // The observation the session's last turn left, with its deadline
        // ahead, stays warm.
        let mut fresh = json!({"totals":{},"context":{}});
        let mut future = reply.clone();
        future["timestamp"] = json!("2099-01-01T00:00:00Z");
        observe(&mut fresh, &future, "claude", "one");
        assert_eq!(settled_cache(&fresh)["status"], "warm");
    }

    #[test]
    fn claude_compaction_replaces_context_and_invalidates_old_cache_evidence() {
        let mut state = json!({"totals":{},"seen":{},"context":{"limit":1000000}});
        let mut reply = json!({"type":"assistant","sessionId":"one","timestamp":"2026-10-11T05:00:00Z",
            "message":{"id":"before","model":"fixture","usage":{"input_tokens":10,"cache_read_input_tokens":331000,
                "cache_creation_input_tokens":200,"output_tokens":30,"cache_creation":{"ephemeral_1h_input_tokens":200}}}});
        observe(&mut state,&reply,"claude","one");
        let totals = state["totals"].clone();
        assert_eq!(state["prompt_cache"]["status"],"warm");
        let mut boundary = json!({"type":"system","subtype":"compact_boundary","sessionId":"other",
            "timestamp":"2026-10-11T06:15:00Z","compactMetadata":{"trigger":"manual","preTokens":331210,"postTokens":23982}});
        observe(&mut state,&boundary,"claude","one");
        assert_eq!(state["context"]["used"],331210);
        boundary["sessionId"]=json!("one");
        observe(&mut state,&boundary,"claude","one");
        assert_eq!(state["context"],json!({"limit":1000000,"used":23982}));
        assert_eq!(state["prompt_cache"]["status"],"unknown");
        assert!(state["prompt_cache"]["expires_at"].is_null());
        assert_eq!(state["totals"],totals);
        // Preserved pre-compaction snapshots cannot revive the old context.
        observe(&mut state,&reply,"claude","one");
        assert_eq!(state["context"]["used"],23982);
        assert_eq!(state["prompt_cache"]["status"],"unknown");
        reply["timestamp"]=json!("2026-10-11T06:16:00Z");
        reply["message"]["id"]=json!("after");
        reply["message"]["usage"]=json!({"input_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":24000,
            "cache_creation":{"ephemeral_5m_input_tokens":24000},"output_tokens":20});
        observe(&mut state,&reply,"claude","one");
        assert_eq!(state["context"]["used"],24010);
        assert_eq!(state["prompt_cache"]["ttl_seconds"],300);
        boundary["timestamp"]=json!("2026-10-11T06:17:00Z");
        boundary["compactMetadata"].as_object_mut().unwrap().remove("postTokens");
        observe(&mut state,&boundary,"claude","one");
        assert!(state["context"]["used"].is_null());
        assert_eq!(state["context"]["limit"],1000000);
        assert_eq!(state["prompt_cache"]["status"],"unknown");
    }

    #[test]
    fn claude_compaction_upgrades_existing_incremental_usage_cache() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("transcript.jsonl");
        let cache = temp.path().join("usage.json");
        fs::write(&path,concat!(
            "{\"type\":\"assistant\",\"sessionId\":\"one\",\"message\":{\"id\":\"before\",\"usage\":{\"input_tokens\":331000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
            "{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"sessionId\":\"one\",\"timestamp\":\"2026-10-11T06:15:00Z\",\"compactMetadata\":{\"postTokens\":24000}}\n"
        )).unwrap();
        assert_eq!(scan(&path,&cache,"claude","one").unwrap()["context"]["used"],24000);
        let mut old: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        old["version"]=json!(3); old["context"]["used"]=json!(331000);
        fs::write(&cache,old.to_string()).unwrap();
        let fresh = scan(&path,&cache,"claude","one").unwrap();
        assert_eq!(fresh["context"]["used"],24000);
        assert_eq!(fresh["prompt_cache"]["source"],"native_compaction");
        assert_eq!(fresh["totals"]["input"],331000);
    }

    #[test]
    fn cache_deadlines_use_reported_ttl_and_do_not_refresh_duplicate_responses() {
        let mut state=json!({});
        let mut event=json!({"timestamp":"2026-10-06T10:00:00Z","message":{"id":"one","model":"claude-model"}});
        let mut usage=json!({"input_tokens":12,"cache_read_input_tokens":500,"cache_creation_input_tokens":100,"cache_creation":{"ephemeral_1h_input_tokens":100,"ephemeral_5m_input_tokens":0}});
        cache_observation(&mut state,&event,&usage,"claude");let deadline=state["prompt_cache"]["expires_at"].clone();
        assert_eq!(state["prompt_cache"]["ttl_seconds"],3600);
        event["timestamp"]=json!("2026-10-06T10:01:00Z");cache_observation(&mut state,&event,&usage,"claude");assert_eq!(state["prompt_cache"]["expires_at"],deadline);
        event["message"]["id"]=json!("two");usage["cache_creation"]["ephemeral_5m_input_tokens"]=json!(10);
        cache_observation(&mut state,&event,&usage,"claude");assert_eq!(state["prompt_cache"]["ttl_seconds"],300);
        cache_observation(&mut state,&event,&json!({"input_tokens":1000,"cached_input_tokens":900}),"codex");
        assert_eq!(state["prompt_cache"]["status"],"unknown");assert!(state["prompt_cache"]["expires_at"].is_null());
    }
    #[test]
    fn kimi_capacity_uses_exact_native_alias_and_not_output_limit() {
        let config = "default_model = 'other'\n[models.'kimi-code/k3']\nmax_context_size = 1048576\nmax_output_size = 16384\n[models.other]\nmax_context_size = 262144\n";
        assert_eq!(kimi_capacity(config, "kimi-code/k3"), Some(1048576));
        assert_eq!(kimi_capacity(config, "k3"), None);
        assert_eq!(kimi_capacity(config, ""), None);
        assert_eq!(
            kimi_capacity("[models.k3]\nmax_context_size = 0", "k3"),
            None
        );
        let mut state = json!({"context":{}});
        observe(
            &mut state,
            &json!({"type":"llm.request","agentId":"main","modelAlias":"kimi-code/k3","maxTokens":16384}),
            "kimi",
            "one",
        );
        assert_eq!(state["model"], "kimi-code/k3");
        assert!(state["context"]["limit"].is_null());
        observe(
            &mut state,
            &json!({"type":"profile.bind","agentId":"main","modelAlias":"other"}),
            "kimi",
            "one",
        );
        assert_eq!(state["model"], "other");
    }
    #[test]
    fn provider_accounting_does_not_double_count_cache_or_mirrors() {
        let mut state = json!({"totals":{},"seen":{},"context":{}});
        let mut event = json!({"type":"assistant","sessionId":"one","message":{"id":"msg-a","usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":5,"output_tokens_details":{"thinking_tokens":3}}}});
        observe(&mut state, &event, "claude", "one");
        observe(&mut state, &event, "claude", "one");
        event["message"]["usage"]["output_tokens"] = json!(9);
        observe(&mut state, &event, "claude", "one");
        observe(&mut state, &event, "claude", "other");
        assert_eq!(state["totals"]["total"], 139);
        assert_eq!(state["totals"]["reasoning"], 3);
        assert_eq!(state["requests"], 1);
        observe(
            &mut state,
            &json!({"type":"cost-state","sessionId":"other","totalCostUSD":99}),
            "claude",
            "one",
        );
        assert!(state["cost_usd"].is_null());
        observe(
            &mut state,
            &json!({"type":"cost-state","sessionId":"one","totalCostUSD":1.2345,"totalAPIDuration":1234}),
            "claude",
            "one",
        );
        assert_eq!(state["cost_usd"], 1.2345);
        assert_eq!(state["extras"]["api_ms"], 1234);
        observe(&mut state, &event, "claude", "one");
        assert_eq!(state["cost_stale"], true);
        let codex = json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":10,"reasoning_output_tokens":6,"total_tokens":110}}}});
        observe(&mut state, &codex, "codex", "one");
        observe(&mut state, &codex, "codex", "one");
        assert_eq!(state["totals"]["total"], 110);
        let projections = json!({"tokenUsage":{"uncachedInputTokens":10,"outputTokens":5,"cacheReadTokens":100,"cacheWriteTokens":20},"contextPressure":{"projectedTokens":145,"contextWindow":1000}});
        let result = dsh(&projections);
        assert_eq!(result["totals"]["total"], 135);
        assert_eq!(result["context"]["used"], 145);
    }
    #[test]
    fn incremental_cache_handles_partial_append_rotation_and_kimi_mirrors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wire.jsonl");
        let cache = temp.path().join("cache.json");
        let event=json!({"type":"usage.record","agentId":"main","usage":{"inputOther":10,"output":5,"inputCacheRead":20,"inputCacheCreation":0}}).to_string()+"\n";
        fs::write(&path, &event).unwrap();
        let first = scan(&path, &cache, "kimi", "one").unwrap();
        assert_eq!(first["totals"]["total"], 35);
        assert_eq!(
            scan(&path, &cache, "kimi", "one").unwrap()["totals"],
            first["totals"]
        );
        fs::write(&path, format!("{event}{}", &event[..30])).unwrap();
        assert_eq!(
            scan(&path, &cache, "kimi", "one").unwrap()["totals"],
            first["totals"]
        );
        fs::write(&path, format!("{event}{event}")).unwrap();
        assert_eq!(
            scan(&path, &cache, "kimi", "one").unwrap()["totals"]["total"],
            70
        );
        fs::write(&path, &event).unwrap();
        assert_eq!(
            scan(&path, &cache, "kimi", "one").unwrap()["totals"]["total"],
            35
        );
        let mut state = json!({"totals":{},"context":{}});
        observe(
            &mut state,
            &json!({"type":"agent.message.appended","message":{"meta":{"usage":{"inputOther":99}}}}),
            "kimi",
            "one",
        );
        assert_eq!(state["totals"], json!({}));
    }
    #[test]
    fn a_scan_waiting_for_the_cache_lock_reads_metadata_after_it() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = now();
        let line = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}}).to_string() + "\n";
        fs::write(&path, line(at - 7000.0, "old")).unwrap();
        let first = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(first["partial"], false);
        let guard = lock(Some(&cache.with_extension("lock"))).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        BEFORE_CACHE_LOCK.lock().unwrap().replace((cache.clone(), sender));
        let (waiter_path, waiter_cache) = (path.clone(), cache.clone());
        let waiter = std::thread::spawn(move || scan(&waiter_path, &waiter_cache, "claude", "one"));
        receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        // The waiter is about to take the lock. Append a recent 1 h write while
        // it waits, then let it read its metadata under the lock.
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(line(at - 3000.0, "recent").as_bytes())
            .unwrap();
        drop(guard);
        let second = waiter.join().unwrap().unwrap();
        assert_eq!(second["partial"], false);
        assert!(
            second["prompt_cache"]["expires_at"].as_f64().unwrap() > at,
            "a waiting scan must see the append, not the pre-lock size: {second}"
        );
    }

    #[test]
    fn an_oversized_record_is_skipped_to_its_newline() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = now();
        let line = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}}).to_string() + "\n";
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(line(at - 7000.0, "old").as_bytes()).unwrap();
        file.write_all(b"{\"type\":\"user\",\"message\":{\"content\":\"").unwrap();
        let chunk = [b'x'; 64 * 1024];
        for _ in 0..(MAX_READ / 65536 + 16) {
            file.write_all(&chunk).unwrap();
        }
        file.write_all(b"\"}}\n").unwrap();
        file.write_all(line(at - 3000.0, "recent").as_bytes()).unwrap();
        drop(file);
        // The oversized record is discarded; the following record is read by
        // a later round, so the scan converges instead of stalling.
        let mut complete = None;
        for _ in 0..4 {
            let value = scan(&path, &cache, "claude", "one").unwrap();
            if value["partial"] == false {
                complete = Some(value);
                break;
            }
        }
        let complete = complete.expect("the oversized record did not converge");
        assert!(
            complete["prompt_cache"]["expires_at"].as_f64().unwrap() > at,
            "the oversized record did not stall the scan: {complete}"
        );
    }

    #[test]
    fn a_long_oversized_record_is_discarded_within_each_scan_budget() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = now();
        let line = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}}).to_string() + "\n";
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(line(at - 7000.0, "old").as_bytes()).unwrap();
        file.write_all(b"{\"type\":\"user\",\"message\":{\"content\":\"").unwrap();
        let chunk = [b'x'; 64 * 1024];
        let mut written = 0u64;
        while written < 128 * 1024 * 1024 {
            let count = chunk.len().min((128 * 1024 * 1024 - written) as usize);
            file.write_all(&chunk[..count]).unwrap();
            written += count as u64;
        }
        file.write_all(b"\"}}\n").unwrap();
        file.write_all(line(at - 3000.0, "recent").as_bytes()).unwrap();
        drop(file);
        let mut previous = 0u64;
        let mut saw_skipping = false;
        let mut complete = None;
        for _ in 0..10 {
            let value = scan(&path, &cache, "claude", "one").unwrap();
            let state: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
            let offset = state["offset"].as_u64().unwrap();
            assert!(offset - previous <= MAX_READ, "one scan consumed {} bytes", offset - previous);
            saw_skipping |= state["skipping"].as_bool() == Some(true);
            previous = offset;
            if value["partial"] == false {
                complete = Some(value);
                break;
            }
        }
        let complete = complete.expect("the 128 MiB record never converged");
        assert!(saw_skipping, "the record must be discarded across scans");
        assert!(complete["prompt_cache"]["expires_at"].as_f64().unwrap() > at,
            "the recent record after the discarded one was not read: {complete}");
    }

    #[test]
    fn a_discarded_record_is_not_parsed_from_its_middle() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = now();
        let line = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}}).to_string() + "\n";
        let old = line(at - 7000.0, "old");
        let inner = line(at - 3000.0, "mid");
        // Byte-for-byte, a full read bound starting at the oversized record's
        // own start ends exactly at a valid assistant record, so a lost
        // skipping flag would parse that suffix on the next scan.
        let open = b"{\"type\":\"user\",\"message\":{\"content\":\"";
        let close = b"\"}}";
        let target = MAX_READ;
        let filler_len = target - open.len() as u64 - close.len() as u64;
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(old.as_bytes()).unwrap();
        file.write_all(open).unwrap();
        let chunk = [b'y'; 64 * 1024];
        let mut written = 0u64;
        while written < filler_len {
            let count = chunk.len().min((filler_len - written) as usize);
            file.write_all(&chunk[..count]).unwrap();
            written += count as u64;
        }
        file.write_all(close).unwrap();
        file.write_all(inner.as_bytes()).unwrap();
        drop(file);
        // The first scan stops before the oversized record; the second reads a
        // full bound from its start and persists the discard point.
        let first = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(first["partial"], true);
        let first_state: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert_eq!(first_state["skipping"], json!(false));
        assert_eq!(first_state["offset"], json!(old.len()));
        let second = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(second["partial"], true);
        let second_state: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert_eq!(second_state["skipping"], json!(true));
        assert_eq!(second_state["offset"], json!(old.len() + MAX_READ as usize));
        // A restart inside the record continues the discard; it does not parse
        // the assistant-shaped suffix that begins at the stored offset.
        let third = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(third["partial"], false);
        assert!(third["prompt_cache"]["expires_at"].as_f64().or(third["prompt_cache"]["lapsed_at"].as_f64()).unwrap() < at,
            "a record was parsed from the middle of the discarded one: {third}");
        assert_eq!(third["requests"], 1);
    }

    #[test]
    fn a_record_straddling_the_scan_budget_is_read_by_the_next_scan() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = now();
        let line = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}}).to_string() + "\n";
        let old = line(at - 7000.0, "old");
        let recent = line(at - 3000.0, "recent");
        assert!(recent.len() > 50);
        // An ordinary row leaves exactly 50 bytes of this scan's budget; the
        // recent assistant row is longer than that, so it straddles the end.
        let open = b"{\"type\":\"user\",\"sessionId\":\"one\",\"message\":{\"content\":\"";
        let close = b"\"}}";
        let target = MAX_READ - 50 - old.len() as u64;
        let filler_len = target - open.len() as u64 - close.len() as u64 - 1;
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(old.as_bytes()).unwrap();
        file.write_all(open).unwrap();
        let chunk = [b'z'; 64 * 1024];
        let mut written = 0u64;
        while written < filler_len {
            let count = chunk.len().min((filler_len - written) as usize);
            file.write_all(&chunk[..count]).unwrap();
            written += count as u64;
        }
        file.write_all(close).unwrap();
        file.write_all(b"\n").unwrap();
        file.write_all(recent.as_bytes()).unwrap();
        drop(file);
        let first = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(first["partial"], true);
        let state: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert_eq!(state["skipping"], json!(false), "a straddling record is not oversized");
        assert_eq!(state["offset"], json!(MAX_READ - 50));
        // The next scan reads the whole record from its start with a full budget.
        let second = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(second["partial"], false);
        assert!(second["prompt_cache"]["expires_at"].as_f64().unwrap() > at,
            "the straddling record was discarded: {second}");
        assert_eq!(second["requests"], 2);
    }

    #[test]
    fn a_newline_exactly_at_the_read_bound_is_still_a_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = now();
        let assistant = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}});
        // The first record's newline is exactly the last byte of a full read
        // bound: the scan must parse it, not mark it oversized.
        let mut first = assistant(at - 7000.0, "old");
        first["pad"] = json!("");
        let pad = MAX_READ as usize - first.to_string().len() - 1;
        first["pad"] = json!("x".repeat(pad));
        let mut text = first.to_string();
        assert_eq!(text.len() + 1, MAX_READ as usize);
        text.push('\n');
        text.push_str(&assistant(at - 3000.0, "recent").to_string());
        text.push('\n');
        fs::write(&path, text).unwrap();
        let value = scan(&path, &cache, "claude", "one").unwrap();
        let state: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert_eq!(state["skipping"], json!(false), "the exact-bound newline was treated as oversized");
        assert_eq!(value["requests"], 1, "the exact-bound record was not parsed: {value}");
        // The record fills the whole read bound, so the next one is pending.
        assert_eq!(value["partial"], true);
        let next = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(next["partial"], false);
        assert_eq!(next["requests"], 2);
        assert!(next["prompt_cache"]["expires_at"].as_f64().unwrap() > at,
            "the exact-bound record and the recent one were not read: {next}");
    }

    #[test]
    fn a_record_without_its_newline_at_eof_stays_pending_until_it_arrives() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = now();
        let line = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}}).to_string() + "\n";
        let old = line(at - 7000.0, "old");
        let recent = line(at - 3000.0, "recent");
        // The second record's newline has not been written yet: EOF inside it.
        fs::write(&path, format!("{old}{}", &recent[..recent.len() - 1])).unwrap();
        let first = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(first["partial"], true, "a record ending at EOF without its newline is pending: {first}");
        assert_eq!(first["requests"], 1, "the newline-less tail must not be parsed: {first}");
        // The newline arrives: the pending record is parsed exactly once.
        fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"\n").unwrap();
        let second = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(second["partial"], false);
        assert_eq!(second["requests"], 2);
        assert!(second["prompt_cache"]["expires_at"].as_f64().unwrap() > at);
        // A clean EOF is the cached fast path; it does not replay the record.
        let again = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(again["partial"], false);
        assert_eq!(again["requests"], 2);
    }

    #[test]
    fn an_oversized_record_ending_at_eof_keeps_the_discard_pending() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = now();
        let line = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}}).to_string() + "\n";
        // A record with no newline that ends exactly at EOF, one read bound
        // plus 64 bytes: the first scan starts discarding it and the second
        // finds EOF inside it, keeping the discard pending.
        let open = b"{\"type\":\"user\",\"message\":{\"content\":\"";
        let target = MAX_READ + 64 - open.len() as u64;
        let chunk = [b'x'; 64 * 1024];
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(open).unwrap();
        let mut written = 0u64;
        while written < target {
            let count = chunk.len().min((target - written) as usize);
            file.write_all(&chunk[..count]).unwrap();
            written += count as u64;
        }
        drop(file);
        let first = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(first["partial"], true);
        let state: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert_eq!(state["skipping"], json!(true));
        assert_eq!(state["offset"], json!(MAX_READ));
        let second = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(second["partial"], true, "EOF inside the discarded record is pending: {second}");
        let state: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert_eq!(state["skipping"], json!(true));
        assert_eq!(state["offset"], json!(MAX_READ + 64));
        // A clean EOF still keeps it pending; nothing is parsed from the middle.
        let third = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(third["partial"], true);
        assert_eq!(third["requests"], Value::Null);
        // Completing the newline and adding a recent record finishes the discard.
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"\n").unwrap();
        file.write_all(line(at - 3000.0, "recent").as_bytes()).unwrap();
        drop(file);
        let fourth = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(fourth["partial"], false);
        assert_eq!(fourth["requests"], 1);
        assert!(fourth["prompt_cache"]["expires_at"].as_f64().unwrap() > at);
    }

    #[test]
    fn a_replaced_or_truncated_transcript_resets_the_cached_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let cache = dir.path().join("cache.json");
        let at = 1_794_000_000.0;
        let line = |at: f64, id: &str| json!({"type":"assistant","sessionId":"one","timestamp":at,
            "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":3,
                "cache_creation":{"ephemeral_1h_input_tokens":20}}}}).to_string() + "\n";
        let old = line(at - 7000.0, "old");
        let recent = line(at - 3000.0, "recent");
        let original = format!("{old}{recent}");
        fs::write(&path, &original).unwrap();
        let first = scan(&path, &cache, "claude", "one").unwrap();
        let old_expiry = first["prompt_cache"]["expires_at"].as_f64().or(first["prompt_cache"]["lapsed_at"].as_f64()).unwrap();
        assert_eq!(first["requests"], 2);
        assert_eq!(first["partial"], false);
        // The same path is rewritten in place at the same length with the same
        // inode and 256-byte prefix: only the size-and-stamp check catches it.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let latest = line(at - 3500.0, "latest");
        let replacement = format!("{old}{latest}");
        assert_eq!(replacement.len(), original.len());
        fs::write(&path, &replacement).unwrap();
        let second = scan(&path, &cache, "claude", "one").unwrap();
        let new_expiry = second["prompt_cache"]["expires_at"].as_f64().or(second["prompt_cache"]["lapsed_at"].as_f64()).unwrap();
        assert_eq!(second["partial"], false);
        assert_eq!(second["requests"], 2);
        assert!(new_expiry < old_expiry - 100.0,
            "the same-size replacement was served from the old offset: {second}");
        // Truncating the transcript leaves the cached offset past its end: the
        // scan resets and reports only the remaining record.
        fs::write(&path, &old).unwrap();
        let third = scan(&path, &cache, "claude", "one").unwrap();
        assert_eq!(third["partial"], false);
        assert_eq!(third["requests"], 1);
        assert!(third["prompt_cache"]["expires_at"].as_f64().or(third["prompt_cache"]["lapsed_at"].as_f64()).unwrap() < new_expiry - 100.0,
            "the truncated transcript was served from the old offset: {third}");
    }

    #[test]
    fn codex_rejects_mismatched_identity_even_with_cached_usage() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rollout.jsonl");
        let cache = temp.path().join("cache.json");
        fs::write(
            &path,
            json!({"type":"session_meta","payload":{"id":"one"}}).to_string() + "\n",
        )
        .unwrap();
        assert_eq!(
            scan(&path, &cache, "codex", "one").unwrap()["status"],
            "unavailable"
        );
        assert!(scan(&path, &cache, "codex", "other").is_err());
        assert_eq!(dsh(&json!({}))["status"], "unavailable");
        assert_eq!(
            buckets(&json!({"input_tokens":-3,"output_tokens":null}), "claude"),
            json!({})
        );
    }

    /// U38: a Claude transcript's requests inside a window, with its cost and
    /// work readings, per model; a request outside the window is not counted.
    #[test]
    fn a_span_sums_claude_tokens_cost_and_models_in_a_window() {
        let temp = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(temp.path(), || {
            let id = "11111111-1111-4111-8111-111111111111";
            let path = temp.path().join(format!("{id}.jsonl"));
            let lines = [
                json!({"type":"cost-state","sessionId":id,"timestamp":"2026-10-11T04:59:30Z","totalCostUSD":0.2,"totalAPIDuration":10,"totalToolDuration":5}),
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-10-11T05:00:00Z","message":{"id":"r1","model":"opus","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}),
                json!({"type":"cost-state","sessionId":id,"timestamp":"2026-10-11T05:00:10Z","totalCostUSD":0.5,"totalAPIDuration":100,"totalToolDuration":50}),
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-10-11T05:01:00Z","message":{"id":"r2","model":"sonnet","usage":{"input_tokens":30,"output_tokens":7,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}),
                json!({"type":"cost-state","sessionId":id,"timestamp":"2026-10-11T05:01:10Z","totalCostUSD":0.9,"totalAPIDuration":400,"totalToolDuration":100}),
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-10-11T05:05:00Z","message":{"id":"r3","model":"opus","usage":{"input_tokens":999,"output_tokens":999}}}),
            ];
            let text = lines.iter().map(|line| line.to_string() + "\n").collect::<String>();
            fs::write(&path, text).unwrap();
            let record = json!({"agent":"claude","conversation_id":id,"transcript":path.to_string_lossy()});
            let result = span(
                &record,
                moment(&json!("2026-10-11T04:59:40Z")),
                moment(&json!("2026-10-11T05:02:00Z")),
            );
            assert_eq!(result["status"], "ok", "{result}");
            let providers = result["providers"].as_array().unwrap();
            assert_eq!(providers.len(), 1, "{result}");
            let provider = &providers[0];
            assert_eq!(provider["provider"], "claude");
            let models = provider["models"].as_array().unwrap();
            let opus = models.iter().find(|model| model["model"] == "opus").unwrap();
            assert_eq!(opus["tokens"], json!({"uncached_input":10,"output":5,"cache_read":100,"cache_write":20,"input":130,"total":135}), "{opus}");
            let sonnet = models.iter().find(|model| model["model"] == "sonnet").unwrap();
            assert_eq!(sonnet["tokens"], json!({"uncached_input":30,"output":7,"cache_read":0,"cache_write":0,"input":30,"total":37}), "{sonnet}");
            assert!(!models.iter().any(|model| model.to_string().contains("999")), "a request outside the window is not counted");
            assert!((provider["cost_usd"].as_f64().unwrap() - 0.7).abs() < 1e-9, "{provider}");
            assert_eq!(provider["work_ms"].as_f64(), Some(485.0), "the work readings differ across the window");
        });
    }

    /// #736: several windows count only the samples inside them: the gap
    /// between a task's own spans is not read.
    #[test]
    fn a_long_sessions_early_hours_stay_countable() {
        // 3000 requests a minute apart: the first 2000 fold per minute instead
        // of going, so a span over the first hour still counts every request.
        let mut state = json!({});
        for n in 0..3000u64 {
            push_history(&mut state, json!({"at":1_000_000.0 + n as f64 * 60.0 + 10.0,"provider":"claude","model":"opus","tokens":{"uncached_input":1,"output":2}}));
        }
        let history = state["history"].as_array().unwrap();
        assert_eq!(history.len(), 3000, "one request a minute folds into one sample a minute");
        assert!(history.iter().filter(|sample| sample["minute"] == true).count() >= 1000);
        assert!(state["history_truncated"] != json!(true));
        let record_state = state.clone();
        let first_hour: u64 = record_state["history"].as_array().unwrap().iter()
            .filter(|sample| sample["at"].as_f64().unwrap() <= 1_000_000.0 + 3600.0 + 60.0)
            .map(|sample| sample["tokens"]["output"].as_u64().unwrap_or(0)).sum();
        assert_eq!(first_hour, 2 * 61, "the first hour's requests are all still counted");
        // A minute with several requests folds into one sample with their sum.
        let mut busy = json!({});
        for n in 0..(2 * FINE as u64 + 50) {
            push_history(&mut busy, json!({"at":2_000_000.0 + n as f64,"provider":"claude","model":"opus","tokens":{"output":1}}));
        }
        let folded: Vec<&Value> = busy["history"].as_array().unwrap().iter().filter(|sample| sample["minute"] == true).collect();
        assert!(!folded.is_empty());
        let sum: u64 = busy["history"].as_array().unwrap().iter().map(|sample| sample["tokens"]["output"].as_u64().unwrap_or(0)).sum();
        assert_eq!(sum, 2 * FINE as u64 + 50, "no request is lost when folded");
    }

    #[test]
    fn a_span_many_counts_only_the_samples_inside_its_windows() {
        let temp = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(temp.path(), || {
            let id = "33333333-3333-4333-8333-333333333333";
            let path = temp.path().join(format!("{id}.jsonl"));
            let lines = [
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-10-11T05:00:00Z","message":{"id":"r1","model":"opus","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}),
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-10-11T05:01:00Z","message":{"id":"r2","model":"opus","usage":{"input_tokens":500,"output_tokens":500,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}),
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-10-11T05:02:00Z","message":{"id":"r3","model":"opus","usage":{"input_tokens":7,"output_tokens":3,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}),
            ];
            let text = lines.iter().map(|line| line.to_string() + "\n").collect::<String>();
            fs::write(&path, text).unwrap();
            let record = json!({"agent":"claude","conversation_id":id,"transcript":path.to_string_lossy()});
            let result = span_many(
                &record,
                &[
                    (moment(&json!("2026-10-11T04:59:40Z")), moment(&json!("2026-10-11T05:00:10Z"))),
                    (moment(&json!("2026-10-11T05:01:50Z")), moment(&json!("2026-10-11T05:02:10Z"))),
                ],
            );
            assert_eq!(result["status"], "ok", "{result}");
            let tokens = &result["providers"][0]["models"][0]["tokens"];
            assert_eq!((tokens["uncached_input"].clone(), tokens["output"].clone()), (json!(17), json!(8)), "{result}");
            assert_eq!(result["providers"][0]["models"][0]["requests"], 2);
        });
    }

    /// U38: a Codex transcript's cumulative token snapshots become per-window
    /// counters by difference, with its model.
    #[test]
    fn a_span_sums_codex_token_snapshots_in_a_window() {
        let temp = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(temp.path(), || {
            let id = "22222222-2222-4222-8222-222222222222";
            let path = temp.path().join(format!("{id}.jsonl"));
            let lines = [
                json!({"type":"session_meta","payload":{"id":id}}),
                json!({"type":"event_msg","timestamp":"2026-10-11T06:00:00Z","payload":{"type":"token_count","model":"gpt-x","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"total_tokens":15}}}}),
                json!({"type":"event_msg","timestamp":"2026-10-11T06:01:00Z","payload":{"type":"token_count","model":"gpt-x","info":{"total_token_usage":{"input_tokens":30,"cached_input_tokens":4,"output_tokens":12,"total_tokens":42}}}}),
                json!({"type":"event_msg","timestamp":"2026-10-11T06:05:00Z","payload":{"type":"token_count","model":"gpt-x","info":{"total_token_usage":{"input_tokens":40,"cached_input_tokens":4,"output_tokens":20,"total_tokens":60}}}}),
            ];
            let text = lines.iter().map(|line| line.to_string() + "\n").collect::<String>();
            fs::write(&path, text).unwrap();
            let record = json!({"agent":"codex","conversation_id":id,"transcript":path.to_string_lossy()});
            let result = span(
                &record,
                moment(&json!("2026-10-11T05:59:00Z")),
                moment(&json!("2026-10-11T06:02:00Z")),
            );
            assert_eq!(result["status"], "ok", "{result}");
            let provider = &result["providers"][0];
            assert_eq!(provider["provider"], "codex");
            let model = &provider["models"][0];
            assert_eq!(model["model"], "gpt-x");
            assert_eq!(model["tokens"], json!({"input":30,"cache_read":4,"output":12,"total":42}), "{model}");
            assert_eq!(model["requests"], 2);
            assert!(provider["cost_usd"].is_null(), "Codex reports no cost");
        });
    }

    /// U38: a window across a session restart reads both runs; the history
    /// survives the new transcript, and the cumulative cost spans them.
    #[test]
    fn a_span_across_a_session_restart_reads_both_runs() {
        let temp = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(temp.path(), || {
            let id = "33333333-3333-4333-8333-333333333333";
            let first = temp.path().join(format!("{id}-one.jsonl"));
            let lines = [
                json!({"type":"cost-state","sessionId":id,"timestamp":"2026-10-11T03:58:00Z","totalCostUSD":0.1}),
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-10-11T04:01:00Z","message":{"id":"a","model":"m","usage":{"input_tokens":10,"output_tokens":2}}}),
                json!({"type":"cost-state","sessionId":id,"timestamp":"2026-10-11T04:02:00Z","totalCostUSD":0.3}),
            ];
            fs::write(&first, lines.iter().map(|line| line.to_string() + "\n").collect::<String>()).unwrap();
            let record = json!({"agent":"claude","conversation_id":id,"transcript":first.to_string_lossy()});
            let early = span(
                &record,
                moment(&json!("2026-10-11T03:59:00Z")),
                moment(&json!("2026-10-11T04:03:00Z")),
            );
            assert_eq!(early["status"], "ok", "{early}");
            assert_eq!(early["providers"][0]["models"][0]["tokens"]["uncached_input"], 10, "{early}");
            assert!((early["providers"][0]["cost_usd"].as_f64().unwrap() - 0.2).abs() < 1e-9, "{early}");
            // The restarted session writes a new transcript; a window across it
            // reads the old run's history and the new one.
            let second = temp.path().join(format!("{id}-two.jsonl"));
            let lines = [
                json!({"type":"cost-state","sessionId":id,"timestamp":"2026-10-11T05:00:00Z","totalCostUSD":0.5}),
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-10-11T05:01:00Z","message":{"id":"b","model":"m","usage":{"input_tokens":20,"output_tokens":3}}}),
                json!({"type":"cost-state","sessionId":id,"timestamp":"2026-10-11T05:02:00Z","totalCostUSD":0.9}),
            ];
            fs::write(&second, lines.iter().map(|line| line.to_string() + "\n").collect::<String>()).unwrap();
            let record = json!({"agent":"claude","conversation_id":id,"transcript":second.to_string_lossy()});
            let whole = span(
                &record,
                moment(&json!("2026-10-11T03:59:00Z")),
                moment(&json!("2026-10-11T05:03:00Z")),
            );
            assert_eq!(whole["status"], "ok", "{whole}");
            let model = &whole["providers"][0]["models"][0];
            assert_eq!((model["tokens"]["uncached_input"].as_u64(), model["tokens"]["output"].as_u64()), (Some(30), Some(5)), "{whole}");
            assert_eq!(model["requests"], 2);
            assert!((whole["providers"][0]["cost_usd"].as_f64().unwrap() - 0.8).abs() < 1e-9, "{whole}");
        });
    }

    /// U38: no timestamped counters is unknown, never zero; a DeepSeek session's
    /// native counters are cumulative projections without a transcript, so a
    /// span of it is unknown too.
    /// Kimi's wire log timestamps every usage.record, so a window of a Kimi
    /// member's work is the sum of the requests inside it (task 78).
    #[test]
    fn a_kimi_span_sums_the_requests_inside_its_windows() {
        let temp = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(temp.path(), || {
            let home = temp.path().join("kimi-home");
            let wire = home.join("sessions/workspace/session-k1/agents/main");
            fs::create_dir_all(&wire).unwrap();
            let request = |at: &str, input: u64, output: u64| {
                json!({"type":"usage.record","agentId":"main","timestamp":at,"modelAlias":"k2",
                    "usage":{"inputOther":input,"output":output,"inputCacheRead":1000,"inputCacheCreation":0}}).to_string() + "
"
            };
            fs::write(
                wire.join("wire.jsonl"),
                request("2026-10-11T05:00:10Z", 10, 5) + &request("2026-10-11T05:01:10Z", 20, 6) + &request("2026-10-11T05:05:10Z", 40, 7),
            )
            .unwrap();
            let record = json!({"agent":"kimi","name":"kimi/p/dev","conversation_id":"session-k1","agent_home":home.to_string_lossy()});
            let at = |text: &str| moment(&json!(text));
            let result = span_many(&record, &[(at("2026-10-11T05:00:00Z"), at("2026-10-11T05:02:00Z"))]);
            assert_eq!(result["status"], "ok", "{result}");
            let tokens = &result["providers"][0]["models"][0]["tokens"];
            assert_eq!((tokens["uncached_input"].clone(), tokens["output"].clone()), (json!(30), json!(11)), "{result}");
            // Two windows count only their own requests.
            let two = span_many(&record, &[(at("2026-10-11T05:00:00Z"), at("2026-10-11T05:00:30Z")), (at("2026-10-11T05:04:00Z"), at("2026-10-11T05:06:00Z"))]);
            assert_eq!(two["providers"][0]["models"][0]["tokens"]["uncached_input"], 50, "{two}");
            // A session with no wire log is unknown, never zero.
            let missing = json!({"agent":"kimi","name":"kimi/p/qa","conversation_id":"session-none","agent_home":home.to_string_lossy()});
            let result = span_many(&missing, &[(1000.0, 2000.0)]);
            assert_eq!(result["status"], "unknown", "{result}");
        });
    }

    #[test]
    fn a_span_without_counters_is_unknown_never_zero() {
        let temp = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(temp.path(), || {
            let id = "44444444-4444-4444-8444-444444444444";
            let path = temp.path().join(format!("{id}.jsonl"));
            fs::write(
                &path,
                json!({"type":"user","sessionId":id,"timestamp":"2026-10-11T05:00:00Z"}).to_string() + "\n",
            )
            .unwrap();
            let record = json!({"agent":"claude","conversation_id":id,"transcript":path.to_string_lossy()});
            let result = span(
                &record,
                moment(&json!("2026-10-11T04:59:00Z")),
                moment(&json!("2026-10-11T05:01:00Z")),
            );
            assert_eq!(result["status"], "unknown", "{result}");
            assert!(result.get("providers").is_none(), "{result}");
            assert!(!result.to_string().contains("\"total\":0"), "unknown is never zero: {result}");
            let dsh = json!({"agent":"dsh","conversation_id":id,"projections":{"tokenUsage":{"uncachedInputTokens":10,"outputTokens":5}}});
            let result = span(&dsh, 1000.0, 2000.0);
            assert_eq!(result["status"], "unknown", "{result}");
            assert!(result["reason"].as_str().unwrap().contains("Native usage unavailable"), "{result}");
            let bad = span(&record, 2000.0, 1000.0);
            assert_eq!(bad["status"], "unknown", "{bad}");
        });
    }
}
