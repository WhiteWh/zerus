//! Read-only native identity/quota adapters. Only allowlisted display fields leave this module.
use super::*;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_OUTPUT: u64 = 1024 * 1024;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn text(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect()
}
fn read_json(path: &Path) -> Value {
    fs::File::open(path)
        .ok()
        .and_then(|f| serde_json::from_reader(std::io::BufReader::new(f.take(MAX_OUTPUT))).ok())
        .unwrap_or(Value::Null)
}
fn stop(child: &mut Child) {
    platform::kill_process_group(child);
    let _ = child.wait();
}
pub(super) fn capture(
    command: Command,
    input: &[u8],
    seconds: u64,
) -> std::result::Result<Vec<u8>, String> {
    capture_with_exit(command, input, seconds, None)
}
pub(super) fn capture_with_exit(
    mut command: Command,
    input: &[u8],
    seconds: u64,
    accepted_exit: Option<i32>,
) -> std::result::Result<Vec<u8>, String> {
    let mut child = platform::spawn_group(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|_| "Native account command is unavailable.".to_owned())?;
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(MAX_OUTPUT + 1).read_to_end(&mut bytes);
        bytes
    });
    if child.stdin.take().unwrap().write_all(input).is_err() {
        stop(&mut child);
        let _ = reader.join();
        return Err("Native account request failed.".into());
    }
    let start = Instant::now();
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break status.success()
                    || accepted_exit.is_some_and(|code| status.code() == Some(code))
            }
            Err(_) => break false,
            _ if start.elapsed() >= Duration::from_secs(seconds) => break false,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    stop(&mut child);
    let bytes = reader.join().unwrap_or_default();
    if !success || bytes.len() > MAX_OUTPUT as usize {
        return Err("Native account request failed or timed out.".into());
    }
    Ok(bytes)
}
pub(super) fn native_command(provider: &str, home: &Path, native: bool) -> Command {
    if provider == "claude" { let _ = crate::macos_session::join(); }
    // As a session starts it (spec D8): on Windows found with PATHEXT, and an npm
    // command shim (`codex.cmd`) runs what it would run, never through cmd.exe.
    let mut cmd = crate::platform::agent_command(&[provider.to_owned()])
        .unwrap_or_else(|_| Command::new(provider));
    if let Ok(key) = home_var(provider) {
        // Preserve native Claude's keychain service name when its directory was implicit.
        if !native || env::var(key).is_ok_and(|v| !v.is_empty()) {
            cmd.env(key, home);
        } else {
            cmd.env_remove(key);
        }
    }
    if !native {
        for key in AUTH_ENV {
            cmd.env_remove(key);
        }
        if provider == "claude" {
            cmd.env("ANTHROPIC_CONFIG_DIR", home.join("anthropic"));
        }
    }
    cmd
}
fn http_json(url: &str, token: &str, extra: &[&str]) -> std::result::Result<Value, String> {
    if token.is_empty() || token.chars().any(char::is_control) {
        return Err("Sign in with the native agent to read usage.".into());
    }
    let mut config = format!(
        "url = {}\nheader = {}\nheader = \"Accept: application/json\"\n",
        json!(url),
        json!(format!("Authorization: Bearer {token}"))
    );
    for header in extra {
        config.push_str(&format!("header = {}\n", json!(header)));
    }
    let mut cmd = Command::new("curl");
    // WhiteWh fork: the status code follows the body, so a refused token
    // (UNAUTHORIZED) is told apart from an unreachable service.
    cmd.args([
        "--silent",
        "--proto",
        "=https",
        "--connect-timeout",
        "3",
        "--max-time",
        "6",
        "--max-filesize",
        "1048576",
        "--write-out",
        "\n%{http_code}",
        "--config",
        "-",
    ]);
    let bytes = capture(cmd, config.as_bytes(), 7)?;
    let (body, code) = bytes
        .rsplit(|b| *b == b'\n')
        .next()
        .map(|code| (&bytes[..bytes.len() - code.len()], code))
        .ok_or("Invalid native account response.")?;
    match code {
        b"200" => {
            serde_json::from_slice(body).map_err(|_| "Invalid native account response.".into())
        }
        b"401" => Err(UNAUTHORIZED.into()),
        _ => Err("Native account request failed or timed out.".into()),
    }
}
/// `http_json`'s error for a token the service refused.
const UNAUTHORIZED: &str = "The native sign-in was refused.";
fn window(
    id: &str,
    label: &str,
    used: Option<f64>,
    minutes: Option<u64>,
    reset: Value,
) -> Option<Value> {
    let used = used.filter(|n| n.is_finite() && *n >= 0.)?;
    Some(
        json!({"id":id,"label":label,"used_percent":used,"window_minutes":minutes,"resets_at":reset}),
    )
}
fn codex(home: &Path, native: bool) -> Value {
    let mut result =
        json!({"identity":{},"windows":[],"source":"Codex App Server","status":"unavailable"});
    let mut cmd = native_command("codex", home, native);
    cmd.args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = platform::spawn_group(&mut cmd) else {
        return result;
    };
    let (tx, rx) = mpsc::channel();
    let out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(out.take(MAX_OUTPUT))
            .lines()
            .map_while(|x| x.ok())
        {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    let mut stdin = child.stdin.take().unwrap();
    let send = |stdin: &mut std::process::ChildStdin, v: Value| writeln!(stdin, "{v}");
    let _ = send(
        &mut stdin,
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"hgs_zerus","title":"HGS Zerus","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}}),
    );
    let start = Instant::now();
    let mut received = 0;
    while start.elapsed() < Duration::from_secs(12) && received < 2 {
        let Ok(v) = rx.recv_timeout(Duration::from_millis(200)) else {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            continue;
        };
        match v["id"].as_u64() {
            Some(1) if v.get("result").is_some() => {
                let _ = send(&mut stdin, json!({"method":"initialized"}));
                let _ = send(
                    &mut stdin,
                    json!({"id":2,"method":"account/read","params":{"refreshToken":false}}),
                );
                let _ = send(
                    &mut stdin,
                    json!({"id":3,"method":"account/rateLimits/read"}),
                );
            }
            Some(2) => {
                received += 1;
                let a = &v["result"]["account"];
                if a.is_object() {
                    result["identity"] = json!({"email":text(a,"email"),"plan":text(a,"planType"),"auth_method":text(a,"type")});
                    if text(a, "type") == "chatgpt" && !text(a, "email").is_empty() {
                        let auth = read_json(&home.join("auth.json"));
                        result["identity"]["account_id"] =
                            json!(text(&auth["tokens"], "account_id"));
                    }
                } else if v.get("result").is_some() {
                    result["status"] = json!("signed_out");
                }
            }
            Some(3) => {
                received += 1;
                let rates = &v["result"]["rateLimits"];
                let mut windows = Vec::new();
                let buckets = v["result"]["rateLimitsByLimitId"]
                    .as_object()
                    .filter(|m| !m.is_empty());
                let values: Vec<&Value> = buckets
                    .map(|m| m.values().collect())
                    .unwrap_or_else(|| vec![rates]);
                for bucket in values {
                    let label = text(bucket, "limitName");
                    let id = text(bucket, "limitId");
                    for part in ["primary", "secondary"] {
                        let w = &bucket[part];
                        if let Some(w) = window(
                            &format!("{id}/{part}"),
                            &label,
                            w["usedPercent"].as_f64(),
                            w["windowDurationMins"].as_u64(),
                            w["resetsAt"].clone(),
                        ) {
                            windows.push(w);
                        }
                    }
                }
                if !windows.is_empty() {
                    result["windows"] = json!(windows);
                    result["status"] = json!("ok");
                }
                if rates["credits"].is_object() {
                    result["credits"] = json!({"balance":text(&rates["credits"],"balance"),"unlimited":rates["credits"]["unlimited"]==true});
                }
            }
            _ => {}
        }
    }
    stop(&mut child);
    drop(stdin);
    let _ = reader.join();
    result
}
fn claude(home: &Path, native: bool) -> Value {
    // WhiteWh fork: a local model has no Anthropic sign-in or plan limits.
    if let Some(local) = super::local_model::read(home).filter(|_| !native) {
        return json!({"identity":{"auth_method":"Ollama","plan":local.model},"windows":[],"source":"Ollama",
            "status":"configured","auth_status":"configured","endpoint":local.endpoint,
            "reachable":super::local_model::reachable(&local.endpoint)});
    }
    let mut result =
        json!({"identity":{},"windows":[],"source":"Claude Code","status":"unavailable"});
    let mut cmd = native_command("claude", home, native);
    cmd.args(["auth", "status", "--json"]);
    // Claude returns valid signed-out JSON with exit code 1.
    if let Ok(bytes) = capture_with_exit(cmd, &[], 5, Some(1)) {
        if let Ok(a) = serde_json::from_slice::<Value>(&bytes) {
            result["identity"] = json!({"email":text(&a,"email"),"plan":text(&a,"subscriptionType"),"organization":text(&a,"orgName"),"auth_method":text(&a,"authMethod")});
            if a["loggedIn"] == false {
                result["status"] = json!(claude::missing_login_status(home, native));
                result["auth_status"] = result["status"].clone();
                if result["status"] != "signed_out" {
                    result["identity"] = claude::saved_identity(home, native);
                    result["identity_cached"] = json!(true);
                }
                return result;
            }
            if a["loggedIn"] == true { result["auth_status"] = json!("signed_in"); }
        }
    }
    // Read only the selected profile's native credential service.
    let mut credentials = read_json(&home.join(".credentials.json"));
    if credentials.is_null()
        && cfg!(target_os = "macos")
    {
        let mut cmd = Command::new("security");
        cmd.args([
            "find-generic-password",
            "-s",
            &claude::keychain_service(home, native),
            "-w",
        ]);
        if let Ok(bytes) = capture(cmd, &[], 3) {
            credentials = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        }
    }
    let auth = &credentials["claudeAiOauth"];
    if text(&result["identity"], "plan").is_empty() {
        result["identity"]["plan"] = json!(text(auth, "subscriptionType"));
    }
    if auth["expiresAt"]
        .as_u64()
        .is_some_and(|expiry| expiry / 1000 <= now())
    {
        // This is the short-lived usage access token, not the native account's
        // sign-in. Claude owns refreshing its grant; never contradict its auth
        // status or refresh credentials just to display a quota report.
        // WhiteWh fork: but say so, as for Kimi: opening Claude Code renews the
        // grant, or a revoked one needs a new sign-in. "limits n/a" said neither.
        result["status"] = json!("expired");
        return result;
    }
    // Native OAuth usage endpoint used by /usage; never sends a message or refreshes a grant.
    let usage = http_json(
        "https://api.anthropic.com/api/oauth/usage",
        auth["accessToken"].as_str().unwrap_or(""),
        &["anthropic-beta: oauth-2025-04-20"],
    );
    if usage.as_ref().is_err_and(|e| e == UNAUTHORIZED) {
        result["status"] = json!("expired");
    }
    if let Ok(usage) = usage {
        let mut windows = Vec::new();
        for (key, label, minutes) in [
            ("five_hour", "", 300),
            ("seven_day", "", 10080),
            ("seven_day_sonnet", "Sonnet", 10080),
            ("seven_day_opus", "Opus", 10080),
        ] {
            let w = &usage[key];
            if let Some(w) = window(
                key,
                label,
                w["utilization"].as_f64(),
                Some(minutes),
                w["resets_at"].clone(),
            ) {
                windows.push(w);
            }
        }
        if !windows.is_empty() {
            result["windows"] = json!(windows);
            result["status"] = json!("ok");
        }
    }
    result
}
struct KimiEnvironment {
    credentials: String,
    base_url: String,
}
fn kimi_environment(home: &Path) -> KimiEnvironment {
    let mut environment = KimiEnvironment {
        credentials: "credentials/kimi-code.json".into(),
        base_url: "https://api.kimi.com/coding/v1".into(),
    };
    let document = fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok());
    let provider = document
        .as_ref()
        .and_then(|document| document.get("providers"))
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|providers| providers.get("managed:kimi-code"))
        .and_then(toml_edit::Item::as_table_like);
    if let Some(provider) = provider {
        if let Some(name) = provider
            .get("oauth")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|oauth| oauth.get("key"))
            .and_then(toml_edit::Item::as_str)
            .and_then(|key| key.strip_prefix("oauth/"))
            .map(|name| format!("{name}.json"))
            .filter(|name| valid_credential_name(name))
        {
            environment.credentials = format!("credentials/{name}");
        }
        if let Some(base_url) = provider.get("base_url").and_then(toml_edit::Item::as_str) {
            environment.base_url = base_url.trim_end_matches('/').into();
        }
    }
    if let Ok(base_url) = env::var("KIMI_CODE_BASE_URL") {
        environment.base_url = base_url.trim_end_matches('/').into();
    }
    environment
}
// A successful authenticated identity answer is the only proof of sign-in: a
// plan may report no quota at all, so a quota answer only ever decides the quota
// status. Returns the allowlisted identity and whether the service named a user.
fn kimi_identity(value: &Value) -> (Value, bool) {
    let user = value
        .get("user")
        .or_else(|| value.get("data"))
        .unwrap_or(value);
    let identity = json!({
        "name": text(user, "nickname"),
        "email": text(user, "email"),
        "plan": text(user, "user_level_name"),
        "auth_method": "Kimi Code",
    });
    // Only a stable account field proves the token belongs to a user; a plan or
    // a nickname alone also appears in anonymous and half-filled answers.
    let known = ["email", "user_id", "global_id"]
        .into_iter()
        .any(|key| !text(user, key).is_empty());
    (identity, known)
}

