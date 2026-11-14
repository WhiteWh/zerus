//! Read-only native identity/quota adapters. Only allowlisted display fields leave this module.
use super::*;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_OUTPUT: u64 = 1024 * 1024;
pub(super) fn now() -> u64 {
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
pub(super) fn read_json(path: &Path) -> Value {
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
    // The inspection's test seam belongs to the inspection: no native agent
    // child (login included) inherits it.
    cmd.env_remove(KIMI_TEST_BASE_ENV);
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
/// How curl is run for one Kimi request. `isolated` is true only for the
/// Zerus test seam: `-q` (which curl honours only as its first argument) and
/// `--noproxy '*'` keep `~/.curlrc`, `$CURL_HOME`, `$XDG_CONFIG_HOME` and every
/// proxy variable from routing the token anywhere but the validated endpoint.
#[derive(Clone, Copy)]
struct CurlPolicy {
    allow_http: bool,
    isolated: bool,
}

fn http_json(
    url: &str,
    token: &str,
    extra: &[&str],
    policy: CurlPolicy,
) -> std::result::Result<Value, String> {
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
    if policy.isolated {
        cmd.args(["-q", "--noproxy", "*"]);
    }
    cmd.args([
        "--silent",
        "--fail",
        "--proto",
        if policy.allow_http { "=http,https" } else { "=https" },
        "--connect-timeout",
        "3",
        "--max-time",
        "6",
        "--max-filesize",
        "1048576",
        "--config",
        "-",
    ]);
    let bytes = capture(cmd, config.as_bytes(), 7)?;
    serde_json::from_slice(&bytes).map_err(|_| "Invalid native account response.".into())
}
fn claude_http(url: &str, token: &str) -> claude_usage::Reply {
    use claude_usage::Reply;
    if token.is_empty() || token.chars().any(char::is_control) {
        return Reply::Unavailable;
    }
    let Ok(dir) = tempfile::tempdir() else {
        return Reply::Unavailable;
    };
    let headers = dir.path().join("headers");
    let Ok(file) = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&headers)
    else {
        return Reply::Unavailable;
    };
    if platform::make_private(&file).is_err() {
        return Reply::Unavailable;
    }
    // Keep the grant in stdin, never argv. curl's stderr and response body are
    // never logged or cached. There is no refresh-token or inference request.
    let config = format!("url = {}\nheader = {}\nheader = \"Accept: application/json\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\n",
        json!(url), json!(format!("Authorization: Bearer {token}")));
    let mut cmd = Command::new("curl");
    cmd.args([
        "--silent",
        "--proto",
        if cfg!(test) && url.starts_with("http://127.0.0.1:") {
            "=http"
        } else {
            "=https"
        },
        "--connect-timeout",
        "3",
        "--max-time",
        "6",
        "--max-filesize",
        "1048576",
        "--dump-header",
    ])
    .arg(&headers)
    .args(["--write-out", "\n%{http_code}", "--config", "-"]);
    let response = capture(cmd, config.as_bytes(), 7);
    let mut header_bytes = Vec::new();
    if fs::File::open(headers)
        .and_then(|f| f.take(32 * 1024 + 1).read_to_end(&mut header_bytes))
        .is_err()
        || header_bytes.len() > 32 * 1024
    {
        header_bytes.clear();
    }
    let headers = String::from_utf8_lossy(&header_bytes);
    let limited = headers
        .lines()
        .rfind(|line| line.starts_with("HTTP/"))
        .and_then(|line| line.split_whitespace().nth(1))
        == Some("429");
    if limited {
        // A known 429 still carries its cooldown when curl refuses an oversized
        // body or the connection closes before that body finishes.
        return Reply::RateLimited(claude_usage::retry_after(&headers, now()));
    }
    let Ok(bytes) = response else {
        return Reply::Unavailable;
    };
    let Some(split) = bytes.iter().rposition(|b| *b == b'\n') else {
        return Reply::Unavailable;
    };
    match &bytes[split + 1..] {
        b"200" => serde_json::from_slice(&bytes[..split])
            .map(Reply::Usage)
            .unwrap_or(Reply::Unavailable),
        b"429" => Reply::RateLimited(claude_usage::retry_after(&headers, now())),
        _ => Reply::Unavailable,
    }
}

pub(super) fn window(
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
fn claude(config: &Config, home: &Path, native: bool, seed: &Value) -> Result<Value> {
    let mut result =
        json!({"identity":{},"windows":[],"source":"Claude Code","status":"unavailable"});
    let mut cmd = native_command("claude", home, native);
    cmd.args(["auth", "status", "--json"]);
    // Claude returns valid signed-out JSON with exit code 1.
    if let Ok(bytes) = capture_with_exit(cmd, &[], 5, Some(1)) {
        if let Ok(a) = serde_json::from_slice::<Value>(&bytes) {
            result["identity"] = json!({"email":text(&a,"email"),"plan":text(&a,"subscriptionType"),"organization":text(&a,"orgName"),"organization_id":text(&a,"orgId"),"auth_method":text(&a,"authMethod")});
            if a["loggedIn"] == false {
                result["status"] = json!(claude::missing_login_status(home, native));
                result["auth_status"] = result["status"].clone();
                if result["status"] != "signed_out" {
                    result["identity"] = claude::saved_identity(home, native);
                    result["identity_cached"] = json!(true);
                }
                return Ok(result);
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
        return Ok(result);
    }
    // Native OAuth usage endpoint used by /usage; never sends a message or refreshes a grant.
    // Without a current native account identity, neither a prior identity nor
    // its quota/cooldown can safely stand in for this account.
    if result["auth_status"] != "signed_in" || text(&result["identity"], "email").is_empty() {
        return Ok(result);
    }
    let key = claude_usage::key(home, &result["identity"], &credentials, true);
    let same_sample = seed["_claude_usage_key"].as_str() == Some(&key)
        || (seed["_claude_usage_key"].is_null()
            && claude_usage::same_identity(&result["identity"], &seed["identity"]));
    let quota = claude_usage::get(&config.dir, &key, if same_sample { seed } else { &Value::Null }, now, || {
        claude_http("https://api.anthropic.com/api/oauth/usage", auth["accessToken"].as_str().unwrap_or(""))
    })?;
    result["_claude_usage_key"] = json!(key);
    for field in ["windows", "status", "checked_at", "attempted_at", "retry_at"] {
        result[field] = quota[field].clone();
    }
    Ok(result)
}
const KIMI_API_BASE: &str = "https://api.kimi.com/coding/v1";
const KIMI_API_BASE_SLASH: &str = "https://api.kimi.com/coding/v1/";
/// Zerus's test seam; a person's Kimi CLI never reads it.
pub(super) const KIMI_TEST_BASE_ENV: &str = "HGS_TEST_KIMI_API_BASE";
/// The Kimi CLI's own platform variable. The inspection reads it only to stay
/// on the provider; the person's custom platform never receives this token.
const KIMI_CLI_BASE_ENV: &str = "KIMI_CODE_BASE_URL";

/// A test endpoint is accepted only when its host is a loopback address. Any
/// other value is ignored and named in the inspection's detail; it never
/// receives the token.
fn ignored_kimi_base(result: &mut Value, variable: &str) {
    let reason = if variable == KIMI_TEST_BASE_ENV {
        "Kimi account inspection uses only a loopback http or https address."
    } else {
        "Kimi account inspection sends its token only to the default platform."
    };
    let sentence = format!("Ignored {variable}: {reason}");
    // An invalid seam and a non-default CLI value can both apply; the later
    // rule adds its own sentence instead of hiding the seam's.
    result["detail"] = match result["detail"].as_str().filter(|text| !text.is_empty()) {
        Some(earlier) => json!(format!("{earlier} {sentence}")),
        None => json!(sentence),
    };
}

/// A `[::1]` or literal loopback host, then the end, a `/`, or `:PORT` with 1
/// to 5 digits inside curl's range. Anything curl would refuse, and anything
/// that would move the host out of the authority, is refused here instead.
fn loopback_authority(rest: &str) -> Option<()> {
    let (host, tail) = match rest.strip_prefix('[') {
        Some(after) => after.split_once(']')?,
        None => {
            let end = rest.find([':', '/']).unwrap_or(rest.len());
            rest.split_at(end)
        }
    };
    if !(host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1") {
        return None;
    }
    if tail.is_empty() || tail.starts_with('/') {
        return Some(());
    }
    let port = tail.strip_prefix(':')?;
    let digits = port.split('/').next().unwrap_or("");
    if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.parse::<u32>().ok()? > 65535 {
        return None;
    }
    let after = &port[digits.len()..];
    (after.is_empty() || after.starts_with('/')).then_some(())
}

fn loopback_test_base(value: &str) -> Option<(String, bool)> {
    let (scheme, rest) = value.split_once("://")?;
    let allow_http = match scheme.to_ascii_lowercase().as_str() {
        "https" => false,
        "http" => true,
        _ => return None,
    };
    if rest.contains('?') || rest.contains('#') || rest.contains('@') {
        return None;
    }
    loopback_authority(rest)?;
    Some((value.trim_end_matches('/').to_owned(), allow_http))
}

/// A valid Zerus test endpoint takes precedence over the Kimi CLI's platform
/// variable; an invalid one is named in the detail, then that variable's rule
/// applies. Any non-default `KIMI_CODE_BASE_URL` means the person's Kimi CLI
/// uses another platform, so the inspection makes no request and sends no
/// token. Only unset or the provider's own address (with or without its
/// trailing slash) reaches the provider.
fn kimi_base(result: &mut Value) -> Option<(String, CurlPolicy)> {
    match env::var(KIMI_TEST_BASE_ENV) {
        // Every account session's environment carries the seam as an empty
        // value; that is unset, never an ignored endpoint.
        Ok(value) if value.is_empty() => {}
        Ok(value) => {
            if let Some((base, allow_http)) = loopback_test_base(&value) {
                return Some((
                    base,
                    CurlPolicy {
                        allow_http,
                        isolated: true,
                    },
                ));
            }
            ignored_kimi_base(result, KIMI_TEST_BASE_ENV);
        }
        Err(env::VarError::NotPresent) => {}
        Err(env::VarError::NotUnicode(_)) => {
            ignored_kimi_base(result, KIMI_TEST_BASE_ENV);
        }
    }
    match env::var(KIMI_CLI_BASE_ENV) {
        Ok(value) if value != KIMI_API_BASE && value != KIMI_API_BASE_SLASH => {
            ignored_kimi_base(result, KIMI_CLI_BASE_ENV);
            None
        }
        Ok(_) | Err(env::VarError::NotPresent) => Some((
            KIMI_API_BASE.to_owned(),
            CurlPolicy {
                allow_http: false,
                isolated: false,
            },
        )),
        Err(env::VarError::NotUnicode(_)) => {
            ignored_kimi_base(result, KIMI_CLI_BASE_ENV);
            None
        }
    }
}

/// Where Kimi Code keeps this account's sign-in and which endpoint it uses:
/// its managed provider in `config.toml`. The global region (api.kimi.ai)
/// names its own OAuth key, so its credentials live in
/// `credentials/<key>.json` rather than `credentials/kimi-code.json`.
struct KimiEnvironment {
    credentials: String,
    base_url: String,
}

fn kimi_environment(home: &Path) -> KimiEnvironment {
    let mut environment = KimiEnvironment {
        credentials: "credentials/kimi-code.json".into(),
        base_url: KIMI_API_BASE.into(),
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
            .filter(|name| kimi_credential_name(name))
        {
            environment.credentials = format!("credentials/{name}");
        }
        if let Some(base_url) = provider.get("base_url").and_then(toml_edit::Item::as_str) {
            environment.base_url = base_url.trim_end_matches('/').into();
        }
    }
    environment
}

fn kimi_credential_name(name: &str) -> bool {
    name.ends_with(".json")
        && !name.starts_with('.')
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// Kimi's managed endpoints: only they receive the account's token.
const KIMI_MANAGED_BASES: [&str; 2] = ["https://api.kimi.com/coding/v1", "https://api.kimi.ai/coding/v1"];

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
            .env("KIMI_CODE_HOME", home)
            // The test seam is the inspection's alone: `node` starts the real
            // `kimi web`, which must never inherit it.
            .env_remove(KIMI_TEST_BASE_ENV);
        for key in AUTH_ENV {
            cmd.env_remove(key);
        }
        let work = tempfile::tempdir().ok();
        if let Some(dir) = &work {
            cmd.current_dir(dir.path());
        }
        return capture(cmd, &[], 18)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(result);
    }
    // The live-token path resolves the base only after the refresh, as on main:
    // an expiring token still gets the native server's own answer.
    let Some((base, policy)) = kimi_base(&mut result) else {
        return result;
    };
    // Without the test seam the account's own managed endpoint is used (the
    // global region's api.kimi.ai); a custom gateway gets no token.
    let base = if policy.isolated {
        base
    } else if KIMI_MANAGED_BASES.contains(&environment.base_url.as_str()) {
        environment.base_url.clone()
    } else {
        result["detail"] = json!("The Kimi account uses a custom endpoint; its usage is not read.");
        return result;
    };
    // The same token goes to the default provider or to a validated loopback
    // test endpoint; the path below the base stays the provider's.
    if let Ok(a) = http_json(&format!("{base}/me"), token, &[], policy) {
        let a = a.get("user").or_else(|| a.get("data")).unwrap_or(&a);
        result["identity"] = json!({"name":text(a,"nickname"),"email":text(a,"email"),"plan":text(a,"user_level_name"),"auth_method":"Kimi Code"});
    }
    if let Ok(usage) = http_json(&format!("{base}/usages"), token, &[], policy) {
        let mut windows = Vec::new();
        for (key, label, minutes) in [
            ("limit_5h", "", Some(300)),
            ("limit_7d", "", Some(10080)),
            ("limit_month_total", "Monthly", None),
            ("limit_month_code", "Monthly code", None),
        ] {
            let w = &usage["usages"][key];
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
        if !windows.is_empty() {
            result["windows"] = json!(windows);
            result["status"] = json!("ok");
        }
    }
    result
}
fn dsh(home: &Path) -> Value {
    let fallback =
        json!({"identity":{},"windows":[],"source":"DeepSeek Harness","status":"unavailable"});
    let Ok(url) = crate::state::dsh::account_ui_url(home) else {
        return fallback;
    };
    let mut cmd = Command::new("node");
    cmd.args([
        "--input-type=module",
        "-e",
        concat!(
            include_str!("../dsh/account-client.mjs"),
            "\n",
            include_str!("../dsh/account-balance.mjs"),
            "\n",
            include_str!("../dsh/account-read.mjs")
        ),
    ]);
    let mut data = capture(
        cmd,
        json!({"url":url,"home":home}).to_string().as_bytes(),
        20,
    )
    .ok()
    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    .filter(Value::is_object)
    .unwrap_or(fallback);
    super::deepseek::project_api_balance(&mut data);
    data
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
        // The region decides which account a sign-in is.
        fingerprint.update(environment.credentials.as_bytes());
        fingerprint.update(environment.base_url.as_bytes());
        vec![&environment.credentials]
    } else {
        vec!["auth.json", ".credentials.json", "credentials/kimi-code.json", ".credentials.yaml"]
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
    let cache = claude_usage::read_cache(&cache_path(config, id, home)).unwrap_or(Value::Null);
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
    let mut status = json!({"identity":identity,"status":text(data,"status"),"auth_status":text(data,"auth_status"),"checked_at":data["checked_at"],"retry_at":data["retry_at"],
        "windows":rows("windows",&["id","label","used_percent","window_minutes","resets_at"]),
        "balances":rows("balances",&["kind","balance","currency"]),"credits":credits});
    if let Some(available) = data["balance_available"].as_bool() {
        status["balance_available"] = json!(available);
    }
    if let Some(spend) = super::deepseek_spend::public(&data["spend"]) {
        status["spend"] = spend;
    }
    if let Some(usage) = super::deepseek_usage::public(&data["local_usage"]) {
        status["local_usage"] = usage;
    }
    status
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
    let cached = claude_usage::read_cache(&cache_path)?;
    let refresh = args.iter().any(|s| s == "--refresh");
    let same = cached["signature"] == signature;
    // Claude must observe native identity before reusing any quota/cooldown:
    // its durable auth revision deliberately excludes rotating bearer tokens.
    let mut data = if profile.provider != "claude" && same && !refresh
        && now().saturating_sub(cached["data"]["checked_at"].as_u64().unwrap_or(0)) < 60
    {
        cached["data"].clone()
    } else {
        let mut data = match profile.provider.as_str() {
            "codex" => codex(&home, native),
            "claude" => claude(config, &home, native, if same { &cached["data"] } else { &Value::Null })?,
            "kimi" => kimi(&home),
            "dsh" => dsh(&home),
            "opencode"=>json!({"status":"configured","source":"OpenCode","identity":{},"windows":[],"reason":"OpenCode uses its native inference-provider configuration; quota inspection is not available"}),
            _ => Value::Null,
        };
        if profile.provider != "claude" {
            retain_cached_identity(&mut data, &cached, &signature);
        }
        // Both reuse this refresh: the balance it just read and local session logs.
        if profile.provider == "dsh" {
            if let Some(spend) = super::deepseek_spend::record(config, &id, &home, &data) {
                data["spend"] = spend;
            }
            if let Some(usage) = super::deepseek_usage::refresh(config, &id, &home) {
                data["local_usage"] = usage;
            }
        }
        if data.get("checked_at").is_none() {
            data["checked_at"] = json!(now());
        }
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
    if let Some(data) = data.as_object_mut() {
        data.remove("_claude_usage_key");
    }
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
    use std::io::Write;

    // The variable names are spelled out here: a mutant that changes the name
    // the inspection reads must fail these tests.
    const TEST_BASE_VAR: &str = "HGS_TEST_KIMI_API_BASE";
    const CLI_BASE_VAR: &str = "KIMI_CODE_BASE_URL";

    fn curl_lock() -> std::sync::MutexGuard<'static, ()> {
        // A panicking test must not turn every later curl/environment test red.
        crate::accounts::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Restores every named variable on drop, including a previously unset one.
    struct EnvVars(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvVars {
        fn set(pairs: &[(&'static str, Option<&str>)]) -> Self {
            let saved = pairs
                .iter()
                .map(|(key, _)| (*key, env::var_os(key)))
                .collect();
            for (key, value) in pairs {
                match value {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
            Self(saved)
        }
    }

    impl Drop for EnvVars {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
        }
    }

    #[cfg(unix)]
    struct PathEnv(Option<String>);

    #[cfg(unix)]
    impl PathEnv {
        fn prepend(path: &Path) -> Self {
            let old = env::var("PATH").ok();
            let current = old.clone().unwrap_or_default();
            env::set_var("PATH", format!("{}:{current}", path.display()));
            Self(old)
        }
    }

    #[cfg(unix)]
    impl Drop for PathEnv {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => env::set_var("PATH", value),
                None => env::remove_var("PATH"),
            }
        }
    }

    fn kimi_home(token: &str, expires_at: u64) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("credentials")).unwrap();
        fs::write(
            home.path().join("credentials/kimi-code.json"),
            json!({"access_token": token, "expires_at": expires_at}).to_string(),
        )
        .unwrap();
        home
    }

    fn accept_with_timeout(server: &std::net::TcpListener) -> std::net::TcpStream {
        let start = Instant::now();
        loop {
            match server.accept() {
                Ok((stream, _)) => return stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        start.elapsed() < Duration::from_secs(3),
                        "the loopback server received no request"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("loopback accept failed: {error}"),
            }
        }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if stream.read_exact(&mut byte).is_err() {
                break;
            }
            request.push(byte[0]);
        }
        String::from_utf8(request).unwrap()
    }

    fn respond(stream: &mut std::net::TcpStream, body: &str) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    /// The stand-in's own writer: `/me` and `/usages` must arrive with the
    /// native bearer token.
    fn identity_worker(server: std::net::TcpListener) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            for (path, body) in [
                (
                    "/me",
                    r#"{"user":{"nickname":"Loop","email":"loop@example.test","user_level_name":"Pro"}}"#,
                ),
                (
                    "/usages",
                    r#"{"usages":{"limit_5h":{"used_ratio":0.5,"reset_time":123}}}"#,
                ),
            ] {
                let mut stream = accept_with_timeout(&server);
                let request = read_request(&mut stream);
                assert!(request.starts_with(&format!("GET {path} ")), "{request}");
                assert!(
                    request.contains("Authorization: Bearer synthetic-token"),
                    "{request}"
                );
                respond(&mut stream, body);
            }
        })
    }

    /// The `https` row's stand-in: curl must reach it directly, so it sends a
    /// TLS handshake and never a proxy `CONNECT`.
    #[cfg(unix)]
    fn tls_worker(server: std::net::TcpListener) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut stream = accept_with_timeout(&server);
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut record = [0u8; 1];
            stream.read_exact(&mut record).unwrap();
            assert_eq!(record[0], 0x16, "curl did not send a TLS handshake");
        })
    }

    #[cfg(unix)]
    fn no_connection(server: &std::net::TcpListener, who: &str) {
        match server.accept() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("{who} accept failed: {error}"),
            Ok((_, peer)) => panic!("{who} received a connection from {peer}"),
        }
    }

    #[cfg(unix)]
    fn recording_curl(dir: &Path) -> PathEnv {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("curl");
        fs::write(
            &script,
            r#"#!/bin/sh
i=0
while [ -e "$CURL_RECORD_DIR/argv.$i" ]; do i=$((i+1)); done
printf '%s\n' "$@" > "$CURL_RECORD_DIR/argv.$i"
cat > "$CURL_RECORD_DIR/stdin.$i"
if grep -q '/me' "$CURL_RECORD_DIR/stdin.$i"; then
  printf '{"user":{"nickname":"Provider","email":"provider@example.test","user_level_name":"Pro"}}'
else
  printf '{"usages":{"limit_5h":{"used_ratio":0.5,"reset_time":123}}}'
fi
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        PathEnv::prepend(dir)
    }

    #[cfg(unix)]
    const KIMI_DEFAULT_ARGV: &str = "--silent\n--fail\n--proto\n=https\n--connect-timeout\n3\n--max-time\n6\n--max-filesize\n1048576\n--config\n-\n";

    #[test]
    fn kimi_inspection_uses_a_loopback_test_base_and_sends_the_token() {
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let worker = identity_worker(server);
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, Some(&base)),
            (CLI_BASE_VAR, None),
        ]);
        let result = kimi(home.path());
        worker.join().unwrap();
        assert_eq!(result["status"], "ok", "{result}");
        assert_eq!(result["identity"]["name"], "Loop");
        assert_eq!(result["identity"]["email"], "loop@example.test");
        assert_eq!(result["identity"]["plan"], "Pro");
        assert_eq!(result["windows"][0]["used_percent"], 50.0);
        assert!(result.get("detail").is_none());
    }

    #[test]
    fn loopback_test_base_forms_reach_the_stand_in() {
        let _lock = curl_lock();
        for (pattern, v6) in [
            ("http://127.0.0.1:{port}", false),
            ("http://LOCALHOST:{port}", false),
            ("http://localhost:{port}/", false),
            ("http://[::1]:{port}", true),
        ] {
            let server = if v6 {
                std::net::TcpListener::bind("[::1]:0")
            } else {
                std::net::TcpListener::bind("127.0.0.1:0")
            };
            let Ok(server) = server else {
                continue; // No IPv6 loopback on this machine.
            };
            server.set_nonblocking(true).unwrap();
            let port = server.local_addr().unwrap().port();
            let base = pattern.replace("{port}", &port.to_string());
            let worker = identity_worker(server);
            let home = kimi_home("synthetic-token", now() + 3600);
            let _env = EnvVars::set(&[
                (TEST_BASE_VAR, Some(&base)),
                (CLI_BASE_VAR, None),
            ]);
            let result = kimi(home.path());
            worker.join().unwrap();
            assert_eq!(result["status"], "ok", "{base}: {result}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_expired_kimi_token_does_not_use_the_test_base_or_leak_it_to_the_refresh_child() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let home = kimi_home("synthetic-token", now().saturating_sub(120));
        let bin = tempfile::tempdir().unwrap();
        let marker = bin.path().join("leaked");
        let fake = bin.path().join("kimi");
        fs::write(
            &fake,
            "#!/bin/sh\nif [ -n \"${HGS_TEST_KIMI_API_BASE+x}\" ]; then printf leaked > \"$KIMI_REFRESH_MARKER\"; fi\nexit 97\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        let _path = PathEnv::prepend(bin.path());
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, Some(&base)),
            (CLI_BASE_VAR, None),
            ("KIMI_REFRESH_MARKER", Some(marker.to_str().unwrap())),
        ]);
        let result = kimi(home.path());
        assert_ne!(result["status"], "ok", "{result}");
        assert!(
            server.accept().is_err(),
            "the expired token reached the loopback base"
        );
        assert!(
            !marker.exists(),
            "the refresh child inherited HGS_TEST_KIMI_API_BASE"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_expired_token_with_a_custom_kimi_cli_base_refreshes_without_a_platform_request() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = curl_lock();
        let platform = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        platform.set_nonblocking(true).unwrap();
        let platform_url = format!(
            "http://{}/kimi-platform",
            platform.local_addr().unwrap()
        );
        let bin = tempfile::tempdir().unwrap();
        let record = tempfile::tempdir().unwrap();
        let marker = record.path().join("node-called");
        let child_env = record.path().join("node-env");
        let node = bin.path().join("node");
        fs::write(
            &node,
            format!(
                "#!/bin/sh\nprintf 'called\\n' > '{}'\nenv > '{}'\nprintf '%s\\n' '{{\"identity\":{{\"name\":\"Fixture\",\"email\":\"fixture@example.test\",\"plan\":\"Pro\",\"auth_method\":\"Kimi Code\"}},\"windows\":[],\"source\":\"Kimi Code\",\"status\":\"ok\"}}'\n",
                marker.display(),
                child_env.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&node, fs::Permissions::from_mode(0o700)).unwrap();
        let _path = PathEnv::prepend(bin.path());
        let home = kimi_home("synthetic-token", now().saturating_sub(120));
        let _env = EnvVars::set(&[
            // An invalid seam: a base-first order would refuse before the refresh.
            (TEST_BASE_VAR, Some("http://127.0.0.2:9")),
            (CLI_BASE_VAR, Some(&platform_url)),
            ("NODE_RECORD_DIR", Some(record.path().to_str().unwrap())),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "ok", "{result}");
        assert_eq!(result["identity"]["auth_method"], "Kimi Code", "{result}");
        assert_eq!(result["identity"]["name"], "Fixture", "{result}");
        assert!(result.get("detail").is_none(), "{result}");
        assert!(marker.exists(), "the expired token did not reach the refresh node");
        let seen = fs::read_to_string(&child_env).unwrap();
        assert!(
            !seen.lines().any(|line| line.starts_with(&format!("{TEST_BASE_VAR}="))),
            "the refresh child inherited {TEST_BASE_VAR}",
        );
        assert!(
            platform.accept().is_err(),
            "the inspection requested the custom platform"
        );
    }

    #[test]
    fn a_non_loopback_test_base_is_ignored_without_a_request() {
        let _lock = curl_lock();
        // Bind all interfaces and use 127.0.0.2 as the override host: the
        // listener would see a request if the non-loopback value were used.
        let server = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let port = server.local_addr().unwrap().port();
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, Some(&format!("http://127.0.0.2:{port}"))),
            (CLI_BASE_VAR, None),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "unavailable", "{result}");
        assert!(
            result["detail"]
                .as_str()
                .unwrap()
                .contains("Ignored HGS_TEST_KIMI_API_BASE"),
            "{result}"
        );
        assert!(
            server.accept().is_err(),
            "a non-loopback base received the token"
        );
    }

    #[test]
    fn a_custom_kimi_cli_base_is_ignored_even_when_it_is_loopback() {
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, None),
            (CLI_BASE_VAR, Some(&base)),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "unavailable", "{result}");
        assert!(
            result["detail"]
                .as_str()
                .unwrap()
                .contains("Ignored KIMI_CODE_BASE_URL"),
            "{result}"
        );
        assert!(
            server.accept().is_err(),
            "a KIMI_CODE_BASE_URL loopback value received the token"
        );
    }

    #[test]
    fn a_non_default_kimi_cli_base_is_ignored_without_a_request() {
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let base = format!("https://{}", server.local_addr().unwrap());
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, None),
            (CLI_BASE_VAR, Some(&base)),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "unavailable", "{result}");
        assert!(
            result["detail"]
                .as_str()
                .unwrap()
                .contains("Ignored KIMI_CODE_BASE_URL"),
            "{result}"
        );
        assert!(
            server.accept().is_err(),
            "a non-default KIMI_CODE_BASE_URL received the token"
        );
    }

    #[test]
    fn empty_and_doubled_kimi_cli_bases_keep_their_no_request_meaning() {
        let _lock = curl_lock();
        let home = kimi_home("synthetic-token", now() + 3600);
        for value in ["", "https://api.kimi.com/coding/v1//"] {
            let _env = EnvVars::set(&[
                (TEST_BASE_VAR, None),
                (CLI_BASE_VAR, Some(value)),
            ]);
            let result = kimi(home.path());
            assert_eq!(result["status"], "unavailable", "{value}: {result}");
            assert!(
                result["detail"]
                    .as_str()
                    .unwrap()
                    .contains("Ignored KIMI_CODE_BASE_URL"),
                "{value}: {result}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_empty_test_base_is_unset_without_a_detail() {
        let _lock = curl_lock();
        let bin = tempfile::tempdir().unwrap();
        let _path = recording_curl(bin.path());
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, Some("")),
            (CLI_BASE_VAR, None),
            ("CURL_RECORD_DIR", Some(bin.path().to_str().unwrap())),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "ok", "{result}");
        assert!(
            result.get("detail").is_none(),
            "an empty seam was reported as ignored: {result}"
        );
        assert_eq!(
            fs::read_to_string(bin.path().join("argv.0")).unwrap(),
            KIMI_DEFAULT_ARGV
        );
    }

    #[test]
    fn an_invalid_test_base_detail_survives_the_cli_base_detail() {
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, Some("http://127.0.0.2:9")),
            (CLI_BASE_VAR, Some(&base)),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "unavailable", "{result}");
        let detail = result["detail"].as_str().unwrap();
        assert!(
            detail.contains("Ignored HGS_TEST_KIMI_API_BASE"),
            "the seam's own detail was lost: {detail}"
        );
        assert!(
            detail.contains("Ignored KIMI_CODE_BASE_URL"),
            "the CLI base detail is missing: {detail}"
        );
        assert!(
            server.accept().is_err(),
            "a refused base received the token"
        );
    }

    #[test]
    fn a_valid_test_base_wins_over_the_kimi_cli_base() {
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let worker = identity_worker(server);
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, Some(&base)),
            (CLI_BASE_VAR, Some("https://example.invalid/")),
        ]);
        let result = kimi(home.path());
        worker.join().unwrap();
        assert_eq!(result["status"], "ok", "{result}");
        assert!(result.get("detail").is_none(), "{result}");
    }

    #[test]
    fn an_invalid_test_base_leaves_the_kimi_cli_base_rule_in_charge() {
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let port = server.local_addr().unwrap().port();
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, Some(&format!("http://127.0.0.2:{port}"))),
            (CLI_BASE_VAR, Some(&format!("https://127.0.0.1:{port}"))),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "unavailable", "{result}");
        assert!(
            result["detail"]
                .as_str()
                .unwrap()
                .contains("Ignored KIMI_CODE_BASE_URL"),
            "{result}"
        );
        assert!(
            server.accept().is_err(),
            "the invalid test base or the CLI base received the token"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_default_kimi_path_keeps_its_curl_arguments_and_stdin() {
        let _lock = curl_lock();
        let bin = tempfile::tempdir().unwrap();
        let _path = recording_curl(bin.path());
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, None),
            (CLI_BASE_VAR, None),
            ("CURL_RECORD_DIR", Some(bin.path().to_str().unwrap())),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "ok", "{result}");
        let argv = fs::read_to_string(bin.path().join("argv.0")).unwrap();
        assert_eq!(argv, KIMI_DEFAULT_ARGV);
        assert!(!argv.contains("synthetic-token"));
        assert_eq!(fs::read_to_string(bin.path().join("argv.1")).unwrap(), KIMI_DEFAULT_ARGV);
        assert_eq!(
            fs::read_to_string(bin.path().join("stdin.0")).unwrap(),
            "url = \"https://api.kimi.com/coding/v1/me\"\nheader = \"Authorization: Bearer synthetic-token\"\nheader = \"Accept: application/json\"\n"
        );
        assert_eq!(
            fs::read_to_string(bin.path().join("stdin.1")).unwrap(),
            "url = \"https://api.kimi.com/coding/v1/usages\"\nheader = \"Authorization: Bearer synthetic-token\"\nheader = \"Accept: application/json\"\n"
        );
        assert_eq!(result["identity"]["name"], "Provider");
        assert_eq!(result["windows"][0]["used_percent"], 50.0);
    }

    #[cfg(unix)]
    #[test]
    fn an_invalid_test_base_falls_through_to_the_provider_rule() {
        let _lock = curl_lock();
        let bin = tempfile::tempdir().unwrap();
        let _path = recording_curl(bin.path());
        let home = kimi_home("synthetic-token", now() + 3600);
        let _env = EnvVars::set(&[
            (TEST_BASE_VAR, Some("http://127.0.0.2:9")),
            (CLI_BASE_VAR, None),
            ("CURL_RECORD_DIR", Some(bin.path().to_str().unwrap())),
        ]);
        let result = kimi(home.path());
        assert_eq!(result["status"], "ok", "{result}");
        assert!(
            result["detail"]
                .as_str()
                .unwrap()
                .contains("Ignored HGS_TEST_KIMI_API_BASE"),
            "{result}"
        );
        assert_eq!(
            fs::read_to_string(bin.path().join("argv.0")).unwrap(),
            KIMI_DEFAULT_ARGV
        );
    }

    #[test]
    fn native_kimi_children_and_sessions_never_inherit_the_test_base() {
        let _lock = curl_lock();
        let home = tempfile::tempdir().unwrap();
        let _env = EnvVars::set(&[(TEST_BASE_VAR, Some("http://127.0.0.1:9"))]);
        for native in [true, false] {
            let command = native_command("kimi", home.path(), native);
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == std::ffi::OsStr::new(TEST_BASE_VAR)
                        && value.is_none()),
                "native_command({native}) passes the test base"
            );
        }
        let environment = crate::accounts::run_environment(
            "work-account",
            "kimi",
            "/private/profile",
        );
        assert!(
            environment.contains(&format!("{TEST_BASE_VAR}=")),
            "{environment:?}"
        );
    }

    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum CurlrcProbe {
        None,
        Proxy,
        ConnectTo,
        Resolve,
    }

    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum Decoy {
        Ephemeral,
        EchoPort,
    }

    /// One report row: the stand-in must receive `/me` and `/usages` with the
    /// token directly, and the second listener must receive nothing.
    #[cfg(unix)]
    fn direct_request(
        label: &str,
        host: &str,
        https: bool,
        proxy_var: Option<&'static str>,
        curlrc: CurlrcProbe,
        decoy: Decoy,
    ) {
        let echo = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        echo.set_nonblocking(true).unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        let decoy = match decoy {
            Decoy::Ephemeral => std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap(),
            Decoy::EchoPort => std::net::TcpListener::bind(("127.0.0.2", echo_port)).unwrap(),
        };
        decoy.set_nonblocking(true).unwrap();
        let decoy_address = decoy.local_addr().unwrap();
        let proxy = format!("http://{decoy_address}");
        let base = format!(
            "{}://{host}:{echo_port}",
            if https { "https" } else { "http" }
        );
        let config = match curlrc {
            CurlrcProbe::None => String::new(),
            CurlrcProbe::Proxy => format!("proxy = \"{proxy}\"\n"),
            CurlrcProbe::ConnectTo => format!("connect-to = \"::{decoy_address}\"\n"),
            CurlrcProbe::Resolve => {
                format!("resolve = \"localhost:{echo_port}:127.0.0.2\"\n")
            }
        };
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join(".curlrc"), &config).unwrap();
        let config_path = home.path().to_str().unwrap().to_owned();
        let account = kimi_home("synthetic-token", now() + 3600);
        let worker = if https {
            tls_worker(echo)
        } else {
            identity_worker(echo)
        };
        // CURL_HOME rather than HOME: curl prefers it for `.curlrc`, and the
        // suite's other tests read the real HOME. XDG has no config file here.
        let mut vars: Vec<(&'static str, Option<&str>)> = vec![
            ("CURL_HOME", Some(&config_path)),
            ("XDG_CONFIG_HOME", Some(&config_path)),
            ("http_proxy", None),
            ("HTTP_PROXY", None),
            ("https_proxy", None),
            ("HTTPS_PROXY", None),
            ("all_proxy", None),
            ("ALL_PROXY", None),
            ("no_proxy", None),
            ("NO_PROXY", None),
            (TEST_BASE_VAR, Some(&base)),
            (CLI_BASE_VAR, None),
        ];
        if let Some(var) = proxy_var {
            vars.push((var, Some(&proxy)));
        }
        let _env = EnvVars::set(&vars);
        let result = kimi(account.path());
        drop(_env);
        worker.join().unwrap();
        if https {
            assert_ne!(result["status"], "ok", "{label}: {result}");
        } else {
            assert_eq!(result["status"], "ok", "{label}: {result}");
            assert_eq!(result["identity"]["name"], "Loop", "{label}: {result}");
        }
        no_connection(&decoy, label);
    }

    #[cfg(unix)]
    mod proxy_rows {
        use super::*;

        // Each ambient setting must reach the stand-in directly and leave the
        // distinct decoy endpoint empty. Five rows use separate ports on the
        // configured loopback address; --resolve requires another address on
        // the same port. A row that never exercises its setting is no test.

        #[test]
        fn http_proxy_cannot_route_the_test_base() {
            let _lock = curl_lock();
            direct_request(
                "http_proxy",
                "127.0.0.1",
                false,
                Some("http_proxy"),
                CurlrcProbe::None,
                Decoy::Ephemeral,
            );
        }

        #[test]
        fn all_proxy_cannot_route_the_test_base() {
            let _lock = curl_lock();
            direct_request(
                "ALL_PROXY with localhost",
                "localhost",
                false,
                Some("ALL_PROXY"),
                CurlrcProbe::None,
                Decoy::Ephemeral,
            );
        }

        #[test]
        fn https_proxy_cannot_tunnel_the_test_base() {
            let _lock = curl_lock();
            direct_request(
                "HTTPS_PROXY with https localhost",
                "localhost",
                true,
                Some("HTTPS_PROXY"),
                CurlrcProbe::None,
                Decoy::Ephemeral,
            );
        }

        #[test]
        fn a_curlrc_proxy_cannot_route_the_test_base() {
            let _lock = curl_lock();
            direct_request(
                "curlrc proxy",
                "127.0.0.1",
                false,
                None,
                CurlrcProbe::Proxy,
                Decoy::Ephemeral,
            );
        }

        #[test]
        fn a_curlrc_connect_to_cannot_move_the_test_base() {
            let _lock = curl_lock();
            direct_request(
                "curlrc connect-to",
                "localhost",
                false,
                None,
                CurlrcProbe::ConnectTo,
                Decoy::Ephemeral,
            );
        }

        #[test]
        #[cfg_attr(
            target_os = "macos",
            ignore = "curl --resolve needs a second loopback address on the same port; standard macOS has no configured 127.0.0.2"
        )]
        fn a_curlrc_resolve_cannot_move_the_test_base() {
            let _lock = curl_lock();
            direct_request(
                "curlrc resolve",
                "localhost",
                false,
                None,
                CurlrcProbe::Resolve,
                Decoy::EchoPort,
            );
        }
    }

    #[test]
    fn hostile_test_bases_are_ignored_without_a_request() {
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let port = server.local_addr().unwrap().port();
        let home = kimi_home("synthetic-token", now() + 3600);
        for value in [
            // Userinfo, which curl would read as `localhost:1` and send the
            // token to `127.0.0.2`. M1 in the review.
            format!("http://localhost:1@127.0.0.2:{port}"),
            // Only the `@` check refuses this one: without it the authority
            // parses as loopback and curl would send `/@` with the token.
            format!("http://127.0.0.1:{port}/@"),
            // A trailing dot and a longer name curl would resolve. M3.
            format!("http://localhost.:{port}"),
            "http://localhost.evil.example".to_string(),
            // A global IPv6 address that ends with `::1`. M5.
            "http://[2001:db8::1]".to_string(),
            // Malformed after the host or the bracket, all refused by curl.
            format!("http://127.0.0.1:{port}:{port}"),
            "http://[::1]evil.example".to_string(),
            "http://127.0.0.1:1evil.example".to_string(),
            "http://[::1]:1evil".to_string(),
            "http://127.0.0.1:".to_string(),
            "http://[::1]:".to_string(),
            "http://127.0.0.1:65536".to_string(),
            "http://127.0.0.1:99999".to_string(),
            "http://[::1]:65536".to_string(),
            "http://127.0.0.1:+80".to_string(),
            "ftp://127.0.0.1/".to_string(),
            "https://user@127.0.0.1/".to_string(),
            "http://127.0.0.1/me?x=1".to_string(),
            "http://127.0.0.1/#x".to_string(),
            " http://127.0.0.1/".to_string(),
        ] {
            let _env = EnvVars::set(&[
                (TEST_BASE_VAR, Some(&value)),
                (CLI_BASE_VAR, None),
            ]);
            let result = kimi(home.path());
            assert_eq!(result["status"], "unavailable", "{value}: {result}");
            assert!(
                result["detail"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Ignored HGS_TEST_KIMI_API_BASE"),
                "{value}: {result}"
            );
        }
        assert!(
            server.accept().is_err(),
            "a hostile test base sent the token"
        );
    }

    #[test]
    fn claude_429_headers_survive_curl_refusing_an_oversized_error_body() {
        // The Kimi tests below replace PATH, CURL_HOME and the proxy
        // variables to record and redirect curl; every test that starts curl
        // shares this lock.
        let _lock = curl_lock();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/usage", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0]; stream.read_exact(&mut byte).unwrap(); request.push(byte[0]);
            }
            write!(stream, "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1000\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n").unwrap();
        });
        let before = now();
        let reply = claude_http(&url, "synthetic-token");
        assert!(matches!(reply, claude_usage::Reply::RateLimited(t) if t >= before + 1000));
        worker.join().unwrap();
    }

    #[test]
    fn claude_http_fixture_preserves_429_cooldown_without_repeating_the_request() {
        use std::net::TcpListener;
        let _lock = curl_lock();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/usage", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for (status, headers, body) in [
                ("200 OK", "", r#"{"five_hour":{"utilization":28,"resets_at":"2030-01-01T00:00:00Z"}}"#),
                ("429 Too Many Requests", "Retry-After: 100\r\n", r#"{"error":{"type":"rate_limit_error"}}"#),
            ] {
                let (mut stream, _) = server.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0]; stream.read_exact(&mut byte).unwrap(); request.push(byte[0]);
                }
                assert!(String::from_utf8(request).unwrap().contains("Authorization: Bearer synthetic-token"));
                write!(stream, "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let sampled = now().saturating_sub(360);
        let first = claude_usage::get(dir.path(), "fixture", &Value::Null, || sampled, || claude_http(&url, "synthetic-token")).unwrap();
        let limited = claude_usage::get(dir.path(), "fixture", &Value::Null, now, || claude_http(&url, "synthetic-token")).unwrap();
        assert_eq!(limited["status"], "rate_limited");
        assert_eq!(limited["checked_at"], sampled);
        assert_eq!(limited["windows"], first["windows"]);
        assert!(limited["retry_at"].as_u64().unwrap() >= now() + 99);
        worker.join().unwrap(); // Only the original two HTTP requests exist.
        let again = claude_usage::get(dir.path(), "fixture", &Value::Null, now, || panic!("manual refresh repeated HTTP")).unwrap();
        assert_eq!(again, limited);
    }

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

    #[test]
    fn listing_reuses_a_cached_deepseek_api_key_balance() {
        let root = tempfile::tempdir().unwrap();
        let config = Config {
            home: root.path().to_string_lossy().into(),
            dir: root.path().join("config"),
            state: root.path().join("state"),
            selves: vec!["fixture".into()],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        let home = root.path().join("dsh");
        private_dir(&config.dir.join("account-status")).unwrap();
        let mut data = json!({"status":"configured","api_key_configured":true,"checked_at":100,
            "identity":{"auth_method":"API key","account_id":"api-key:abc"},"windows":[],
            "api_balance":{"is_available":false,"balance_infos":[
                {"currency":"USD","total_balance":"0.00","granted_balance":"0.00","topped_up_balance":"0.00"}]}});
        super::super::deepseek::project_api_balance(&mut data);
        let mut cache = tempfile::NamedTempFile::new_in(config.dir.join("account-status")).unwrap();
        platform::make_private(cache.as_file()).unwrap();
        cache
            .write_all(
                json!({"signature":signature(&home,"dsh"),"data":data})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        platform::persist(cache, cache_path(&config, "native-dsh", &home)).unwrap();
        let status = catalog_status(&config, "native-dsh", "dsh", &home);
        assert_eq!(status["status"], "configured");
        assert_eq!(status["balance_available"], false);
        assert_eq!(
            status["balances"][0],
            json!({"kind":"Total","balance":"0.00","currency":"USD"})
        );
        assert_eq!(status["balances"].as_array().unwrap().len(), 3);
        assert!(status.get("api_balance").is_none());
    }

    fn jwt(claims: Value) -> String {
        use base64::Engine;
        let part = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
        format!("{}.{}.signature", part(json!({"alg":"none"})), part(claims))
    }

    #[test]
    fn kimi_global_region_reads_its_scoped_credentials_and_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("credentials")).unwrap();
        fs::write(home.join("config.toml"), r#"[providers."managed:kimi-code"]
type = "kimi"
base_url = "https://api.kimi.ai/coding/v1"
[providers."managed:kimi-code".oauth]
storage = "file"
key = "oauth/kimi-code-env-0e4f99c69cc27850"
oauth_host = "https://auth.kimi.ai"
"#).unwrap();
        let environment = kimi_environment(home);
        assert_eq!(environment.credentials, "credentials/kimi-code-env-0e4f99c69cc27850.json");
        assert_eq!(environment.base_url, "https://api.kimi.ai/coding/v1");
        // The scoped file's user decides the signature; the default file does not.
        let token = |user: &str| {
            use base64::Engine;
            let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({"user_id": user}).to_string());
            format!("e30.{claims}.sig")
        };
        fs::write(home.join(&environment.credentials), json!({"access_token": token("user-a")}).to_string()).unwrap();
        fs::write(home.join("credentials/kimi-code.json"), r#"{"access_token":"old-default"}"#).unwrap();
        let first = signature(home, "kimi");
        fs::write(home.join("credentials/kimi-code.json"), r#"{"access_token":"changed-default"}"#).unwrap();
        assert_eq!(signature(home, "kimi"), first);
        fs::write(home.join(&environment.credentials), json!({"access_token": token("user-b")}).to_string()).unwrap();
        assert_ne!(signature(home, "kimi"), first);
        // A malformed key never leaves the credentials folder.
        fs::write(home.join("config.toml"), "[providers.\"managed:kimi-code\".oauth]\nkey = \"oauth/../secret\"\n").unwrap();
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
