//! Per-session reasoning settings. Read provider-owned telemetry and operate the
//! native TUI only after verifying the exact process and an empty idle composer.
//! Neither shared defaults nor the user's draft are changed by this adapter.
use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

// Original ID Effort/Settings custody. The borrowed facade remains separate;
// legacy controllers keep their existing native behavior.
#[cfg(unix)]
#[allow(dead_code)]
mod picker_scope;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) use picker_scope::front_send as front_pending_send;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn picker_custodian_entry() -> Result<i32> {
    picker_scope::custodian_entry()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn remember_picker_caller_machine(config: &crate::config::Config, expected: &str) {
    picker_scope::remember_caller_machine(config, expected);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use picker_scope::{
    deadline as picker_deadline, entered as picker_entered, helper_read as picker_helper_read,
    own_marker as picker_own_marker, refuse as picker_refuse, ReadCall as PickerRead,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use picker_scope::owned_send as picker_owned_send;

const MAX_TAIL: u64 = 8 * 1024 * 1024;
const LEVELS: &[&str] = &[
    "off", "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra", "on",
];

pub(super) fn valid(value: &str) -> bool {
    LEVELS.contains(&value)
}

#[derive(Clone, Default, Debug)]
struct Settings {
    model: String,
    effort: String,
    path: String,
    offset: u64,
}

fn agent_home(record: &Value) -> PathBuf {
    if !string(record, "agent_home").is_empty() {
        return PathBuf::from(string(record, "agent_home"));
    }
    home().join(match string(record, "agent") {
        "codex" => ".codex",
        "kimi" => ".kimi-code",
        _ => ".claude",
    })
}

fn event_settings(agent: &str, event: &Value) -> Option<(String, String)> {
    let value = match (agent, string(event, "type")) {
        ("codex", "turn_context") => &event["payload"],
        ("kimi", "profile.bind" | "llm.request") => event,
        _ => return None,
    };
    let model = if agent == "kimi" {
        string(value, "modelAlias")
    } else {
        string(value, "model")
    };
    let effort = if agent == "kimi" {
        string(value, "thinkingEffort")
    } else {
        value["effort"]
            .as_str()
            .or(value["reasoning_effort"].as_str())
            .unwrap_or("")
    };
    if model.is_empty() || model.len() > 120 || model.chars().any(char::is_control) {
        return None;
    }
    Some((
        model.into(),
        if valid(effort) {
            effort.into()
        } else {
            String::new()
        },
    ))
}

/// Claude names each transcript after its conversation; never trust another file.
fn claude_transcript(record: &Value) -> Result<PathBuf> {
    let id = string(record, "conversation_id");
    let path = PathBuf::from(string(record, "transcript"));
    if id.is_empty()
        || !path.is_absolute()
        || path.file_name().map(|name| name.to_string_lossy().into_owned())
            != Some(format!("{id}.jsonl"))
    {
        return Err("Claude's conversation transcript is not confirmed; refresh or use Terminal".into());
    }
    Ok(path)
}

/// Settings of the newest main-conversation Claude turn, or of a /model
/// receipt naming a catalog model. Rows of other conversations never count.
fn claude_settings(
    event: &Value,
    conversation: &str,
    models: &[ModelOption],
) -> Option<(String, String)> {
    if conversation.is_empty()
        || string(event, "sessionId") != conversation
        || event["isSidechain"] == true
    {
        return None;
    }
    match string(event, "type") {
        "assistant" => {
            let model = event["requestedModel"]
                .as_str()
                .filter(|model| !model.is_empty())
                .or(event["message"]["model"].as_str())?;
            // Locally generated replies carry "<synthetic>", not a model.
            if !safe_model(model) || model.starts_with('<') {
                return None;
            }
            let effort = event["effort"].as_str().filter(|value| valid(value));
            Some((model.into(), effort.unwrap_or_default().into()))
        }
        "user" | "system" => {
            let text = event["message"]["content"]
                .as_str()
                .or(event["content"].as_str())?;
            let (name, effort) = claude_receipt(text)?;
            let model = models.iter().find(|model| model.label == name)?;
            Some((model.id.clone(), effort))
        }
        _ => None,
    }
}

/// Claude's /model result: "Set model to `Name` for this session only", or
/// "... and saved as your default for new sessions", with an optional
/// " with `level` effort" when the picker changed effort.
fn claude_receipt(text: &str) -> Option<(String, String)> {
    let text = text
        .strip_prefix("<local-command-stdout>")?
        .strip_suffix("</local-command-stdout>")?;
    let (name, rest) = text.strip_prefix("Set model to `")?.split_once('`')?;
    let rest = rest
        .strip_prefix(" for this session only")
        .or_else(|| rest.strip_prefix(" and saved as your default for new sessions"))?;
    let effort = rest
        .strip_prefix(" with `")
        .and_then(|rest| rest.split_once("` effort"))
        .map(|(level, _)| level)
        .filter(|level| valid(level))
        .unwrap_or_default();
    (!name.is_empty()).then(|| (name.to_owned(), effort.to_owned()))
}

/// An unindented full-width rule, such as the borders of Claude's composer.
fn composer_rule(line: &str) -> bool {
    line.chars().count() >= 12 && line.chars().all(|ch| ch == '─')
}

/// Claude's right-aligned effort indicator directly above its composer:
/// "◐ medium · /effort", "● high · ultracode · /effort" or "effort: high · /effort".
/// It shares a notification slot, so its absence is not evidence of a change.
fn claude_footer(screen: &str) -> Option<String> {
    let lines: Vec<_> = screen.lines().collect();
    lines.iter().enumerate().rev().find_map(|(index, line)| {
        if !lines.get(index + 1).is_some_and(|next| composer_rule(next)) {
            return None;
        }
        let line = line.trim_end();
        let rest = line.trim_start().strip_suffix(" · /effort")?;
        if line.len() - line.trim_start().len() < 8 {
            return None;
        }
        let rest = rest.strip_suffix(" · ultracode").unwrap_or(rest);
        let words: Vec<_> = rest.split_whitespace().collect();
        let [.., mark, level] = words.as_slice() else {
            return None;
        };
        let glyph = mark.chars().count() == 1 && !mark.chars().all(char::is_alphanumeric);
        (valid(level) && (glyph || *mark == "effort:")).then(|| (*level).to_owned())
    })
}

/// Claude Code caches its account's signed model catalog for the /model picker.
/// The picker lists main models and its expandable overflow section.
fn claude_catalog(record: &Value) -> Vec<ModelOption> {
    type Cached = (u64, Option<SystemTime>, Vec<ModelOption>);
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Cached>>> = OnceLock::new();
    let Ok(entries) = fs::read_dir(agent_home(record).join("cache").join("model-catalog")) else {
        return vec![];
    };
    // One file per account and organization; the newest belongs to this sign-in.
    let newest = entries
        .flatten()
        .take(64)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with("-cc.json"))
        .filter_map(|entry| {
            let meta = entry.metadata().ok().filter(|meta| meta.is_file())?;
            Some((meta.modified().ok(), meta.len(), entry.path()))
        })
        .max_by_key(|(modified, _, _)| *modified);
    let Some((modified, size, path)) = newest else {
        return vec![];
    };
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some((cached_size, stamp, models)) = cache.lock().ok().as_ref().and_then(|c| c.get(&path)) {
        if *cached_size == size && *stamp == modified {
            return models.clone();
        }
    }
    let Some(value) = File::open(&path)
        .ok()
        .and_then(|file| serde_json::from_reader::<_, Value>(BufReader::new(file.take(MAX_TAIL))).ok())
    else {
        return vec![];
    };
    let catalog = &value["catalog"];
    if value["version"] != 2 || string(catalog, "surface") != "cc" {
        return vec![];
    }
    let defaults = catalog["state"]["thinking_by_model"].as_array();
    let mut models = Vec::new();
    for entry in catalog["config"]["models"].as_array().into_iter().flatten().take(64) {
        let (id, label) = (string(entry, "id"), string(entry, "name"));
        if !["main", "overflow"].contains(&string(entry, "section")) || !safe_model(id) || !safe_model(label) {
            continue;
        }
        let thinking = &entry["thinking"];
        let options = thinking["effort_options"].as_array().filter(|_| string(thinking, "type") == "effort");
        let efforts: Vec<String> = options
            .into_iter()
            .flatten()
            .map(|option| string(option, "id"))
            .filter(|value| valid(value))
            .map(str::to_owned)
            .collect();
        let recommended = options.into_iter().flatten().find_map(|option| {
            (option["badge"]["message"] == "Recommended").then(|| string(option, "id").to_owned())
        });
        let default_effort = defaults
            .into_iter()
            .flatten()
            .find(|state| string(state, "id") == id)
            .map(|state| string(&state["thinking"], "effort").to_owned())
            .filter(|value| efforts.contains(value))
            .or(recommended.filter(|value| efforts.contains(value)))
            .unwrap_or_default();
        models.push(ModelOption {
            id: id.into(),
            label: label.into(),
            efforts,
            default_effort,
        });
    }
    if let Ok(mut cached) = cache.lock() {
        cached.insert(path, (size, modified, models.clone()));
    }
    models
}

fn history(record: &Value) -> Settings {
    let agent = string(record, "agent");
    let path = if agent == "kimi" {
        questions::wire_paths(record)
            .ok()
            .and_then(|paths| paths.into_iter().next())
    } else if agent == "codex" && !string(record, "conversation_id").is_empty() {
        let path = PathBuf::from(string(record, "transcript"));
        // A manifest points at the exact conversation, never a recent/global file.
        if path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .contains(string(record, "conversation_id"))
        }) {
            Some(path)
        } else {
            None
        }
    } else if agent == "claude" {
        claude_transcript(record).ok()
    } else {
        None
    };
    let Some(path) = path else {
        return Settings::default();
    };
    let Ok(mut file) = File::open(&path) else {
        return Settings::default();
    };
    let Ok(meta) = file.metadata() else {
        return Settings::default();
    };
    if !meta.is_file() {
        return Settings::default();
    }
    // ls/inspect may include archived bindings to the same conversation. Reuse
    // the bounded metadata scan within this invocation; file changes invalidate it.
    type Cached = (u64, Option<SystemTime>, Settings);
    static CACHE: OnceLock<Mutex<BTreeMap<String, Cached>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let key = format!(
        "{agent}\0{}\0{}",
        string(record, "conversation_id"),
        path.display()
    );
    let modified = meta.modified().ok();
    if let Ok(cached) = cache.lock() {
        if let Some((size, stamp, value)) = cached.get(&key) {
            if *size == meta.len() && *stamp == modified {
                return value.clone();
            }
        }
    }
    // Claude's /model receipts name a model as its picker does.
    let models = if agent == "claude" { catalog(record) } else { Vec::new() };
    // Stopped and archived sessions keep their transcripts: reuse the scan across
    // listings instead of reading up to 8 MiB of each on every poll.
    let scanned = scan_cache::cached("effort-settings", &path, &key, || {
        if agent == "codex" {
            let mut header = Vec::new();
            if BufReader::new((&mut file).take(256 * 1024))
                .read_until(b'\n', &mut header)
                .is_err()
                || !header.ends_with(b"\n")
            {
                return None;
            }
            let Ok(event) = serde_json::from_slice::<Value>(&header) else {
                return None;
            };
            if string(&event, "type") != "session_meta"
                || string(&event["payload"], "id") != string(record, "conversation_id")
            {
                return None;
            }
        }
        let mut window = 64 * 1024u64;
        let found = loop {
            let start = meta.len().saturating_sub(window);
            if file.seek(SeekFrom::Start(start)).is_err() {
                return None;
            }
            let mut bytes = Vec::new();
            if (&mut file).take(window).read_to_end(&mut bytes).is_err() {
                return None;
            }
            let mut offset = start + bytes.len() as u64;
            let mut found = None;
            // Most providers repeat settings per turn; start with 64 KiB and stop
            // at the newest metadata record instead of parsing every event in 8 MiB.
            for line in bytes.split_inclusive(|byte| *byte == b'\n').rev() {
                let end = offset;
                offset -= line.len() as u64;
                if (start > 0 && offset == start) || line.len() > 1024 * 1024 || !line.ends_with(b"\n")
                {
                    continue;
                }
                let markers: &[&[u8]] = match agent {
                    "kimi" => &[b"thinkingEffort"],
                    "claude" => &[b"\"requestedModel\"", b"Set model to `"],
                    _ => &[b"turn_context"],
                };
                if !markers
                    .iter()
                    .any(|marker| line.windows(marker.len()).any(|part| part == *marker))
                {
                    continue;
                }
                let Ok(event) = serde_json::from_slice::<Value>(line) else {
                    continue;
                };
                let found_settings = if agent == "claude" {
                    claude_settings(&event, string(record, "conversation_id"), &models)
                } else {
                    event_settings(agent, &event)
                };
                if let Some((model, effort)) = found_settings {
                    found = Some(Settings {
                        model,
                        effort,
                        path: path.to_string_lossy().into_owned(),
                        offset: end,
                    });
                    break;
                }
            }
            if let Some(value) = found {
                break Some(value);
            }
            if start == 0 || window == MAX_TAIL {
                // Searched to the limit without settings: keep that answer too,
                // it is the most expensive one to repeat.
                break Some(Settings::default());
            }
            window = (window * 4).min(MAX_TAIL);
        };
        found.map(|value| json!({"model": value.model, "effort": value.effort, "path": value.path, "offset": value.offset}))
    });
    let output = scanned
        .map(|value| Settings {
            model: string(&value, "model").into(),
            effort: string(&value, "effort").into(),
            path: string(&value, "path").into(),
            offset: value["offset"].as_u64().unwrap_or(0),
        })
        .unwrap_or_default();
    if let Ok(mut cached) = cache.lock() {
        cached.insert(key, (meta.len(), modified, output.clone()));
    }
    output
}

fn capture(record: &Value) -> Result<String> {
    let pane = string(record, "pane");
    if !pane
        .strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("invalid tracked pane".into());
    }
    // A status summary shares this screen with its terminal choosers.
    let output = kimi_tui_choice::plain(pane)?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

fn footer(agent: &str, model: &str, screen: &str) -> Option<String> {
    if agent == "claude" {
        return claude_footer(screen);
    }
    for line in screen.lines().rev().take(3) {
        let line = line.trim().to_ascii_lowercase();
        if agent == "codex" {
            let prefix = format!("{} ", model.to_ascii_lowercase());
            let Some(tail) = line.strip_prefix(&prefix) else {
                continue;
            };
            let value = tail.split_whitespace().next()?;
            if valid(value) {
                return Some(value.into());
            }
        }
    }
    None
}

const KIMI_MODES: [&str; 4] = ["Ask When Needed", "Never Ask", "Plan", "Always Ask"];

