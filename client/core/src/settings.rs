//! The client's local preferences (Settings, phase 3b; see
//! `docs/superpowers/plans/2026-10-28-tauri-client-phase3b-settings.md`).
//!
//! They live in `<data_dir>/settings.json` under Qt's QSettings names, with
//! Qt's defaults and ranges (tray/src/SettingsPage.h and the headers it
//! reads). Under the switch identity the store is seeded once, read-only,
//! from Qt's own store; Qt's store is never written, so a rollback finds Qt as
//! it was. Loading, seeding and writing run on the store's own thread.

mod capture;
pub mod geometry;
pub use capture::{CapturedQt, CapturedSettings};
mod imports;
pub mod qt;
mod qt_layout;
pub mod repository;
pub use imports::OrganizationImport;
pub(crate) use imports::{family_metadata, valid_family_metadata};

use qt::QtValue;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How a setting's value is read, as Qt reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// `toBool()`.
    Flag(bool),
    /// A device-local bool whose unset value follows the active skin.
    AutoFlag,
    /// `toString()`: one of the choices, otherwise the first (Qt's
    /// `findData` falling back to index 0, or `== "ru" ? "ru" : "en"`).
    Choice(&'static [&'static str]),
    /// `toDouble()` (0 when it fails), then `qBound(min, value, max)`.
    Real { default: f64, min: f64, max: f64 },
    /// `toInt(&ok)`: the default when it fails, otherwise `qBound`.
    Whole { default: i64, min: i64, max: i64 },
    /// `toLongLong()` (0 when it fails), at least `min`.
    Long { default: i64, min: i64 },
    /// `toString()`.
    Text(&'static str),
    /// `toByteArray()`, held as UTF-8 JSON text (the verified update catalogs).
    Bytes,
    /// `toStringList()`: the shortcut group, one list of portable chord
    /// strings per command id, an empty list meaning no chord.
    Chords,
    /// Device-local row density: at most 50,000 Qt-compatible SHA-256 keys.
    RowKeys,
}

/// One setting: its QSettings name, how Qt reads it, and whether the
/// switch seeds it from Qt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
    pub seeded: bool,
}

const fn key(name: &'static str, kind: Kind) -> Key {
    Key {
        name,
        kind,
        seeded: true,
    }
}

/// A Next preference excluded from the original switch seed. Explicit new
/// G3 families may import compatible Qt values through their own journal.
const fn client(name: &'static str, kind: Kind) -> Key {
    Key {
        name,
        kind,
        seeded: false,
    }
}

/// The prefix of the client's own settings.
pub const CLIENT: &str = "client/";

/// The group of shortcut overrides (`shortcuts/<command id>`, Shortcuts.h).
pub const SHORTCUTS: &str = "shortcuts/";

/// Qt's value of `automaticApprovals/enabled` at the switch, kept only so the
/// page can say Qt had it on. The client never enables automatic approvals
/// from it: the opt-in stays explicit (zerus-5wmh.54).
pub const QT_AUTOMATIC_APPROVALS: &str = "automaticApprovals/qtEnabledAtSwitch";

