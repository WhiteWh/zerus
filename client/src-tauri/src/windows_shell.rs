//! Windows parity decisions shared by the native adapter and its unit tests.
//! Qt: AppConfig::chooseHgs/restartArguments and SwarmWorkerSupervisor.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct HgsChoice {
    pub path: PathBuf,
    pub refresh: bool,
    pub fallback: PathBuf,
}

fn version(value: &str) -> Option<semver::Version> {
    semver::Version::parse(value).ok()
}

/// The session-host enabled marker is authoritative, not leftover staged files.
/// An explicit CLI selector never stages or switches to another executable.
pub fn choose_hgs(
    status: &Value,
    bundled: &Path,
    digest: &str,
    configured: Option<&Path>,
) -> HgsChoice {
    let choice = |path: PathBuf, refresh, fallback: PathBuf| HgsChoice {
        path,
        refresh,
        fallback,
    };
    if let Some(path) = configured {
        return choice(path.into(), false, path.into());
    }
    let cli = &status["cli"];
    let Some(path) = cli["path"].as_str().filter(|p| !p.is_empty()) else {
        return choice(bundled.into(), false, bundled.into());
    };
    if status["enabled"] != true {
        return choice(bundled.into(), false, bundled.into());
    }
    let stable = PathBuf::from(path);
    let staged = cli["sha256"].as_str().unwrap_or_default();
    if cli["installed"] != true || staged.is_empty() {
        return choice(stable, true, bundled.into());
    }
    let differs = !digest.is_empty() && !digest.eq_ignore_ascii_case(staged);
    let newer = version(status["version"].as_str().unwrap_or_default()).is_some_and(|current| {
        version(cli["version"].as_str().unwrap_or_default())
            .is_none_or(|prior| current.cmp_precedence(&prior).is_ge())
    });
    choice(stable.clone(), differs && newer, stable)
}

/// Value options are walked as pairs, so a value spelling --sessions survives.
pub fn restart_arguments(args: &[String]) -> Vec<String> {
    let valued = [
        "--hgs",
        "--local-poll",
        "--peer-poll",
        "--terminal",
        "--icon-color",
        "--open-mode",
    ];
    let mut out = vec!["--background".into()];
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if valued.contains(&arg.as_str()) {
            out.push(arg.clone());
            if let Some(value) = args.next() {
                out.push(value.clone());
            }
        } else if ![
            "--sessions",
            "--background",
            "--install-login-item",
            "--remove-login-item",
        ]
        .contains(&arg.as_str())
        {
            out.push(arg.clone());
        }
    }
    out
}

/// Windows tray clicks do not identify a message; Qt also routes only the latest.
#[derive(Default)]
pub struct NoticeClicks(Option<String>);
impl NoticeClicks {
    pub fn raised(&mut self, token: String) {
        self.0 = Some(token);
    }
    pub fn cleared(&mut self, tokens: &[String]) {
        if self.0.as_ref().is_some_and(|token| tokens.contains(token)) {
            self.0 = None;
        }
    }
    pub fn clicked(&self) -> Option<String> {
        self.0.clone()
    }
}

pub fn restart_delay(previous: u64, uptime: std::time::Duration) -> u64 {
    if previous == 0 || uptime.as_secs() >= 60 {
        1000
    } else {
        (previous * 2).min(300_000)
    }
}