// Only the windows the service reports; a plan without quota answers `{}`, or
// leaves `usages` out, which is not an error.
fn kimi_windows(usage: &Value) -> Vec<Value> {
    let usages = &usage["usages"];
    let mut windows = Vec::new();
    for (key, label, minutes) in [
        ("limit_5h", "", Some(300)),
        ("limit_7d", "", Some(10080)),
        ("limit_month_total", "Monthly", None),
        ("limit_month_code", "Monthly code", None),
    ] {
        let w = &usages[key];
        if let Some(w) = window(
            key,
            label,
            w["used_ratio"].as_f64().map(|x| x * 100.),
            minutes,
            w["reset_time"].clone(),
        ) {
            windows.push(w);
        }
    }
    windows
}

fn kimi_report(me: Option<&Value>, usage: Option<&Value>) -> Value {
    let mut result =
        json!({"identity":{},"windows":[],"source":"Kimi Code","status":"unavailable"});
    if let Some(me) = me {
        let (identity, verified) = kimi_identity(me);
        if verified {
            result["auth_status"] = json!("signed_in");
        }
        result["identity"] = identity;
    }
    if let Some(usage) = usage {
        let windows = kimi_windows(usage);
        if !windows.is_empty() {
            result["windows"] = json!(windows);
            result["status"] = json!("ok");
        }
    }
    result
}