/// Every local setting the client has. Sources are in the plan's table.
pub const KEYS: &[Key] = &[
    // Dangerous Next opt-in: never seed Qt's enabled value or transient error.
    Key {
        name: crate::approvals::automatic::ENABLED,
        kind: Kind::Flag(false),
        seeded: false,
    },
    Key {
        name: crate::approvals::automatic::ERROR,
        kind: Kind::Text(""),
        seeded: false,
    },
    // Appearance (SettingsPage.h:46-95, UiLanguage.h, ContentScale.h,
    // ActivityWidth.h, WindowLayer.cpp).
    key(
        "workspace/theme",
        Kind::Choice(&["system", "dark", "light"]),
    ),
    key("workspace/language", Kind::Choice(&["en", "ru"])),
    key(
        "workspace/usageIndicator",
        Kind::Choice(&["rails", "marker", "marker-countdown"]),
    ),
    key(
        "workspace/contentScale",
        Kind::Real {
            default: 1.0,
            min: 0.75,
            max: 2.0,
        },
    ),
    key(
        "workspace/activityWidth",
        Kind::Whole {
            default: 900,
            min: 480,
            max: 2400,
        },
    ),
    key("workspace/activityFullWidth", Kind::Flag(false)),
    key("workspace/alwaysOnTop", Kind::Flag(false)),
    // Sessions (SettingsPage.h:97-107, QuestionNotificationSettings.h).
    key("workspace/compact", Kind::Flag(false)),
    key("workspace/expandSessionsOnHover", Kind::Flag(true)),
    key("workspace/hideEmptyProjects", Kind::Flag(true)),
    key("workspace/animateActivity", Kind::Flag(true)),
    key("workspace/showCompletedSubagents", Kind::Flag(false)),
    key("workspace/reduceMotion", Kind::Flag(false)),
    key("notifications/optionalQuestions", Kind::Flag(false)),
    // A remembered Terminate checkbox, never maintenance/removal consent.
    Key {
        name: "workspace/removeWorktreeAfterTermination",
        kind: Kind::Flag(false),
        seeded: false,
    },
    // TasksView's local scope switch.
    // A first-G3 family also covers clients whose original seed predates Tasks.
    Key {
        name: "tasks/onlySession",
        kind: Kind::Flag(false),
        seeded: false,
    },
    // New session's remembered choices (NewSessionDialog.cpp:845-851,
    // SessionCreationController.cpp:244-249). launchAccounts is compact JSON
    // stored by Qt as a QByteArray, read as text by the single Qt seed.
    key("workspace/launchAgent", Kind::Text("")),
    key("workspace/launchOpenTerminal", Kind::Flag(false)),
    key("workspace/launchAccounts", Kind::Text("")),
    // A first-G3 family import handles this separately from the old seed.
    Key {
        name: "workspace/allSwarmProjects",
        kind: Kind::Flag(false),
        seeded: false,
    },
    Key {
        name: crate::row_preferences::KEY,
        kind: Kind::RowKeys,
        seeded: false,
    },
    // Processes (ProcessSettings.h), web client phase 4 Task 11.
    key("processes/enabled", Kind::Flag(false)),
    key(
        "processes/activeSeconds",
        Kind::Real {
            default: 2.5,
            min: 1.0,
            max: 60.0,
        },
    ),
    key(
        "processes/backgroundSeconds",
        Kind::Real {
            default: 30.0,
            min: 5.0,
            max: 600.0,
        },
    ),
    key(
        "processes/unknownSeconds",
        Kind::Real {
            default: 10.0,
            min: 1.0,
            max: 600.0,
        },
    ),
    key(
        "processes/loadingSeconds",
        Kind::Real {
            default: 0.5,
            min: 0.0,
            max: 5.0,
        },
    ),
    // Voice input (speech/SpeechSettings.cpp, speech/SpeechComposer.cpp).
    // speech/microphone is not seeded: Qt keeps a QAudioDevice id, which
    // names no device of the client's.
    key("speech/preview_before_send", Kind::Flag(false)),
    key("speech/video_mode", Kind::Flag(false)),
    key("speech/manage_host", Kind::Text("")),
    // The computer Use as default chose on, and a provider change not yet
    // confirmed (speech_settings), as Qt stores them.
    key("speech/host", Kind::Text("")),
    key(crate::speech_settings::PENDING_KEY, Kind::Text("")),
    key("speech/machine_id", Kind::Text("")),
    key("speech/profile", Kind::Text("")),
    key("speech/profile_revision", Kind::Long { default: 0, min: 0 }),
    Key {
        name: "speech/microphone",
        kind: Kind::Text(""),
        seeded: false,
    },
    // Automatic recovery: the newest shared policy known (recovery_settings).
    key(crate::recovery_settings::SHARED_POLICY_KEY, Kind::Text("")),
    // Files (DownloadCache), Updates (UpdateController.cpp).
    key("downloads/folder", Kind::Text("")),
    key("downloads/cacheBytes", Kind::Long { default: 0, min: 0 }),
    key("updates/channel", Kind::Text("stable")),
    key("updates/automatic", Kind::Flag(true)),
    // Automatic approvals: Qt's choice, for the notice only.
    key(QT_AUTOMATIC_APPROVALS, Kind::Flag(false)),
    // The client's own: the skin for each appearance (the theme picks one)
    // and the Overview column (design round 7).
    client("client/darkSkin", Kind::Text("zerus-dark")),
    client("client/lightSkin", Kind::Text("zerus-light")),
    client(
        "client/overviewWidth",
        Kind::Whole {
            default: 1200,
            min: 720,
            max: 2400,
        },
    ),
    client("client/overviewFullWidth", Kind::Flag(false)),
    client("workspace/inspectorOpen", Kind::AutoFlag),
    client(
        "workspace/inspectorTab",
        Kind::Choice(&["plan", "details", "worktrees", "tasks"]),
    ),
    client(
        "workspace/inspectorWidth",
        Kind::Whole {
            default: 400,
            min: 240,
            max: 900,
        },
    ),
    client("workspace/inspectorClosed", Kind::Text("[]")),
    // New G3 families use the family journal, not the original switch seed.
    client("client/windowGeometry", Kind::Text("")),
    client(
        "workspace/sessionsWidth",
        Kind::Whole {
            default: 310,
            min: 270,
            max: 2400,
        },
    ),
    client(
        "workspace/projectFolderSplit",
        Kind::Whole {
            default: 230,
            min: 160,
            max: 4096,
        },
    ),
];

/// The setting called `name`: a listed key, a shortcut override, or one of
/// UpdateController's per-channel and per-notification records.
pub fn find(name: &str) -> Option<Key> {
    KEYS.iter()
        .copied()
        .find(|key| key.name == name)
        .or_else(|| {
            let id = name.strip_prefix(SHORTCUTS)?;
            (!id.is_empty() && !id.contains('/')).then_some(Key {
                name: SHORTCUTS,
                kind: Kind::Chords,
                seeded: true,
            })
        })
        .or_else(|| update_family(name))
        .or_else(|| machine_family(name))
        .or_else(|| project_family(name))
}

