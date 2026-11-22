//! The client's local preferences (Settings, phase 3b; see
//! `docs/superpowers/plans/2026-10-28-tauri-client-phase3b-settings.md`).
//!
//! They live in `<data_dir>/settings.json` under Qt's QSettings names, with
//! Qt's defaults and ranges (tray/src/SettingsPage.h and the headers it
//! reads). Loading and writing run on the store's own thread. Task #55 (team
//! ede724c4, 2026-11-10): the store is no longer seeded from Qt's settings;
//! an older file's `seed` record stays in the file as an extension field.

pub mod geometry;
pub mod repository;

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

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

/// One setting: its QSettings name and how Qt reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
}

const fn key(name: &'static str, kind: Kind) -> Key {
    Key { name, kind }
}

/// The client's own preference (named as such in the table).
const fn client(name: &'static str, kind: Kind) -> Key {
    Key { name, kind }
}

/// The prefix of the client's own settings.
pub const CLIENT: &str = "client/";

/// The group of shortcut overrides (`shortcuts/<command id>`, Shortcuts.h).
pub const SHORTCUTS: &str = "shortcuts/";

/// Every local setting the client has. Sources are in the plan's table.
pub const KEYS: &[Key] = &[
    // Dangerous Next opt-in: never seed Qt's enabled value or transient error.
    Key {
        name: crate::approvals::automatic::ENABLED,
        kind: Kind::Flag(false),
    },
    Key {
        name: crate::approvals::automatic::ERROR,
        kind: Kind::Text(""),
    },
    // Its categories (zerus-sz42x): all except paid usage until changed.
    Key {
        name: "automaticApprovals/category/read",
        kind: Kind::Flag(true),
    },
    Key {
        name: "automaticApprovals/category/edit",
        kind: Kind::Flag(true),
    },
    Key {
        name: "automaticApprovals/category/shell",
        kind: Kind::Flag(true),
    },
    Key {
        name: "automaticApprovals/category/web",
        kind: Kind::Flag(true),
    },
    Key {
        name: "automaticApprovals/category/mcp",
        kind: Kind::Flag(true),
    },
    Key {
        name: "automaticApprovals/category/agents",
        kind: Kind::Flag(true),
    },
    Key {
        name: "automaticApprovals/category/other",
        kind: Kind::Flag(true),
    },
    Key {
        name: "automaticApprovals/category/paid",
        kind: Kind::Flag(false),
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
    // Qt's workspace/collapsedTeams: the team ids whose members are folded
    // under the lead (SessionsWindowRows.cpp).
    key("workspace/collapsedTeams", Kind::Chords),
    key("workspace/expandSessionsOnHover", Kind::Flag(true)),
    key("workspace/hideEmptyProjects", Kind::Flag(true)),
    // Session cards mark their account with its letter, or its emoji when set.
    client("sessions/accountMarks", Kind::Choice(&["letter", "emoji"])),
    // Session cards show account marks at all (zerus-sa9qn.13).
    client("sessions/showAccountMarks", Kind::Flag(true)),
    // A new child session's row starts compact (zerus-sa9qn.14); the second
    // key is the device's record of the child links it already handled.
    client("sessions/compactNewChildren", Kind::Flag(true)),
    client("sessions/childRowsSeen", Kind::Text("")),
    // Explicit child visibility choices, by machine UUID and session ID,
    // including retained ancestors of favorites (zerus-sa9qn.15, zerus-e4evu).
    client("sessions/collapsedTeams", Kind::Text("")),
    client("sessions/expandedTeams", Kind::Text("")),
    client("sessions/favorites", Kind::Text("")),
    key("workspace/animateActivity", Kind::Flag(true)),
    key("workspace/showCompletedSubagents", Kind::Flag(false)),
    key("workspace/reduceMotion", Kind::Flag(false)),
    key("notifications/optionalQuestions", Kind::Flag(false)),
    // Machines (SettingsPage.h:251-258): keep a Windows session host running
    // for other computers. Off by default.
    key("machines/residentHost", Kind::Flag(false)),
    // A remembered Terminate checkbox, never maintenance/removal consent.
    Key {
        name: "workspace/removeWorktreeAfterTermination",
        kind: Kind::Flag(false),
    },
    // TasksView's local scope switch.
    // A first-G3 family also covers clients whose original seed predates Tasks.
    Key {
        name: "tasks/onlySession",
        kind: Kind::Flag(false),
    },
    // New session's remembered choices (NewSessionDialog.cpp:845-851,
    // SessionCreationController.cpp:244-249). launchAccounts is compact JSON
    // stored by Qt as a QByteArray, read as text by the single Qt seed.
    key("workspace/launchAgent", Kind::Text("")),
    key("workspace/launchOpenTerminal", Kind::Flag(false)),
    key("workspace/launchAccounts", Kind::Text("")),
    // Task 102: the folder mode New session starts with, a generated worktree
    // ("auto") or the folder itself ("none"), as Qt's workspace/launchWorktree
    // (NewSessionDialog.cpp:259, :949); imported once by its family.
    Key {
        name: NEW_SESSION_FOLDER_MODE,
        kind: Kind::Choice(&["auto", "none"]),
    },
    // Task 74: Qt's SessionLeads JSON object, by session identity.
    key("workspace/sessionLeads", Kind::Text("")),
    // WhiteWh fork: Keep cache warm, Qt's compact JSON object of session
    // identity -> hours (CacheKeepalive.cpp); its family imports it once.
    Key {
        name: crate::cache_keepalive::KEY,
        kind: Kind::Text(""),
    },
    // A first-G3 family import handles this separately from the old seed.
    Key {
        name: "workspace/allSwarmProjects",
        kind: Kind::Flag(false),
    },
    Key {
        name: crate::row_preferences::KEY,
        kind: Kind::RowKeys,
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
    },
    // Task 102: Qt's microphone as its name, never its id: the Voice page
    // picks the device with this name when it lists them, then drops it.
    Key {
        name: MICROPHONE_HINT,
        kind: Kind::Text(""),
    },
    // Task 102: the fork's Windows mobile services (WindowsMobileServices.h):
    // a relay for this computer on 127.0.0.1 and the connector started with
    // Zerus. Qt keeps them only in its store; imported once by their family.
    Key {
        name: "mobile/localRelay",
        kind: Kind::Flag(false),
    },
    Key {
        name: "mobile/localRelayPort",
        kind: Kind::Whole {
            default: 8787,
            min: 1,
            max: 65535,
        },
    },
    Key {
        name: "mobile/runConnector",
        kind: Kind::Flag(true),
    },
    // Automatic recovery: the newest shared policy known (recovery_settings).
    key(crate::recovery_settings::SHARED_POLICY_KEY, Kind::Text("")),
    // Files (DownloadCache), Updates (UpdateController.cpp).
    key("downloads/folder", Kind::Text("")),
    key("downloads/cacheBytes", Kind::Long { default: 0, min: 0 }),
    key("updates/channel", Kind::Text("stable")),
    key("updates/automatic", Kind::Flag(true)),
    // The client's own: the skin for each appearance (the theme picks one)
    // and the Overview column (design round 7).
    client("client/darkSkin", Kind::Text("zerus-dark")),
    client("client/lightSkin", Kind::Text("zerus-light")),
    // WhiteWh fork's skin choice; Tauri ports it as whitewh-dark/whitewh-light.
    key("workspace/skin", Kind::Text("zerus")),
    client(
        "client/overviewWidth",
        Kind::Whole {
            default: 1200,
            min: 720,
            max: 2400,
        },
    ),
    client("client/overviewFullWidth", Kind::Flag(false)),
    client("client/overviewSubSessions", Kind::Flag(false)),
    Key { name: "workspace/inspectorOpen", kind: Kind::AutoFlag },
    Key {
        name: "workspace/inspectorTab",
        kind: Kind::Choice(&["plan", "details", "worktrees", "tasks"]),
    },
    Key {
        name: "workspace/inspectorWidth",
        kind: Kind::Whole {
            default: 400,
            min: 240,
            max: 900,
        },
    },
    Key { name: "workspace/inspectorClosed", kind: Kind::Text("[]") },
    // New G3 families use the family journal, not the original switch seed.
    client("client/windowGeometry", Kind::Text("")),
    // Task 243: the sessions list hidden (Qt's SessionPanelDock Collapsed, the same key).
    client("workspace/sessionsCollapsed", Kind::Flag(false)),
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
    // WhiteWh fork: optional extensions (tray/src/Extensions.h). A fork build
    // turns them on by default; the client is the fork's. Both names are Qt's,
    // so the switch seeds the owner's choice.
    key("extensions/socraticode", Kind::Flag(true)),
    key("socraticode/nativeQdrant", Kind::Flag(false)),
    // The Team tab's dividers (task 9): the side column's width beside the
    // canvas, and the members' share of it above the coordinator, in percent.
    client(
        "client/teamSideWidth",
        Kind::Whole {
            default: 420,
            min: 340,
            max: 900,
        },
    ),
    client(
        "client/teamMembersShare",
        Kind::Whole {
            default: 45,
            min: 15,
            max: 85,
        },
    ),
    // Task #8 (team ede724c4): New team's how-to, folded by the owner.
    // Task #31 (team ede724c4): the New team how-to starts collapsed, as the
    // web client's defaults (settings-defaults.json) already say.
    client("client/newTeamHowToClosed", Kind::Flag(true)),
    // Task 103: the Teams view preferences the move keeps. Qt keeps "hide
    // finished" and the catalog's unused-role switch as plain flags; its heat
    // mode is per team, so the imported map holds team -> "time" | "tokens".
    key("teams/hideFinished", Kind::Flag(false)),
    // Task 244: how a lead session shows the team's notices (Full, Collapsed
    // by default, Hidden), and each lead session's own choice by its row key.
    key("teams/leadNotices", Kind::Choice(&["collapsed", "full", "silent"])),
    Key {
        name: "teams/leadNoticeSessions",
        kind: Kind::Text("{}"),
    },
    key("ui/catalogHideUnusedRoles", Kind::Flag(false)),
    // Task 200 (Qt TeamTemplateList, F6): New team's favourite templates by
    // id, a string list as Qt keeps it, starred first in the template list.
    key("teams/templateFavourites", Kind::Chords),
    Key {
        name: "teams/heatModes",
        kind: Kind::Text("{}"),
    },
    Key {
        name: "teams/planModes",
        kind: Kind::Text("{}"),
    },
    // W2 (Qt's `teams/attentionNotified`): the team-waiting notices already
    // shown, as a JSON object of item id -> notice token, so a restart or a
    // reopened window does not raise them again.
    Key {
        name: "teams/attentionNotified",
        kind: Kind::Text("{}"),
    },
];

/// Task 102: New session's remembered folder mode.
pub const NEW_SESSION_FOLDER_MODE: &str = "newSession/folderMode";
/// Task 102: the name of Qt's chosen microphone, a hint for the Voice page.
pub const MICROPHONE_HINT: &str = "speech/microphoneName";

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
            })
        })
        .or_else(|| update_family(name))
        .or_else(|| machine_family(name))
        .or_else(|| project_family(name))
        .or_else(|| account_family(name))
}