fn kimi(home: &Path) -> Value {
    let mut result =
        json!({"identity":{},"windows":[],"source":"Kimi Code","status":"unavailable"});
    let environment = kimi_environment(home);
    let credentials = home.join(&environment.credentials);
    let auth = read_json(&credentials);
    let token = auth["access_token"].as_str().unwrap_or("");
    if token.is_empty() {
        result["status"] = json!("signed_out");
        return result;
    }
    if auth["expires_at"]
        .as_u64()
        .is_some_and(|expiry| expiry <= now() + 60)
    {
        // Let the native server refresh/persist its own OAuth grant under its own lock.
        let mut cmd = Command::new("node");
        cmd.args(["--input-type=module", "-e", include_str!("kimi-read.mjs")])
            .env("KIMI_CODE_HOME", home);
        for key in AUTH_ENV {
            cmd.env_remove(key);
        }
        let work = tempfile::tempdir().ok();
        if let Some(dir) = &work {
            cmd.current_dir(dir.path());
        }
        let mut output: Value = capture(cmd, &[], 18)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(result);
        // Without renewal the grant stays expired: opening Kimi or signing in again
        // renews it, which "unavailable" would not say. A sign-in the native server
        // just verified outranks that stale on-disk expiry, though.
        let renewed = read_json(&credentials);
        if output["auth_status"] != "signed_in"
            && output["status"] != "ok"
            && renewed["expires_at"]
                .as_u64()
                .is_some_and(|expiry| expiry <= now() + 60)
        {
            output["status"] = json!("expired");
        }
        return output;
    }
    // Only Kimi's managed endpoints receive this token; custom gateways stay untouched.
    if ![
        "https://api.kimi.com/coding/v1",
        "https://api.kimi.ai/coding/v1",
    ]
    .contains(&environment.base_url.as_str())
    {
        return result;
    }
    let me = http_json(&format!("{}/me", environment.base_url), token, &[]).ok();
    let usage = http_json(&format!("{}/usages", environment.base_url), token, &[]).ok();
    kimi_report(me.as_ref(), usage.as_ref())
}
pub(super) fn dsh(home: &Path) -> Value {
    let fallback =
        json!({"identity":{},"windows":[],"source":"DeepSeek Harness","status":"unavailable"});
    // Without a running native UI only the API key, its balance and public models are read.
    let url = crate::state::dsh::account_ui_url(home).ok();
    let mut cmd = Command::new("node");
    cmd.args([
        "--input-type=module",
        "-e",
        concat!(
            include_str!("../dsh/account-client.mjs"),
            "\n",
            include_str!("../dsh/account-read.mjs")
        ),
    ]);
    capture(
        cmd,
        json!({"url":url,"home":home}).to_string().as_bytes(),
        20,
    )
    .ok()
    .and_then(|b| serde_json::from_slice(&b).ok())
    .unwrap_or(fallback)
}