// Project IDs are flat JSON keys, never filesystem paths or machine aliases.
fn project_family(name: &str) -> Option<Key> {
    let id = name.strip_prefix("projects/vivid/")?;
    (!id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)).then_some(Key {
        name: "projects/vivid/",
        kind: Kind::Flag(false),
        seeded: false,
    })
}

fn machine_family(name: &str) -> Option<Key> {
    let (prefix, kind) = if name.starts_with("machines/colors/") {
        ("machines/colors/", Kind::Text(""))
    } else if name.starts_with("machines/vivid/") {
        ("machines/vivid/", Kind::Flag(false))
    } else {
        return None;
    };
    let alias = name.strip_prefix(prefix)?;
    (!alias.is_empty()
        && alias.len() <= 256
        && !alias
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\'))
    .then_some(Key {
        name: "machines/",
        kind,
        seeded: false,
    })
}

fn update_family(name: &str) -> Option<Key> {
    let channel = |prefix: &str| {
        name.strip_prefix(prefix)
            .is_some_and(|channel| crate::updates::Channel::parse(channel).is_some())
    };
    let kind = if channel("updates/lastAttempt/") || channel("updates/lastSuccess/") {
        Kind::Long { default: 0, min: 0 }
    } else if channel("updates/catalog/") {
        Kind::Bytes
    } else if name
        .strip_prefix("updates/notified/")
        .is_some_and(crate::updates::is_update_token)
    {
        Kind::Flag(false)
    } else {
        return None;
    };
    Some(Key {
        name: "updates/",
        kind,
        seeded: true,
    })
}

/// A setting's value when nothing is stored.
pub fn default_value(kind: Kind) -> Value {
    match kind {
        Kind::Flag(on) => Value::Bool(on),
        Kind::AutoFlag => Value::from("auto"),
        Kind::Choice(choices) => Value::from(choices[0]),
        Kind::Real { default, .. } => json!(default),
        Kind::Whole { default, .. } | Kind::Long { default, .. } => Value::from(default),
        Kind::Text(text) => Value::from(text),
        Kind::Bytes => Value::from(""),
        Kind::Chords | Kind::RowKeys => Value::Array(Vec::new()),
    }
}

/// `value` read as Qt reads the setting, or its default.
pub fn effective(kind: Kind, value: Option<&Value>) -> Value {
    let Some(value) = value else {
        return default_value(kind);
    };
    match kind {
        Kind::Flag(on) => Value::Bool(value.as_bool().unwrap_or(on)),
        Kind::AutoFlag => value
            .as_bool()
            .map_or_else(|| Value::from("auto"), Value::Bool),
        Kind::Choice(choices) => Value::from(
            value
                .as_str()
                .filter(|text| choices.contains(text))
                .unwrap_or(choices[0]),
        ),
        Kind::Real { default, min, max } => {
            let number = value.as_f64().filter(|n| !n.is_nan()).unwrap_or(default);
            json!(number.clamp(min, max))
        }
        Kind::Whole { default, min, max } => {
            Value::from(value.as_i64().map_or(default, |n| n.clamp(min, max)))
        }
        Kind::Long { default, min } => Value::from(value.as_i64().map_or(default, |n| n.max(min))),
        Kind::Text(text) => Value::from(value.as_str().unwrap_or(text)),
        Kind::Bytes => Value::from(value.as_str().unwrap_or("")),
        Kind::Chords => Value::Array(
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(Value::from)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Kind::RowKeys => {
            if crate::row_preferences::valid(value) {
                value.clone()
            } else {
                default_value(kind)
            }
        }
    }
}

/// Qt's stored value converted the way Qt's reader converts it.
pub fn from_qt(kind: Kind, value: &QtValue) -> Value {
    let stored = match kind {
        Kind::Flag(_) | Kind::AutoFlag => Value::Bool(value.to_bool()),
        Kind::Choice(_) | Kind::Text(_) | Kind::Bytes => Value::from(value.to_text()),
        Kind::Real { .. } => {
            let (number, _) = value.to_double();
            json!(if number.is_nan() { 0.0 } else { number })
        }
        // toInt(&ok) converts through a long long and keeps its low 32 bits.
        Kind::Whole { default, .. } => match value.to_long() {
            (number, true) => Value::from(i64::from(number as i32)),
            _ => Value::from(default),
        },
        Kind::Long { .. } => Value::from(value.to_long().0),
        Kind::Chords => Value::Array(value.to_text_list().into_iter().map(Value::from).collect()),
        Kind::RowKeys => {
            crate::row_preferences::from_qt(value).unwrap_or_else(|| default_value(kind))
        }
    };
    effective(kind, Some(&stored))
}

/// Where Qt keeps its settings: the format, read-only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QtStore {
    /// macOS: Qt's preference domain, read through cfprefsd (`defaults
    /// export`), which may hold values its plist file does not have yet;
    /// the file only when that fails.
    Plist {
        domain: Option<String>,
        path: PathBuf,
    },
    /// Linux `hgs-tray.conf` and Windows `hgs-tray.ini`.
    Ini(PathBuf),
}

/// macOS's preference tool, by its full path.
const DEFAULTS: &str = "/usr/bin/defaults";

/// What a read of Qt's store found, and from where: `qt-defaults`
/// (cfprefsd), `qt-plist` (the plist file) or `qt-ini`.
pub type QtValues = (BTreeMap<String, QtValue>, &'static str);

/// One bounded read, including the hash of exactly the bytes parsed. The
/// import report stores metadata only, never source payloads or credentials.
pub struct QtSnapshot {
    pub values: BTreeMap<String, QtValue>,
    pub source: &'static str,
    pub sha256: String,
    pub modified_at: Option<u64>,
}

/// Qt's platform-specific settings location. Kept explicit so migration
/// tests can resolve all three platforms without changing process globals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QtPlatform {
    Linux,
    Mac,
    Windows,
}

impl QtStore {
    /// Qt's store on this computer (tray/src/main.cpp:178, :276-277).
    pub fn here() -> Option<QtStore> {
        let home = || {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        };
        let (platform, config) = if cfg!(windows) {
            (QtPlatform::Windows, std::env::var_os("APPDATA"))
        } else if cfg!(target_os = "macos") {
            (QtPlatform::Mac, None)
        } else {
            (QtPlatform::Linux, std::env::var_os("XDG_CONFIG_HOME"))
        };
        Self::locate(
            platform,
            home().as_deref(),
            config.as_deref().map(Path::new),
        )
    }

    /// Resolve Qt's source without opening it. `config` is XDG_CONFIG_HOME
    /// on Linux and APPDATA on Windows; macOS uses its existing domain.
    pub fn locate(
        platform: QtPlatform,
        home: Option<&Path>,
        config: Option<&Path>,
    ) -> Option<Self> {
        let config = config.filter(|path| !path.as_os_str().is_empty());
        match platform {
            QtPlatform::Linux => config
                .filter(|path| path.as_os_str().as_encoded_bytes().starts_with(b"/"))
                .map(Path::to_path_buf)
                .or_else(|| home.map(|home| home.join(".config")))
                .map(|root| Self::Ini(root.join("hgdev/hgs-tray.conf"))),
            QtPlatform::Windows => config.map(|root| Self::Ini(root.join("hgdev/hgs-tray.ini"))),
            QtPlatform::Mac => home.map(|home| Self::Plist {
                domain: Some("com.hgdev.hgs-tray".into()),
                path: home.join("Library/Preferences/com.hgdev.hgs-tray.plist"),
            }),
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            QtStore::Plist { path, .. } | QtStore::Ini(path) => path,
        }
    }

    /// A value Qt keeps in a JSON file of its own beside the store
    /// (tray/src/SettingsJson.cpp): `<store>.<name>.json`.
    pub fn json_file(&self, name: &str) -> PathBuf {
        let mut path = self.path().as_os_str().to_owned();
        path.push(format!(".{name}.json"));
        PathBuf::from(path)
    }

    /// Every value of the store by Qt name and where it was read;
    /// `Ok(None)` when the store does not exist.
    pub fn read(&self) -> Result<Option<QtValues>, String> {
        self.snapshot()
            .map(|snapshot| snapshot.map(|s| (s.values, s.source)))
    }

    pub fn snapshot(&self) -> Result<Option<QtSnapshot>, String> {
        if let QtStore::Plist {
            domain: Some(domain),
            ..
        } = self
        {
            if let Some((values, bytes)) = export_domain(domain) {
                return Ok(Some(QtSnapshot {
                    values,
                    source: "qt-defaults",
                    sha256: zerus_desktop_migration::sha256(&bytes),
                    modified_at: fs::metadata(self.path())
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(seconds),
                }));
            }
        }
        let file = match File::open(self.path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let modified_at = file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(seconds);
        let mut bytes = Vec::new();
        file.take(MAX_QT_STORE + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_QT_STORE {
            return Err("Qt store exceeds its byte limit".into());
        }
        let (values, source) = match self {
            QtStore::Plist { .. } => (qt::read_plist(&bytes)?, "qt-plist"),
            QtStore::Ini(_) => (qt::read_ini(&bytes), "qt-ini"),
        };
        Ok(Some(QtSnapshot {
            values,
            source,
            sha256: zerus_desktop_migration::sha256(&bytes),
            modified_at,
        }))
    }

    /// Native report capture. The fixed Mac export child and every original
    /// pipe retain the supplied source resources; failure never substitutes a
    /// potentially stale plist for an uncertain current defaults export.
    pub fn snapshot_retained(
        &self,
        resources: Arc<dyn Send + Sync>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<QtSnapshot>, String> {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Qt source capture stopped".into());
        }
        if let QtStore::Plist {
            domain: Some(domain),
            ..
        } = self
        {
            let invocation = crate::actions::Invocation {
                args: vec!["export".into(), domain.into(), "-".into()],
                stdin: None,
                timeout: Duration::from_secs(10),
                request_id: None,
            };
            let result = crate::run::run_streamed_retained(
                Path::new(DEFAULTS),
                &invocation,
                &[],
                MAX_QT_STORE as usize,
                cancel,
                resources,
                |_| true,
            );
            let bytes = match result {
                Ok(crate::actions::Exit::Code {
                    code: 0, stdout, ..
                }) => stdout,
                _ => return Err("Qt source export is unconfirmed".into()),
            };
            let values = qt::read_plist(&bytes)?;
            return Ok(Some(QtSnapshot {
                values,
                source: "qt-defaults",
                sha256: zerus_desktop_migration::sha256(&bytes),
                modified_at: fs::metadata(self.path())
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(seconds),
            }));
        }
        self.snapshot()
    }
}

fn seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// `defaults export DOMAIN -`: the domain as cfprefsd holds it, read-only,
/// or `None` when the tool is missing, fails or prints no dictionary.
fn export_domain(domain: &str) -> Option<(BTreeMap<String, QtValue>, Vec<u8>)> {
    let invocation = crate::actions::Invocation {
        args: vec!["export".into(), domain.into(), "-".into()],
        stdin: None,
        timeout: Duration::from_secs(10),
        request_id: None,
    };
    match crate::run::run_limited(Path::new(DEFAULTS), &invocation, &[], MAX_QT_STORE as usize) {
        Ok(crate::actions::Exit::Code {
            code: 0, stdout, ..
        }) => qt::read_plist(&stdout).ok().map(|values| (values, stdout)),
        _ => None,
    }
}

const MAX_STORE: u64 = 4 * 1024 * 1024;
const MAX_QT_STORE: u64 = 16 * 1024 * 1024;

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other("too large"));
    }
    Ok(bytes)
}