/// Windows native strings are bounded in UTF-16 without splitting a surrogate.
pub fn wide_text<const N: usize>(text: &str) -> [u16; N] {
    let mut out = [0; N];
    let mut at = 0;
    for c in text.chars().filter(|c| *c != '\0') {
        if at + c.len_utf16() >= N {
            break;
        }
        let mut units = [0; 2];
        for unit in c.encode_utf16(&mut units) {
            out[at] = *unit;
            at += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[cfg(windows)]
    #[test]
    fn library_test_image_embeds_common_controls_v6() {
        use windows_sys::Win32::{System::LibraryLoader::*, UI::WindowsAndMessaging::RT_MANIFEST};
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            // MAKEINTRESOURCEW(1) encodes an integer, not an aligned string pointer.
            let resource =
                FindResourceW(module, std::ptr::without_provenance::<u16>(1), RT_MANIFEST);
            assert!(
                !resource.is_null(),
                "unit-test image must carry its own manifest"
            );
            let size = SizeofResource(module, resource) as usize;
            assert!((1..=16_384).contains(&size));
            let loaded = LoadResource(module, resource);
            let pointer = LockResource(loaded).cast::<u8>();
            assert!(!pointer.is_null());
            let manifest = std::str::from_utf8(std::slice::from_raw_parts(pointer, size)).unwrap();
            assert!(manifest.contains("Microsoft.Windows.Common-Controls"));
            assert!(manifest.contains("6.0.0.0"));
        }
    }
    #[test]
    fn only_an_enabled_host_stages_and_explicit_selectors_are_preserved() {
        let bin = Path::new("C:/Zerus/hgs.exe");
        let status = json!({"enabled":true,"version":"1.56.0","cli":{"path":"C:/State/hgs.exe","installed":true,"sha256":"old","version":"1.55.0"}});
        let choice = choose_hgs(&status, bin, "new", None);
        assert!(choice.refresh);
        assert_eq!(choice.path, Path::new("C:/State/hgs.exe"));
        let mut older = status.clone();
        older["version"] = json!("1.54.0");
        assert!(!choose_hgs(&older, bin, "new", None).refresh);
        older["version"] = json!("1.56.0-dev.9");
        older["cli"]["version"] = json!("1.56.0-dev.10");
        assert!(!choose_hgs(&older, bin, "new", None).refresh);
        older["enabled"] = json!(false);
        assert_eq!(choose_hgs(&older, bin, "new", None).path, bin);
        let explicit = Path::new("D:/Pinned/hgs.exe");
        let choice = choose_hgs(&status, bin, "new", Some(explicit));
        assert!(!choice.refresh);
        assert_eq!(choice.path, explicit);
        let mut absent = status;
        absent["cli"]["installed"] = json!(false);
        assert_eq!(choose_hgs(&absent, bin, "", None).fallback, bin);
    }
    #[test]
    fn restart_keeps_selector_and_values_but_never_opens_the_window() {
        let args = [
            "--sessions",
            "--hgs",
            "C:/Program Files/Zerus/hgs.exe",
            "--terminal",
            "--sessions",
        ]
        .map(str::to_owned);
        assert_eq!(
            restart_arguments(&args),
            [
                "--background",
                "--hgs",
                "C:/Program Files/Zerus/hgs.exe",
                "--terminal",
                "--sessions"
            ]
        );
    }
    #[test]
    fn worker_backoff_is_bounded_and_healthy_runs_reset_it() {
        use std::time::Duration;
        assert_eq!(restart_delay(0, Duration::ZERO), 1000);
        assert_eq!(restart_delay(1000, Duration::ZERO), 2000);
        assert_eq!(restart_delay(300_000, Duration::ZERO), 300_000);
        assert_eq!(restart_delay(300_000, Duration::from_secs(60)), 1000);
    }
    #[test]
    fn withdrawn_notices_cannot_route_and_replacement_uses_the_latest_token() {
        let mut clicks = NoticeClicks::default();
        assert_eq!(clicks.clicked(), None);
        clicks.raised("old".into());
        clicks.raised("current".into());
        clicks.cleared(&["old".into()]);
        assert_eq!(clicks.clicked().as_deref(), Some("current"));
        clicks.cleared(&["current".into()]);
        assert_eq!(clicks.clicked(), None);
    }
    #[test]
    fn windows_text_limits_preserve_unicode_and_termination() {
        assert_eq!(wide_text::<4>("a😀z"), [97, 0xd83d, 0xde00, 0]);
        assert_eq!(wide_text::<3>("ab😀"), [97, 98, 0]);
        assert_eq!(wide_text::<3>("a\0b"), [97, 98, 0]);
    }
}