// The signature identifies the signed-in account, so it is also the catalog's
// auth revision. Token refreshes rewrite Kimi credentials every 15 minutes and
// Claude's every few hours, and Claude Code rewrites .claude.json constantly:
// hashing those bytes discarded every cached status and limit within minutes.
// Sign-out and another account still change it.
pub(super) fn signature(home: &Path, provider: &str) -> String {
    let mut fingerprint = Sha256::new();
    fingerprint.update(home.as_os_str().as_encoded_bytes());
    fingerprint.update(provider.as_bytes());
    if provider == "claude" {
        fingerprint.update(b"desktop-session-v2");
        if let Ok(bytes) = fs::read(home.join(".claude.json")) {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(settings) => {
                    fingerprint.update(settings["oauthAccount"].to_string());
                    fingerprint.update(settings["primaryApiKey"].to_string());
                }
                Err(_) => fingerprint.update(bytes),
            }
        }
    }
    let kimi = (provider == "kimi").then(|| kimi_environment(home));
    let names: Vec<&str> = if let Some(environment) = &kimi {
        fingerprint.update(environment.credentials.as_bytes());
        fingerprint.update(environment.base_url.as_bytes());
        vec![&environment.credentials]
    } else {
        vec![
            "auth.json",
            ".credentials.json",
            "credentials/kimi-code.json",
            ".credentials.yaml",
        ]
    };
    for name in names {
        if let Ok(bytes) = fs::read(home.join(name)) {
            match signed_in(provider, name, &bytes) {
                Some(account) => fingerprint.update(account),
                None => fingerprint.update(&bytes),
            }
        }
    }
    format!("{:x}", fingerprint.finalize())
}
// The lasting part of credentials whose tokens rotate, or None to use all bytes.
// Kimi tokens are JWTs that name their user; claims are read only to tell
// accounts apart, never to authenticate.
fn signed_in(provider: &str, name: &str, bytes: &[u8]) -> Option<String> {
    let credentials = serde_json::from_slice::<Value>(bytes).ok()?;
    match (provider, name) {
        ("kimi", name) if name.starts_with("credentials/") => {
            use base64::Engine;
            ["access_token", "refresh_token"].iter().find_map(|key| {
                let payload = credentials[*key].as_str()?.split('.').nth(1)?;
                let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(payload.trim_end_matches('='))
                    .ok()?;
                let claims = serde_json::from_slice::<Value>(&decoded).ok()?;
                let user = json!([claims["user_id"], claims["sub"]]);
                (!text(&claims, "user_id").is_empty() || !text(&claims, "sub").is_empty())
                    .then(|| user.to_string())
            })
        }
        ("claude", ".credentials.json") => {
            let mut oauth = credentials["claudeAiOauth"].as_object()?.clone();
            for key in ["accessToken", "refreshToken", "expiresAt", "refreshTokenExpiresAt"] {
                oauth.remove(key);
            }
            Some(Value::Object(oauth).to_string())
        }
        _ => None,
    }
}
fn cache_path(config: &Config, id: &str, home: &Path) -> PathBuf {
    config.dir.join("account-status").join(format!(
        "{:x}.json",
        Sha256::digest(format!("{}:{id}", home.display()).as_bytes())
    ))
}