/// The values to seed from Qt's `values` for the settings named in `names`:
/// only those Qt has stored. `speech/manage_host` falls back to Qt's older
/// `speech/host` (SpeechSettings.cpp:233), and Qt's automatic approvals
/// choice goes to [`QT_AUTOMATIC_APPROVALS`].
pub fn seed_values(values: &BTreeMap<String, QtValue>, names: &[&str]) -> BTreeMap<String, Value> {
    let mut seeded = BTreeMap::new();
    for name in names {
        let Some(key) = find(name).filter(|key| key.seeded) else {
            continue;
        };
        let source = match *name {
            "speech/manage_host" => values
                .get("speech/manage_host")
                .or_else(|| values.get("speech/host")),
            QT_AUTOMATIC_APPROVALS => values.get("automaticApprovals/enabled"),
            other => values.get(other),
        };
        if let Some(value) = source {
            seeded.insert((*name).to_owned(), from_qt(key.kind, value));
        }
    }
    seeded
}

/// The names a seed considers: every seeded key plus Qt's shortcut overrides
/// and update records under their recognized channel/token names.
fn seed_names(values: &BTreeMap<String, QtValue>) -> Vec<String> {
    KEYS.iter()
        .filter(|key| key.seeded)
        .map(|key| key.name.to_owned())
        .chain(
            values
                .keys()
                .filter(|name| find(name).is_some_and(|key| key.seeded))
                .cloned(),
        )
        .collect()
}