/// The effort Kimi's status line shows for a model displayed as one of `names`.
/// The line joins slots (mode, goal, model, tasks, folder, git) with two spaces.
/// Older releases print it in the last rows, starting with the permission mode;
/// 2.1 prints it on the first row under the boxed composer. The model slot is
/// `<name>`, `<name> thinking` or `<name> thinking: <level>`. A bare name means
/// effort off on the built-in line, but a custom status line command need not
/// show effort, so `custom` never reads a bare name as off.
fn kimi_effort(screen: &str, names: &[&str], custom: bool) -> Option<String> {
    let lines: Vec<_> = screen.lines().collect();
    let below = lines
        .iter()
        .rposition(|line| {
            let line = line.trim();
            line.starts_with('╰') && line.ends_with('╯')
        })
        .and_then(|border| lines[border + 1..].iter().find(|line| !line.trim().is_empty()));
    let legacy = lines.iter().rev().take(3).filter(|line| {
        let first = line.trim().split("  ").next().unwrap_or_default();
        KIMI_MODES.contains(&first)
    });
    for line in below.into_iter().chain(legacy) {
        for slot in line.trim().split("  ").map(str::trim) {
            for name in names.iter().filter(|name| !name.is_empty()) {
                let Some(tail) = slot.strip_prefix(*name) else {
                    continue;
                };
                let value = if tail.is_empty() && !custom {
                    "off".to_owned()
                } else if tail == " thinking" {
                    "on".to_owned()
                } else if let Some(level) = tail.strip_prefix(" thinking: ") {
                    level.split_whitespace().next().unwrap_or_default().to_ascii_lowercase()
                } else {
                    continue;
                };
                if valid(&value) {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Whether tui.toml replaces Kimi's status line with a command's output.
/// An unreadable configuration counts as custom.
fn kimi_custom_status(record: &Value) -> bool {
    let Ok(file) = File::open(agent_home(record).join("tui.toml")) else {
        return false;
    };
    let mut text = String::new();
    if file.take(256 * 1024).read_to_string(&mut text).is_err() {
        return true;
    }
    text.parse::<toml_edit::DocumentMut>().map_or(true, |config| {
        config
            .get("status_line")
            .and_then(|line| line.get("command"))
            .is_some_and(|command| !command.as_str().is_some_and(|c| c.trim().is_empty()))
    })
}

/// A Kimi model entry's value, as overridden by its `overrides` table.
fn kimi_value<'a>(entry: &'a toml_edit::Item, key: &str) -> Option<&'a toml_edit::Item> {
    entry
        .get("overrides")
        .and_then(|overrides| overrides.get(key))
        .or_else(|| entry.get(key))
}

/// The displayed effort of the session's model (an ID or a display name).
/// Kimi's status line names its model, so it must match that catalog entry.
fn visible_effort(record: &Value, model: &str, screen: &str) -> Option<String> {
    if string(record, "agent") != "kimi" {
        return footer(string(record, "agent"), model, screen);
    }
    let option = catalog(record)
        .into_iter()
        .find(|option| option.id == model || option.label == model)?;
    target_footer(record, &option, screen)
}

#[cfg(test)]
thread_local! { static MODEL_CACHE_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// Codex's models_cache.json (hundreds of KiB) for this record's account. Every
/// listed Codex session reads its options and catalog from it, so one listing
/// parsed it twice per session. Reuse the parse while the file is unchanged.
fn codex_models(record: &Value) -> Option<std::sync::Arc<Value>> {
    type Cached = (u64, Option<SystemTime>, std::sync::Arc<Value>);
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Cached>>> = OnceLock::new();
    let path = agent_home(record).join("models_cache.json");
    let file = File::open(&path).ok()?;
    let meta = file.metadata().ok()?;
    let modified = meta.modified().ok();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some((size, stamp, value)) = cache.lock().ok()?.get(&path) {
        if *size == meta.len() && *stamp == modified {
            return Some(value.clone());
        }
    }
    #[cfg(test)]
    MODEL_CACHE_PARSES.with(|count| count.set(count.get() + 1));
    // serde_json's reader requests individual bytes. Buffer the bounded
    // file so every session does not issue hundreds of thousands of reads.
    let value = std::sync::Arc::new(
        serde_json::from_reader::<_, Value>(BufReader::new(file.take(MAX_TAIL))).ok()?,
    );
    if let Ok(mut cached) = cache.lock() {
        cached.insert(path, (meta.len(), modified, value.clone()));
    }
    Some(value)
}

fn options(record: &Value, model: &str) -> Vec<String> {
    match string(record, "agent") {
        "codex" => {
            let Some(cache) = codex_models(record) else {
                return vec![];
            };
            cache["models"]
                .as_array()
                .and_then(|models| {
                    models
                        .iter()
                        .find(|entry| string(entry, "slug").eq_ignore_ascii_case(model))
                })
                .and_then(|entry| entry["supported_reasoning_levels"].as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value["effort"].as_str())
                        .filter(|value| valid(value))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        }
        "kimi" => {
            let Ok(file) = File::open(agent_home(record).join("config.toml")) else {
                return vec![];
            };
            let mut text = String::new();
            if file.take(1024 * 1024).read_to_string(&mut text).is_err() {
                return vec![];
            }
            let Ok(config) = text.parse::<toml_edit::DocumentMut>() else {
                return vec![];
            };
            let Some(entry) = config.get("models").and_then(|models| models.get(model)) else {
                return vec![];
            };
            let mut levels: Vec<String> = kimi_value(entry, "support_efforts")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str())
                        .filter(|v| valid(v))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let always = kimi_value(entry, "capabilities")
                .and_then(|v| v.as_array())
                .is_some_and(|v| v.iter().any(|v| v.as_str() == Some("always_thinking")));
            if levels.is_empty() {
                let thinking = always
                    || kimi_value(entry, "capabilities")
                        .and_then(|v| v.as_array())
                        .is_some_and(|v| v.iter().any(|v| v.as_str() == Some("thinking")))
                    || kimi_value(entry, "adaptive_thinking").and_then(|v| v.as_bool()) == Some(true);
                if thinking {
                    levels.push("on".into());
                }
                if !always {
                    levels.push("off".into());
                }
            } else if !always {
                levels.insert(0, "off".into());
            }
            levels
        }
        "claude" => claude_catalog(record)
            .into_iter()
            .find(|option| option.id == model)
            .map(|option| option.efforts)
            .unwrap_or_default(),
        _ => vec![],
    }
}

#[derive(Clone, Debug)]
struct ModelOption {
    id: String,
    label: String,
    efforts: Vec<String>,
    default_effort: String,
}

fn safe_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && !value.chars().any(char::is_control)
        && !value.starts_with('-')
}

fn catalog(record: &Value) -> Vec<ModelOption> {
    if string(record, "agent") == "claude" {
        return claude_catalog(record);
    }
    let mut result = Vec::new();
    if string(record, "agent") == "codex" {
        let parsed = codex_models(record);
        if let Some(models) = parsed.as_ref().and_then(|v| v["models"].as_array()) {
            let mut entries: Vec<_> = models.iter().take(256).collect();
            // Codex's ModelsManager stably sorts by its signed i32 priority.
            // Incomplete legacy entries follow ranked models in source order.
            entries.sort_by_key(|entry| {
                let priority = entry["priority"].as_i64().and_then(|value| i32::try_from(value).ok());
                (priority.is_none(), priority)
            });
            for entry in entries {
                let id = string(entry, "slug");
                if !safe_model(id)
                    || (!string(entry, "visibility").is_empty()
                        && string(entry, "visibility") != "list")
                {
                    continue;
                }
                let label = if safe_model(string(entry, "display_name")) {
                    string(entry, "display_name")
                } else {
                    id
                };
                let mut efforts: Vec<String> = entry["supported_reasoning_levels"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(|v| v["effort"].as_str())
                            .filter(|v| valid(v))
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                efforts.sort_by_key(|v| match v.as_str() {
                    "max" => 1,
                    "ultra" => 2,
                    _ => 0,
                });
                if id.starts_with("codex-auto-") {
                    efforts.retain(|e| e != "ultra");
                }
                result.push(ModelOption {
                    id: id.into(),
                    label: label.into(),
                    efforts,
                    default_effort: string(entry, "default_reasoning_level").into(),
                });
            }
        }
    } else if string(record, "agent") == "kimi" {
        let mut text = String::new();
        if File::open(agent_home(record).join("config.toml"))
            .and_then(|f| f.take(1024 * 1024).read_to_string(&mut text))
            .is_ok()
        {
            if let Ok(config) = text.parse::<toml_edit::DocumentMut>() {
                if let Some(models) = config.get("models").and_then(|v| v.as_table_like()) {
                    for (id, entry) in models.iter().take(256) {
                        if !safe_model(id) {
                            continue;
                        }
                        // Kimi shows the overridden alias on its status line.
                        let label = kimi_value(entry, "display_name")
                            .or_else(|| kimi_value(entry, "model"))
                            .and_then(|v| v.as_str())
                            .filter(|v| safe_model(v))
                            .unwrap_or(id);
                        let efforts = options(record, id);
                        let default_effort = kimi_value(entry, "default_effort")
                            .and_then(|v| v.as_str())
                            .filter(|v| efforts.iter().any(|e| e == v))
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                efforts.get(efforts.len() / 2).cloned().unwrap_or_default()
                            });
                        result.push(ModelOption {
                            id: id.into(),
                            label: label.into(),
                            efforts,
                            default_effort,
                        });
                    }
                }
            }
        }
    }
    result
}

pub(super) fn account_models(provider: &str, home: &Path) -> Value {
    let models = catalog(&json!({"agent":provider,"agent_home":home}));
    json!({"models":models.iter().take(64).map(|m| json!({"id":m.id,"label":m.label,
        "effort_options":m.efforts,"default_effort":m.default_effort})).collect::<Vec<_>>(),
        "truncated":models.len()>64})
}

fn settings_reason(record: &Value, models: &[ModelOption]) -> &'static str {
    if record.get("archive_id").is_some() || string(record, "state") == "archived" {
        return "Restore the archived session before changing its settings";
    }
    if !["codex", "kimi", "claude"].contains(&string(record, "agent")) {
        return "Model settings are available in Terminal for this provider.";
    }
    if models.is_empty() {
        return "No model catalog is available for this account; open the agent once to populate it";
    }
    // Claude confirms each session-only change in its own transcript.
    if string(record, "agent") == "claude" && claude_transcript(record).is_err() {
        return "Waiting for the agent to confirm this conversation";
    }
    if string(record, "run_id").is_empty()
        || (string(record, "conversation_id").is_empty() && !input::first_message_candidate(record))
        || !string(record, "expected_id").is_empty()
    {
        return "Waiting for the agent to confirm this conversation";
    }
    if string(record, "conversation_id").is_empty()
        && {
            let ordinary = || input::checked_settings(string(record,"name"),string(record,"run_id"),"")
                .and_then(|r| input::checked_terminal(&r,true));
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let check = picker_scope::send_settings_ready(record).unwrap_or_else(ordinary);
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let check = ordinary();
            check.is_err()
        }
    {
        return "Wait for the agent's empty prompt before changing model settings";
    }
    if !string(record, "error").is_empty() || pause_active(record) {
        return "Wait for the session transition to finish";
    }
    ""
}

/// Settings apply now while the session is Ready as the listing reports it, or
/// while a new session waits at its first prompt; otherwise they wait for Ready.
fn ready(record: &Value) -> bool {
    telemetry::settled(record) || input::first_message_ready(record)
}

/// Claude's PostModelSwitch: the exact model the session runs from now on,
/// after /model in Terminal or from Zerus, an automatic switch or a resume. It
/// stands until the transcript records a later turn or /model receipt; the
/// transcript's length now marks the history it replaces.
pub(super) fn observe_switch(record: &mut Value, event: &Value) {
    let model = string(event, "to_model");
    if string(record, "agent") != "claude" || !safe_model(model) {
        return;
    }
    record["model"] = json!(model);
    match claude_transcript(record) {
        Ok(path) => {
            // Claude writes the transcript with its first message.
            record["model_source_offset"] = json!(fs::metadata(&path).map_or(0, |meta| meta.len()));
            record["model_source_path"] = json!(path.to_string_lossy());
        }
        Err(_) => {
            let fields = record.as_object_mut().unwrap();
            fields.remove("model_source_path");
            fields.remove("model_source_offset");
        }
    }
}

fn settings(record: &Value, live_pane: bool) -> Settings {
    let mut value = history(record);
    let confirmed = string(record, "effort_source_path") == value.path
        && record["effort_source_offset"]
            .as_u64()
            .is_some_and(|offset| offset >= value.offset);
    let switched = string(record, "model_source_path") == value.path
        && record["model_source_offset"]
            .as_u64()
            .is_some_and(|offset| offset >= value.offset);
    if value.model.is_empty() || confirmed || switched {
        value.model = string(record, "model").into();
    }
    if value.effort.is_empty() || confirmed {
        value.effort = string(record, "effort").into();
    }
    if live_pane && process_alive(record) {
        if let Ok(screen) = capture(record) {
            {
                // The native status line also reflects changes made in Terminal
                // before the next turn writes fresh history. The account catalog
                // resolves its exact displayed name, never supplies a default.
                let observed: Vec<_> = catalog(record)
                    .into_iter()
                    .filter_map(|model| {
                        target_footer(record, &model, &screen).map(|effort| (model.id, effort))
                    })
                    .collect();
                if let [(model, effort)] = observed.as_slice() {
                    value.model = model.clone();
                    value.effort = effort.clone();
                }
            }
            // Kimi's status line names its model; the catalog match above covers it.
            if string(record, "agent") != "kimi" {
                if let Some(effort) = footer(string(record, "agent"), &value.model, &screen) {
                    value.effort = effort;
                }
            }
        }
    }
    value
}

/// Claude asks before a change that discards its prompt cache. Zerus states the
/// cost before Apply and then confirms only that built-in notice.
const CLAUDE_NOTICE: &str = "In a conversation that already has replies, Claude re-reads the full history on the next message after a model or effort change. That response is slower and uses more tokens.";

/// Why `hgs effort` is unavailable, or "". It dispatches through
/// `input::checked_settings`, so it waits for Ready as the listing reports it,
/// including Kimi profile groups that outlast the main turn's Stop.
fn effort_reason(record: &Value, known: bool, running: bool) -> &'static str {
    if !["codex", "kimi", "claude"].contains(&string(record, "agent")) {
        "Use the provider's Terminal to change effort"
    } else if !known {
        "Waiting for the provider's model and effort settings"
    } else if !running {
        "Resume the session to change effort"
    } else if !telemetry::settled(record) {
        "Wait for Ready to change effort"
    } else {
        ""
    }
}

pub(super) fn summary(record: &Value, live_pane: bool) -> Value {
    let value = settings(record, live_pane);
    let mut choices = options(record, &value.model);
    // Codex deliberately requires its explicit advanced picker for Ultra.
    if string(record, "agent") == "codex" {
        choices.retain(|value| value != "ultra");
    }
    let reason = effort_reason(
        record,
        !value.effort.is_empty() && !choices.is_empty(),
        live_pane && process_alive(record),
    );
    let models = catalog(record);
    let settings_reason = settings_reason(record, &models);
    let when = if !live_pane || !process_alive(record) {
        "resume"
    } else if ready(record) {
        "now"
    } else {
        "ready"
    };
    let pending = &record["pending_settings"];
    let notice = if string(record, "agent") == "claude" { CLAUDE_NOTICE } else { "" };
    json!({"model":value.model,"effort":value.effort,"effort_options":choices,
        "model_options":models.iter().map(|m| json!({"id":m.id,"label":m.label,"effort_options":m.efforts})).collect::<Vec<_>>(),
        "settings_change_supported":settings_reason.is_empty(),"settings_change_reason":settings_reason,
        "settings_change_notice":notice,"settings_scope":"session","settings_apply_when":when,
        "pending_model":string(pending,"model"),"pending_effort":string(pending,"effort"),
        "pending_settings_id":string(pending,"request_id"),
        "effort_change_supported":reason.is_empty(),"effort_change_reason":reason,
        "effort_scope":"session"})
}

/// A live session's summary while a chooser that only the terminal shows
/// waits for an answer. Its saved record stays idle, but `hgs effort` and an
/// immediate settings change refuse the chooser (`input::checked_settings`):
/// effort changes wait for it, and settings changes are scheduled for Ready,
/// as `dispatch_settings` schedules them. Earlier reasons keep precedence.
pub(super) fn wait_for_chooser(output: &mut Value, reason: &str) {
    if output["settings_apply_when"] == "now" {
        output["settings_apply_when"] = json!("ready");
    }
    if output["effort_change_supported"] == true {
        output["effort_change_supported"] = json!(false);
        output["effort_change_reason"] = json!(reason);
    }
}

