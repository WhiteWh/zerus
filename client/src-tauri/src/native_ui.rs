//! Pure policy for the native workspace, following `NativeUiView.cpp` and
//! `NativeUiPlugin.cpp`. This module creates no webviews, invokes no CLI and
//! grants no IPC. Runtime integration must preserve these decisions.

use sha2::{Digest, Sha256};
use std::fmt;
use tauri::Url;

#[cfg(all(target_os = "linux", feature = "live"))]
pub(crate) mod linux;
#[cfg(feature = "live")]
pub(crate) mod runtime;

/// Bound the private launch address before parsing or hashing it.
pub const ADDRESS_LIMIT: usize = 16 * 1024;

/// A validated launch generation. The URL contains a private credential and
/// must never enter logs, frontend status, or persistent session metadata.
#[derive(Clone)]
pub struct Launch {
    url: Url,
    fingerprint: String,
}

impl fmt::Debug for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Launch")
            .field("origin", &self.url.origin().ascii_serialization())
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// A decision only: the caller must enforce the actual navigation or guarded
/// browser handoff. It must not infer a user gesture from a URL or a redirect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    Allow,
    Browser,
    Deny,
}

impl Launch {
    /// Qt's exact lexical loopback shape, with a nonzero u16 port. Do not
    /// normalize an untrusted URL before checking its original spelling.
    pub fn parse(address: &str) -> Option<Self> {
        if address.len() > ADDRESS_LIMIT {
            return None;
        }
        let rest = address.strip_prefix("http://127.0.0.1:")?;
        let (port, credential) = rest.split_once("/?token=")?;
        if port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().ok()? == 0
            || credential.is_empty()
            || !credential
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_%.-".contains(&byte))
        {
            return None;
        }
        Some(Self {
            url: Url::parse(address).ok()?,
            // Qt hashes the exact returned address. Even equivalent URL
            // spellings must not revive another retained page or profile.
            fingerprint: format!("{:x}", Sha256::digest(address.as_bytes())),
        })
    }

    /// For the native renderer only; never serialize this into app state.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Qt's computer, origin and exact launch-credential generation. This is
    /// cache identity only, not proof of the current selected session.
    pub fn key(&self, computer: &str) -> String {
        format!(
            "{computer}\n{}\n{}",
            self.url.origin().ascii_serialization(),
            self.fingerprint
        )
    }

    fn same_origin(&self, next: &Url) -> bool {
        next.scheme() == self.url.scheme()
            && next.username().is_empty()
            && next.password().is_none()
            && next.origin() == self.url.origin()
    }

    /// Main-frame policy. External browser handoff is reserved for an actual
    /// clicked HTTP(S) link; script redirects never open external programs.
    pub fn navigation(&self, next: &Url, user_initiated: bool) -> Navigation {
        if next.as_str() == "about:blank" || self.same_origin(next) {
            Navigation::Allow
        } else {
            Self::external(next, user_initiated)
        }
    }

    /// A same-origin popup loads in the existing child, never a new window.
    /// The caller denies creation even when this returns `Allow`.
    pub fn new_window(&self, next: &Url, user_initiated: bool) -> Navigation {
        if self.same_origin(next) {
            Navigation::Allow
        } else {
            Self::external(next, user_initiated)
        }
    }

    fn external(next: &Url, user_initiated: bool) -> Navigation {
        if user_initiated
            && matches!(next.scheme(), "http" | "https")
            && next.host_str().is_some()
            && next.username().is_empty()
            && next.password().is_none()
        {
            Navigation::Browser
        } else {
            Navigation::Deny
        }
    }
}