/// What `settings.json` holds.
#[derive(Clone, Debug, Default, PartialEq)]
struct Contents {
    values: Map<String, Value>,
    /// `{"source", "path", "at", "keys"}` once seeded from Qt.
    seed: Option<Value>,
    /// Versioned family records and unknown extension fields stay intact.
    extra: Map<String, Value>,
}

impl Contents {
    fn parse(bytes: &[u8]) -> Option<Contents> {
        let Value::Object(mut object) = serde_json::from_slice(bytes).ok()? else {
            return None;
        };
        if object.get("version").is_some_and(|v| v != 1) {
            return None;
        }
        object.remove("version");
        let values = match object.remove("values") {
            Some(Value::Object(values)) => values,
            None => Map::new(),
            _ => return None,
        };
        let seed = match object.remove("seed") {
            None => None,
            Some(value @ Value::Object(_)) => Some(value),
            _ => return None,
        };
        Some(Contents {
            values,
            seed,
            extra: object,
        })
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut object = self.extra.clone();
        object.insert("version".into(), Value::from(1));
        if let Some(seed) = &self.seed {
            object.insert("seed".into(), seed.clone());
        }
        object.insert("values".into(), Value::Object(self.values.clone()));
        serde_json::to_vec_pretty(&Value::Object(object)).unwrap_or_default()
    }
}

/// The existing settings decoder, without creating a live store or writer.
/// Migration reads only bounded original bytes and retains unsupported sources.
pub(crate) fn snapshot_values(bytes: &[u8]) -> Result<Map<String, Value>, SnapshotError> {
    if bytes.len() as u64 > MAX_STORE {
        return Err(SnapshotError::TooLarge);
    }
    Contents::parse(bytes)
        .map(|contents| contents.values)
        .ok_or(SnapshotError::Unsupported)
}