/// The desktop's own color and emoji mark for one account profile:
/// `accounts/color/<token>` and `accounts/mark/<token>`, where the token is
/// the URI-encoded profile key (computer and profile ID). Like machine colors
/// these are this desktop's preferences, never the host's catalog.
pub const ACCOUNT_COLORS: &str = "accounts/color/";
pub const ACCOUNT_MARKS: &str = "accounts/mark/";
/// The account's mark is not shown on session cards (zerus-sa9qn.13).
pub const ACCOUNT_HIDDEN: &str = "accounts/hide/";
fn account_family(name: &str) -> Option<Key> {
    let (token, kind) = if let Some(token) = name.strip_prefix(ACCOUNT_HIDDEN) {
        (token, Kind::Flag(false))
    } else {
        (
            name.strip_prefix(ACCOUNT_COLORS)
                .or_else(|| name.strip_prefix(ACCOUNT_MARKS))?,
            Kind::Text(""),
        )
    };
    (!token.is_empty()
        && token.len() <= 512
        && !token
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\'))
    .then_some(Key {
        name: "accounts/",
        kind,
    })
}

// Project IDs are flat JSON keys, never filesystem paths or machine aliases.
fn project_family(name: &str) -> Option<Key> {
    let id = name.strip_prefix("projects/vivid/")?;
    (!id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)).then_some(Key {
        name: "projects/vivid/",
        kind: Kind::Flag(false),
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

const MAX_STORE: u64 = 4 * 1024 * 1024;

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other("too large"));
    }
    Ok(bytes)
}