// Catalog loading remains read-only and does not contact providers. A cached
// identity is usable only for the same home and unchanged file credentials.
pub(super) fn catalog_status(config: &Config, id: &str, provider: &str, home: &Path) -> Value {
    let cache = read_json(&cache_path(config, id, home));
    if cache["signature"] != signature(home, provider) {
        return Value::Null;
    }
    let data = &cache["data"];
    let mut identity = json!({});
    for field in [
        "name",
        "email",
        "organization",
        "plan",
        "auth_method",
        "account_id",
    ] {
        let value = text(&data["identity"], field);
        if !value.is_empty() {
            identity[field] = json!(value);
        }
    }
    // Reuse only public quota fields from the same credential-validated cache.
    // Listing accounts never starts a provider probe, including after GUI restart.
    let rows = |key: &str, fields: &[&str]| {
        data[key]
            .as_array()
            .into_iter()
            .flatten()
            .take(16)
            .filter_map(|row| {
                if !row.is_object() {
                    return None;
                }
                let mut projected = json!({});
                for field in fields {
                    let value = &row[*field];
                    if value.is_number() {
                        projected[*field] = value.clone();
                    } else if let Some(value) = value.as_str() {
                        projected[*field] = json!(value
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(160)
                            .collect::<String>());
                    }
                }
                Some(projected)
            })
            .collect::<Vec<Value>>()
    };
    let mut credits = json!({});
    if let Some(balance) = data["credits"]["balance"].as_str() {
        if balance.parse::<f64>().is_ok_and(f64::is_finite) {
            credits["balance"] = json!(balance);
        }
    }
    if let Some(unlimited) = data["credits"]["unlimited"].as_bool() {
        credits["unlimited"] = json!(unlimited);
    }
    json!({"identity":identity,"status":text(data,"status"),"auth_status":text(data,"auth_status"),"checked_at":data["checked_at"],
        "windows":rows("windows",&["id","label","used_percent","window_minutes","resets_at"]),
        "balances":rows("balances",&["kind","balance","currency"]),"credits":credits})
}