pub(crate) enum SnapshotError {
    TooLarge,
    Unsupported,
}

/// Reject damaged/unsupported flag values before the existing Qt conversion.
/// Migration never turns an unreadable policy into an assumed false choice.
pub(crate) fn checked_qt_flag(value: &QtValue) -> Option<bool> {
    let valid = match value {
        QtValue::Bool(_) | QtValue::Int(_) => true,
        QtValue::Double(n) => n.is_finite(),
        QtValue::Text(s) => {
            s.len() <= 5
                && ["", "0", "1", "true", "false"].contains(&s.to_ascii_lowercase().as_str())
        }
        _ => false,
    };
    valid.then(|| value.to_bool())
}

/// A change other code reacts to: the setting's name, its new value and the
/// store's revision after it. Revisions grow with every change, in the order
/// the store took them, so a reader can drop an answer or event that arrives
/// after a newer one.
pub type Listener = Box<dyn Fn(&str, &Value, u64) + Send + Sync>;

struct State {
    contents: Contents,
    loaded: bool,
    /// The store could not be read: settings run at their defaults and
    /// nothing is written over the unreadable file.
    unreadable: Option<String>,
    dirty: bool,
    writing: bool,
    closed: bool,
    seed: Option<QtStore>,
    error: Option<String>,
    revision: u64,
    capturing: bool,
}

struct Shared {
    path: PathBuf,
    state: Mutex<State>,
    signal: Condvar,
    listeners: Mutex<Vec<Listener>>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The settings store: reads answer from memory once loaded, and writes go
/// to disk on the store's thread, the latest values only.
#[derive(Clone)]
pub struct Settings {
    shared: Arc<Shared>,
}

impl Settings {
    /// Opens `<data_dir>/settings.json`. With `seed`, a store never seeded
    /// before takes Qt's values first (the switch identity only).
    pub fn open(path: PathBuf, seed: Option<QtStore>) -> Settings {
        let shared = Arc::new(Shared {
            path,
            state: Mutex::new(State {
                contents: Contents::default(),
                loaded: false,
                unreadable: None,
                dirty: false,
                writing: false,
                closed: false,
                seed,
                error: None,
                revision: 0,
                capturing: false,
            }),
            signal: Condvar::new(),
            listeners: Mutex::new(Vec::new()),
        });
        let worker = Arc::clone(&shared);
        let started = std::thread::Builder::new()
            .name("settings".into())
            .spawn(move || run(&worker));
        if started.is_err() {
            let mut state = shared.lock();
            state.loaded = true;
            state.unreadable = Some("The settings thread did not start.".into());
            shared.signal.notify_all();
        }
        Settings { shared }
    }

    /// Waits until the store is loaded (and seeded, when asked).
    fn loaded(&self) -> std::sync::MutexGuard<'_, State> {
        let state = self.shared.lock();
        self.shared
            .signal
            .wait_while(state, |state| !state.loaded)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The setting's value as Qt would read it, or `Null` for an unknown
    /// name.
    pub fn get(&self, name: &str) -> Value {
        let Some(key) = find(name) else {
            return Value::Null;
        };
        let state = self.loaded();
        effective(key.kind, state.contents.values.get(name))
    }

    /// Whether this key is stored, rather than only supplied by its default.
    pub fn contains(&self, name: &str) -> bool {
        find(name).is_some() && self.loaded().contents.values.contains_key(name)
    }

    pub fn flag(&self, name: &str) -> bool {
        self.get(name).as_bool().unwrap_or(false)
    }

    pub fn number(&self, name: &str) -> f64 {
        self.get(name).as_f64().unwrap_or(0.0)
    }

    pub fn text(&self, name: &str) -> String {
        self.get(name).as_str().unwrap_or_default().to_owned()
    }

    /// Every listed setting, shortcut override and update record, as Qt reads them.
    pub fn all(&self) -> Map<String, Value> {
        self.all_tracked().0
    }

    /// [`Settings::all`] with the store's revision, read under the same lock,
    /// so a reader can tell older changes from newer ones.
    pub fn all_tracked(&self) -> (Map<String, Value>, u64) {
        let state = self.loaded();
        let mut all: Map<String, Value> = KEYS
            .iter()
            .map(|key| {
                (
                    key.name.to_owned(),
                    effective(key.kind, state.contents.values.get(key.name)),
                )
            })
            .collect();
        for (name, value) in &state.contents.values {
            if let Some(key) = find(name).filter(|_| {
                name.starts_with(SHORTCUTS)
                    || name.starts_with("updates/")
                    || name.starts_with("machines/")
            }) {
                all.insert(name.clone(), effective(key.kind, Some(value)));
            }
        }
        (all, state.revision)
    }

    /// Stores a value, normalized as Qt reads it, and returns what is now in
    /// effect. Removing an override (`Null`) restores its defaults and returns
    /// `Null` where that setting supports removal.
    pub fn set(&self, name: &str, value: Value) -> Result<Value, String> {
        self.set_tracked(name, value).map(|(value, _)| value)
    }