/// What `settings.json` holds.
#[derive(Clone, Debug, Default, PartialEq)]
struct Contents {
    values: Map<String, Value>,
    /// Versioned family records and unknown extension fields (an older
    /// file's Qt `seed` record among them) stay intact.
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
        Some(Contents {
            values,
            extra: object,
        })
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut object = self.extra.clone();
        object.insert("version".into(), Value::from(1));
        object.insert("values".into(), Value::Object(self.values.clone()));
        serde_json::to_vec_pretty(&Value::Object(object)).unwrap_or_default()
    }
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
    error: Option<String>,
    revision: u64,
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
    /// Opens `<data_dir>/settings.json`.
    pub fn open(path: PathBuf) -> Settings {
        let shared = Arc::new(Shared {
            path,
            state: Mutex::new(State {
                contents: Contents::default(),
                loaded: false,
                unreadable: None,
                dirty: false,
                writing: false,
                closed: false,
                error: None,
                revision: 0,
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

    /// Waits until the store is loaded.
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
                    || name.starts_with("accounts/")
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
        let stored = match (&key.kind, &value) {
            (Kind::Chords, Value::Null) => None,
            (Kind::Text(""), Value::Null)
                if name.starts_with("machines/colors/") || name.starts_with("accounts/") =>
            {
                None
            }
            (Kind::Flag(_), Value::Null) if name.starts_with(ACCOUNT_HIDDEN) => None,
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

/// The store's thread: load, then write the latest values whenever they
/// change.
fn run(shared: &Shared) {
    let (contents, unreadable) = match read_bounded(&shared.path, MAX_STORE) {
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
    {
        let mut state = shared.lock();
        state.contents = contents;
        state.unreadable = unreadable;
        state.dirty = false;
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