/// Whether a settings change applies now rather than waiting for Ready. A
/// chooser that only the terminal shows leaves the saved record Ready, but
/// applying refuses it and no key may reach it, so the change is scheduled.
fn applies_now(record: &Value, running: bool) -> bool {
    running && ready(record) && journal::terminal_chooser(record).is_none()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request_id: String,
    expected_run_id: String,
    expected_conversation_id: String,
    effort: String,
}

thread_local! {
    /// Whether this settings attempt sent keys, and whether the agent has yet to
    /// visibly react to the last of them; a reaction proves it read the earlier ones.
    static ATTEMPT_KEYS: std::cell::Cell<(bool, bool)> = const { std::cell::Cell::new((false, false)) };
}

fn start_attempt() {
    ATTEMPT_KEYS.with(|keys| keys.set((false, false)));
}

fn sent_keys() {
    ATTEMPT_KEYS.with(|keys| keys.set((true, true)));
}

fn reacted() {
    ATTEMPT_KEYS.with(|keys| keys.set((keys.get().0, false)));
}

/// After a failed attempt nothing is unconfirmed when it sent no keys, or when
/// the agent reacted to the last of them and is back at its empty prompt.
/// Otherwise input stays blocked until a native event, as for sent text.
fn mark_unconfirmed(record: &mut Value) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if picker_entered() {
        let _ = picker_refuse::<()>("Legacy failure writes are forbidden in the original picker keeper".into());
        return;
    }
    let (sent, unread) = ATTEMPT_KEYS.with(|keys| keys.replace((false, false)));
    if !sent || (!unread && input::checked_terminal(record, true).is_ok()) {
        return;
    }
    record["settings_unconfirmed_at"] = json!(now());
    let _ = write(record);
}

fn checked(name: &str, request: &Request, empty: bool) -> Result<Value> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if picker_entered() { return picker_scope::native_checked(name, request, empty); }
    let record = input::checked_settings(
        name,
        &request.expected_run_id,
        &request.expected_conversation_id,
    )?;
    input::checked_terminal(&record, empty)?;
    Ok(record)
}

fn send_key(record: &Value, key: &str) -> Result<()> {
    sent_keys();
    backend::current().send(string(record, "pane"), &[backend::Key::Named(key)])
}

/// The cursor as reported, in cells.
fn cursor(record: &Value) -> Result<backend::PaneInfo> {
    backend::current().pane_info(
        backend::Target::Pane(string(record, "pane")),
        &[backend::PaneField::CursorX, backend::PaneField::CursorY],
    )
}

fn wait_until<T>(read: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    wait_until_for(Duration::from_secs(2), read)
}