    /// [`Settings::set`] with the store's revision after the change.
    pub fn set_tracked(&self, name: &str, value: Value) -> Result<(Value, u64), String> {
        let key = find(name).ok_or_else(|| format!("Unknown setting: {name}"))?;
        if name == QT_AUTOMATIC_APPROVALS {
            return Err("This setting only records Qt's choice.".into());
        }
        let stored = match (&key.kind, &value) {
            (Kind::Chords, Value::Null) => None,
            (Kind::Text(""), Value::Null) if name.starts_with("machines/colors/") => None,
            (Kind::AutoFlag, Value::String(s)) if s == "auto" => Some(Value::from("auto")),
            (Kind::AutoFlag | Kind::Flag(_), Value::Bool(_))
            | (Kind::Choice(_) | Kind::Text(_) | Kind::Bytes, Value::String(_))
            | (Kind::Chords, Value::Array(_)) => Some(effective(key.kind, Some(&value))),
            (Kind::RowKeys, Value::Array(_)) if crate::row_preferences::valid(&value) => {
                Some(value.clone())
            }
            (Kind::Real { .. }, Value::Number(_)) => Some(effective(key.kind, Some(&value))),
            (Kind::Whole { .. } | Kind::Long { .. }, Value::Number(number))
                if number.is_i64() || number.is_u64() =>
            {
                Some(effective(key.kind, Some(&value)))
            }
            _ => return Err(format!("Wrong type for {name}")),
        };
        let (now, revision) = {
            let mut state = self.loaded();
            if state.capturing {
                return Err("Settings are temporarily paused for the migration snapshot.".into());
            }
            if let Some(error) = &state.unreadable {
                return Err(error.clone());
            }
            let mut candidate = state.contents.clone();
            match &stored {
                Some(value) => {
                    candidate.values.insert(name.to_owned(), value.clone());
                }
                None => {
                    candidate.values.remove(name);
                }
            }
            if candidate.to_bytes().len() as u64 > MAX_STORE {
                return Err("Settings exceed their byte limit; existing values were kept.".into());
            }
            state.contents = candidate;
            state.dirty = true;
            state.revision += 1;
            self.shared.signal.notify_all();
            // A removed shortcut override is null: its defaults apply again,
            // which differs from an override without chords ([]).
            let now = match &stored {
                Some(value) => effective(key.kind, Some(value)),
                None => Value::Null,
            };
            (now, state.revision)
        };
        let listeners = self
            .shared
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for listener in listeners.iter() {
            listener(name, &now, revision);
        }
        Ok((now, revision))
    }

    /// Calls `listener` after every change made through [`Settings::set`].
    pub fn subscribe(&self, listener: Listener) {
        self.shared
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(listener);
    }

    /// The seed record (`source`, `path`, `at`, `keys`), once seeded.
    pub fn seed_record(&self) -> Option<Value> {
        self.loaded().contents.seed.clone()
    }

    /// Background organization recovery shares this writer and its existing
    /// G3 journal. Unsupported settings/metadata never admit an import.
    pub fn organization_import(&self) -> Result<OrganizationImport, String> {
        let state = self.loaded();
        if state.unreadable.is_some() {
            return Err("Settings cannot record a project import.".into());
        }
        imports::organization_state(&state.contents).map_err(str::to_owned)
    }

    /// Durably reserve metadata before the idempotent missing-field import.
    /// No Qt values or pending operation bodies enter this record.
    pub fn prepare_organization_import(
        &self,
        node_id: &str,
        source: &QtStore,
        snapshot: Option<&QtSnapshot>,
    ) -> Result<bool, String> {
        match (
            zerus_desktop_migration::canonical(&self.shared.path),
            zerus_desktop_migration::canonical(source.path()),
        ) {
            (Ok(target), Ok(source)) if target != source => (),
            _ => {
                return Err(
                    "Project import cannot overwrite an overlapping or unverified Qt source."
                        .into(),
                )
            }
        }
        self.update_organization_record(|contents| {
            imports::prepare_organization(contents, node_id, source, snapshot)
        })
    }

    /// Same-node initialized catalog evidence completes the pending record.
    pub fn complete_organization_import(&self, node_id: &str) -> Result<(), String> {
        self.update_organization_record(|contents| {
            imports::complete_organization(contents, node_id)?;
            Ok(true)
        })
        .map(|_| ())
    }

    fn update_organization_record(
        &self,
        update: impl FnOnce(&mut Contents) -> Result<bool, &'static str>,
    ) -> Result<bool, String> {
        let changed = {
            let mut state = self.loaded();
            if state.unreadable.is_some() || state.closed || state.capturing {
                return Err("Settings cannot record a project import.".into());
            }
            let mut candidate = state.contents.clone();
            let changed = update(&mut candidate).map_err(str::to_owned)?;
            if candidate.to_bytes().len() as u64 > MAX_STORE {
                return Err("Project import metadata exceeds the settings limit.".into());
            }
            if changed {
                state.contents = candidate;
                state.dirty = true;
                self.shared.signal.notify_all();
            }
            changed
        };
        if !self.flush_within(Duration::from_secs(10)) {
            return Err("Project import metadata could not be saved; import stays pending.".into());
        }
        Ok(changed)
    }

