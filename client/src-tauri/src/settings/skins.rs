//! Device-local, bounded data-only skin files. The webview never supplies a path.
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};
const MAX_FILE: usize = 256 * 1024;
const MAX_TOTAL: usize = 1024 * 1024;
const MAX_FILES: usize = 32;
static WRITER: Mutex<()> = Mutex::new(());
const BUILTINS: [&str; 4] = [
    include_str!("../../../src/skin/skins/zerus-dark.json"),
    include_str!("../../../src/skin/skins/zerus-light.json"),
    include_str!("../../../src/skin/skins/fjord.json"),
    include_str!("../../../src/skin/skins/paper.json"),
];
#[derive(Default, Serialize)]
pub struct Loaded {
    files: Vec<Value>,
    errors: Vec<String>,
}
fn object(v: &Value) -> Result<&serde_json::Map<String, Value>, String> {
    v.as_object()
        .ok_or_else(|| "A skin field must be an object.".into())
}
fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn number(v: &Value, lo: f64, hi: f64) -> bool {
    v.as_f64()
        .is_some_and(|n| n.is_finite() && (lo..=hi).contains(&n))
}
fn color(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let s = lower.as_str();
    if let Some(hex) = s.strip_prefix('#') {
        return [3, 4, 6, 8].contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if s == "transparent" {
        return true;
    }
    let rgba = s.starts_with("rgba(");
    let Some(inner) = s
        .strip_prefix(if rgba { "rgba(" } else { "rgb(" })
        .and_then(|s| s.strip_suffix(')'))
    else {
        return false;
    };
    let parts: Vec<_> = inner.split(',').map(str::trim).collect();
    parts.len() == if rgba { 4 } else { 3 }
        && parts.iter().enumerate().all(|(i, s)| {
            !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && s.parse::<f64>().is_ok_and(|n| {
                    n.is_finite() && (0.0..=if i == 3 { 1.0 } else { 255.0 }).contains(&n)
                })
        })
}
fn shadow(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let s = lower.as_str();
    if s.len() > 160 {
        return false;
    }
    let parts: Vec<_> = s.split("),").collect();
    parts.iter().enumerate().all(|(i, part)| {
        let part = if i + 1 < parts.len() {
            format!("{part})")
        } else {
            part.to_string()
        };
        let Some(at) = part.find("rgb") else {
            return false;
        };
        let lengths = part[..at]
            .trim()
            .strip_prefix("inset ")
            .unwrap_or(part[..at].trim());
        let lengths: Vec<_> = lengths.split_whitespace().collect();
        (2..=4).contains(&lengths.len())
            && lengths.iter().all(|s| {
                s.strip_suffix("px")
                    .unwrap_or(s)
                    .parse::<f64>()
                    .is_ok_and(f64::is_finite)
            })
            && color(part[at..].trim())
    })
}
fn easing(s: &str) -> bool {
    if ["linear", "ease", "ease-in", "ease-out", "ease-in-out"].contains(&s) {
        return true;
    }
    let Some(inner) = s
        .strip_prefix("cubic-bezier(")
        .and_then(|s| s.strip_suffix(')'))
    else {
        return false;
    };
    let p: Vec<_> = inner.split(',').collect();
    p.len() == 4
        && p.iter().enumerate().all(|(i, s)| {
            s.trim()
                .parse::<f64>()
                .is_ok_and(|v| v.is_finite() && (i % 2 == 1 || (0.0..=1.0).contains(&v)))
        })
}
fn builtin() -> BTreeMap<String, Value> {
    BUILTINS
        .iter()
        .map(|s| {
            let v: Value = serde_json::from_str(s).expect("bundled skin JSON");
            (v["id"].as_str().unwrap().into(), v)
        })
        .collect()
}
fn validate(v: &Value) -> Result<(), String> {
    let f = object(v)?;
    for key in f.keys() {
        if ![
            "schema",
            "id",
            "name",
            "appearance",
            "extends",
            "tokens",
            "components",
            "layout",
        ]
        .contains(&key.as_str())
        {
            return Err(format!(
                "Unknown skin field: {key}. CSS and code are not allowed."
            ));
        }
    }
    let skin_id = v["id"]
        .as_str()
        .filter(|s| id(s))
        .ok_or("Invalid skin id.")?;
    let all = builtin();
    if all.contains_key(skin_id) {
        return Err("A built-in skin already uses this id.".into());
    }
    if v["schema"] != 1 || !matches!(v["appearance"].as_str(), Some("dark" | "light")) {
        return Err("Unsupported skin schema or appearance.".into());
    }
    let name = v["name"].as_str().ok_or("Invalid skin name.")?;
    if name.trim().is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        return Err("name: 1 to 40 characters.".into());
    }
    if f.get("extends")
        .is_some_and(|v| !v.as_str().is_some_and(id))
    {
        return Err("Invalid parent skin id.".into());
    }
    let base = &all["zerus-dark"]["tokens"];
    if let Some(tokens) = f.get("tokens") {
        for (group, values) in object(tokens)? {
            if base.get(group).is_none() {
                return Err(format!("Unknown token group {group}."));
            }
            for (key, value) in object(values)? {
                if base[group].get(key).is_none() {
                    return Err(format!("Unknown token {group}.{key}."));
                }
                let s = value.as_str().unwrap_or("");
                let reference = s
                    .strip_prefix('{')
                    .and_then(|s| s.strip_suffix('}'))
                    .and_then(|s| s.split_once('.'))
                    .is_some_and(|(g, k)| {
                        matches!(g, "color" | "agent" | "list") && base[g].get(k).is_some()
                    });
                let valid = match group.as_str() {
                    "color" | "agent" | "list" => color(s) || reference,
                    "entity" if key == "palette" => value.as_array().is_some_and(|a| {
                        !a.is_empty()
                            && a.len() <= 16
                            && a.iter().all(|v| v.as_str().is_some_and(color))
                    }),
                    "entity" => number(value, 0.0, 100.0),
                    "font" if key == "base" => number(value, 0.0, 32.0),
                    "font" if key == "line" => number(value, 1.0, 2.0),
                    "font" => value.as_array().is_some_and(|a| {
                        !a.is_empty()
                            && a.len() <= 8
                            && a.iter().all(|v| {
                                v.as_str().is_some_and(|s| {
                                    !s.is_empty()
                                        && s.len() <= 48
                                        && s.bytes().all(|b| {
                                            b.is_ascii_alphanumeric() || b == b' ' || b == b'-'
                                        })
                                })
                            })
                    }),
                    "radius" => number(value, 0.0, 32.0),
                    "density" => number(value, 0.8, 1.3),
                    "motion" if key == "ease" => easing(s),
                    "motion" => number(value, 0.0, 5000.0),
                    "shadow" => shadow(s),
                    _ => false,
                };
                if !valid {
                    return Err(format!("Invalid token {group}.{key}."));
                }
            }
        }
    }
    for (field, choices) in [
        (
            "components",
            json!({"status-badge":["pill","dot"],"agent-mark":["chip","glyph"],"tree-lines":["lines","indent"],"attention-edge":["bar","tint"]}),
        ),
        (
            "layout",
            json!({"rail":["icons","labeled"],"sessions-pane":["left","right"],"inspector":["docked","collapsed"],"activity-column":["centered","full"]}),
        ),
    ] {
        if let Some(values) = f.get(field) {
            for (k, v) in object(values)? {
                if !choices[k].as_array().is_some_and(|a| a.contains(v)) {
                    return Err(format!("Invalid {field}.{k}."));
                }
            }
        }
    }
    Ok(())
}
fn library(files: &[Value]) -> Result<(), String> {
    let mut all = builtin();
    for file in files {
        validate(file)?;
        all.insert(file["id"].as_str().unwrap().into(), file.clone());
    }
    for file in files {
        let mut seen = BTreeSet::new();
        let mut current = file;
        let mut chain = Vec::new();
        loop {
            let name = current["id"].as_str().ok_or("Invalid parent skin.")?;
            if !seen.insert(name) {
                return Err("Skin inheritance cycle.".into());
            }
            chain.push(current);
            let parent = current
                .get("extends")
                .and_then(Value::as_str)
                .or_else(|| (name != "zerus-dark").then_some("zerus-dark"));
            let Some(parent) = parent else { break };
            current = all.get(parent).ok_or("Unknown parent skin.")?;
        }
        let mut tokens = BTreeMap::new();
        for ancestor in chain.into_iter().rev() {
            if let Some(groups) = ancestor["tokens"].as_object() {
                for (group, values) in groups {
                    if let Some(values) = values.as_object() {
                        for (key, value) in values {
                            tokens.insert(format!("{group}.{key}"), value);
                        }
                    }
                }
            }
        }
        for key in tokens.keys() {
            let mut current = key.as_str();
            let mut seen = BTreeSet::new();
            loop {
                if !seen.insert(current) {
                    return Err("Skin token reference cycle.".into());
                }
                let value = tokens.get(current).ok_or("Unknown skin token reference.")?;
                let next = value
                    .as_str()
                    .and_then(|s| s.strip_prefix('{'))
                    .and_then(|s| s.strip_suffix('}'));
                match next {
                    Some(next) => current = next,
                    None => break,
                }
            }
        }
    }
    Ok(())
}
fn read(dir: &Path) -> Result<Loaded, String> {
    if !dir.exists() {
        return Ok(Loaded::default());
    }
    if fs::symlink_metadata(dir)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("The skins folder must not be a symbolic link.".into());
    }
    let mut result = Loaded::default();
    let mut total = 0;
    for (index, entry) in fs::read_dir(dir).map_err(|e| e.to_string())?.enumerate() {
        if index >= MAX_FILES {
            return Err("The skins folder contains too many files (maximum 32).".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_none_or(|s| s != "json") {
            continue;
        }
        let item: Result<Value, String> = (|| {
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                return Err("Skin files must be regular files.".into());
            }
            let mut bytes = Vec::new();
            fs::File::open(&path)
                .map_err(|e| e.to_string())?
                .take((MAX_FILE + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            total += bytes.len();
            if bytes.len() > MAX_FILE || total > MAX_TOTAL {
                return Err("Skin files exceed the size limit.".into());
            }
            let v: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            validate(&v)?;
            if path.file_stem().and_then(|s| s.to_str()) != v["id"].as_str() {
                return Err("Skin filename must match its id.".into());
            }
            Ok(v)
        })();
        match item {
            Ok(value) => result.files.push(value),
            Err(e) => result
                .errors
                .push(format!("{}: {e}", entry.file_name().to_string_lossy())),
        }
    }
    Ok(result)
}
fn save(dir: &Path, text: &str) -> Result<Loaded, String> {
    if text.len() > MAX_FILE {
        return Err("A skin file must be at most 256 KiB.".into());
    }
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    validate(&v)?;
    let mut loaded = read(dir)?;
    loaded.files.retain(|f| f["id"] != v["id"]);
    loaded.files.push(v.clone());
    if loaded.files.len() > MAX_FILES {
        return Err("At most 32 user skins are supported.".into());
    }
    library(&loaded.files)?;
    let bytes = serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?;
    let target = dir.join(format!("{}.json", v["id"].as_str().unwrap()));
    let entries: Vec<_> = if dir.exists() {
        fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .take(MAX_FILES + 1)
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    if !target.exists() && entries.len() >= MAX_FILES {
        return Err("At most 32 user skins are supported.".into());
    }
    let mut total = bytes.len() as u64;
    for entry in entries {
        if entry.path() != target && entry.path().extension().is_some_and(|s| s == "json") {
            total = total.saturating_add(entry.metadata().map_err(|e| e.to_string())?.len());
        }
    }
    if bytes.len() > MAX_FILE || total > MAX_TOTAL as u64 {
        return Err("Skin files exceed the size limit.".into());
    }
    zerus_client_core::fsx::write_private_atomic(
        &dir.join(format!("{}.json", v["id"].as_str().unwrap())),
        &bytes,
        true,
    )
    .map_err(|e| e.to_string())?;
    Ok(loaded)
}
fn folder() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("HGS_CONFIG_DIR") {
        let p = PathBuf::from(path);
        if !p.is_absolute() {
            return Err("HGS_CONFIG_DIR must be absolute.".into());
        }
        return Ok(p.join("skins"));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("hgs"))
    } else {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config/hgs"))
    };
    base.filter(|p| p.is_absolute())
        .map(|p| p.join("skins"))
        .ok_or_else(|| "Could not locate the skins folder.".into())
}
#[tauri::command]
pub async fn user_skins_load() -> Result<Loaded, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let _guard = WRITER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read(&folder()?)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn user_skin_import(text: String) -> Result<Loaded, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = WRITER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        save(&folder()?, &text)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn user_skins_open() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = folder()?;
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("The skins folder must not be a symbolic link.".into());
        }
        fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        super::open_url(path.to_str().ok_or("Invalid skins folder path.")?)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Value {
        json!({"schema":1,"id":"ember","name":"Ember","appearance":"dark","extends":"zerus-dark","tokens":{"color":{"accent":"#f0a35e"}}})
    }
    #[test]
    fn rejects_code_bad_tokens_and_inheritance() {
        for value in [
            json!({"css":"body{}"}),
            json!({"tokens":{"color":{"accent":"url(https://example.invalid)"}}}),
            json!({"tokens":{"radius":{"card":100}}}),
            json!({"components":{"status-badge":"new"}}),
        ] {
            let mut v = sample();
            for (k, vv) in value.as_object().unwrap() {
                v[k] = vv.clone();
            }
            assert!(validate(&v).is_err());
        }
        let mut v = sample();
        v["extends"] = json!("ember");
        assert!(library(&[v]).is_err());
        let mut cycle = sample();
        cycle["tokens"]["color"]["accent"] = json!("{color.accent}");
        assert!(library(&[cycle]).is_err());
    }
    #[test]
    fn saves_atomically_and_invalid_import_preserves_previous() {
        let dir = std::env::temp_dir().join(format!("zerus-skins-{}", uuid::Uuid::new_v4()));
        let good = sample().to_string();
        save(&dir, &good).unwrap();
        let before = fs::read(dir.join("ember.json")).unwrap();
        assert!(save(&dir, "{}").is_err());
        assert_eq!(fs::read(dir.join("ember.json")).unwrap(), before);
        assert_eq!(read(&dir).unwrap().files.len(), 1);
        assert!(save(&dir, &" ".repeat(MAX_FILE + 1)).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn skips_symlink_files_and_refuses_a_symlink_folder() {
        let root = std::env::temp_dir().join(format!("zerus-skins-links-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("skins")).unwrap();
        fs::write(root.join("outside.json"), sample().to_string()).unwrap();
        std::os::unix::fs::symlink(root.join("outside.json"), root.join("skins/ember.json"))
            .unwrap();
        let loaded = read(&root.join("skins")).unwrap();
        assert!(loaded.files.is_empty());
        assert_eq!(loaded.errors.len(), 1);
        std::os::unix::fs::symlink(root.join("skins"), root.join("alias")).unwrap();
        assert!(read(&root.join("alias")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn refuses_links_and_unknown_tokens() {
        let mut v = sample();
        v["id"] = json!("../outside");
        assert!(validate(&v).is_err());
        let mut v = sample();
        v["tokens"]["color"]["not-a-token"] = json!("#fff");
        assert!(validate(&v).is_err());
        assert!(shadow("0 12px 40px rgba(0,0,0,.45)"));
        assert!(easing("cubic-bezier(.2,.8,.2,1)"));
    }
}