fn wait_until_for<T>(timeout: Duration, mut read: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = Instant::now() + timeout;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let deadline = picker_scope::deadline(deadline)?;
    loop {
        if let Some(value) = read()? {
            reacted();
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err("provider did not confirm the expected change; inspect Terminal".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn codex_change(
    name: &str,
    request: &Request,
    current: &Settings,
    levels: &[String],
) -> Result<()> {
    let mut index = levels
        .iter()
        .position(|value| value == &current.effort)
        .ok_or("current effort is not in the provider model catalog; open Terminal")?;
    let target = levels
        .iter()
        .position(|value| value == &request.effort)
        .ok_or("unsupported effort for this model")?;
    while index != target {
        let record = checked(name, request, true)?;
        if footer("codex", &current.model, &capture(&record)?).as_deref()
            != Some(levels[index].as_str())
        {
            return Err("the provider effort changed; refresh before trying again".into());
        }
        let next = if target > index { index + 1 } else { index - 1 };
        send_key(&record, if target > index { "M-." } else { "M-," })?;
        // Released Codex hides its model/effort footer for a 2.5-second Max or
        // Ultra status-line animation. Wait for the exact footer; never repeat
        // the step or treat the animation label as confirmation.
        wait_until_for(Duration::from_secs(5), || {
            let record = checked(name, request, false)?;
            Ok(
                (footer("codex", &current.model, &capture(&record)?).as_deref()
                    == Some(levels[next].as_str()))
                .then_some(()),
            )
        })?;
        input::checked_terminal(&record, true)?;
        index = next;
    }
    Ok(())
}

fn kimi_picker(screen: &str) -> Option<(Vec<String>, usize)> {
    let lines: Vec<_> = screen.lines().collect();
    let title = lines
        .iter()
        .position(|line| line.trim() == "Select thinking effort")?;
    if !lines
        .get(title + 1)?
        .contains("←→ switch · Enter select · Alt+S session-only · Esc cancel")
    {
        return None;
    }
    for line in lines.iter().skip(title + 2) {
        if !line.contains("[ ") || !line.contains(" ]") {
            continue;
        }
        let mut selected = None;
        let mut levels = Vec::new();
        let mut mark = false;
        for word in line.split_whitespace() {
            if word == "[" {
                mark = true;
                continue;
            }
            if word == "]" {
                mark = false;
                continue;
            }
            let word = word.to_ascii_lowercase();
            if !valid(&word) {
                return None;
            }
            if mark {
                if selected.is_some() {
                    return None;
                }
                selected = Some(levels.len());
            }
            if levels.contains(&word) {
                return None;
            }
            levels.push(word);
        }
        return Some((levels, selected?));
    }
    None
}

fn kimi_command_ready(record: &Value) -> Result<bool> {
    let cursor = cursor(record)?;
    let Some((x, y)) = cursor.pair() else {
        return Ok(false);
    };
    let screen = capture(record)?;
    let Some(row) = y.parse::<usize>().ok().and_then(|y| screen.lines().nth(y)) else {
        return Ok(false);
    };
    Ok(x == "12"
        && row.starts_with(" │ > /effort")
        && row.chars().skip(12).collect::<String>().trim() == "│")
}

fn kimi_change(name: &str, request: &Request, current: &Settings) -> Result<()> {
    let record = checked(name, request, true)?;
    if visible_effort(&record, &current.model, &capture(&record)?).as_deref()
        != Some(current.effort.as_str())
    {
        return Err("the current Kimi effort is not visible; open Terminal".into());
    }
    sent_keys();
    backend::current().send(string(&record, "pane"), &[backend::Key::Literal("/effort")])?;
    wait_until(|| {
        let record = checked(name, request, false)?;
        Ok(kimi_command_ready(&record)?.then_some(()))
    })?;
    let record = checked(name, request, false)?;
    if !kimi_command_ready(&record)? {
        return Err("terminal changed before opening effort picker".into());
    }
    send_key(&record, "Enter")?;
    let (levels, mut index) = wait_until(|| {
        let record = checked(name, request, false)?;
        Ok(kimi_picker(&capture(&record)?))
    })?;
    let target = levels
        .iter()
        .position(|value| value == &request.effort)
        .ok_or("effort is not offered by the native picker; inspect Terminal")?;
    while index != target {
        let record = checked(name, request, false)?;
        if kimi_picker(&capture(&record)?) != Some((levels.clone(), index)) {
            return Err("effort picker changed; inspect Terminal".into());
        }
        let next = if target > index { index + 1 } else { index - 1 };
        send_key(&record, if target > index { "Right" } else { "Left" })?;
        wait_until(|| {
            let record = checked(name, request, false)?;
            Ok((kimi_picker(&capture(&record)?) == Some((levels.clone(), next))).then_some(()))
        })?;
        index = next;
    }
    let record = checked(name, request, false)?;
    if kimi_picker(&capture(&record)?) != Some((levels, target)) {
        return Err("effort picker changed before applying; inspect Terminal".into());
    }
    send_key(&record, "M-s")?; // Native session-only selection; Enter changes shared defaults.
    wait_until(|| {
        let record = checked(name, request, false)?;
        let screen = capture(&record)?;
        Ok((kimi_picker(&screen).is_none()
            && visible_effort(&record, &current.model, &screen).as_deref()
                == Some(request.effort.as_str()))
        .then_some(()))
    })?;
    input::checked_terminal(&record, true)
}

// Only interact with provider-owned settings dialogs that advertise their
// session-only action. Enter on the final selection would change shared defaults.
fn command_ready(record: &Value, command: &str) -> Result<bool> {
    let cursor = cursor(record)?;
    let Some((x, y)) = cursor.pair() else {
        return Ok(false);
    };
    let screen = capture(record)?;
    let Some(row) = y.parse::<usize>().ok().and_then(|y| screen.lines().nth(y)) else {
        return Ok(false);
    };
    let Some(x) = x.parse::<usize>().ok() else {
        return Ok(false);
    };
    let before = row.chars().take(x).collect::<String>();
    let after = row.chars().skip(x).collect::<String>();
    Ok(match string(record, "agent") {
        "kimi" => before.trim() == format!("│ > {command}") && after.trim() == "│",
        // Claude follows its prompt glyph with a non-breaking space. Enter runs
        // the highlighted completion, so it must be this exact command.
        "claude" => {
            before.replace('\u{a0}', " ").trim() == format!("❯ {command}")
                && after.trim().is_empty()
                && screen.lines().all(|line| {
                    let Some(completion) = line.strip_prefix("  ❯ /") else {
                        return true;
                    };
                    completion.split_whitespace().next() == command.strip_prefix('/')
                })
        }
        "codex" => input::codex_prompt(before.trim()).is_some_and(|text| text.trim() == command)
            && after.trim().is_empty(),
        _ => false,
    })
}

fn open_model(name: &str, request: &Request) -> Result<()> {
    open_model_tracked(name, request, None)
}

fn open_model_tracked(name: &str, request: &Request, opened: Option<&std::cell::Cell<bool>>) -> Result<()> {
    let record = checked(name, request, true)?;
    sent_keys();
    backend::current().send(string(&record, "pane"), &[backend::Key::Literal("/model")])?;
    wait_until(|| {
        let r = checked(name, request, false)?;
        Ok(command_ready(&r, "/model")?.then_some(()))
    })?;
    let record = checked(name, request, false)?;
    if !command_ready(&record, "/model")? {
        return Err("terminal changed before opening model settings".into());
    }
    // This is only a cleanup correlation, minted before our possibly effectful
    // opening key. A pre-existing/replacement picker never supplies it.
    if let Some(opened) = opened { opened.set(true); }
    send_key(&record, "Enter")
}

fn selected_codex(screen: &str) -> Option<String> {
    let mut selected = screen.lines().filter_map(|line| {
        let line = line.trim().strip_prefix('›')?.trim();
        let (number, rest) = line.split_once(". ")?;
        number.parse::<usize>().ok()?;
        Some(rest.to_owned())
    });
    let row = selected.next()?;
    selected.next().is_none().then_some(row)
}

fn row_is(row: &str, label: &str) -> bool {
    row.strip_prefix(label).is_some_and(|tail| {
        tail.is_empty()
            || tail.starts_with("  ")
            || tail.starts_with(" (current)")
            || tail.starts_with(" (default)")
    })
}

fn codex_select_row(name: &str, request: &Request, title: &str, label: &str) -> Result<String> {
    let mut visited = Vec::new();
    for _ in 0..256 {
        let record = checked(name, request, false)?;
        let screen = capture(&record)?;
        if !screen.lines().any(|l| l.trim() == title) {
            return Err("model settings dialog changed; inspect Terminal".into());
        }
        let selected =
            selected_codex(&screen).ok_or("model selection is not visible; enlarge Terminal")?;
        if row_is(&selected, label) {
            return Ok(screen);
        }
        if visited.contains(&selected) {
            return Err("requested choice is unavailable in the native model picker".into());
        }
        visited.push(selected.clone());
        send_key(&record, "Down")?;
        wait_until(|| {
            let r = checked(name, request, false)?;
            let s = capture(&r)?;
            Ok((selected_codex(&s).as_deref() != Some(selected.as_str())).then_some(()))
        })?;
    }
    Err("model picker exceeded the supported catalog size".into())
}

fn effort_label(value: &str) -> &str {
    match value {
        "xhigh" => "Extra high",
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "max" => "Max",
        _ => value,
    }
}

fn target_footer(record: &Value, model: &ModelOption, screen: &str) -> Option<String> {
    // Claude's indicator shows effort only; it cannot identify the model.
    if string(record, "agent") == "claude" {
        return None;
    }
    if string(record, "agent") == "codex" && model.efforts.is_empty() {
        return screen
            .lines()
            .rev()
            .take(3)
            .any(|line| {
                let line = line.trim().to_ascii_lowercase();
                [&model.id, &model.label]
                    .iter()
                    .any(|id| row_is(&line, &id.to_ascii_lowercase()))
            })
            .then(String::new);
    }
    if string(record, "agent") == "codex" {
        footer("codex", &model.id, screen).or_else(|| footer("codex", &model.label, screen))
    } else {
        kimi_effort(screen, &[&model.label, &model.id], kimi_custom_status(record))
    }
}

fn codex_model(name: &str, request: &Request, model: &ModelOption) -> Result<()> {
    open_model(name, request)?;
    let screen = wait_until(|| {
        let r = checked(name, request, false)?;
        let s = capture(&r)?;
        Ok(s.lines()
            .any(|l| ["Select Model", "Select Model and Effort"].contains(&l.trim()))
            .then_some(s))
    })?;
    if screen.lines().any(|l| l.trim() == "Select Model") {
        if model.id.starts_with("codex-auto-") {
            let s = codex_select_row(name, request, "Select Model", &model.label)?;
            if !s.contains("s session") {
                return Err(
                    "model picker does not offer a session-only action; use Terminal".into(),
                );
            }
            send_key(&checked(name, request, false)?, "s")?;
            let initial = wait_until(|| {
                let r = checked(name, request, false)?;
                Ok(target_footer(&r, model, &capture(&r)?))
            })?;
            let current = Settings {
                model: model.label.clone(),
                effort: initial,
                ..Default::default()
            };
            return codex_change(name, request, &current, &model.efforts);
        }
        codex_select_row(name, request, "Select Model", "All models")?;
        send_key(&checked(name, request, false)?, "Enter")?;
        wait_until(|| {
            let r = checked(name, request, false)?;
            Ok(capture(&r)?
                .lines()
                .any(|l| l.trim() == "Select Model and Effort")
                .then_some(()))
        })?;
    }
    let screen = codex_select_row(name, request, "Select Model and Effort", &model.label)?;
    if model.efforts.len() <= 1 && screen.contains("s session") {
        send_key(&checked(name, request, false)?, "s")?;
    } else {
        // This Enter opens the reasoning subdialog, it does not apply defaults.
        if model.efforts.len() <= 1 {
            return Err("model picker cannot safely apply this model session-only".into());
        }
        send_key(&checked(name, request, false)?, "Enter")?;
        let title = format!("Select Reasoning Level for {}", model.label);
        wait_until(|| {
            let r = checked(name, request, false)?;
            Ok(capture(&r)?
                .lines()
                .any(|l| l.trim() == title)
                .then_some(()))
        })?;
        let final_screen = if ["max", "ultra"].contains(&request.effort.as_str()) {
            codex_select_row(name, request, &title, "More reasoning…")?;
            send_key(&checked(name, request, false)?, "Enter")?;
            wait_until(|| {
                let r = checked(name, request, false)?;
                Ok(capture(&r)?
                    .lines()
                    .any(|l| l.trim() == "Advanced Reasoning")
                    .then_some(()))
            })?;
            codex_select_row(
                name,
                request,
                "Advanced Reasoning",
                if request.effort == "ultra" {
                    "Ultra"
                } else {
                    "Max"
                },
            )?
        } else {
            codex_select_row(name, request, &title, effort_label(&request.effort))?
        };
        if !final_screen.contains("s session") {
            return Err(
                "reasoning picker does not offer a session-only action; use Terminal".into(),
            );
        }
        send_key(&checked(name, request, false)?, "s")?;
    }
    wait_until_for(Duration::from_secs(5), || {
        let r = checked(name, request, false)?;
        Ok(
            (target_footer(&r, model, &capture(&r)?).as_deref() == Some(request.effort.as_str()))
                .then_some(()),
        )
    })?;
    input::checked_terminal(&checked(name, request, false)?, true)
}

fn kimi_model_row(screen: &str) -> Option<String> {
    if !screen
        .lines()
        .any(|l| l.trim().starts_with("Select a model"))
        || !screen.contains("Alt+S session-only")
    {
        return None;
    }
    let mut rows = screen
        .lines()
        .filter_map(|l| l.trim().strip_prefix("❯ ").map(str::to_owned));
    let row = rows.next()?;
    rows.next().is_none().then_some(row)
}

/// Whether the selected picker row is visibly the first and the last model row.
/// Model names on unselected rows start where the selected row's name starts.
fn kimi_model_edges(screen: &str) -> (bool, bool) {
    let lines: Vec<_> = screen.lines().collect();
    let Some(selected) = lines.iter().position(|l| l.trim_start().starts_with("❯ ")) else {
        return (false, false);
    };
    let column = lines[selected].chars().position(|c| c == '❯').unwrap_or(0) + 2;
    let row = |index: Option<usize>| {
        index.and_then(|i| lines.get(i)).is_some_and(|line| {
            let indent = line.chars().take_while(|c| *c == ' ').count();
            indent == column && line.chars().count() > column
        })
    };
    (!row(selected.checked_sub(1)), !row(Some(selected + 1)))
}

fn kimi_thinking(screen: &str) -> Option<String> {
    let lines: Vec<_> = screen.lines().collect();
    // The picker is below the conversation, which can quote "Thinking set to …".
    let title = lines
        .iter()
        .rposition(|l| l.trim().starts_with("Thinking"))?;
    let row = lines.get(title + 1)?;
    let value = row
        .split_once("[ ")?
        .1
        .split_once(" ]")?
        .0
        .to_ascii_lowercase();
    valid(&value).then_some(value)
}

fn kimi_model(name: &str, request: &Request, model: &ModelOption) -> Result<()> {
    open_model(name, request)?;
    wait_until(|| {
        let r = checked(name, request, false)?;
        Ok(kimi_model_row(&capture(&r)?))
    })?;
    let mut visited = Vec::new();
    let mut tabs = Vec::new();
    // Older pickers wrap around; Kimi 2.1 stops at either end, so a key that
    // does not move the selection turns the walk around once per provider.
    let mut down = true;
    let mut turned = false;
    for _ in 0..256 {
        let record = checked(name, request, false)?;
        let screen = capture(&record)?;
        let row = kimi_model_row(&screen).ok_or("model picker changed; inspect Terminal")?;
        if row_is(&row, &model.label) {
            break;
        }
        if visited.contains(&row) {
            if !screen.contains("Tab toggle provider") || tabs.contains(&row) {
                return Err("model is not available in the native picker".into());
            }
            tabs.push(row.clone());
            visited.clear();
            (down, turned) = (true, false);
            send_key(&record, "Tab")?;
            wait_until(|| {
                let r = checked(name, request, false)?;
                Ok((kimi_model_row(&capture(&r)?).as_deref() != Some(row.as_str())).then_some(()))
            })?;
            continue;
        }
        send_key(&record, if down { "Down" } else { "Up" })?;
        let deadline = Instant::now() + Duration::from_secs(2);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let deadline = picker_scope::deadline(deadline)?;
        let moved = loop {
            let r = checked(name, request, false)?;
            if kimi_model_row(&capture(&r)?).as_deref() != Some(row.as_str()) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if moved {
            visited.push(row);
            continue;
        }
        // Turn only at a visible end of the list. A key that does not move the
        // selection elsewhere may still be pending, so stop instead of guessing.
        let (top, bottom) = kimi_model_edges(&capture(&checked(name, request, false)?)?);
        if down && !turned && bottom {
            // Walk up; rows passed on the way down are not a loop.
            (down, turned) = (false, true);
            visited.clear();
        } else if !down && top {
            visited.push(row);
        } else {
            return Err("model picker selection did not move; inspect Terminal".into());
        }
    }
    for _ in 0..16 {
        let record = checked(name, request, false)?;
        let screen = capture(&record)?;
        if !kimi_model_row(&screen).is_some_and(|r| row_is(&r, &model.label)) {
            return Err("requested model is not selected".into());
        }
        let effort =
            kimi_thinking(&screen).ok_or("native model thinking selection is not visible")?;
        if effort == request.effort {
            send_key(&record, "M-s")?;
            wait_until(|| {
                let r = checked(name, request, false)?;
                Ok((target_footer(&r, model, &capture(&r)?).as_deref()
                    == Some(request.effort.as_str()))
                .then_some(()))
            })?;
            return input::checked_terminal(&checked(name, request, false)?, true);
        }
        let current = model
            .efforts
            .iter()
            .position(|v| v == &effort)
            .ok_or("unsupported native thinking selection")?;
        let target = model
            .efforts
            .iter()
            .position(|v| v == &request.effort)
            .ok_or("unsupported effort")?;
        send_key(&record, if target > current { "Right" } else { "Left" })?;
        wait_until(|| {
            let r = checked(name, request, false)?;
            Ok((kimi_thinking(&capture(&r)?).as_deref() != Some(effort.as_str())).then_some(()))
        })?;
    }
    Err("native model effort did not settle".into())
}

#[derive(Debug, PartialEq)]
struct ClaudePicker {
    /// A selected model row or a section with its advertised expansion action.
    row: String,
    section: Option<ClaudeSection>,
    /// The shown effort; empty when the model has none, None when not visible.
    effort: Option<String>,
    /// Enter saves a new default; only `s` keeps the choice in this session.
    session_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ClaudeSection {
    Collapsed,
    Expanded,
}

/// Recognize the native panel independently of its selected row. In particular,
/// a collapsed section has no numbered selection and offers no model apply key.
fn claude_model_panel(screen: &str) -> Option<Vec<&str>> {
    let lines: Vec<_> = screen.lines().collect();
    let title = lines.iter().rposition(|line| line.trim() == "Select model")?;
    let below = &lines[title + 1..];
    if !below.iter().find(|line| !line.trim().is_empty())?
        .trim_start().starts_with("Switch between Claude models.")
        || !below.iter().rfind(|line| !line.trim().is_empty())?.contains("Esc to cancel")
    {
        return None;
    }
    Some(below.to_vec())
}

/// Claude's /model picker: numbered rows (the label column, then the model's
/// name and description), the effort adjusted with ←/→, and the key legend.
fn claude_picker(screen: &str) -> Option<ClaudePicker> {
    let below = claude_model_panel(screen)?;
    let mut rows = below.iter().filter_map(|line| {
        let row = line.trim_start().strip_prefix("❯ ")?.trim_end();
        let (number, _) = row.split_once(". ")?;
        number.parse::<usize>().ok()?;
        Some(row.to_owned())
    });
    let selected = rows.next();
    if rows.next().is_some() {
        return None;
    }
    let section = if below.iter().any(|line| line.contains("Enter to expand")) {
        Some(ClaudeSection::Collapsed)
    } else if below.iter().any(|line| line.contains("Enter to collapse")) {
        Some(ClaudeSection::Expanded)
    } else {
        None
    };
    let row = if let Some(section) = section {
        // Only an explicit expansion/collapse legend permits Enter. A model
        // row with Enter's save-as-default action never takes this branch.
        if selected.is_some() {
            return None;
        }
        let marker = match section {
            ClaudeSection::Collapsed => '▸',
            ClaudeSection::Expanded => '▾',
        };
        let mut sections = below.iter().filter_map(|line| {
            let row = line.trim_start().strip_prefix("❯ ")?;
            row.starts_with(marker).then(|| row.trim_end().to_owned())
        });
        let row = sections.next()?;
        if sections.next().is_some() {
            return None;
        }
        row
    } else {
        selected?
    };
    let effort = below.iter().find_map(|line| {
        if line.contains("Effort not supported") {
            return Some(String::new());
        }
        let (shown, legend) = line.trim().split_once(" effort")?;
        let level = shown.split_whitespace().last()?.to_ascii_lowercase();
        (legend.contains("←/→ to adjust") && valid(&level)).then_some(level)
    });
    let session_only = below.iter().any(|line| {
        line.contains("Enter to set as default") && line.contains("s to use this session only")
    });
    Some(ClaudePicker {
        row,
        section,
        effort,
        session_only,
    })
}

/// Whether a picker row shows this model: its description starts with the name.
fn claude_row_is(row: &str, name: &str) -> bool {
    let Some((_, rest)) = row.split_once(". ") else {
        return false;
    };
    // The label column ("Opus", "Haiku ✔") ends at the first double space.
    let Some((label, description)) = rest.split_once("  ") else {
        return false;
    };
    if label.trim_end().trim_end_matches('✔').trim_end() == "Default (recommended)" {
        return false;
    }
    // Served catalogs put the full model name in the label column; the
    // compiled picker uses an alias and repeats the name in its description.
    if label.trim_end().trim_end_matches('✔').trim_end() == name {
        return true;
    }
    let description = description.trim_start();
    description == name || description.starts_with(&format!("{name} · "))
}

#[derive(Debug, PartialEq)]
enum ClaudeSwitch {
    /// Claude's own prompt-cache notice; `choice` follows "Yes, switch to".
    Cache { model: bool, choice: String },
    /// A PreModelSwitch hook asks; its policy is never answered for the user.
    Hook,
    Unknown,
}

fn claude_switch_dialog(screen: &str) -> Option<ClaudeSwitch> {
    let lines: Vec<_> = screen.lines().collect();
    let title = lines
        .iter()
        .rposition(|line| ["Switch model?", "Change effort level?"].contains(&line.trim()))?;
    let model = lines[title].trim() == "Switch model?";
    match lines.get(title + 1).map(|line| line.trim()) {
        Some("A PreModelSwitch hook asked you to confirm") => return Some(ClaudeSwitch::Hook),
        Some("Your next response will be slower and use more tokens") => {}
        _ => return Some(ClaudeSwitch::Unknown),
    }
    let below = &lines[title + 2..];
    let selected: Vec<_> = below
        .iter()
        .filter_map(|line| line.trim_start().strip_prefix("❯ "))
        .collect();
    let choice = match selected.as_slice() {
        [row] => row.trim_end().strip_prefix("1. Yes, switch to "),
        _ => None,
    };
    let declinable = below.iter().any(|line| line.trim() == "2. No, go back");
    match choice {
        Some(choice) if declinable => Some(ClaudeSwitch::Cache {
            model,
            choice: choice.to_owned(),
        }),
        _ => Some(ClaudeSwitch::Unknown),
    }
}

/// The effort of a "Set model to `name` …" receipt appended after `offset` to
/// this conversation's transcript, or None while Claude has not written it.
fn claude_receipt_since(path: &Path, conversation: &str, offset: u64, name: &str) -> Result<Option<String>> {
    let Ok(mut file) = File::open(path) else {
        return Ok(None);
    };
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if length <= offset {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_TAIL).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if string(&event, "sessionId") != conversation || event["isSidechain"] == true {
            continue;
        }
        let text = event["message"]["content"].as_str().or(event["content"].as_str());
        if let Some((receipt, effort)) = text.and_then(claude_receipt) {
            if receipt == name && text.is_some_and(|text| text.contains("` for this session only")) {
                return Ok(Some(effort));
            }
        }
    }
    Ok(None)
}

/// Esc cancels Claude's picker without saving anything.
fn claude_close_picker(name: &str, request: &Request) {
    let closed = || -> Result<bool> {
        let record = checked(name, request, false)?;
        Ok(claude_model_panel(&capture(&record)?).is_none())
    };
    if closed().unwrap_or(true) {
        return;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if picker_entered() {
        let _ = picker_scope::close_claude_model(name, request);
        let _ = wait_until(|| Ok(closed()?.then_some(())));
        return;
    }
    if let Ok(record) = checked(name, request, false) {
        let _ = send_key(&record, "Escape");
        let _ = wait_until(|| Ok(closed()?.then_some(())));
    }
}

/// Select the model and effort in Claude's /model picker and apply them with its
/// session-only key. Enter would save both as defaults for new sessions.
fn claude_model(name: &str, request: &Request, model: &ModelOption) -> Result<()> {
    let record = checked(name, request, true)?;
    let transcript = claude_transcript(&record)?;
    let conversation = string(&record, "conversation_id").to_owned();
    let offset = fs::metadata(&transcript).map(|meta| meta.len()).unwrap_or(0);
    let opened = std::cell::Cell::new(false);
    let result = claude_model_opened(name, request, model, &transcript, &conversation, offset, &opened);
    if result.is_err() && opened.get() {
        claude_close_picker(name, request);
    }
    result
}

fn claude_model_opened(
    name: &str,
    request: &Request,
    model: &ModelOption,
    transcript: &Path,
    conversation: &str,
    offset: u64,
    opened: &std::cell::Cell<bool>,
) -> Result<()> {
    open_model_tracked(name, request, Some(opened))?;
    let selected = (|| -> Result<()> {
        let mut observed = wait_until(|| {
            let record = checked(name, request, false)?;
            Ok(claude_picker(&capture(&record)?).map(|picker| (record, picker)))
        })?;
        if !observed.1.session_only && observed.1.section.is_none() {
            return Err("Claude's model picker does not offer a session-only choice; use Terminal".into());
        }
        let mut visited = Vec::new();
        loop {
            // The checked observation that found this row supplies its next
            // key's fresh turn. Reprobing it doubled native reads and could
            // exhaust the original host deadline before failure cleanup.
            let (record, picker) = observed;
            if claude_row_is(&picker.row, &model.label) {
                break;
            }
            // The list wraps around: a repeated row means every row was seen.
            let position = (picker.row.clone(), picker.section);
            if visited.contains(&position) || visited.len() >= 64 {
                return Err("model is not offered by Claude's model picker".into());
            }
            visited.push(position);
            let key = if picker.section == Some(ClaudeSection::Collapsed) { "Enter" } else { "Down" };
            send_key(&record, key)?;
            observed = wait_until(|| {
                let record = checked(name, request, false)?;
                Ok(claude_picker(&capture(&record)?)
                    .filter(|next| next.row != picker.row || next.section != picker.section)
                    .map(|next| (record, next)))
            })?;
        }
        for _ in 0..=model.efforts.len() {
            let record = checked(name, request, false)?;
            let picker = claude_picker(&capture(&record)?).ok_or("model picker changed; inspect Terminal")?;
            // In search mode `s` would be typed into the filter instead.
            if !picker.session_only {
                return Err("Claude's model picker does not offer a session-only choice; use Terminal".into());
            }
            if !claude_row_is(&picker.row, &model.label) {
                return Err("model picker selection changed; inspect Terminal".into());
            }
            let shown = picker.effort.ok_or("Claude's effort selection is not visible; enlarge Terminal")?;
            if shown == request.effort {
                return Ok(());
            }
            let index = model.efforts.iter().position(|value| value == &shown);
            let target = model.efforts.iter().position(|value| value == &request.effort);
            let (Some(index), Some(target)) = (index, target) else {
                return Err("effort is not offered for this model by Claude's picker".into());
            };
            send_key(&record, if target > index { "Right" } else { "Left" })?;
            wait_until(|| {
                let record = checked(name, request, false)?;
                let changed = claude_picker(&capture(&record)?)
                    .is_some_and(|next| next.effort.as_deref() != Some(shown.as_str()));
                Ok(changed.then_some(()))
            })?;
        }
        Err("Claude's effort selection did not settle; inspect Terminal".into())
    })();
    selected?;
    send_key(&checked(name, request, false)?, "s")?;
    let mut confirmed = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let deadline = picker_scope::deadline(deadline)?;
    loop {
        let record = checked(name, request, false)?;
        let screen = capture(&record)?;
        match claude_switch_dialog(&screen) {
            Some(ClaudeSwitch::Cache { model: kind, choice }) if !confirmed.contains(&kind) => {
                let expected = if kind {
                    choice == model.label
                } else {
                    choice.eq_ignore_ascii_case(&request.effort)
                };
                if !expected {
                    return Err("Claude asks to confirm a different change; inspect Terminal".into());
                }
                // The person accepted this notice in Zerus before applying.
                send_key(&record, "Enter")?;
                confirmed.push(kind);
                continue;
            }
            Some(ClaudeSwitch::Hook) => {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if picker_entered() { return picker_refuse("a PreModelSwitch hook asks for confirmation; change the model in Terminal".into()); }
                send_key(&record, "Escape")?;
                let _ = wait_until(|| {
                    let record = checked(name, request, false)?;
                    Ok(claude_switch_dialog(&capture(&record)?).is_none().then_some(()))
                });
                return Err("a PreModelSwitch hook asks for confirmation; change the model in Terminal".into());
            }
            Some(ClaudeSwitch::Unknown) => {
                return Err("Claude shows an unrecognized confirmation; inspect Terminal".into());
            }
            _ => {}
        }
        if claude_picker(&screen).is_none() {
            // Without a dialog, a closed picker shows Claude read the last key.
            reacted();
            if let Some(effort) = claude_receipt_since(transcript, conversation, offset, &model.label)? {
                // The receipt names effort only when the picker changed it; the
                // indicator, when shown, must agree with the request.
                let shown = claude_footer(&screen);
                if (effort.is_empty() || effort == request.effort)
                    && shown.as_deref().is_none_or(|shown| shown == request.effort)
                    && input::checked_terminal(&record, true).is_ok()
                {
                    return Ok(());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err("Claude did not confirm the session-only change; inspect Terminal".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn claude_change(name: &str, request: &Request, current: &Settings) -> Result<()> {
    let record = checked(name, request, true)?;
    let model = catalog(&record)
        .into_iter()
        .find(|model| model.id == current.model)
        .ok_or("the current model is not in Claude's model catalog; use Terminal")?;
    claude_model(name, request, &model)
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if picker_entered() {return picker_refuse("A picker keeper cannot enter another public controller".into());}
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs effort <session> --json (JSON from stdin)".into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("effort payload exceeds 4 KiB".into());
    }
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid effort payload: {e}"))?;
    let id = uuid::Uuid::parse_str(&request.request_id)
        .map_err(|_| "request_id must be a UUID")?
        .to_string();
    if request.expected_run_id.is_empty()
        || request.expected_conversation_id.is_empty()
        || !valid(&request.effort)
    {
        return Err("a confirmed session identity and supported effort are required".into());
    }
    let name = &args[0];
    let digest = format!("{:x}", Sha256::digest(&bytes));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(result) = picker_scope::front(name, &bytes) { return result; }
    let receipt_path = absolute_root()?
        .join("effort_receipts")
        .join(format!("{id}.json"));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _guard = Guard::exclusive(storage::lock_file(&root().join(".lock"))?)
        .map_err(|error|error.to_string())?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _guard = lock(None)?;
    if receipt_path.exists() {
        let text = fs::read_to_string(&receipt_path).map_err(|e| e.to_string())?;
        let receipt: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if string(&receipt, "name") != name || string(&receipt, "digest") != digest {
            return Err("request ID was already used for different content".into());
        }
        if receipt["status"] != "confirmed" {
            return Err("effort change may already have reached the provider; inspect Terminal before retrying".into());
        }
        println!("{text}");
        return Ok(0);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if session_id::selected().is_none() {
            super::resolve_effort_label(name, super::LabelGuard::Given {
                run:request.expected_run_id.clone(),conversation:request.expected_conversation_id.clone()
            }).map_err(|error|error.text)?;
        }
        let original=storage::read_metadata(name)?;
        if storage::in_id_layout(&original) {
            return Err("Session moved into the ID layout; refresh before changing effort".into());
        }
        // Given metadata resolution already pinned the unique raw-guarded
        // legacy route. Its old deferred report is now only informational;
        // ordinary expect must not resolve the siblings again under that pin.
        session_id::take_deferred(name);
        // Only the verified old legacy route reaches its unchanged recovery
        // and native controller. Cached UUID results above need none of it.
        rename::recover_locked()?;
        rename::sync_legacy_locked()?;
        worktree_lock::recover_locked();
        let current=storage::read_metadata(name)?;
        if storage::in_id_layout(&current) || !picker_scope::same_target(&original,&current) {
            return Err("Original legacy Effort target changed during recovery".into());
        }
    }
    let mut record = checked(name, &request, true)?;
    let current = settings(&record, true);
    if visible_effort(&record, &current.model, &capture(&record)?).as_deref()
        != Some(current.effort.as_str())
    {
        return Err("the current provider effort is not visible; open Terminal".into());
    }
    let mut levels = options(&record, &current.model);
    if string(&record, "agent") == "codex" && request.effort == "ultra" {
        return Err("Ultra requires the explicit advanced reasoning picker in Terminal".into());
    }
    if levels.len() > 16 || !levels.contains(&request.effort) || current.effort.is_empty() {
        return Err("effort is unavailable for the active model; refresh or use Terminal".into());
    }
    // Codex walks regular choices followed by advanced choices (Max, Ultra).
    if string(&record, "agent") == "codex" {
        levels.sort_by_key(|v| match v.as_str() {
            "max" => 1,
            "ultra" => 2,
            _ => 0,
        });
    }
    let mut receipt = json!({"request_id":id,"name":name,"digest":digest,"status":"in_progress", "run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id,"effort":request.effort,"model":current.model,"scope":"session","at":now()});
    atomic(&receipt_path, &receipt.to_string())?;
    start_attempt();
    let result = if current.effort == request.effort {
        Ok(())
    } else {
        match string(&record, "agent") {
            "codex" => codex_change(name, &request, &current, &levels),
            "kimi" => kimi_change(name, &request, &current),
            "claude" => claude_change(name, &request, &current),
            _ => Err("change this provider's effort in Terminal".into()),
        }
    };
    if let Err(error) = result {
        mark_unconfirmed(&mut record);
        return Err(format!("effort change not confirmed: {error}"));
    }
    let latest = history(&record);
    record["effort"] = json!(request.effort);
    record["model"] = json!(current.model);
    record["effort_source_path"] = json!(latest.path);
    record["effort_source_offset"] = json!(latest.offset);
    write(&mut record)?;
    receipt["status"] = json!("confirmed");
    atomic(&receipt_path, &receipt.to_string())?;
    println!("{receipt}");
    Ok(0)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsRequest {
    request_id: String,
    expected_run_id: String,
    expected_conversation_id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    expected_pending_id: Option<String>,
}

fn requested_model(record: &Value, request: &SettingsRequest) -> Result<(ModelOption, String)> {
    let models = catalog(record);
    let reason = settings_reason(record, &models);
    if !reason.is_empty() {
        return Err(reason.into());
    }
    let current = settings(record, true);
    let id = request.model.as_deref().unwrap_or_else(|| {
        let pending = string(&record["pending_settings"], "model");
        if pending.is_empty() {
            &current.model
        } else {
            pending
        }
    });
    let model = models
        .iter()
        .find(|m| m.id == id)
        .ok_or("model is not in this account's provider catalog")?
        .clone();
    let effort = request.effort.clone().unwrap_or_else(|| {
        let pending = string(&record["pending_settings"], "effort");
        if string(&record["pending_settings"], "model") == id
            && model.efforts.iter().any(|e| e == pending)
        {
            pending.into()
        } else if model.efforts.contains(&current.effort) {
            current.effort.clone()
        } else if model.efforts.contains(&model.default_effort) {
            model.default_effort.clone()
        } else {
            model.efforts.first().cloned().unwrap_or_default()
        }
    });
    if (!effort.is_empty() && !model.efforts.contains(&effort))
        || (effort.is_empty() && !model.efforts.is_empty())
    {
        return Err("effort is unavailable for the selected model".into());
    }
    if id != current.model && models.iter().filter(|m| m.label == model.label).count() != 1 {
        return Err("provider model names are ambiguous; select this model in Terminal".into());
    }
    Ok((model, effort))
}

fn apply_settings(
    name: &str,
    request: &Request,
    model: &ModelOption,
    receipt_path: &Path,
    receipt: &mut Value,
) -> Result<Value> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if picker_entered() {
        return picker_refuse("Legacy Settings application is forbidden in the original picker keeper".into());
    }
    // The lock is the session's own, keyed by its verified record (#1437
    // review); the record and its terminal are checked once it is held.
    let owner = input::checked_settings(name, &request.expected_run_id, &request.expected_conversation_id)?;
    let _driving = startup_choice::driving(&owner)?;
    let mut record = checked(name, request, true)?;
    let current = settings(&record, true);
    receipt["status"] = json!("in_progress");
    receipt["run_id"] = json!(request.expected_run_id);
    receipt["name"] = json!(name);
    atomic(receipt_path, &receipt.to_string())?;
    let screen = capture(&record)?;
    let visible_effort = target_footer(&record, model, &screen);
    start_attempt();
    let result = if string(&record, "agent") == "claude" {
        // Its indicator shows effort only; history or a receipt names the model.
        let shown = claude_footer(&screen);
        if current.model == model.id
            && (shown.as_deref() == Some(request.effort.as_str())
                || (model.efforts.is_empty() && request.effort.is_empty()))
        {
            Ok(())
        } else {
            claude_model(name, request, model)
        }
    } else if visible_effort.as_deref() == Some(request.effort.as_str()) {
        Ok(())
    } else if current.model != model.id
        || visible_effort.is_none()
        || (string(&record, "agent") == "codex" && request.effort == "ultra")
    {
        match string(&record, "agent") {
            "codex" => codex_model(name, request, model),
            "kimi" => kimi_model(name, request, model),
            _ => Err("unsupported model adapter".into()),
        }
    } else {
        match string(&record, "agent") {
            "codex" => {
                let visible = Settings {
                    model: model.label.clone(),
                    effort: visible_effort
                        .clone()
                        .unwrap_or_else(|| current.effort.clone()),
                    ..current.clone()
                };
                codex_change(name, request, &visible, &model.efforts)
            }
            "kimi" => kimi_change(
                name,
                request,
                &Settings {
                    effort: visible_effort
                        .clone()
                        .unwrap_or_else(|| current.effort.clone()),
                    ..current.clone()
                },
            ),
            _ => Err("unsupported effort adapter".into()),
        }
    };
    if let Err(error) = result {
        mark_unconfirmed(&mut record);
        return Err(format!("settings change not confirmed: {error}"));
    }
    let latest = history(&record);
    record["model"] = json!(model.id);
    record["effort"] = json!(request.effort);
    record["effort_source_path"] = json!(latest.path);
    record["effort_source_offset"] = json!(latest.offset);
    record["resume_settings"] = json!({"model":model.id,"effort":request.effort});
    record.as_object_mut().unwrap().remove("pending_settings");
    write(&mut record)?;
    receipt["status"] = json!("applied");
    atomic(receipt_path, &receipt.to_string())?;
    Ok(record)
}

pub(super) fn dispatch_settings(args: &[String]) -> Result<i32> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if picker_entered() {return picker_refuse("A picker keeper cannot enter another public controller".into());}
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs settings <session> --json (JSON from stdin)".into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("settings payload exceeds 4 KiB".into());
    }
    let request: SettingsRequest =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid settings payload: {e}"))?;
    let id = uuid::Uuid::parse_str(&request.request_id)
        .map_err(|_| "request_id must be a UUID")?
        .to_string();
    if request.expected_run_id.is_empty()
        || (request.model.is_none() && request.effort.is_none())
        || request.model.as_deref().is_some_and(|v| !safe_model(v))
        || request
            .effort
            .as_deref()
            .is_some_and(|v| !v.is_empty() && !valid(v))
    {
        return Err(
            "confirmed session identity and a supported model or effort are required".into(),
        );
    }
    let name = &args[0];
    let digest = format!("{:x}", Sha256::digest(&bytes));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(result) = picker_scope::front_settings(name, &bytes) { return result; }
    let receipt_path = absolute_root()?
        .join("settings_receipts")
        .join(format!("{id}.json"));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _guard = Guard::exclusive(storage::lock_file(&root().join(".lock"))?).map_err(|e|e.to_string())?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _guard = lock(None)?;
    if receipt_path.exists() {
        let text = fs::read_to_string(&receipt_path).map_err(|e| e.to_string())?;
        let receipt: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if string(&receipt, "name") != name || string(&receipt, "digest") != digest {
            return Err("request ID was already used for different content".into());
        }
        if !["applied", "scheduled"].contains(&string(&receipt, "status")) {
            return Err("settings change may already have reached the provider; inspect Terminal before retrying".into());
        }
        println!("{text}");
        return Ok(0);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if session_id::selected().is_none() {
            super::resolve_effort_label(name, super::LabelGuard::Given {
                run:request.expected_run_id.clone(),conversation:request.expected_conversation_id.clone()
            }).map_err(|error|error.text)?;
        }
        let original=storage::read_metadata(name)?;
        if storage::in_id_layout(&original) {return Err("Session moved into the ID layout; refresh before changing settings".into());}
        session_id::take_deferred(name);
        rename::recover_locked()?;
        rename::sync_legacy_locked()?;
        worktree_lock::recover_locked();
        let current=storage::read_metadata(name)?;
        if storage::in_id_layout(&current) || !picker_scope::same_target(&original,&current) {
            return Err("Original legacy Settings target changed during recovery".into());
        }
    }
    // zerus-ucck P3c: the request's identities select among sessions that
    // share the label.
    expect(name, &request.expected_run_id, &request.expected_conversation_id)?;
    let mut record = read(name)?;
    terminating::check_mutation(&record)?;
    if string(&record, "run_id") != request.expected_run_id
        || string(&record, "conversation_id") != request.expected_conversation_id
    {
        return Err("session identity changed; refresh before changing settings".into());
    }
    if request
        .expected_pending_id
        .as_deref()
        .is_some_and(|id| id != string(&record["pending_settings"], "request_id"))
    {
        return Err("pending settings changed; refresh before applying".into());
    }
    let (model, effort) = requested_model(&record, &request)?;
    if request.expected_pending_id.is_some()
        && (string(&record["pending_settings"], "model") != model.id
            || string(&record["pending_settings"], "effort") != effort)
    {
        return Err("pending settings target changed; refresh before applying".into());
    }
    let running = process_alive(&record);
    if running && !matches(&record, panes_of(&live()?, &record)) {
        return Err(
            "the tracked terminal identity changed; refresh before changing settings".into(),
        );
    }
    let mut receipt = json!({"request_id":id,"name":name,"digest":digest,"status":"scheduled","run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id,"model":model.id,"effort":effort,"scope":"session","at":now()});
    let immediate = applies_now(&record, running);
    if request.expected_pending_id.is_some() {
        let pending_id = string(&record["pending_settings"], "request_id").to_owned();
        if immediate {
            receipt["status"] = json!("in_progress");
            atomic(&receipt_path, &receipt.to_string())?;
            apply_pending(name, record)?;
            receipt["status"] = json!("applied");
        } else {
            receipt["pending_settings_id"] = json!(pending_id);
            receipt["apply_when"] = json!(if running { "ready" } else { "resume" });
        }
        atomic(&receipt_path, &receipt.to_string())?;
        println!("{receipt}");
        return Ok(0);
    }
    if immediate {
        let native = Request {
            request_id: id,
            expected_run_id: request.expected_run_id,
            expected_conversation_id: request.expected_conversation_id,
            effort,
        };
        apply_settings(name, &native, &model, &receipt_path, &mut receipt)?;
    } else {
        if !running {
            recipes::argv(&record)?;
        }
        // No terminal bytes on a busy or stopped session, or to a chooser. The
        // UI applies this explicit request once Ready; send also applies it
        // before the next idle message. A busy message keeps the currently
        // active settings.
        let pending = json!({"request_id":id,"model":model.id,"effort":effort,"run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id,"receipt_name":name,"receipt_run_id":request.expected_run_id});
        let resume = json!({"model":model.id,"effort":effort});
        schedule_settings(&mut record, pending, resume, || {
            receipt["pending_settings_id"] = json!(id);
            receipt["apply_when"] = json!(if running { "ready" } else { "resume" });
            atomic(&receipt_path, &receipt.to_string())
        })?;
    }
    println!("{receipt}");
    Ok(0)
}

/// Saves scheduled settings, then publishes their receipt. When publishing
/// fails, the previous record is written back. A record an older hgs wrote
/// gets its session ID before the snapshot, so the rollback keeps the ID the
/// first write saved instead of assigning another (zerus-ucck, #1317 R6).
fn schedule_settings(
    record: &mut Value,
    pending: Value,
    resume: Value,
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    ensure_session_id(record);
    let original = record.clone();
    record["pending_settings"] = pending;
    record["resume_settings"] = resume;
    write(record)?;
    if let Err(error) = publish() {
        let mut restore = original;
        write(&mut restore).map_err(|rollback| {
            format!("{error}; could not restore previous settings: {rollback}")
        })?;
        return Err(error);
    }
    Ok(())
}

// Called under the existing input transaction lock; never a mutation from ls or
// inspect. It deliberately does nothing while the agent is still working.
/// Whether [`apply_pending`] would type pending settings into the terminal.
pub(super) fn pending_applies(record: &Value) -> bool {
    record["pending_settings"].is_object() && ready(record)
}

pub(super) fn apply_pending(name: &str, record: Value) -> Result<Value> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if picker_entered() {
        return picker_refuse("Legacy pending application is forbidden in the original picker keeper".into());
    }
    terminating::check_mutation(&record)?;
    if !pending_applies(&record) {
        return Ok(record);
    }
    let pending = &record["pending_settings"];
    if string(pending, "run_id") != string(&record, "run_id")
        || string(pending, "conversation_id") != string(&record, "conversation_id")
    {
        return Err("pending settings belong to a different session; refresh settings".into());
    }
    let request = SettingsRequest {
        request_id: string(pending, "request_id").into(),
        expected_run_id: string(&record, "run_id").into(),
        expected_conversation_id: string(&record, "conversation_id").into(),
        model: Some(string(pending, "model").into()),
        effort: Some(string(pending, "effort").into()),
        expected_pending_id: None,
    };
    let (model, effort) = requested_model(&record, &request)?;
    let id = uuid::Uuid::parse_str(&request.request_id)
        .map_err(|_| "invalid pending settings ID")?
        .to_string();
    let path = absolute_root()?
        .join("settings_receipts")
        .join(format!("{id}.json"));
    let mut receipt: Value =
        serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if string(&receipt, "status") != "scheduled"
        || string(&receipt, "name") != string(pending, "receipt_name")
        || string(&receipt, "run_id") != string(pending, "receipt_run_id")
        || string(&receipt, "conversation_id") != request.expected_conversation_id
    {
        return Err("pending settings are not safe to retry; inspect Terminal".into());
    }
    let native = Request {
        request_id: id,
        expected_run_id: request.expected_run_id,
        expected_conversation_id: request.expected_conversation_id,
        effort,
    };
    apply_settings(name, &native, &model, &path, &mut receipt)
}

// Kimi has a per-invocation model flag but no effort flag. Restore its saved
// session effort through the same guarded adapter once the resumed TUI is Ready.
pub(super) fn restore_pending(record: &mut Value) -> Result<()> {
    if record["resume_settings"].is_object() {
        record["resume_settings"] = effective_resume_settings(record);
    }
    if string(record, "agent") != "kimi"
        || record["pending_settings"].is_object()
        || !safe_model(string(&record["resume_settings"], "model"))
    {
        return Ok(());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let model = string(&record["resume_settings"], "model").to_owned();
    let effort = string(&record["resume_settings"], "effort").to_owned();
    record["pending_settings"] = json!({"request_id":id,"run_id":record["run_id"],"conversation_id":record["conversation_id"],"model":model,"effort":effort,"receipt_name":record["name"],"receipt_run_id":record["run_id"]});
    let receipt = json!({"request_id":id,"name":record["name"],"run_id":record["run_id"],"conversation_id":record["conversation_id"],"model":model,"effort":effort,"scope":"session","status":"scheduled","apply_when":"ready","at":now()});
    atomic(
        &absolute_root()?
            .join("settings_receipts")
            .join(format!("{id}.json")),
        &receipt.to_string(),
    )
}

fn effective_resume_settings(record: &Value) -> Value {
    // Claude resumes with the account's settings unless this session has a
    // choice made through Zerus; then its newer native history wins as usual.
    if string(record, "agent") == "claude" && !record["resume_settings"].is_object() {
        return Value::Null;
    }
    let native = history(record);
    if !record["pending_settings"].is_object()
        && safe_model(&native.model)
        && (native.path != string(record, "effort_source_path")
            || native.offset > record["effort_source_offset"].as_u64().unwrap_or(0))
    {
        json!({"model":native.model,"effort":native.effort})
    } else {
        record["resume_settings"].clone()
    }
}

pub(super) fn resume_argv(record: &Value, mut args: Vec<String>) -> Vec<String> {
    let effective = effective_resume_settings(record);
    let settings = &effective;
    if let Some(model) = settings["model"].as_str().filter(|m| safe_model(m)) {
        // argparse rejects duplicate scalar options in some providers. Remove
        // only the two owned overrides; keep every unrelated launch option.
        let mut clean = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let key = args[i].split('=').next().unwrap_or("");
            if ["--model", "-m"].contains(&key) {
                i += if args[i].contains('=') { 1 } else { 2 };
                continue;
            }
            if string(record, "agent") == "claude" && key == "--effort" {
                i += if args[i].contains('=') { 1 } else { 2 };
                continue;
            }
            if string(record, "agent") == "codex" && ["-c", "--config"].contains(&key) {
                let value = if let Some((_, v)) = args[i].split_once('=') {
                    Some(v)
                } else {
                    args.get(i + 1).map(String::as_str)
                };
                if value.is_some_and(|v| {
                    v.starts_with("model=") || v.starts_with("model_reasoning_effort=")
                }) {
                    i += if args[i].contains('=') { 1 } else { 2 };
                    continue;
                }
            }
            clean.push(args[i].clone());
            i += 1;
        }
        args = clean;
        args.extend(["--model".into(), model.into()]);
        if string(record, "agent") == "codex" {
            if let Some(effort) = settings["effort"].as_str().filter(|v| valid(v)) {
                args.extend(["-c".into(), format!("model_reasoning_effort=\"{effort}\"")]);
            }
        }
        // Claude's --effort applies to the launched session only.
        if string(record, "agent") == "claude" {
            if let Some(effort) = settings["effort"].as_str().filter(|v| valid(v)) {
                args.extend(["--effort".into(), effort.into()]);
            }
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    /// #1317 R6: a failed settings receipt writes the previous record back
    /// with the session ID the first write gave a record an older hgs wrote.
    /// The fixture runs in a child process with a private state directory.
    #[test]
    fn a_failed_settings_receipt_keeps_the_session_id_the_first_write_saved() {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::effort::tests::settings_rollback_fixture",
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path())
            .env("ZERUS_SETTINGS_ROLLBACK_FIXTURE", "1")
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    #[ignore = "run by a_failed_settings_receipt_keeps_the_session_id_the_first_write_saved"]
    fn settings_rollback_fixture() {
        if std::env::var_os("ZERUS_SETTINGS_ROLLBACK_FIXTURE").is_none() {
            return;
        }
        fs::create_dir_all(root()).unwrap();
        let name = "codex/example/legacy";
        // A busy session saved by an older hgs, without a session ID.
        let legacy = json!({"version":1,"agent":"codex","name":name,"run_id":"run-1",
            "conversation_id":"conversation-1","activity":"busy","phase":"working","created":100});
        atomic(&record_path(name), &legacy.to_string()).unwrap();
        let mut record = read(name).unwrap();
        assert_eq!(string(&record, "session_id"), "");
        let mut first = String::new();
        let error = schedule_settings(&mut record, json!({"request_id":"one"}), json!({"effort":"low"}), || {
            let saved = read(name).unwrap();
            assert_eq!(saved["pending_settings"]["request_id"], "one");
            first = string(&saved, "session_id").to_owned();
            Err("receipt publication failed".into())
        })
        .unwrap_err();
        assert_eq!(error, "receipt publication failed");
        assert!(uuid::Uuid::parse_str(&first).is_ok(), "{first:?}");
        let restored = read(name).unwrap();
        assert!(restored["pending_settings"].is_null() && restored["resume_settings"].is_null());
        assert_eq!(string(&restored, "session_id"), first, "the rollback keeps the first write's ID");
        // A schedule whose receipt is published keeps it too.
        let mut record = read(name).unwrap();
        schedule_settings(&mut record, json!({"request_id":"two"}), json!({"effort":"low"}), || Ok(())).unwrap();
        let scheduled = read(name).unwrap();
        assert_eq!(scheduled["pending_settings"]["request_id"], "two");
        assert_eq!(string(&scheduled, "session_id"), first);
    }
    #[test]
    fn a_failed_attempt_without_keys_preserves_the_first_message() {
        let mut record = json!({
            "agent": "codex", "run_id": "fresh", "run_identity_version": 1,
            "supervisor": {}, "last_event_at": 0, "activity": "unknown", "phase": "unknown"
        });
        let before = record.clone();
        assert!(input::first_message_candidate(&record));
        start_attempt();
        mark_unconfirmed(&mut record);
        assert_eq!(record, before);
        assert!(input::first_message_candidate(&record));
    }

    #[test]
    fn settings_apply_now_when_the_listing_reports_ready() {
        assert!(ready(&telemetry::stale_hook_child()));
        for (case, record) in telemetry::remaining_work() {
            assert!(!ready(&record), "{case}");
        }
    }
    #[test]
    fn effort_changes_are_offered_when_dispatch_accepts_them() {
        // `hgs effort` dispatches through input::checked_settings, which
        // requires the listing's Ready; a Kimi profile group still working
        // after the main Stop leaves the saved activity idle.
        assert_eq!(effort_reason(&telemetry::stale_hook_child(), true, true), "");
        for (case, record) in telemetry::remaining_work() {
            assert_eq!(effort_reason(&record, true, true), "Wait for Ready to change effort", "{case}");
        }
        let mut kimi = telemetry::stale_hook_child();
        kimi.as_object_mut().unwrap().remove("kimi_native_roster");
        kimi["subagents"]["coder"]["state"] = json!("finished");
        assert_eq!(effort_reason(&kimi, true, true), "Wait for Ready to change effort");
        assert_eq!(effort_reason(&kimi, true, false), "Resume the session to change effort");
        assert_eq!(effort_reason(&kimi, false, true), "Waiting for the provider's model and effort settings");
        kimi["agent"] = json!("opencode");
        assert_eq!(effort_reason(&kimi, true, true), "Use the provider's Terminal to change effort");
    }
    #[test]
    fn a_terminal_chooser_schedules_settings_and_keeps_earlier_reasons() {
        let reason = "Wait until the agent's question is answered";
        let output = |when: &str, effort: &str| {
            let mut output = json!({"settings_apply_when":when,"settings_change_supported":true,"settings_change_reason":"",
                "effort_change_supported":effort.is_empty(),"effort_change_reason":effort});
            wait_for_chooser(&mut output, reason);
            output
        };
        assert_eq!(output("now", ""), json!({"settings_apply_when":"ready","settings_change_supported":true,
            "settings_change_reason":"","effort_change_supported":false,"effort_change_reason":reason}));
        // A busy or stopped session keeps its own timing and reasons.
        for (when, effort) in [("ready", "Wait for Ready to change effort"), ("resume", "Resume the session to change effort")] {
            assert_eq!(output(when, effort), json!({"settings_apply_when":when,"settings_change_supported":true,
                "settings_change_reason":"","effort_change_supported":false,"effort_change_reason":effort}));
        }
        // A stopped or working session schedules without reading a chooser.
        assert!(!applies_now(&telemetry::stale_hook_child(), false));
        for (case, record) in telemetry::remaining_work() {
            assert!(!applies_now(&record, true), "{case}");
        }
    }
    #[test]
    fn provider_settings_ignore_text_and_thinking_content() {
        assert_eq!(
            event_settings(
                "codex",
                &json!({"type":"turn_context","payload":{"model":"gpt-6-astra","effort":"xhigh"}})
            ),
            Some(("gpt-6-astra".into(), "xhigh".into()))
        );
        assert_eq!(
            event_settings(
                "kimi",
                &json!({"type":"llm.request","modelAlias":"kimi-code/k3","thinkingEffort":"max"})
            ),
            Some(("kimi-code/k3".into(), "max".into()))
        );
        assert!(event_settings(
            "kimi",
            &json!({"type":"agent.message.appended","modelAlias":"wrong","thinkingEffort":"high"})
        )
        .is_none());
    }
    #[test]
    fn exact_conversation_history_and_confirmed_override() {
        let root = tempfile::tempdir().unwrap();
        let id = "12345678-1234-1234-1234-123456789abc";
        let path = root.path().join(format!("rollout-{id}.jsonl"));
        let context = |effort: &str| {
            json!({"type":"turn_context","payload":{"model":"gpt-6-astra","effort":effort}})
                .to_string()
                + "\n"
        };
        let header = json!({"type":"session_meta","payload":{"id":id}}).to_string() + "\n";
        fs::write(&path, header.clone() + &context("high")).unwrap();
        let mut record =
            json!({"agent":"codex","conversation_id":id,"transcript":path,"model":"gpt-6-astra"});
        let initial = history(&record);
        assert_eq!(initial.effort, "high");
        record["effort"] = json!("low");
        record["effort_source_path"] = json!(initial.path);
        record["effort_source_offset"] = json!(initial.offset);
        assert_eq!(settings(&record, false).effort, "low");
        fs::write(&path, header.clone() + &context("high") + &context("max")).unwrap();
        assert_eq!(settings(&record, false).effort, "max");
        // A matching filename is not enough to trust another conversation.
        fs::write(
            &path,
            json!({"type":"session_meta","payload":{"id":"different"}}).to_string()
                + "\n"
                + &context("max"),
        )
        .unwrap();
        assert!(history(&record).effort.is_empty());
        fs::write(
            &path,
            header + &context("medium") + "{\"type\":\"turn_context\",\"payload\":",
        )
        .unwrap();
        assert_eq!(history(&record).effort, "medium");
    }

    #[test]
    fn native_model_names_require_exact_column_boundaries() {
        assert!(row_is("K3  kimi", "K3"));
        assert!(!row_is("K3 Pro  kimi", "K3"));
        assert!(row_is("GPT-6-Sol (current)  Workhorse", "GPT-6-Sol"));
        assert!(!row_is("GPT-6-Sol Mini  Lightweight", "GPT-6-Sol"));
        let model = ModelOption {
            id: "k3".into(),
            label: "K3".into(),
            efforts: vec!["high".into()],
            default_effort: "high".into(),
        };
        assert_eq!(
            target_footer(
                &json!({"agent":"kimi"}),
                &model,
                "Ask When Needed  K3 thinking: high  /tmp\n context: 2%"
            ),
            Some("high".into())
        );
        assert!(target_footer(
            &json!({"agent":"kimi","model":"k3"}),
            &model,
            "Ask When Needed  K3 Pro thinking: high  /tmp\n context: 2%"
        )
        .is_none());
    }

    #[test]
    fn resume_preserves_newer_native_settings_unless_an_intent_is_pending() {
        let root = tempfile::tempdir().unwrap();
        let id = "native-session";
        let path = root.path().join(format!("rollout-{id}.jsonl"));
        let header = json!({"type":"session_meta","payload":{"id":id}}).to_string() + "\n";
        fs::write(
            &path,
            header.clone()
                + &json!({"type":"turn_context","payload":{"model":"native-model","effort":"high"}})
                    .to_string() + "\n",
        )
        .unwrap();
        let mut record = json!({"agent":"codex","conversation_id":id,"transcript":path,"resume_settings":{"model":"old-override","effort":"low"},"effort_source_path":path,"effort_source_offset":header.len()});
        let args = resume_argv(&record, vec!["codex".into(), "resume".into(), id.into()]);
        assert!(args.contains(&"native-model".into()));
        assert!(args.contains(&"model_reasoning_effort=\"high\"".into()));
        record["pending_settings"] = json!({"model":"old-override","effort":"low"});
        let args = resume_argv(&record, vec!["codex".into(), "resume".into(), id.into()]);
        assert!(args.contains(&"old-override".into()));
        assert!(!args.contains(&"native-model".into()));
        let model = ModelOption {
            id: "k3".into(),
            label: "K3".into(),
            efforts: vec!["high".into()],
            default_effort: "high".into(),
        };
        assert!(target_footer(
            &json!({"agent":"kimi","model":"k3"}),
            &model,
            "Ask When Needed  Pro K3 thinking: high  /tmp\n context: 2%"
        )
        .is_none());
    }

    #[test]
    fn settings_searches_without_a_result_are_kept_across_listings() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        scan_cache::DIR.with(|dir| *dir.borrow_mut() = Some(cache.clone()));
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{id}.jsonl"));
        // A long conversation whose tail has no settings record searches up to 8 MiB.
        let filler = json!({"type":"event_msg","payload":{"type":"agent_message","message":"x".repeat(1000)}}).to_string() + "\n";
        fs::write(&path, json!({"type":"session_meta","payload":{"id":id}}).to_string() + "\n" + &filler.repeat(200)).unwrap();
        let record = json!({"agent":"codex","conversation_id":id,"transcript":path});
        assert!(history(&record).model.is_empty());
        let entries: Vec<_> = fs::read_dir(&cache).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let stored: Value = serde_json::from_str(&fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap()).unwrap();
        assert_eq!(stored["value"]["model"], "");
        scan_cache::DIR.with(|dir| *dir.borrow_mut() = None);
    }

    #[test]
    fn model_cache_is_parsed_once_per_file_version() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("models_cache.json");
        let models = |levels: &[&str]| json!({"models":[{"slug":"m","supported_reasoning_levels":
            levels.iter().map(|level| json!({"effort":level})).collect::<Vec<_>>()}]}).to_string();
        fs::write(&path, models(&["low"])).unwrap();
        let record = json!({"agent":"codex","agent_home":root.path()});
        let parses = || MODEL_CACHE_PARSES.with(|count| count.get());
        let before = parses();
        // Each listed session asks for its options and catalog: one parse serves them all.
        for _ in 0..5 {
            assert_eq!(options(&record, "m"), vec!["low"]);
            assert_eq!(catalog(&record)[0].efforts, vec!["low"]);
        }
        assert_eq!(parses() - before, 1);
        // A rewritten file is parsed again, even within the same second.
        fs::write(&path, models(&["low", "high"])).unwrap();
        assert_eq!(options(&record, "m"), vec!["low", "high"]);
        assert_eq!(catalog(&record)[0].efforts, vec!["low", "high"]);
        assert_eq!(parses() - before, 2);
        fs::remove_file(&path).unwrap();
        assert!(options(&record, "m").is_empty());
        assert!(catalog(&record).is_empty());
    }

    #[test]
    fn codex_catalog_uses_native_priority_with_stable_ties() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("models_cache.json");
        let source = json!({"models":[
            {"slug":"later","display_name":"Later model","priority":8,"supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}]},
            {"slug":"tie-zeta","priority":2},
            {"slug":"first","priority":i32::MIN},
            {"slug":"tie-alpha","priority":2},
            {"slug":"zero","priority":0},
            {"slug":"hidden","priority":i32::MIN,"visibility":"hide"},
            {"slug":"-invalid","priority":i32::MIN},
            {"slug":"last-ranked","priority":i32::MAX}
        ]}).to_string();
        fs::write(&path, &source).unwrap();
        let models = catalog(&json!({"agent":"codex","agent_home":root.path()}));
        assert_eq!(models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "zero", "tie-zeta", "tie-alpha", "later", "last-ranked"]);
        assert_eq!(models[4].label, "Later model");
        assert_eq!(models[4].efforts, vec!["low", "high"]);
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
    }

    #[test]
    fn codex_catalog_keeps_legacy_unranked_order_after_ranked_models() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("models_cache.json");
        let mut entries = vec![
            json!({"slug":"missing"}),
            json!({"slug":"string","priority":"1"}),
            json!({"slug":"fraction","priority":1.5}),
            json!({"slug":"null","priority":null}),
            json!({"slug":"too-large","priority":i64::from(i32::MAX) + 1}),
            json!({"slug":"too-small","priority":i64::from(i32::MIN) - 1}),
            json!({"slug":"boolean","priority":false}),
        ];
        let record = json!({"agent":"codex","agent_home":root.path()});
        fs::write(&path, json!({"models":entries}).to_string()).unwrap();
        let ids = |models: Vec<ModelOption>| models.into_iter().map(|model| model.id).collect::<Vec<_>>();
        let legacy = vec!["missing", "string", "fraction", "null", "too-large", "too-small", "boolean"];
        assert_eq!(ids(catalog(&record)), legacy);
        entries.insert(2, json!({"slug":"positive","priority":3}));
        entries.push(json!({"slug":"negative","priority":-1}));
        fs::write(&path, json!({"models":entries}).to_string()).unwrap();
        let expected = [vec!["negative", "positive"], legacy].concat();
        assert_eq!(ids(catalog(&record)), expected);
    }

    #[test]
    fn account_catalog_and_resume_override_are_scoped() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("models_cache.json"),json!({"models":[
            {"slug":"available","display_name":"Available model","supported_reasoning_levels":[{"effort":"low"},{"effort":"unknown-level"},{"effort":"ultra"}]},
            {"slug":"hidden","visibility":"hide","supported_reasoning_levels":[]}
        ]}).to_string()).unwrap();
        let mut record = json!({"agent":"codex","agent_home":root.path(),"resume_settings":{"model":"available","effort":"low"}});
        let models = catalog(&record);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "available");
        assert_eq!(models[0].efforts, vec!["low", "ultra"]);
        let args = vec![
            "codex",
            "resume",
            "conversation",
            "--model=old",
            "-c",
            "model_reasoning_effort=high",
            "--profile",
            "work",
            "-c",
            "sandbox_mode=read-only",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(
            resume_argv(&record, args),
            vec![
                "codex",
                "resume",
                "conversation",
                "--profile",
                "work",
                "-c",
                "sandbox_mode=read-only",
                "--model",
                "available",
                "-c",
                "model_reasoning_effort=\"low\""
            ]
        );
        record["archive_id"] = json!("archived");
        assert!(settings_reason(&record, &models).contains("Restore"));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn parses_only_native_footer_and_picker() {
        assert_eq!(
            footer(
                "codex",
                "gpt-6-astra",
                "\n  GPT-6-Astra xhigh · /work\n  ? for shortcuts"
            ),
            Some("xhigh".into())
        );
        let effort = |screen: &str, name: &str| kimi_effort(screen, &[name], false);
        assert_eq!(effort(" Ask When Needed  K3 thinking: max  /work\n context: 22%", "K3").as_deref(), Some("max"));
        assert!(effort("User said thinking: high", "K3").is_none());
        // Kimi 2.1 status rows under its composer, captured from the native TUI or
        // laid out by its footer slots (mode, goal, model, tasks, folder, git).
        let composer = |status: &str| {
            format!(" ● Fixture reply: hello thinking: max\n ╭────╮\n │ >  │\n ╰────╯\n {status}\n                    context: 1% (15/32k)")
        };
        for (status, name, expected) in [
            ("kimi-levels thinking: high  ~/sample-project  main [±]          ctrl+c: cancel", "kimi-levels", "high"),
            ("kimi-think thinking  ~/sample-project  main [±]", "kimi-think", "on"),
            ("kimi-think  ~/sample-project  main [±]", "kimi-think", "off"),
            ("Levels Pro thinking: max  /work", "Levels Pro", "max"),
            ("Never Ask  kimi-think thinking  ~/work", "kimi-think", "on"),
            ("plan  K3 thinking: max  ~/work", "K3", "max"),
            ("Ask When Needed plan  K3 thinking: max  ~/work", "K3", "max"),
            ("swarm  [goal ● paused · 7m · 3 turns]  K3 thinking: high  ~/work", "K3", "high"),
            ("~/work  K3 thinking: low", "K3", "low"),
            ("Ask When Needed  K3 thinking: high /fixture", "K3", "high"),
        ] {
            assert_eq!(effort(&composer(status), name).as_deref(), Some(expected), "{status}");
        }
        assert!(effort(&composer("K3 Pro thinking: max  ~/work"), "K3").is_none());
        assert!(effort(&composer("plan  K3 thinking: unknown  ~/work"), "K3").is_none());
        assert!(effort(&composer("ctrl+c again to exit"), "K3").is_none());
        assert!(effort(" ● Fixture reply: K3  ~/work", "K3").is_none());
        // A custom status line command need not show effort: never read "off".
        assert!(kimi_effort(&composer("K3  ~/work  main"), &["K3"], true).is_none());
        assert_eq!(kimi_effort(&composer("K3 thinking: max  ~/work"), &["K3"], true).as_deref(), Some("max"));
        let model = |id: &str, label: &str, efforts: &[&str]| ModelOption {
            id: id.into(),
            label: label.into(),
            efforts: efforts.iter().map(|v| v.to_string()).collect(),
            default_effort: String::new(),
        };
        let home = tempfile::tempdir().unwrap();
        let kimi = json!({"agent":"kimi","agent_home":home.path()});
        let plain = composer("kimi-plain  ~/sample-project  main [±]");
        assert_eq!(
            target_footer(&kimi, &model("fixture-plain", "kimi-plain", &["off"]), &plain),
            Some("off".into())
        );
        assert!(target_footer(&kimi, &model("fixture-think", "kimi-think", &["on", "off"]), &plain).is_none());
        // A status line command replaces the built-in line: a bare name is not "off".
        fs::write(home.path().join("tui.toml"), "[status_line]\ncommand = \"my-status\"\n").unwrap();
        assert!(target_footer(&kimi, &model("fixture-plain", "kimi-plain", &["off"]), &plain).is_none());
        fs::write(home.path().join("tui.toml"), "[status_line]\nitems = [\"cwd\", \"model\"]\n").unwrap();
        assert_eq!(
            target_footer(&kimi, &model("fixture-plain", "kimi-plain", &["off"]), &plain),
            Some("off".into())
        );
        // The status line shows the alias as overridden, like Kimi's effective model.
        fs::write(
            home.path().join("config.toml"),
            "[models.\"kimi-code/k3\"]\nmodel = \"k3\"\ndisplay_name = \"K3\"\nsupport_efforts = [\"low\", \"high\"]\n[models.\"kimi-code/k3\".overrides]\ndisplay_name = \"K3 Max\"\nsupport_efforts = [\"high\", \"max\"]\n",
        )
        .unwrap();
        let k3 = catalog(&kimi).into_iter().find(|m| m.id == "kimi-code/k3").unwrap();
        assert_eq!((k3.label.as_str(), k3.efforts.clone()), ("K3 Max", vec!["off".to_owned(), "high".into(), "max".into()]));
        assert_eq!(
            visible_effort(&kimi, "kimi-code/k3", &composer("K3 Max thinking: max  ~/work")).as_deref(),
            Some("max")
        );
        let panel = "────\n Select thinking effort\n ←→ switch · Enter select · Alt+S session-only · Esc cancel\n\n Low  [ High ]  Max\n────";
        assert_eq!(
            kimi_picker(panel),
            Some((vec!["low".into(), "high".into(), "max".into()], 1))
        );
        assert!(kimi_picker(&panel.replace("Alt+S session-only · ", "")).is_none());
        // Kimi 2.1 model picker below a conversation that reported an earlier change.
        let models = " Thinking set to max for this session only.\n ────\n  Select a model  (type to search)\n  Tab toggle provider · ↑↓ navigate · Enter select · Alt+S session-only · Es...\n   All   fixture\n     kimi-think   fixture\n   ❯ kimi-plain   fixture\n     kimi-levels  fixture ← current\n  Thinking\n     On (Unsupported)   [ Off ]\n ────";
        assert_eq!(kimi_model_row(models).as_deref(), Some("kimi-plain   fixture"));
        assert_eq!(kimi_thinking(models).as_deref(), Some("off"));
        assert_eq!(
            kimi_thinking(&models.replace("On (Unsupported)   [ Off ]", "[ On ]    Off")).as_deref(),
            Some("on")
        );
        assert_eq!(kimi_model_edges(models), (false, false));
        let top = models.replace("     kimi-think", "   ❯ kimi-think").replace("   ❯ kimi-plain", "     kimi-plain");
        assert_eq!(kimi_model_edges(&top), (true, false));
        let bottom = models.replace("   ❯ kimi-plain", "     kimi-plain").replace("     kimi-levels", "   ❯ kimi-levels");
        assert_eq!(kimi_model_edges(&bottom), (false, true));
        assert!(kimi_picker(&panel.replace("[ High ]", "[ Low ]")).is_none());
    }

    // Captured from Claude Code 2.1.296 at 120 columns, trailing spaces trimmed.
    const CLAUDE_PICKER: &str = "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔\n   Select model\n   Switch between Claude models. Your pick becomes the default for new sessions. For other/previous model names,\n   specify with --model.\n\n   ❯ 1. Default (recommended) ✔  Use the default model (currently Opus 5.5) · $4/$20 per Mtok\n     2. Opus                     Opus 5.5 · Best for everyday, complex tasks · $4/$20 per Mtok\n     3. Fable                    Fable 5.1 · Most capable for your hardest and longest-running tasks · $10/$50 per\n                                 Mtok\n     4. Sonnet                   Sonnet 5.5 · Efficient for routine tasks · $2/$10 per Mtok\n     5. Haiku                    Haiku 5.5 · Fastest for quick answers · $0.10/$0.50 per Mtok ($0.50/$2.50 for\n                                 prompts over 100k)\n\n   ◐ Medium effort (default) ←/→ to adjust\n\n   Enter to set as default · s to use this session only · Esc to cancel\n";
    const CLAUDE_SWITCH: &str = "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔\n   Switch model?\n   Your next response will be slower and use more tokens\n   This conversation is cached for the current model. Switching to Haiku 5.5 means the full history gets re-read on\n   your next message.\n   ❯ 1. Yes, switch to Haiku 5.5\n     2. No, go back\n";

    #[test]
    fn claude_picker_rows_effort_and_session_only_legend() {
        let picker = claude_picker(CLAUDE_PICKER).unwrap();
        assert!(picker.row.starts_with("1. Default (recommended) ✔"));
        assert_eq!(picker.effort.as_deref(), Some("medium"));
        assert!(picker.session_only);
        // The default row describes the default model; it is not that model's row.
        assert!(!claude_row_is(&picker.row, "Opus 5.5"));
        assert!(!claude_row_is("1. Default (recommended) ✔  Opus 5.5 · Best for everyday tasks", "Opus 5.5"));
        let opus = CLAUDE_PICKER.replace("   ❯ 1. Default", "     1. Default").replace("     2. Opus", "   ❯ 2. Opus");
        let picker = claude_picker(&opus).unwrap();
        assert!(claude_row_is(&picker.row, "Opus 5.5"));
        assert!(!claude_row_is(&picker.row, "Opus 5"));
        assert!(claude_row_is("5. Haiku ✔                Haiku 5.5 · Fastest for quick answers", "Haiku 5.5"));
        assert!(claude_row_is("3. Fable                    Fable 5.1 · Most capable for your hardest and", "Fable 5.1"));
        assert!(claude_row_is("6. Fable 5.2                Most capable for your hardest tasks", "Fable 5.2"));
        assert!(claude_row_is("6. Fable 5.2 ✔              Most capable for your hardest tasks", "Fable 5.2"));
        assert!(!claude_row_is("6. Fable 5.2                Most capable for your hardest tasks", "Fable 5"));
        for (shown, level) in [("○ Low effort", "low"), ("● High effort", "high"), ("◉ xHigh effort", "xhigh"), ("◈ Max effort", "max")] {
            let screen = CLAUDE_PICKER.replace("◐ Medium effort (default)", shown);
            assert_eq!(claude_picker(&screen).unwrap().effort.as_deref(), Some(level), "{shown}");
        }
        let none = CLAUDE_PICKER.replace("◐ Medium effort (default) ←/→ to adjust", "○ Effort not supported for Haiku 4.5");
        assert_eq!(claude_picker(&none).unwrap().effort.as_deref(), Some(""));
        // Older pickers without the session-only key, or an ambiguous selection, are never driven.
        assert!(!claude_picker(&CLAUDE_PICKER.replace(" · s to use this session only", "")).unwrap().session_only);
        assert!(claude_picker(&CLAUDE_PICKER.replace("     4. Sonnet", "   ❯ 4. Sonnet")).is_none());
        assert!(claude_picker(&CLAUDE_PICKER.replace("Select model", "Select a model")).is_none());
    }

    #[test]
    fn claude_picker_sections_have_only_their_advertised_navigation_action() {
        let section = CLAUDE_PICKER
            .replace("   ❯ 1. Default", "     1. Default")
            .replace("   ◐ Medium effort (default) ←/→ to adjust", "   ❯ ▸ More models")
            .replace("Enter to set as default · s to use this session only", "Enter to expand");
        let picker = claude_picker(&section).unwrap();
        assert_eq!(picker.section, Some(ClaudeSection::Collapsed));
        assert_eq!(picker.row, "▸ More models");
        assert!(!picker.session_only);
        assert!(!claude_row_is(&picker.row, "More models"));
        let expanded = section.replace('▸', "▾").replace("Enter to expand", "Enter to collapse");
        assert_eq!(claude_picker(&expanded).unwrap().section, Some(ClaudeSection::Expanded));
        assert!(claude_model_panel(&section).is_some());
        assert!(claude_model_panel(&expanded).is_some());
        // A section and a model cannot both be selected, and an unadvertised
        // Enter must never become the model picker's save-as-default action.
        assert!(claude_picker(&section.replace("     1. Default", "   ❯ 1. Default")).is_none());
        assert!(claude_picker(&section.replace("Enter to expand", "Enter to set as default")).is_none());
        assert!(claude_picker(&section.replace("   ❯ ▸ More models", "   ❯ ▸ More models\n   ❯ ▸ Another section")).is_none());
        assert!(claude_picker(&section.replace("   ❯ ▸ More models", "   ❯ ▸ More models\n     ▸ Another section")).is_some());
    }

    #[test]
    fn claude_picker_cleanup_recognizes_the_panel_without_a_model_selection() {
        let unknown = CLAUDE_PICKER.replace("   ❯ 1. Default", "     1. Default");
        assert!(claude_picker(&unknown).is_none());
        assert!(claude_model_panel(&unknown).is_some());
        assert!(claude_model_panel(&format!("{unknown}\nA different dialog")).is_none());
        assert!(claude_model_panel(&unknown.replace("Switch between Claude models.", "Quoted conversation text.")).is_none());
        assert!(claude_model_panel(CLAUDE_SWITCH).is_none());
    }

    #[test]
    fn claude_switch_dialog_distinguishes_its_notice_from_hooks() {
        assert_eq!(
            claude_switch_dialog(CLAUDE_SWITCH),
            Some(ClaudeSwitch::Cache { model: true, choice: "Haiku 5.5".into() })
        );
        let effort = CLAUDE_SWITCH.replace("Switch model?", "Change effort level?").replace("Haiku 5.5", "high");
        assert_eq!(claude_switch_dialog(&effort), Some(ClaudeSwitch::Cache { model: false, choice: "high".into() }));
        let hook = CLAUDE_SWITCH.replace("Your next response will be slower and use more tokens", "A PreModelSwitch hook asked you to confirm");
        assert_eq!(claude_switch_dialog(&hook), Some(ClaudeSwitch::Hook));
        let focus = CLAUDE_SWITCH.replace("   ❯ 1. Yes", "     1. Yes").replace("     2. No", "   ❯ 2. No");
        assert_eq!(claude_switch_dialog(&focus), Some(ClaudeSwitch::Unknown));
        assert!(claude_switch_dialog(CLAUDE_PICKER).is_none());
    }

    #[test]
    fn claude_effort_indicator_is_only_the_line_above_the_composer() {
        let rule = "─".repeat(40);
        let screen = |indicator: &str| format!("● Hi!\n{indicator}\n{rule}\n❯\u{a0}\n{rule}\n  ⏸ manual mode on");
        assert_eq!(claude_footer(&screen("                    ◐ medium · /effort")).as_deref(), Some("medium"));
        assert_eq!(claude_footer(&screen("                    ● high · ultracode · /effort")).as_deref(), Some("high"));
        assert_eq!(claude_footer(&screen("                    effort: xhigh · /effort")).as_deref(), Some("xhigh"));
        // Conversation text quoting the indicator, or an unknown level, is not the indicator.
        assert!(claude_footer(&screen("◐ medium · /effort")).is_none());
        assert!(claude_footer(&format!("                    ◐ medium · /effort\n\n{rule}\n❯\u{a0}")).is_none());
        assert!(claude_footer(&screen("                    ◐ turbo · /effort")).is_none());
        assert!(claude_footer(&screen("                    word medium · /effort")).is_none());
    }

    fn claude_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("cache/model-catalog");
        fs::create_dir_all(&dir).unwrap();
        let effort = |recommended: &str| {
            let options: Vec<_> = ["low", "medium", "high", "xhigh", "max"].iter()
                .map(|id| if *id == recommended { json!({"id":id,"badge":{"message":"Recommended"}}) } else { json!({"id":id}) }).collect();
            json!({"type":"effort","effort_options":options})
        };
        fs::write(dir.join("org-0123456789ab-cc.json"), json!({"version":2,"catalog":{"surface":"cc","config":{"models":[
            {"id":"claude-opus-5-5","name":"Opus 5.5","short_name":"Opus","section":"main","thinking":effort("medium")},
            {"id":"claude-haiku-5-5","name":"Haiku 5.5","short_name":"Haiku","section":"main","thinking":effort("medium")},
            {"id":"claude-haiku-4-5","name":"Haiku 4.5","short_name":"Haiku","section":"overflow","thinking":{"type":"none"}},
            {"id":"-flag","name":"Bad","section":"main"}
        ]},"state":{"thinking_by_model":[{"id":"claude-opus-5-5","thinking":{"type":"effort","effort":"high"}}]}}}).to_string()).unwrap();
        fs::write(dir.join("masked-ids.json"), "{}").unwrap();
        home
    }

    #[test]
    fn claude_catalog_offers_the_pickers_main_and_overflow_models() {
        let home = claude_home();
        let record = json!({"agent":"claude","agent_home":home.path()});
        let models = catalog(&record);
        assert_eq!(models.iter().map(|m| (m.id.as_str(), m.label.as_str())).collect::<Vec<_>>(),
            vec![("claude-opus-5-5", "Opus 5.5"), ("claude-haiku-5-5", "Haiku 5.5"), ("claude-haiku-4-5", "Haiku 4.5")]);
        assert_eq!(models[0].efforts, vec!["low", "medium", "high", "xhigh", "max"]);
        assert_eq!((models[0].default_effort.as_str(), models[1].default_effort.as_str()), ("high", "medium"));
        assert_eq!(options(&record, "claude-haiku-5-5").len(), 5);
        assert!(options(&record, "claude-haiku-4-5").is_empty());
        assert!(catalog(&json!({"agent":"claude","agent_home":home.path().join("missing")})).is_empty());
    }

    #[test]
    fn claude_history_reads_turns_and_session_only_receipts_of_this_conversation() {
        let home = claude_home();
        let id = "11111111-2222-4333-8444-555555555555";
        let path = home.path().join(format!("{id}.jsonl"));
        let row = |value: Value| value.to_string() + "\n";
        let turn = |model: &str, effort: &str, session: &str| row(json!({"type":"assistant","sessionId":session,"isSidechain":false,
            "requestedModel":model,"effort":effort,"message":{"model":model,"content":[]}}));
        let receipt = |text: &str, session: &str| row(json!({"type":"user","sessionId":session,"message":{"content":format!("<local-command-stdout>{text}</local-command-stdout>")}}));
        fs::write(&path, turn("claude-opus-5-5", "xhigh", id)).unwrap();
        let record = json!({"agent":"claude","agent_home":home.path(),"conversation_id":id,"transcript":path});
        let found = history(&record);
        assert_eq!((found.model.as_str(), found.effort.as_str()), ("claude-opus-5-5", "xhigh"));
        // A later /model receipt changes the model before the next turn is written.
        fs::write(&path, turn("claude-opus-5-5", "xhigh", id) + &receipt("Set model to `Haiku 5.5` for this session only with `high` effort", id)).unwrap();
        let found = history(&record);
        assert_eq!((found.model.as_str(), found.effort.as_str()), ("claude-haiku-5-5", "high"));
        // Subagents, synthetic replies, other conversations and unknown names never count.
        for extra in [
            turn("claude-sonnet-5-5", "low", "another-conversation"),
            row(json!({"type":"assistant","sessionId":id,"isSidechain":true,"requestedModel":"claude-sonnet-5-5","effort":"low"})),
            row(json!({"type":"assistant","sessionId":id,"message":{"model":"<synthetic>"}})),
            receipt("Set model to `Mystery 9` for this session only", id),
            receipt("Kept model as `Sonnet 5.5`", id),
        ] {
            fs::write(&path, turn("claude-opus-5-5", "xhigh", id) + &extra).unwrap();
            assert_eq!(history(&record).model, "claude-opus-5-5", "{extra}");
        }
        assert_eq!(claude_receipt("<local-command-stdout>Set model to `Sonnet 5.5` and saved as your default for new sessions</local-command-stdout>"),
            Some(("Sonnet 5.5".into(), String::new())));
        let other = json!({"agent":"claude","agent_home":home.path(),"conversation_id":"another","transcript":path});
        assert!(history(&other).model.is_empty());
        assert!(settings_reason(&other, &catalog(&other)).contains("confirm this conversation"));
    }

    #[test]
    fn claude_model_switch_names_the_exact_model_until_newer_history() {
        let home = claude_home();
        let id = "22222222-3333-4444-8555-666666666666";
        let path = home.path().join(format!("{id}.jsonl"));
        let turn = |model: &str| json!({"type":"assistant","sessionId":id,"isSidechain":false,
            "requestedModel":model,"effort":"high","message":{"model":model,"content":[]}}).to_string() + "\n";
        let switch = |model: Value| json!({"hook_event_name":"PostModelSwitch","session_id":id,
            "from_model":"claude-opus-5-5","to_model":model,"requested_model":null,"source":"auto"});
        fs::write(&path, turn("claude-opus-5-5")).unwrap();
        let mut record = json!({"agent":"claude","agent_home":home.path(),"conversation_id":id,"transcript":path,
            "model":"claude-opus-5-5","effort":"low"});
        // /model in Terminal names the exact ID before Claude's next turn; effort stays Claude's.
        observe_switch(&mut record, &switch(json!("claude-haiku-5-5")));
        assert_eq!(record["model"], "claude-haiku-5-5");
        assert_eq!(record["model_source_offset"], fs::metadata(&path).unwrap().len());
        let value = settings(&record, false);
        assert_eq!((value.model.as_str(), value.effort.as_str()), ("claude-haiku-5-5", "high"));
        // An automatic switch has no /model receipt, and its model need not be in the picker.
        observe_switch(&mut record, &switch(json!("claude-sonnet-5-5[1m]")));
        assert_eq!(settings(&record, false).model, "claude-sonnet-5-5[1m]");
        // A later turn is newer evidence.
        fs::write(&path, turn("claude-opus-5-5") + &turn("claude-fable-5-1")).unwrap();
        assert_eq!(settings(&record, false).model, "claude-fable-5-1");
        // Invalid models, and other agents, change nothing.
        let before = record.clone();
        for model in [json!(""), json!("-flag"), json!("x".repeat(121)), json!("a\nb"), json!(5), Value::Null] {
            observe_switch(&mut record, &switch(model));
            assert_eq!(record, before);
        }
        let mut codex = json!({"agent":"codex","model":"gpt-6-astra"});
        observe_switch(&mut codex, &switch(json!("claude-haiku-5-5")));
        assert_eq!(codex, json!({"agent":"codex","model":"gpt-6-astra"}));
        // Before the first message there is no transcript: every row written later is newer.
        let fresh = home.path().join("33333333-4444-4555-8666-777777777777.jsonl");
        let mut first = json!({"agent":"claude","agent_home":home.path(),
            "conversation_id":"33333333-4444-4555-8666-777777777777","transcript":fresh});
        observe_switch(&mut first, &switch(json!("claude-haiku-5-5")));
        assert_eq!((first["model_source_path"].as_str(), first["model_source_offset"].as_u64()), (fresh.to_str(), Some(0)));
        assert_eq!(settings(&first, false).model, "claude-haiku-5-5");
        // An unconfirmed transcript keeps the model, with no stale mark.
        let mut unconfirmed = json!({"agent":"claude","conversation_id":id,"transcript":"relative.jsonl",
            "model_source_path":"relative.jsonl","model_source_offset":5});
        observe_switch(&mut unconfirmed, &switch(json!("claude-haiku-5-5")));
        assert_eq!(unconfirmed, json!({"agent":"claude","conversation_id":id,"transcript":"relative.jsonl",
            "model":"claude-haiku-5-5"}));
    }

    #[test]
    fn claude_receipt_after_the_request_must_be_session_only() {
        let root = tempfile::tempdir().unwrap();
        let id = "conversation";
        let path = root.path().join(format!("{id}.jsonl"));
        let receipt = |text: &str| json!({"type":"user","sessionId":id,"message":{"content":format!("<local-command-stdout>{text}</local-command-stdout>")}}).to_string() + "\n";
        let old = receipt("Set model to `Haiku 5.5` for this session only with `high` effort");
        fs::write(&path, &old).unwrap();
        let offset = old.len() as u64;
        assert_eq!(claude_receipt_since(&path, id, offset, "Haiku 5.5").unwrap(), None);
        fs::write(&path, old.clone() + &receipt("Set model to `Haiku 5.5` and saved as your default for new sessions")).unwrap();
        assert_eq!(claude_receipt_since(&path, id, offset, "Haiku 5.5").unwrap(), None);
        fs::write(&path, old.clone() + &receipt("Set model to `Haiku 5.5` for this session only")).unwrap();
        assert_eq!(claude_receipt_since(&path, id, offset, "Haiku 5.5").unwrap(), Some(String::new()));
        assert_eq!(claude_receipt_since(&path, "other", offset, "Haiku 5.5").unwrap(), None);
        assert_eq!(claude_receipt_since(&path, id, offset, "Opus 5.5").unwrap(), None);
    }

    #[test]
    fn claude_resume_replays_only_a_zerus_choice() {
        let args: Vec<String> = ["claude", "--resume", "conversation", "--effort", "low", "--model=old", "--name", "x"]
            .into_iter().map(str::to_owned).collect();
        let record = json!({"agent":"claude","conversation_id":"conversation"});
        assert_eq!(resume_argv(&record, args.clone()), args);
        let record = json!({"agent":"claude","conversation_id":"conversation","resume_settings":{"model":"claude-haiku-5-5","effort":"high"}});
        assert_eq!(resume_argv(&record, args), ["claude", "--resume", "conversation", "--name", "x", "--model", "claude-haiku-5-5", "--effort", "high"]);
    }
}