    /// Why the store could not be read or written, if it could not.
    pub fn error(&self) -> Option<String> {
        let state = self.loaded();
        state.unreadable.clone().or_else(|| state.error.clone())
    }

    /// Waits at most `limit` for every change to reach the disk; whether it
    /// did: loaded, clean, idle, and the last write succeeded.
    pub fn flush_within(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut state = self.shared.lock();
        loop {
            if state.loaded && !state.dirty && !state.writing {
                return state.error.is_none();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .shared
                .signal
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        // The last handle besides the thread's: let the thread write and end.
        if Arc::strong_count(&self.shared) == 2 {
            self.shared.lock().closed = true;
            self.shared.signal.notify_all();
        }
    }
}

/// The store's thread: load, seed once when asked, then write the latest
/// values whenever they change.
fn run(shared: &Shared) {
    let (contents, mut unreadable) = match read_bounded(&shared.path, MAX_STORE) {
        Ok(bytes) => match Contents::parse(&bytes) {
            Some(contents) => (contents, None),
            None => (
                Contents::default(),
                Some(
                    "Settings are not readable; Zerus uses the defaults and keeps the file.".into(),
                ),
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Contents::default(), None),
        Err(error) => (
            Contents::default(),
            Some(format!("Settings could not be read: {error}")),
        ),
    };
    let seed = shared.lock().seed.take();
    if let Some(source) = &seed {
        match (
            zerus_desktop_migration::canonical(&shared.path),
            zerus_desktop_migration::canonical(source.path()),
        ) {
            (Ok(target), Ok(source)) if target != source => (),
            _ => {
                unreadable =
                    Some("Settings cannot overwrite an overlapping or unverified Qt source.".into())
            }
        }
    }
    let mut contents = contents;
    let mut dirty = false;
    if let (Some(store), None) = (seed, &unreadable) {
        let families = imports::pending(&contents);
        if families.is_ok() && (contents.seed.is_none() || families.as_ref().is_ok_and(|p| *p)) {
            match store.snapshot() {
                Ok(snapshot) => {
                    if contents.seed.is_none() {
                        let (values, record) = seed_from(&store, snapshot.as_ref());
                        for (name, value) in values {
                            contents.values.entry(name).or_insert(value);
                        }
                        contents.seed = Some(record);
                        dirty = true;
                    }
                    if families.as_ref().is_ok_and(|p| *p) {
                        dirty |= imports::apply(&mut contents, &store, snapshot.as_ref());
                    }
                }
                Err(error) => eprintln!(
                    "zerus-client-core: settings seed: {error}; retried at the next start"
                ),
            }
        }
        if let Err(error) = families {
            eprintln!("zerus-client-core: family import: {error}; existing records preserved");
        }
    }
    {
        let mut state = shared.lock();
        state.contents = contents;
        state.unreadable = unreadable;
        state.dirty = dirty;
        state.loaded = true;
        shared.signal.notify_all();
    }
    loop {
        let snapshot = {
            let state = shared.lock();
            let mut state = shared
                .signal
                .wait_while(state, |state| !state.dirty && !state.closed)
                .unwrap_or_else(PoisonError::into_inner);
            if !state.dirty {
                return;
            }
            state.dirty = false;
            state.writing = true;
            state.contents.to_bytes()
        };
        let written = write_store(&shared.path, &snapshot);
        let mut state = shared.lock();
        state.writing = false;
        state.error = written.err();
        shared.signal.notify_all();
        if state.closed && !state.dirty {
            return;
        }
    }
}

/// Qt's values for the old settings seed, and its record. The caller has
/// already obtained one bounded snapshot shared with the new family imports.
fn seed_from(store: &QtStore, snapshot: Option<&QtSnapshot>) -> (BTreeMap<String, Value>, Value) {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (values, source) = match snapshot {
        Some(snapshot) => {
            let names = seed_names(&snapshot.values);
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            (seed_values(&snapshot.values, &names), snapshot.source)
        }
        None => (BTreeMap::new(), "none"),
    };
    let domain = match store {
        QtStore::Plist { domain, .. } => domain.clone(),
        QtStore::Ini(_) => None,
    };
    let record = json!({
        "source": source,
        "domain": domain,
        "path": store.path().to_string_lossy(),
        "sha256": snapshot.map(|s| &s.sha256),
        "at": at,
        "keys": values.keys().collect::<Vec<_>>(),
    });
    (values, record)
}

/// Writes the store through a synced private temporary file and a rename,
/// so a cut-off write keeps the old file.
fn write_store(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let failure = |error| format!("Settings not saved: {error}");
    crate::fsx::write_private_atomic(path, bytes, true).map_err(failure)?;
    #[cfg(unix)]
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )
    .and_then(|directory| directory.sync_all())
    .map_err(failure)?;
    Ok(())
}