// Quota and identity requests can succeed independently. A partial response
// must not erase a verified identity for unchanged credentials. Never carry
// it across sign-out, credential replacement or a newly identified account.
fn retain_cached_identity(data: &mut Value, cached: &Value, current_signature: &str) {
    if cached["signature"] != current_signature
        || data["status"] == "signed_out"
        || cached["data"]["status"] == "signed_out"
        || !text(&data["identity"], "email").is_empty()
        || !text(&data["identity"], "account_id").is_empty()
    {
        return;
    }
    let previous = &cached["data"]["identity"];
    if !text(previous, "email").is_empty() || !text(previous, "account_id").is_empty() {
        data["identity"] = previous.clone();
        data["identity_cached"] = json!(true);
    }
}

pub(super) fn inspect(config: &Config, args: &[String]) -> Result<Value> {
    let session = arg(args, "--session");
    let context = session
        .map(crate::state::account_context)
        .transpose()
        .map_err(|e| Error::new(1, e))?;
    let id = if let Some(c) = &context {
        if c["id"].as_str().is_some_and(|s| !s.is_empty()) {
            text(c, "id")
        } else {
            format!("native-{}", text(c, "provider"))
        }
    } else {
        args.get(1)
            .filter(|s| !s.starts_with('-'))
            .cloned()
            .ok_or_else(|| Error::new(1, "Account profile is required."))?
    };
    let (profile, mut home, mut native) = lookup(config, &id)?;
    if let Some(c) = &context {
        if text(c, "provider") != profile.provider {
            return Err(Error::new(
                1,
                "Session account provider differs from the profile.",
            ));
        }
        if !text(c, "home").is_empty() {
            let captured = PathBuf::from(text(c, "home"));
            if captured != home {
                native = false;
                home = captured;
            }
        }
    }
    let signature = signature(&home, &profile.provider);
    let cache_dir = config.dir.join("account-status");
    let cache_path = cache_path(config, &id, &home);
    let cached = read_json(&cache_path);
    let refresh = args.iter().any(|s| s == "--refresh");
    let mut data = if !refresh
        && cached["signature"] == signature
        && now().saturating_sub(cached["data"]["checked_at"].as_u64().unwrap_or(0)) < 60
    {
        cached["data"].clone()
    } else {
        let mut data = match profile.provider.as_str() {
            "codex" => codex(&home, native),
            "claude" => claude(&home, native),
            "kimi" => kimi(&home),
            "dsh" => dsh(&home),
            "opencode"=>json!({"status":"configured","source":"OpenCode","identity":{},"windows":[],"reason":"OpenCode uses its native inference-provider configuration; quota inspection is not available"}),
            _ => Value::Null,
        };
        retain_cached_identity(&mut data, &cached, &signature);
        data["checked_at"] = json!(now());
        private_dir(&cache_dir)?;
        let mut f = tempfile::NamedTempFile::new_in(&cache_dir)?;
        platform::make_private(f.as_file())?;
        f.write_all(
            json!({"signature":signature,"data":data})
                .to_string()
                .as_bytes(),
        )?;
        platform::persist(f, cache_path)
            .map_err(|_| Error::new(1, "Could not cache account status."))?;
        data
    };
    data["id"] = json!(id);
    data["provider"] = json!(profile.provider);
    data["label"] = json!(profile.label);
    data["home"] = json!(home);
    data["host"] = json!(config.host());
    if let Some(c) = context {
        data["run_id"] = c["run_id"].clone();
        data["session"] = json!(session);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_quota_refresh_keeps_identity_only_for_same_credentials() {
        let cached = json!({"signature":"same-auth","data":{"status":"ok","identity":{"email":"work@example.test","plan":"Pro"}}});
        let partial = json!({"status":"ok","identity":{},"windows":[{"used_percent":42}]});
        let mut data = partial.clone();
        retain_cached_identity(&mut data, &cached, "same-auth");
        assert_eq!(data["identity"]["email"], "work@example.test");
        assert_eq!(data["windows"], partial["windows"]);
        assert_eq!(data["identity_cached"], true);
        let mut changed = partial.clone();
        retain_cached_identity(&mut changed, &cached, "different-auth");
        assert_eq!(changed["identity"], json!({}));
        let mut signed_out = json!({"status":"signed_out","identity":{}});
        retain_cached_identity(&mut signed_out, &cached, "same-auth");
        assert_eq!(signed_out["identity"], json!({}));
        let mut identified = json!({"status":"ok","identity":{"email":"other@example.test"}});
        retain_cached_identity(&mut identified, &cached, "same-auth");
        assert_eq!(identified["identity"]["email"], "other@example.test");
    }

    fn jwt(claims: Value) -> String {
        use base64::Engine;
        let part = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
        format!("{}.{}.signature", part(json!({"alg":"none"})), part(claims))
    }

    #[test]
    fn kimi_global_region_uses_its_active_scoped_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("credentials")).unwrap();
        let config = r#"[providers."managed:kimi-code"]
type = "kimi"
base_url = "https://api.kimi.ai/coding/v1"
[providers."managed:kimi-code".oauth]
storage = "file"
key = "oauth/kimi-code-env-0e4f99c69cc27850"
oauth_host = "https://auth.kimi.ai"
"#;
        fs::write(home.join("config.toml"), config).unwrap();
        let environment = kimi_environment(home);
        assert_eq!(environment.credentials, "credentials/kimi-code-env-0e4f99c69cc27850.json");
        assert_eq!(environment.base_url, "https://api.kimi.ai/coding/v1");

        let token = |user: &str, issued: u64| jwt(json!({"user_id":user,"sub":user,"iat":issued}));
        let scoped = home.join(&environment.credentials);
        fs::write(&scoped, json!({"access_token":token("user-a",1_000)}).to_string()).unwrap();
        fs::write(home.join("credentials/kimi-code.json"), r#"{"access_token":"old-default"}"#).unwrap();
        let first = signature(home, "kimi");
        fs::write(&scoped, json!({"access_token":token("user-a",1_900)}).to_string()).unwrap();
        fs::write(home.join("credentials/kimi-code.json"), r#"{"access_token":"changed-default"}"#).unwrap();
        assert_eq!(signature(home, "kimi"), first);
        fs::write(&scoped, json!({"access_token":token("user-b",1_900)}).to_string()).unwrap();
        assert_ne!(signature(home, "kimi"), first);

        fs::write(home.join("config.toml"), config.replace("oauth/kimi-code-env-0e4f99c69cc27850", "oauth/../outside")).unwrap();
        assert_eq!(kimi_environment(home).credentials, "credentials/kimi-code.json");
    }

    #[test]
    fn kimi_token_refresh_keeps_the_account_signature() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("credentials")).unwrap();
        let sign_in = |user: &str, issued: u64| {
            let token = |kind: &str| jwt(json!({"user_id":user,"sub":user,"device_id":"device","iat":issued,"jti":format!("{kind}-{issued}")}));
            let credentials = json!({"access_token":token("access"),"refresh_token":token("refresh"),
                "expires_at":issued + 900,"expires_in":900,"scope":"kimi-code","token_type":"Bearer"});
            fs::write(home.join("credentials/kimi-code.json"), credentials.to_string()).unwrap();
        };
        sign_in("user-a", 1_000);
        let first = signature(home, "kimi");
        sign_in("user-a", 1_900);
        assert_eq!(signature(home, "kimi"), first);
        sign_in("user-b", 1_900);
        assert_ne!(signature(home, "kimi"), first);
        fs::remove_file(home.join("credentials/kimi-code.json")).unwrap();
        assert_ne!(signature(home, "kimi"), first);
        // Credentials without a readable account keep the byte-for-byte rule.
        fs::write(home.join("credentials/kimi-code.json"), r#"{"access_token":"opaque-1"}"#).unwrap();
        let opaque = signature(home, "kimi");
        fs::write(home.join("credentials/kimi-code.json"), r#"{"access_token":"opaque-2"}"#).unwrap();
        assert_ne!(signature(home, "kimi"), opaque);
    }

    #[test]
    fn kimi_verified_identity_stands_without_any_quota() {
        let me = json!({"user":{"user_id":"u-1","nickname":"Fixture","email":"kimi@example.invalid","user_level_name":"Free"}});
        // An empty quota answer is a plan without limits to report, not a sign-out.
        let signed_in = kimi_report(Some(&me), Some(&json!({})));
        assert_eq!(signed_in["auth_status"], "signed_in");
        assert_eq!(signed_in["status"], "unavailable");
        assert_eq!(signed_in["windows"], json!([]));
        assert_eq!(signed_in["identity"]["plan"], "Free");
        // A quota read that fails entirely changes nothing about the sign-in.
        let no_usage = kimi_report(Some(&me), None);
        assert_eq!(no_usage["auth_status"], "signed_in");
        assert_eq!(no_usage["status"], "unavailable");
        // The identity rides in data too, and quotas keep their own status.
        let wrapped = json!({"data":{"email":"other@example.invalid"}});
        assert_eq!(kimi_report(Some(&wrapped), None)["identity"]["email"], "other@example.invalid");
        // A stable id verifies even when the display identity carries no email.
        let id_only = json!({"data":{"global_id":"g-1","user_level_name":"Free"}});
        let id_report = kimi_report(Some(&id_only), None);
        assert_eq!(id_report["auth_status"], "signed_in");
        assert_eq!(id_report["identity"]["email"], "");
        let quota = json!({"usages":{"limit_5h":{"used_ratio":0.25,"reset_time":null}}});
        let both = kimi_report(Some(&me), Some(&quota));
        assert_eq!((both["auth_status"].as_str(),both["status"].as_str()), (Some("signed_in"),Some("ok")));
    }

    #[test]
    fn kimi_identity_report_never_invents_a_sign_in() {
        // A plan, a nickname or an empty answer proves nothing on its own.
        for empty in [
            json!({"user":{}}),
            json!({}),
            json!({"message":"ok"}),
            json!({"user":{"nickname":"Fixture","user_level_name":"Free"}}),
        ] {
            let report = kimi_report(Some(&empty), None);
            assert_eq!(report["auth_status"], Value::Null, "{empty}");
            assert_eq!(report["status"], "unavailable");
        }
        // Without a verified profile a quota answer adds no explicit auth_status;
        // its "ok" status keeps the meaning it already had.
        let usage = json!({"usages":{"limit_7d":{"used_ratio":1.5}}});
        let quota_only = kimi_report(None, Some(&usage));
        assert_eq!(quota_only["auth_status"], Value::Null);
        assert_eq!(quota_only["status"], "ok");
    }

    #[test]
    fn kimi_missing_quota_sections_are_not_windows() {
        for usage in [json!({}),json!({"usages":null}),json!({"usages":[]}),json!({"usages":{}})] {
            assert!(kimi_windows(&usage).is_empty(), "{usage}");
        }
        let usage = json!({"usages":{"limit_7d":{"used_ratio":1.5,"reset_time":1793000000}}});
        let windows = kimi_windows(&usage);
        assert_eq!(windows.len(), 1);
        assert_eq!((windows[0]["id"].as_str(),windows[0]["window_minutes"].as_u64()), (Some("limit_7d"),Some(10080)));
        assert_eq!(windows[0]["used_percent"], 150.0);
    }

    #[test]
    fn claude_signature_follows_the_signed_in_account() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let settings = |account: &str, startups: u64| {
            let settings = json!({"numStartups":startups,"projects":{"/workspace/example":{"lastCost":startups}},
                "oauthAccount":{"accountUuid":account,"emailAddress":format!("{account}@example.test")}});
            fs::write(home.join(".claude.json"), settings.to_string()).unwrap();
        };
        let credentials = |token: &str, expiry: u64| {
            let credentials = json!({"claudeAiOauth":{"accessToken":token,"refreshToken":format!("refresh-{token}"),
                "expiresAt":expiry,"refreshTokenExpiresAt":expiry + 1,"scopes":["user:inference"],"subscriptionType":"max"}});
            fs::write(home.join(".credentials.json"), credentials.to_string()).unwrap();
        };
        settings("account-a", 1);
        credentials("token-1", 1_000);
        let first = signature(home, "claude");
        settings("account-a", 2);
        credentials("token-2", 2_000);
        assert_eq!(signature(home, "claude"), first);
        settings("account-b", 2);
        assert_ne!(signature(home, "claude"), first);
        settings("account-a", 3);
        fs::remove_file(home.join(".credentials.json")).unwrap();
        assert_ne!(signature(home, "claude"), first);
    }
}