/// All app commands must reject native children, even for a loopback page.
/// Runtime wiring and capabilities are reviewed separately from this policy.
pub fn app_commands_allowed(webview_label: &str) -> bool {
    webview_label == crate::MAIN_WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch::parse("http://127.0.0.1:43210/?token=fixture_one").unwrap()
    }

    #[test]
    fn only_qts_bounded_loopback_launch_shape_is_accepted() {
        for good in [
            "http://127.0.0.1:1/?token=a",
            "http://127.0.0.1:65535/?token=a_B-1.c%3D",
        ] {
            assert!(Launch::parse(good).is_some(), "{good}");
        }
        for bad in [
            "http://127.0.0.1:0/?token=a",
            "http://127.0.0.1:65536/?token=a",
            "http://127.0.0.1:-1/?token=a",
            "http://127.0.0.1:/?token=a",
            "https://127.0.0.1:4321/?token=a",
            "http://localhost:4321/?token=a",
            "http://127.1:4321/?token=a",
            "http://[::1]:4321/?token=a",
            "http://127.0.0.1:4321/x/../?token=a",
            "http://127.0.0.1:4321/?token=",
            "http://127.0.0.1:4321/?token=a b",
            "http://127.0.0.1:4321/?token=a\n",
            "http://127.0.0.1:4321/?token=α",
            "http://127.0.0.1:4321/?token=a&other=b",
            "http://127.0.0.1:4321/?token=a#fragment",
            "http://user@127.0.0.1:4321/?token=a",
        ] {
            assert!(Launch::parse(bad).is_none(), "{bad:?}");
        }
        assert!(Launch::parse(&format!(
            "http://127.0.0.1:1/?token={}",
            "a".repeat(ADDRESS_LIMIT)
        ))
        .is_none());
    }

    #[test]
    fn computer_and_exact_address_generation_separate_retained_pages() {
        let first = launch();
        let other = Launch::parse("http://127.0.0.1:43210/?token=fixture_two").unwrap();
        assert_ne!(first.key(""), first.key("peer"));
        assert_ne!(first.key(""), other.key(""));
        let spelling_a = Launch::parse("http://127.0.0.1:80/?token=fixture_one").unwrap();
        let spelling_b = Launch::parse("http://127.0.0.1:080/?token=fixture_one").unwrap();
        assert_eq!(spelling_a.url(), spelling_b.url());
        assert_ne!(spelling_a.key(""), spelling_b.key(""));
        assert!(!first.key("").contains("fixture_one"));
        assert!(!format!("{first:?}").contains("fixture_one"));
    }

    #[test]
    fn same_origin_stays_and_only_clicked_external_web_links_reach_the_browser() {
        let launch = launch();
        for address in ["about:blank", "http://127.0.0.1:43210/settings"] {
            assert_eq!(
                launch.navigation(&Url::parse(address).unwrap(), false),
                Navigation::Allow
            );
        }
        let external = Url::parse("https://example.com/docs").unwrap();
        assert_eq!(launch.navigation(&external, false), Navigation::Deny);
        assert_eq!(launch.navigation(&external, true), Navigation::Browser);
        let changed_port = Url::parse("http://127.0.0.1:43211/").unwrap();
        assert_eq!(launch.navigation(&changed_port, false), Navigation::Deny);
        for address in [
            "file:///tmp/fixture",
            "tauri://localhost",
            "asset://localhost/file",
            "ipc://localhost/metrics",
            "javascript:alert(1)",
            "data:text/html,fixture",
            "blob:http://127.0.0.1:43210/fixture",
            "http://user@127.0.0.1:43210/",
        ] {
            assert_eq!(
                launch.navigation(&Url::parse(address).unwrap(), true),
                Navigation::Deny
            );
        }
    }

    #[test]
    fn popups_load_same_origin_in_place_and_never_bypass_the_gesture_check() {
        let launch = launch();
        assert_eq!(
            launch.new_window(
                &Url::parse("http://127.0.0.1:43210/settings").unwrap(),
                false
            ),
            Navigation::Allow
        );
        let external = Url::parse("https://example.com/docs").unwrap();
        assert_eq!(launch.new_window(&external, false), Navigation::Deny);
        assert_eq!(launch.new_window(&external, true), Navigation::Browser);
        assert_eq!(
            launch.new_window(&Url::parse("about:blank").unwrap(), true),
            Navigation::Deny
        );
    }

    #[test]
    fn only_the_exact_main_webview_may_call_app_commands() {
        assert!(app_commands_allowed("main"));
        for label in ["", "native-ui-1", "main-child", "main\n", "MAIN"] {
            assert!(!app_commands_allowed(label));
        }
    }
}
