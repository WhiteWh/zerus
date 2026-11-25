//! U21: the built-in command templates Zerus runs outside the member's sandbox.
//! The owner's decision after 133 escalations in 6 hours: routine commands are
//! general rules, not approvals. Each template is a fixed program and argv with
//! typed placeholders, never a shell string; Zerus runs it in the member's own
//! folder, with the environment Zerus sets, bounded time and output. The set is
//! off by the global switch alone; a project may add its own templates.
use super::super::*;
use anyhow::{bail, ensure, Context};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The toolchain's own PATH: Zerus keeps the worker's PATH (the team's
/// toolchain) and adds nothing.
const TAIL_BYTES: usize = 4000;
/// How much output a run may produce before only its tail is kept.
const OUTPUT_BYTES: usize = 128 * 1024;

/// What a placeholder accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A name that fits one argv element: `dev`, `cargo-build`, `a::b`.
    Identifier,
    /// A list of such names, each its own argv element.
    Identifiers,
    /// Free text that is still refused shell metacharacters (a commit message).
    Text,
    /// A list of paths inside the member's folder.
    Paths,
    /// Task 85: one path inside the member's folder that may not exist yet
    /// (a build folder the configure creates).
    Path,
}

/// One typed placeholder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Param {
    pub name: &'static str,
    pub kind: Kind,
    pub required: bool,
}

/// A fixed program and argv with typed placeholders (U21).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Template {
    pub id: &'static str,
    pub summary: &'static str,
    pub program: &'static str,
    pub args: &'static [&'static str],
    pub params: &'static [Param],
    pub timeout_secs: u64,
}

const fn param(name: &'static str, kind: Kind) -> Param {
    Param { name, kind, required: true }
}

const fn optional(name: &'static str, kind: Kind) -> Param {
    Param { name, kind, required: false }
}

/// The built-in set, versioned with hgs like the guides: cargo build/test/clippy
/// and the two git routes the members needed most (U21, from the owner's 6-hour
/// count), then the other stacks' builds and tests. Task #58: the Qt desktop's
/// qt-configure, qt-build and qt-test went with Qt (`REMOVED`).
/// Task #58 (owner, 2026-11-10: Qt goes from Zerus completely): the Qt
/// desktop's templates and their replacements. A team's stored run, a
/// project's `.zerus/tests.toml` or a request that still names one is refused
/// with its replacement, never run or crashed.
pub(crate) const REMOVED: &[(&str, &str)] = &[("qt-configure", "cmake-configure"), ("qt-build", "cmake-build"), ("qt-test", "ctest")];

/// The refusal for a removed template's id, naming its replacement.
pub(crate) fn removed(id: &str) -> Option<String> {
    REMOVED.iter().find(|(old, _)| *old == id).map(|(old, new)| {
        format!("The {old} template was removed with the Qt desktop: use {new} (hgs team templates --json shows its arguments)")
    })
}

pub(crate) const BUILTIN: &[Template] = &[
    Template { id: "artifact-test", summary: "Deliver a build with desync and execute its pinned scenario on an explicitly paired model-free executor", program: "hgs-executor", args: &[], params: &[param("profile", Kind::Identifier), param("job", Kind::Identifier)], timeout_secs: 604800 },
    Template {
        id: "cargo-build",
        summary: "cargo build --locked in your folder (optional package and toolchain)",
        program: "cargo",
        args: &["+{toolchain}", "build", "--locked", "--package={package}"],
        params: &[optional("toolchain", Kind::Identifier), optional("package", Kind::Identifier), optional("files", Kind::Paths)],
        timeout_secs: 1800,
    },
    Template {
        id: "cargo-test",
        summary: "cargo test --locked in your folder (a filter or the filters and skips hgs test affected names, a binary, a package or manifest)",
        program: "cargo",
        args: &["+{toolchain}", "test", "--locked", "--package={package}", "--bin={bin}", "{filter}"],
        params: &[
            optional("toolchain", Kind::Identifier),
            optional("package", Kind::Identifier),
            optional("bin", Kind::Identifier),
            optional("filter", Kind::Identifier),
            // #741: an affected set: several filters with skips, another crate.
            optional("filters", Kind::Identifiers),
            optional("skips", Kind::Identifiers),
            optional("manifest", Kind::Paths),
            // #752: the files this member changes, for the build broker.
            optional("files", Kind::Paths),
        ],
        timeout_secs: 1800,
    },
    Template {
        id: "cargo-clippy",
        summary: "cargo clippy --locked --all-targets -- -D warnings (an optional package and toolchain)",
        program: "cargo",
        args: &["+{toolchain}", "clippy", "--locked", "--all-targets", "--package={package}", "--", "-D", "warnings"],
        params: &[optional("toolchain", Kind::Identifier), optional("package", Kind::Identifier), optional("files", Kind::Paths)],
        timeout_secs: 1800,
    },
    Template {
        id: "py-test",
        summary: "the project .venv interpreter runs pytest (typed -k expression, -m mark, node names and test file or folder paths; JUnit XML in .zerus-team/python); project: a Python project in a subfolder, run there",
        program: "python",
        args: &["-m", "pytest", "-q", "--junitxml=.zerus-team/python/junit.xml", "-k={filter}", "-m={mark}", "{tests}", "{paths}"],
        params: &[
            optional("filter", Kind::Text),
            optional("mark", Kind::Identifier),
            optional("tests", Kind::Identifiers),
            // Task 137: the test files `hgs test affected` chose.
            optional("paths", Kind::Paths),
            // Task 153: a Python project in a subfolder: its environment, and
            // the folder pytest runs in.
            optional("project", Kind::Path),
        ],
        timeout_secs: 1800,
    },
    Template {
        id: "cython-build",
        summary: "the project .venv interpreter rebuilds the Cython extensions in place (setup.py build_ext --inplace); project: a Python project in a subfolder, built there",
        program: "python",
        args: &["setup.py", "build_ext", "--inplace", "{sources}"],
        params: &[optional("sources", Kind::Paths), optional("project", Kind::Path)],
        timeout_secs: 1800,
    },    // Owner, 2026-10-28: QA ran the Android tests by hand, a permission each.
    // Task #280: the Tauri app's only build/dev/launch path. The templates
    // carry 273's development identity; the release-candidate/package step is
    // the only place that builds the switch identity.
    Template {
        id: "tauri-build",
        summary: "builds Zerus Next with the development identity (npm --prefix client run tauri -- build --no-bundle); the package step alone builds the switch identity",
        program: "python",
        args: &["scripts/tauri_template.py", "build"],
        params: &[optional("files", Kind::Paths)],
        timeout_secs: 1800,
    },
    Template {
        id: "tauri-dev",
        summary: "runs Zerus Next's dev server with the development identity (npm --prefix client run tauri -- dev)",
        program: "python",
        args: &["scripts/tauri_template.py", "dev"],
        params: &[optional("files", Kind::Paths)],
        timeout_secs: 1800,
    },
    Template {
        id: "tauri-launch",
        summary: "launches the development build after checking its identity; a switch-identity build is refused outside the package",
        program: "python",
        args: &["scripts/tauri_template.py", "launch"],
        params: &[optional("files", Kind::Paths)],
        timeout_secs: 1800,
    },
    Template {
        id: "android-test",
        summary: "the Android unit tests (gradlew :app:testDebugUnitTest) with Gradle's home and cache in your folder",
        program: "mobile/android/gradlew",
        args: &["-p", "mobile/android", ":app:testDebugUnitTest", "--no-daemon"],
        params: &[],
        timeout_secs: 3600,
    },
    // Task 157: the release candidate's local leg a Windows worker runs before
    // the release branch moves: the fork-windows steps, recorded per commit.
    // Task 169 review: a built-in template runs for any member without an
    // approval, so it never takes --exp-tag (that pushes a tag to origin): the
    // runner leg stays the owner's step by hand (docs/release-candidate.md).
    Template {
        id: "release-candidate",
        summary: "the fork release candidate's local leg on a Windows worker: fork handoff/update, the Tauri build and portable zip self-test with the fork build and feed (no Qt), each step's seconds and the zip size recorded for the commit",
        program: "python",
        args: &["scripts/release-candidate.py", "--commit={commit}", "--fork-build={fork_build}", "--fork-feed={fork_feed}"],
        params: &[
            optional("commit", Kind::Identifier),
            // Task 213: the local leg has no tag, so the fork identity the
            // workflow takes from it is passed explicitly; both or neither.
            optional("fork_build", Kind::Identifier),
            optional("fork_feed", Kind::Text),
        ],
        timeout_secs: 10800,
    },
    // Owner, 2026-10-28: a stale generated build folder (an AAPT intermediate)
    // needed a permission to remove; only a build output can go.
    Template {
        id: "clean-build",
        summary: "removes generated build folders in your folder (.ci-build, build, target, .gradle, out)",
        program: "cmake",
        args: &["-E", "rm", "-rf"],
        params: &[param("paths", Kind::Paths)],
        timeout_secs: 600,
    },
    Template {
        id: "git-merge-lead",
        summary: "git merge --no-edit the lead's branch into your own folder",
        program: "git",
        args: &["merge", "--no-edit"],
        params: &[],
        timeout_secs: 300,
    },
    Template {
        id: "git-commit",
        summary: "Zerus commits your work (hgs team commit): message, paths in your folder",
        program: "hgs",
        args: &["team", "commit", "--json"],
        params: &[param("message", Kind::Text), optional("paths", Kind::Paths)],
        timeout_secs: 300,
    },
    // Task 87: the engine is a per-machine path; the project must exist.
    Template {
        id: "ue-build",
        summary: "Unreal Build Tool builds a target (engine path, project, target, platform, configuration; -Project=*.uproject)",
        program: "{engine}/Engine/Build/BatchFiles/Build.bat",
        args: &["{project}", "{target}", "{platform}", "{config}", "-Project={project}"],
        params: &[
            optional("engine", Kind::Text), param("project", Kind::Path),
            param("target", Kind::Identifier), optional("platform", Kind::Identifier), optional("config", Kind::Identifier),
            optional("files", Kind::Paths),
        ],
        timeout_secs: 3600,
    },
    Template {
        id: "ue-test",
        summary: "UnrealEditor-Cmd runs automation tests headless and exports a report (-nullrhi -unattended -ExecCmds)",
        program: "{engine}/Engine/Binaries/Win64/UnrealEditor-Cmd.exe",
        args: &["{project}", "-nullrhi", "-unattended", "-saveddir=.zerus-team/ue/Saved", "-abslog=.zerus-team/ue/Unreal.log", "-ExecCmds=Automation RunTests {filters};Quit", "-ReportExportPath=.zerus-team/results/unreal"],
        params: &[optional("engine", Kind::Text), param("project", Kind::Path), optional("filters", Kind::Text), optional("files", Kind::Paths)],
        timeout_secs: 3600,
    },    // Task 85 (owner, 2026-11-06: builds and tests of any stack): any CMake
    // project.
    Template {
        id: "cmake-configure",
        summary: "cmake configure of any CMake project: source (default .), build_dir (default build), optional generator, config, preset",
        program: "cmake",
        args: &["-S", "{source}", "-B", "{build_dir}", "-G", "{generator}", "-DCMAKE_BUILD_TYPE={config}", "--preset", "{preset}"],
        params: &[
            optional("source", Kind::Path),
            optional("build_dir", Kind::Path),
            optional("generator", Kind::Text),
            optional("config", Kind::Identifier),
            optional("preset", Kind::Identifier),
            optional("files", Kind::Paths),
        ],
        timeout_secs: 1200,
    },
    Template {
        id: "cmake-build",
        summary: "cmake --build of a configured build folder (default build): optional config and targets",
        program: "cmake",
        args: &["--build", "{build_dir}", "--config", "{config}", "--target", "{targets}"],
        params: &[optional("build_dir", Kind::Path), optional("config", Kind::Identifier), optional("targets", Kind::Identifiers), optional("files", Kind::Paths)],
        timeout_secs: 3600,
    },
    Template {
        id: "ctest",
        summary: "ctest in a build folder (default build): the tests by name or a -R regex, optional config; one result per test from its JUnit report",
        program: "ctest",
        args: &["--test-dir", "{build_dir}", "-C", "{config}", "-R", "{regex}", "{tests}"],
        params: &[
            optional("build_dir", Kind::Path),
            optional("config", Kind::Identifier),
            optional("tests", Kind::Identifiers),
            optional("regex", Kind::Text),
            optional("files", Kind::Paths),
        ],
        timeout_secs: 1800,
    },
    Template {
        id: "msbuild-build",
        summary: "MSBuild of a .sln or project file in your folder (Windows, Visual Studio's MSBuild): optional config, platform and targets",
        program: "msbuild",
        args: &["{project}", "/p:Configuration={config}", "/p:Platform={platform}", "/t:{targets}"],
        params: &[
            param("project", Kind::Paths),
            optional("config", Kind::Identifier),
            optional("platform", Kind::Text),
            optional("targets", Kind::Identifiers),
            optional("files", Kind::Paths),
        ],
        timeout_secs: 3600,
    },
    Template {
        id: "vstest",
        summary: "vstest.console on test assemblies in your folder (Windows), optional test case filter; one result per test from its TRX report",
        program: "vstest.console",
        args: &["{tests}", "/TestCaseFilter:{filter}"],
        params: &[param("tests", Kind::Paths), optional("filter", Kind::Text), optional("files", Kind::Paths)],
        timeout_secs: 1800,
    },
    // Task #239: the web client's tests (vitest) run through the worker from
    // the project's own node_modules, installing them first when they are
    // missing; the JUnit report gives one count and failure per test file.
    Template {
        id: "web-test",
        summary: "vitest run in the web project (default client/), from its own node_modules; npm ci through the worker when they are missing; a JUnit report per test file",
        program: "npx",
        args: &[],
        params: &[optional("project", Kind::Path), optional("paths", Kind::Paths), optional("files", Kind::Paths)],
        timeout_secs: 1800,
    },
];

/// Task 85: the build folder a CMake template uses when none is named.
pub(crate) const CMAKE_BUILD_DIR: &str = "build";

/// A validated run: the program, argv and the JSON payload of `git-commit`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plan {
    pub template: String,
    pub program: String,
    pub argv: Vec<String>,
    /// U21: B4's commit is Zerus' own code, not a shell: the request goes as JSON.
    pub commit: Option<Value>,
    /// #741: further commands of the same run, in turn.
    pub more: Vec<(String, Vec<String>)>,
    /// Task 153: the folder the commands run in, relative to the member's
    /// folder and inside it (a Python project in a subfolder); None is the
    /// member's folder itself.
    pub cwd: Option<String>,
}

/// The built-in template with `id`, if the set has one.
pub(crate) fn builtin(id: &str) -> Option<&'static Template> {
    BUILTIN.iter().find(|template| template.id == id)
}

/// The global off switch (Settings > Teams): on unless the owner turned it off.
/// A test overrides the file with its own, so no test touches the live setting.
pub(crate) fn settings_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = SETTINGS.with(|path| path.borrow().clone()) {
        return path;
    }
    root().join("teams/templates.json")
}

#[cfg(test)]
thread_local! {
    static SETTINGS: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn settings_override(path: Option<PathBuf>) {
    SETTINGS.with(|settings| *settings.borrow_mut() = path);
}

/// The settings file: the global switch and the owner's additions. It lives in
/// Zerus state, never in a member's folder, so a sandboxed member can neither
/// read nor write it (U21, attempt 2).
pub(crate) fn read_settings() -> Value {
    std::fs::read(settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"enabled":true}))
}

pub(crate) fn write_settings(value: &Value) -> anyhow::Result<()> {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec(value)?)?;
    Ok(())
}

pub(crate) fn enabled() -> bool {
    read_settings()["enabled"].as_bool().unwrap_or(true)
}

/// Turns the built-in set on or off for every team, keeping the additions.
pub(crate) fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    let mut settings = read_settings();
    settings["enabled"] = json!(enabled);
    write_settings(&settings)
}

/// U62 (#721): one build cache that every worktree of a team shares - sccache
/// for rustc, ccache for the C and C++ compilers of a CMake build. It is the
/// project's choice, never this repository's built into Zerus: the project
/// file `.zerus/tests.toml` turns it on in `[build_cache]`, and the owner's
/// `build_cache` in the templates file (Settings > Teams) overrides any key of
/// it. Without either it is off and builds run as before.
///
/// ```toml
/// [build_cache]
/// enabled = true            # off when absent
/// size = "20G"              # each tool's bound: K, M, G or T
/// compilers = ["rust", "cxx"]
/// dir = "D:/caches/zerus"   # absolute; default: Zerus state's build-cache
/// ```
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BuildCache {
    pub dir: PathBuf,
    pub size: String,
    pub rust: bool,
    pub cxx: bool,
}

/// The default bound of each tool's cache.
const CACHE_SIZE: &str = "20G";

/// The project's `[build_cache]` with the owner's overrides, or None when it
/// is off. A malformed value turns the cache off rather than guess.
pub(crate) fn build_cache(folder: &Path) -> Option<BuildCache> {
    let mut config = serde_json::Map::new();
    let project = std::fs::read_to_string(folder.join(".zerus").join("tests.toml")).ok().and_then(|text| text.parse::<toml_edit::DocumentMut>().ok());
    if let Some(table) = project.as_ref().and_then(|document| document.get("build_cache")).and_then(toml_edit::Item::as_table_like) {
        for (key, item) in table.iter() {
            let value = match item.as_value() {
                Some(toml_edit::Value::Boolean(flag)) => json!(*flag.value()),
                Some(toml_edit::Value::String(text)) => json!(text.value()),
                Some(toml_edit::Value::Array(list)) => json!(list.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
                _ => continue,
            };
            config.insert(key.to_owned(), value);
        }
    }
    if let Some(owner) = read_settings()["build_cache"].as_object() {
        for (key, value) in owner {
            config.insert(key.clone(), value.clone());
        }
    }
    if config.get("enabled").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let size = config.get("size").and_then(Value::as_str).unwrap_or(CACHE_SIZE).trim().to_ascii_uppercase();
    let digits = size.trim_end_matches(['K', 'M', 'G', 'T']);
    if digits.is_empty() || digits.len() + 1 < size.len() || !digits.chars().all(|ch| ch.is_ascii_digit()) || digits.parse::<u64>().ok()? == 0 {
        return None;
    }
    let dir = match config.get("dir").and_then(Value::as_str) {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()),
        _ => root().join("build-cache"),
    };
    if !dir.is_absolute() {
        return None;
    }
    let compilers: Vec<String> = match config.get("compilers") {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => vec!["rust".into(), "cxx".into()],
    };
    let cache = BuildCache { dir, size, rust: compilers.iter().any(|name| name == "rust"), cxx: compilers.iter().any(|name| name == "cxx") };
    (cache.rust || cache.cxx).then_some(cache)
}

#[cfg(test)]
thread_local! {
    /// A test names the cache tools' paths instead of searching PATH.
    static TOOLS: std::cell::RefCell<Option<std::collections::HashMap<String, PathBuf>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn tools_override(tools: Option<std::collections::HashMap<String, PathBuf>>) {
    TOOLS.with(|found| *found.borrow_mut() = tools);
}

/// A cache tool on PATH, or in cargo's own bin folder.
fn cache_tool(name: &str) -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(tools) = TOOLS.with(|found| found.borrow().clone()) {
        return tools.get(name).cloned();
    }
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() };
    let mut folders: Vec<PathBuf> = std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| crate::platform::home_dir().map(|home| home.join(".cargo"))) {
        folders.push(home.join("bin"));
    }
    folders.into_iter().map(|folder| folder.join(&file)).find(|path| path.is_file())
}

/// What a run of `template` needs for the build cache: the environment, the
/// compiler launcher for a CMake build (empty: none) and a note when a tool the
/// cache names is missing, so the run says why it built without it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CacheUse {
    pub env: Vec<(String, String)>,
    pub launcher: Option<String>,
    pub note: String,
}

pub(crate) fn cache_use(template: &str, folder: &Path) -> CacheUse {
    let mut used = CacheUse::default();
    let cargo = matches!(template, "cargo-build" | "cargo-test" | "cargo-clippy");
    // Task 85: the CMake templates of any project use the C and C++ cache.
    let cmake = matches!(template, "cmake-configure" | "cmake-build");
    if !cargo && !cmake {
        return used;
    }
    // Not configured: builds run as before and the launcher is left alone.
    let Some(cache) = build_cache(folder) else { return used };
    if cargo && cache.rust {
        match cache_tool("sccache") {
            Some(sccache) => {
                // A server of the run's own on a free port, inside the run's
                // process tree: it ends with the run and never serves another.
                let port = std::net::TcpListener::bind(("127.0.0.1", 0)).ok().and_then(|socket| socket.local_addr().ok()).map(|address| address.port());
                used.env.push(("RUSTC_WRAPPER".into(), sccache.display().to_string()));
                used.env.push(("SCCACHE_DIR".into(), cache.dir.join("sccache").display().to_string()));
                used.env.push(("SCCACHE_CACHE_SIZE".into(), cache.size.clone()));
                if let Some(port) = port {
                    used.env.push(("SCCACHE_SERVER_PORT".into(), port.to_string()));
                }
            }
            None => {
                // #742: the owner is offered it in Waiting for you, once.
                let offer = crate::test_discipline::tools::offer_missing(folder, "sccache");
                used.note = format!("build cache: sccache was not found; this run built without it. {}; never install it yourself", offer_waits(offer.as_ref()));
            }
        }
    }
    if cmake {
        if !cache.cxx {
            used.launcher = Some(String::new());
            return used;
        }
        match cache_tool("ccache") {
            Some(ccache) => {
                used.launcher = Some(ccache.display().to_string());
                used.env.push(("CCACHE_DIR".into(), cache.dir.join("ccache").display().to_string()));
                used.env.push(("CCACHE_MAXSIZE".into(), cache.size.clone()));
                // Paths under the member's folder are hashed relative to it,
                // so every worktree of the team finds the others' entries.
                used.env.push(("CCACHE_BASEDIR".into(), folder.display().to_string()));
                used.env.push(("CCACHE_NOHASHDIR".into(), "1".into()));
            }
            None => {
                used.launcher = Some(String::new());
                let offer = crate::test_discipline::tools::offer_missing(folder, "ccache");
                used.note = format!("build cache: ccache was not found; this run built without it. {}; never install it yourself", offer_waits(offer.as_ref()));
            }
        }
    }
    used
}

/// Task #220: where a run's missing tool waits for its decision: the team's
/// lead's inbox when a team's session raised the offer, else the owner.
fn offer_waits(offer: Option<&crate::test_discipline::tools::Offer>) -> &'static str {
    match offer {
        Some(offer) if !offer.team.is_empty() => "Zerus offers it to your lead's inbox",
        _ => "Zerus offers it to the owner in Waiting for you",
    }
}

/// The owner's additions: fixed programs and argv kept in Zerus state and set
/// only with `hgs team templates set` (owner-only). Their ids never shadow a
/// built-in one.
pub(crate) fn additions() -> Vec<Value> {
    read_settings()["templates"].as_array().cloned().unwrap_or_default()
}

/// Replaces the additions, validated the same way a run is: a fixed program,
/// argv strings, a bounded timeout, an id that is not a built-in.
pub(crate) fn set_additions(list: &[Value]) -> anyhow::Result<()> {
    ensure!(list.len() <= 50, "The owner adds at most 50 templates");
    let mut ids = std::collections::BTreeSet::new();
    for template in list {
        let id = template["id"].as_str().context("An added template names its id")?;
        check_identifier(id, "id")?;
        ensure!(builtin(id).is_none(), "Template {id} is built in");
        ensure!(ids.insert(id.to_owned()), "Template {id} is listed twice");
        let program = template["program"].as_str().context("An added template names its program")?;
        ensure!(!program.trim().is_empty() && program.chars().count() <= 200, "An added template has a program");
        ensure!(!program.contains('{') && !program.contains('}'), "An added template's program has no placeholders");
        let args = template["args"].as_array().cloned().unwrap_or_default();
        ensure!(args.len() <= 64, "An added template has at most 64 arguments");
        for arg in &args {
            let arg = arg.as_str().context("An added template's arguments are strings")?;
            ensure!(arg.chars().count() <= 400 && !arg.contains('\0'), "An added template's argument is too long");
        }
        if let Some(seconds) = template.get("timeout_secs").and_then(|seconds| seconds.as_u64()) {
            ensure!((1..=86_400).contains(&seconds), "An added template's timeout_secs is 1-86400");
        }
    }
    let mut settings = read_settings();
    settings["templates"] = json!(list);
    write_settings(&settings)
}

/// How long Zerus lets a template run: the built-in's own bound, or the added
/// template's `timeout_secs` (1-86400; an hour by default).
pub(crate) fn timeout_for(id: &str) -> u64 {
    if let Some(template) = builtin(id) {
        return template.timeout_secs;
    }
    additions()
        .iter()
        .find(|template| template["id"].as_str() == Some(id))
        .and_then(|template| template["timeout_secs"].as_u64())
        .map(|seconds| seconds.clamp(1, 86_400))
        .unwrap_or(1800)
}

/// Task 85: a project template's own timeout, read from the folder it runs in.
pub(crate) fn timeout_in(id: &str, folder: &Path) -> u64 {
    if builtin(id).is_none() && addition(id).is_none() {
        if let Ok(loaded) = crate::test_discipline::config::load(folder, None) {
            if let Some(seconds) = loaded.config.templates.iter().find(|template| template.id == id).and_then(|template| template.timeout_secs) {
                return seconds.clamp(1, 86_400);
            }
        }
    }
    timeout_for(id)
}

/// Every template a member may name: the built-ins (when the switch is on) and
/// the owner's additions, marked so the owner sees only the additions.
pub(crate) fn listed() -> Value {
    // Task #5 (team ede724c4): the settings file is read once per listing
    // (it was read three times on every status poll).
    let settings = read_settings();
    let enabled = settings["enabled"].as_bool().unwrap_or(true);
    let mut items: Vec<Value> = Vec::new();
    if enabled {
        for template in BUILTIN {
            items.push(described(template, "builtin"));
        }
    }
    for template in settings["templates"].as_array().into_iter().flatten() {
        let mut item = template.clone();
        item["source"] = json!("owner");
        items.push(item);
    }
    json!({"enabled":enabled, "items":items})
}

fn described(template: &Template, source: &str) -> Value {
    json!({"id":template.id,"summary":template.summary,"program":template.program,"args":template.args,
        "timeout_secs":template.timeout_secs,"source":source,
        "params":template.params.iter().map(|param| json!({"name":param.name,"kind":format!("{:?}",param.kind).to_lowercase(),"required":param.required})).collect::<Vec<_>>()})
}

/// The owner-added template with `id`, if there is one.
fn addition(id: &str) -> Option<(String, Vec<String>)> {
    let list = additions();
    let template = list.iter().find(|template| template["id"].as_str() == Some(id))?;
    let program = template["program"].as_str().unwrap_or_default().to_owned();
    let args: Vec<String> = template["args"].as_array().into_iter().flatten().filter_map(|arg| arg.as_str()).map(str::to_owned).collect();
    Some((program, args))
}

/// U21: a value that fits one argv element and cannot be read as shell syntax.
fn check_identifier(value: &str, what: &str) -> anyhow::Result<()> {
    ensure!(!value.is_empty() && value.chars().count() <= 200, "A template {what} is 1-200 characters");
    ensure!(
        value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '+' | '@')),
        "A template {what} is letters, digits, _ - . : + @ (got \"{value}\")"
    );
    ensure!(!value.contains("..") && !value.starts_with('.') && !value.starts_with('-'), "A template {what} is a plain name (got \"{value}\")");
    Ok(())
}

/// A path inside the member's folder: relative, no `..`, no shell syntax.
fn check_path(value: &str, what: &str) -> anyhow::Result<()> {
    ensure!(!value.trim().is_empty() && value.chars().count() <= 400, "A template {what} is 1-400 characters");
    ensure!(!value.starts_with('/') && !value.starts_with('\\'), "A template {what} is relative to your folder (got \"{value}\")");
    ensure!(!value.contains(':'), "A template {what} stays inside your folder (got \"{value}\")");
    ensure!(
        !value.split(['/', '\\']).any(|part| part == ".." || part.is_empty()),
        "A template {what} stays inside your folder (got \"{value}\")"
    );
    ensure!(!has_shell_syntax(value), "A template {what} has no shell metacharacters (got \"{value}\")");
    Ok(())
}

/// The characters a shell would read as syntax, refused wherever a value is a
/// single word or a path even though Zerus never runs a shell (U21).
fn has_shell_syntax(value: &str) -> bool {
    value.chars().any(|c| matches!(c, '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '"' | '\'' | '^' | '!' | '*' | '?' | '\n' | '\r' | '\0'))
}

/// N3: a commit message is any text up to 4000 characters. It goes to Zerus'
/// own commit code as JSON and to git as one argv element, never to a shell, so
/// "( ) ' \" ! ? *" and newlines are allowed; only NUL is not.
fn check_message(value: &str) -> anyhow::Result<()> {
    ensure!(!value.trim().is_empty() && value.chars().count() <= 4000, "A commit message is 1-4000 characters");
    ensure!(!value.contains('\0'), "A commit message has no NUL character");
    Ok(())
}

/// N1: a branch name Zerus merges as one argv element: the lead's own branch may
/// carry a `/` (teams-v2/lead), but nothing a ref cannot have.
pub(crate) fn check_branch(value: &str) -> anyhow::Result<()> {
    ensure!(!value.is_empty() && value.chars().count() <= 200, "A branch name is 1-200 characters");
    ensure!(value != "HEAD", "The folder is on no branch");
    ensure!(
        value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '@')),
        "A branch name is letters, digits, _ - . / + @ (got \"{value}\")"
    );
    ensure!(
        !value.starts_with('-') && !value.starts_with('/') && !value.ends_with('/') && !value.contains("..") && !value.contains("//"),
        "A branch name is a plain name (got \"{value}\")"
    );
    Ok(())
}

/// One placeholder's value: a single string, or the list of words to splice in.
enum Words {
    One(String),
    Many(Vec<String>),
}

fn value_of(params: &[Param], args: &Value, name: &str, folder: &Path) -> anyhow::Result<Option<Words>> {
    let Some(param) = params.iter().find(|param| param.name == name) else { return Ok(None) };
    typed_value(param.kind, param.required, args, name, folder)
}

/// Task 85: one placeholder's checked value, for a built-in's parameter or a
/// project template's declared one alike.
fn typed_value(kind: Kind, required: bool, args: &Value, name: &str, folder: &Path) -> anyhow::Result<Option<Words>> {
    let param = Param { name: "", kind, required };
    let value = &args[name];
    // A value of the wrong type is refused, not read as empty.
    ensure!(value.is_null() || value.is_string() || matches!(value, Value::Array(_)), "The template's \"{name}\" has the wrong type");
    match param.kind {
        Kind::Path => {
            let text = value.as_str().unwrap_or_default();
            if text.is_empty() {
                ensure!(!param.required, "The template needs \"{name}\"");
                return Ok(None);
            }
            check_path(text, name)?;
            Ok(Some(Words::One(text.trim_end_matches(['/', '\\']).to_owned())))
        }
        Kind::Identifier | Kind::Text => {
            let text = value.as_str().unwrap_or_default();
            if text.is_empty() {
                ensure!(!param.required, "The template needs \"{name}\"");
                return Ok(None);
            }
            if param.kind == Kind::Identifier {
                check_identifier(text, name)?;
            } else {
                check_message(text)?;
            }
            Ok(Some(Words::One(text.to_owned())))
        }
        Kind::Identifiers | Kind::Paths => {
            let list = match value {
                Value::Null => Vec::new(),
                Value::String(one) => vec![one.clone()],
                Value::Array(items) => items.iter().map(|item| item.as_str().unwrap_or_default().to_owned()).collect(),
                _ => bail!("The template's \"{name}\" is a list"),
            };
            // An affected set's filters, skips and suites run to hundreds.
            let bound = if matches!(name, "filters" | "skips" | "suites" | "functions") { 512 } else { 64 };
            ensure!(list.len() <= bound, "The template's \"{name}\" has at most {bound} values");
            let mut words = Vec::new();
            for word in list {
                if param.kind == Kind::Paths {
                    check_path(&word, name)?;
                    ensure!(folder.join(&word).lexists(), "The template's path {word} does not exist in your folder");
                } else {
                    check_identifier(&word, name)?;
                }
                words.push(word);
            }
            ensure!(!param.required || !words.is_empty(), "The template needs \"{name}\"");
            if words.is_empty() {
                return Ok(None);
            }
            Ok(Some(Words::Many(words)))
        }
    }
}

/// Task 86: the project's own interpreter, never created or synced. uv,
/// poetry and plain venvs all live in `.venv` or `venv`; a project without one
/// is refused with the reason.
fn interpreter(folder: &Path) -> anyhow::Result<(String, Vec<String>)> {
    for candidate in [".venv/Scripts/python.exe", ".venv/bin/python", "venv/Scripts/python.exe", "venv/bin/python"] {
        if folder.join(candidate).is_file() {
            return Ok((candidate.to_owned(), Vec::new()));
        }
    }
    // uv and poetry own their environment: use their runner, never create one.
    if folder.join("uv.lock").is_file() {
        return Ok(("uv".into(), vec!["run".into(), "python".into()]));
    }
    if folder.join("poetry.lock").is_file() {
        let output = Command::new("poetry").args(["env", "info", "-p"]).current_dir(folder).output();
        if let Ok(output) = output {
            if output.status.success() {
                let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                let python = if cfg!(windows) { format!("{root}/Scripts/python.exe") } else { format!("{root}/bin/python") };
                if Path::new(&python).is_file() {
                    return Ok((python, Vec::new()));
                }
            }
        }
        bail!("poetry.lock has no Poetry environment; run `poetry env use` yourself; Zerus never creates or syncs one");
    }
    bail!("No project Python environment: create .venv with uv, poetry or venv first; Zerus never creates or syncs one")
}
/// Task 87: one registry string; the system reader queries reg.exe.
trait EngineRegistry {
    fn value(&self, root: &str, key: &str, name: &str) -> Option<String>;
}
struct SystemRegistry;
impl EngineRegistry for SystemRegistry {
    fn value(&self, root: &str, key: &str, name: &str) -> Option<String> {
        // reg.exe from System32, never one found on the PATH.
        let system = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        let output = Command::new(system.join("System32").join("reg.exe")).args(["query", &format!("{root}\\{key}"), "/v", name]).output().ok()?;
        if !output.status.success() { return None; }
        parse_reg_value(&String::from_utf8_lossy(&output.stdout), name)
    }
}

/// The data of the value `name` in what `reg query KEY /v NAME` prints:
/// `    InstalledDirectory    REG_SZ    D:\EGS\UE_5.3`. The type token is not
/// part of it and the data is kept verbatim (a path with spaces stays whole).
fn parse_reg_value(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix(name)?;
        // The whole name, not a longer one that starts with it.
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let rest = rest.trim_start();
        let (kind, data) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let data = data.trim();
        (matches!(kind, "REG_SZ" | "REG_EXPAND_SZ") && !data.is_empty()).then(|| data.to_owned())
    })
}

/// The engine from the project's EngineAssociation: a launcher version reads
/// HKLM InstalledDirectory, a source-build GUID reads HKCU Builds. The
/// per-machine override wins; a missing engine names every place tried.
fn unreal_engine(project: &Path, override_path: Option<&str>, registry: &dyn EngineRegistry) -> anyhow::Result<String> {
    if let Some(path) = override_path.filter(|path| !path.trim().is_empty()) {
        return Ok(path.to_owned());
    }
    if let Some(path) = std::env::var("ZERUS_UE_ENGINE").ok().filter(|path| !path.trim().is_empty()) {
        return Ok(path);
    }
    let text = std::fs::read_to_string(project).unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let association = value.get("EngineAssociation").and_then(serde_json::Value::as_str).unwrap_or_default();
    if association.is_empty() {
        bail!("{} names no EngineAssociation", project.display());
    }
    // A launcher engine ("5.3", "4.27"): HKLM\SOFTWARE\EpicGames\Unreal Engine\<version>,
    // value InstalledDirectory. A source build (a GUID): HKCU\Software\Epic Games\
    // Unreal Engine\Builds, a value named like the GUID. Both as reg.exe prints them.
    let version = association.chars().all(|c| c.is_ascii_digit() || c == '.');
    let (root, key, name) = if version {
        ("HKLM", format!("SOFTWARE\\EpicGames\\Unreal Engine\\{association}"), "InstalledDirectory")
    } else {
        ("HKCU", "Software\\Epic Games\\Unreal Engine\\Builds".to_owned(), association)
    };
    if let Some(path) = registry.value(root, &key, name) {
        return Ok(path);
    }
    bail!("No Unreal engine for EngineAssociation {association}: looked in {root}\\{key} (value {name}), ZERUS_UE_ENGINE, and the engine override")
}

/// Task 87/210: the two Unreal templates. Both take the project as an absolute
/// path: Build.bat changes into Engine/Build/BatchFiles and UnrealEditor-Cmd
/// resolves against the engine, so a path relative to the member's folder finds
/// nothing (the merger's task 189 report: exit 6 "Unable to find project file",
/// exit 1 with an empty tail). The test's saved folder, log and report stay in
/// the member's `.zerus-team`, absolute for the same reason.
fn unreal(template: &Template, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    let id = template.id;
    let project = one(template, args, "project", folder)?.context("The template needs a project")?;
    let project = folder.join(&project).display().to_string();
    let engine = unreal_engine(Path::new(&project), one(template, args, "engine", folder)?.as_deref(), &SystemRegistry)?;
    let ue = folder.join(".zerus-team").join("ue");
    if id == "ue-build" {
        let target = one(template, args, "target", folder)?.context("The template ue-build needs a target")?;
        let mut argv = vec![project.clone(), target];
        if let Some(platform) = one(template, args, "platform", folder)? {
            argv.push(platform);
        }
        if let Some(config) = one(template, args, "config", folder)? {
            argv.push(config);
        }
        argv.push(format!("-Project={project}"));
        let program = format!("{}/Engine/Build/BatchFiles/Build.bat", engine.trim_end_matches(['/', '\\']));
        return Ok(Plan { template: id.to_owned(), program, argv, commit: None, more: Vec::new(), cwd: None });
    }
    // Task 90: -ExecCmds takes `;`-separated console commands, so the filter is
    // one filter: no `;` and no line break. The engine must hold the editor.
    let filters = one(template, args, "filters", folder)?.unwrap_or_default();
    ensure!(!filters.contains(';') && !filters.contains(['\n', '\r']), "The template's \"filters\" is one automation filter: no ';' and no line break");
    let bin = Path::new(&engine).join("Engine").join("Binaries").join("Win64");
    let name = ["UnrealEditor-Cmd.exe", "UE4Editor-Cmd.exe"].into_iter().find(|name| bin.join(name).is_file());
    let Some(name) = name else { bail!("The Unreal engine {engine} holds no Engine/Binaries/Win64/UnrealEditor-Cmd.exe (or UE4Editor-Cmd.exe)") };
    let mut argv = vec![
        project,
        "-nullrhi".to_owned(),
        "-unattended".to_owned(),
        format!("-saveddir={}", ue.join("Saved").display()),
        format!("-abslog={}", ue.join("Unreal.log").display()),
    ];
    if !filters.is_empty() {
        argv.push(format!("-ExecCmds=Automation RunTests {filters};Quit"));
    }
    argv.push(format!("-ReportExportPath={}", folder.join(".zerus-team").join("results").join("unreal").display()));
    let program = format!("{}/Engine/Binaries/Win64/{name}", engine.trim_end_matches(['/', '\\']));
    Ok(Plan { template: id.to_owned(), program, argv, commit: None, more: Vec::new(), cwd: None })
}
/// Validates a run request and builds the exact program and argv (U21). Nothing
/// here reads a shell: every value is one argv element, or spliced as whole
/// elements for the list placeholders.
pub(crate) fn plan(id: &str, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    ensure!(args.is_object(), "The template's arguments are a JSON object");
    // A built-in id always means the built-in template: an owner addition may
    // not shadow it.
    if builtin(id).is_none() {
        if let Some((program, argv)) = addition(id) {
            ensure!(enabled(), "The built-in command templates are turned off in Settings > Teams");
            if let Some((key, _)) = args.as_object().into_iter().flatten().next() {
                bail!("The project template {id} takes no placeholders (got \"{key}\")");
            }
            return Ok(Plan { template: id.to_owned(), program, argv, commit: None, more: Vec::new(), cwd: None });
        }
        // Task 85: a [[template]] of the folder's .zerus/tests.toml, typed like
        // a built-in, once the owner accepted its exact content.
        if let Some(found) = project_template(folder, id)? {
            ensure!(enabled(), "The built-in command templates are turned off in Settings > Teams");
            return project_plan(&found, args, folder);
        }
    }
    if let Some(refusal) = removed(id) {
        bail!("{refusal}");
    }
    let Some(template) = builtin(id) else { bail!("Unknown template {id}") };
    ensure!(enabled(), "The built-in command templates are turned off in Settings > Teams");
    let mut args = args.clone();
    let args = &mut args;
    for (key, _) in args.as_object().into_iter().flatten() {
        ensure!(template.params.iter().any(|param| param.name == key), "The template {id} has no \"{key}\"");
    }
    // Task 85: the CMake, MSBuild, Unreal and Python templates build their own
    // argv.
    match template.id {
        "artifact-test" => {
            let profile=one(template,args,"profile",folder)?.context("Name an executor profile")?;
            let job=one(template,args,"job",folder)?.context("Name a stable artifact job")?;
            return super::artifacts::plan(&profile,&job);
        }
        "ue-build" | "ue-test" => return unreal(template, args, folder),
        "cmake-configure" => return cmake_configure(template, args, folder),
        "cmake-build" => return cmake_build(template, args, folder),
        "ctest" => return ctest(template, args, folder),
        "msbuild-build" => return msbuild_build(template, args, folder),
        "vstest" => return vstest(template, args, folder),
        "web-test" => return web_test(template, args, folder),
        _ => {}
    }
    if template.id == "git-commit" {
        let message = args["message"].as_str().unwrap_or_default().to_owned();
        check_message(&message)?;
        let paths: Vec<String> = match &args["paths"] {
            Value::Null => Vec::new(),
            Value::String(one) => vec![one.clone()],
            Value::Array(items) => items.iter().map(|item| item.as_str().unwrap_or_default().to_owned()).collect(),
            _ => bail!("The template's \"paths\" is a list"),
        };
        for path in &paths {
            check_path(path, "paths")?;
            ensure!(folder.join(path).lexists(), "The template's path {path} does not exist in your folder");
        }
        return Ok(Plan { template: "git-commit".to_owned(), program: "hgs".into(), argv: vec!["team".into(), "commit".into(), "--json".into()], commit: Some(json!({"message":message,"paths":paths})), more: Vec::new(), cwd: None });
    }
    // Task 153: a Python project in a subfolder resolves its environment and
    // setup.py there and runs there; every path it is given stays the
    // member's, made absolute so the project's folder reads it the same.
    let python_project = if matches!(template.id, "py-test" | "cython-build") { python_project(template, args, folder)? } else { None };
    let cwd = python_project.clone();
    let (mut program, prefix) = if matches!(template.id, "py-test" | "cython-build") {
        let project_dir = python_project.as_ref().map_or_else(|| folder.to_path_buf(), |project| folder.join(project));
        let (python, prefix) = interpreter(&project_dir)?;
        if template.id == "py-test" { let _ = std::fs::create_dir_all(folder.join(".zerus-team").join("python")); }
        // A relative venv interpreter is the project's own.
        let python = if python_project.is_some() && (python.contains('/') || python.contains('\\')) && Path::new(&python).is_relative() {
            project_dir.join(&python).display().to_string()
        } else {
            python
        };
        (python, prefix)
    } else {
        (template.program.to_owned(), Vec::new())
    };
    let mut argv = Vec::new();
    for raw in template.args {
        if let Some(name) = whole_placeholder(raw) {
            match value_of(template.params, args, name, folder)? {
                Some(Words::One(word)) => argv.push(word),
                Some(Words::Many(words)) => argv.extend(words),
                None => {}
            }
            continue;
        }
        let mut element: String = (*raw).to_owned();
        let mut keep = true;
        for placeholder in placeholders(raw) {
            match value_of(template.params, args, &placeholder, folder)? {
                Some(Words::One(word)) => element = element.replace(&format!("{{{placeholder}}}"), &word),
                // An absent optional placeholder drops the whole argument, so
                // `--package={filter}` is never a bare `--package=`.
                None => {
                    keep = false;
                    break;
                }
                Some(Words::Many(_)) => bail!("The template {id} uses \"{{{placeholder}}}\" inside an argument, which needs one value"),
            }
        }
        if keep {
            argv.push(element);
        }    }
    argv.splice(0..0, prefix);
    for placeholder in placeholders(&program) {
        let Some(Words::One(word)) = value_of(template.params, args, &placeholder, folder)? else {
            bail!("The template {id} needs a single \"{placeholder}\" for its program");
        };
        program = program.replace(&format!("{{{placeholder}}}"), &word);
    }
    let more = Vec::new();
    if template.id == "cargo-test" {
        // #741: another crate's manifest, then the filters and skips of an
        // affected set after `--`, each its own argv element.
        if let Some(Words::Many(manifest)) = value_of(template.params, args, "manifest", folder)? {
            ensure!(manifest.len() == 1 && manifest[0].ends_with("Cargo.toml"), "The template's \"manifest\" is one Cargo.toml in your folder");
            argv.push("--manifest-path".into());
            argv.push(manifest[0].clone());
        }
        let filters = match value_of(template.params, args, "filters", folder)? {
            Some(Words::Many(filters)) => filters,
            _ => Vec::new(),
        };
        let skips = match value_of(template.params, args, "skips", folder)? {
            Some(Words::Many(skips)) => skips,
            _ => Vec::new(),
        };
        if !filters.is_empty() || !skips.is_empty() {
            argv.push("--".into());
            argv.extend(filters);
            for skip in skips {
                argv.push("--skip".into());
                argv.push(skip);
            }
        }
    }
    if template.id == "clean-build" {
        // Only a generated build output, a real folder (not a link) below the
        // member's folder, never the folder itself or a source tree.
        if let Some(Words::Many(paths)) = value_of(template.params, args, "paths", folder)? {
            for path in &paths {
                let last = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or_default();
                ensure!(
                    matches!(last, ".ci-build" | "build" | "target" | ".gradle" | "out"),
                    "clean-build removes only build outputs (.ci-build, build, target, .gradle, out), not {path}"
                );
                let meta = std::fs::symlink_metadata(folder.join(path)).context("The build folder is not there")?;
                ensure!(meta.is_dir() && !meta.file_type().is_symlink(), "clean-build removes a real folder, not {path}");
            }
            argv.extend(paths);
        }
    }
    if template.id == "android-test" {
        let path = folder.join(&program);
        let path = if cfg!(windows) { folder.join(format!("{program}.bat")) } else { path };
        ensure!(path.is_file(), "The Android Gradle wrapper {} is not in your folder", path.display());
        program = path.display().to_string();
        let home = folder.join(".zerus-team").join("gradle");
        argv.extend(["-g".to_owned(), home.join("home").display().to_string(), "--project-cache-dir".to_owned(), home.join("cache").display().to_string()]);
    }
    if cwd.is_some() {
        argv = argv.into_iter().map(|word| member_path_absolute(template, args, folder, word)).collect();
    }
    Ok(Plan { template: template.id.to_owned(), program, argv, commit: None, more, cwd })
}

/// One optional placeholder of a built-in as a single word.
/// Task 153: the Python project folder of py-test or cython-build: a folder
/// inside the member's folder (its real path too, so no link leads out), or
/// None for the member's folder itself.
fn python_project(template: &Template, args: &Value, folder: &Path) -> anyhow::Result<Option<String>> {
    let Some(project) = one(template, args, "project", folder)? else { return Ok(None) };
    let project = project.replace('\\', "/").trim_end_matches('/').to_owned();
    if project == "." {
        return Ok(None);
    }
    let dir = folder.join(&project);
    ensure!(dir.is_dir(), "The template's project {project} is not a folder in your folder");
    let real = std::fs::canonicalize(&dir).context("The project folder cannot be read")?;
    let base = std::fs::canonicalize(folder).context("Your folder cannot be read")?;
    ensure!(real.starts_with(&base), "The template's project {project} leads outside your folder");
    Ok(Some(project))
}

/// Task 153: in a project subfolder, the member-relative paths a Python
/// template was given (test files, sources, its JUnit report) as absolute
/// paths, so they name the same files from the project's folder.
fn member_path_absolute(template: &Template, args: &Value, folder: &Path, word: String) -> String {
    if let Some(report) = word.strip_prefix("--junitxml=") {
        return format!("--junitxml={}", folder.join(report).display());
    }
    let given = |name: &str| -> bool {
        template.params.iter().any(|param| param.name == name && param.kind == Kind::Paths)
            && args[name].as_array().is_some_and(|items| items.iter().any(|item| item.as_str() == Some(word.as_str())))
            || args[name].as_str() == Some(word.as_str())
    };
    if given("paths") || given("sources") {
        folder.join(&word).display().to_string()
    } else {
        word
    }
}

fn one(template: &Template, args: &Value, name: &str, folder: &Path) -> anyhow::Result<Option<String>> {
    Ok(match value_of(template.params, args, name, folder)? {
        Some(Words::One(word)) => Some(word),
        Some(Words::Many(mut words)) if words.len() == 1 => words.pop(),
        Some(Words::Many(_)) => bail!("The template's \"{name}\" is one value"),
        None => None,
    })
}

/// One optional list placeholder of a built-in.
fn many(template: &Template, args: &Value, name: &str, folder: &Path) -> anyhow::Result<Vec<String>> {
    Ok(match value_of(template.params, args, name, folder)? {
        Some(Words::Many(words)) => words,
        Some(Words::One(word)) => vec![word],
        None => Vec::new(),
    })
}

/// Task 85: the configure argv of a CMake project.
pub(crate) fn cmake_configure_argv(source: &str, build_dir: &str, generator: Option<&str>, config: Option<&str>, preset: Option<&str>, launcher: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = vec!["-S".into(), source.into(), "-B".into(), build_dir.into()];
    if let Some(generator) = generator {
        argv.extend(["-G".into(), generator.into()]);
    }
    if let Some(config) = config {
        argv.push(format!("-DCMAKE_BUILD_TYPE={config}"));
    }
    if let Some(preset) = preset {
        argv.extend(["--preset".into(), preset.into()]);
    }
    // U62 (#721): the compiler launcher of the build cache, or none.
    if let Some(launcher) = launcher {
        argv.push(format!("-DCMAKE_C_COMPILER_LAUNCHER={launcher}"));
        argv.push(format!("-DCMAKE_CXX_COMPILER_LAUNCHER={launcher}"));
    }
    argv
}

fn plain(template: &str, program: &str, argv: Vec<String>) -> Plan {
    Plan { template: template.to_owned(), program: program.to_owned(), argv, commit: None, more: Vec::new(), cwd: None }
}

fn cmake_configure(template: &Template, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    let launcher = cache_use(template.id, folder).launcher;
    let source = one(template, args, "source", folder)?.unwrap_or_else(|| ".".into());
    ensure!(folder.join(&source).join("CMakeLists.txt").is_file(), "There is no CMakeLists.txt in {source} of your folder");
    let build_dir = one(template, args, "build_dir", folder)?.unwrap_or_else(|| CMAKE_BUILD_DIR.into());
    ensure!(build_dir != "." && build_dir != source, "The build folder is its own folder, not the sources");
    let generator = one(template, args, "generator", folder)?;
    if let Some(generator) = &generator {
        ensure!(generator.chars().count() <= 100 && !generator.contains('\n'), "A CMake generator is one line of at most 100 characters");
    }
    let config = one(template, args, "config", folder)?;
    let preset = one(template, args, "preset", folder)?;
    let argv = cmake_configure_argv(&source, &build_dir, generator.as_deref(), config.as_deref(), preset.as_deref(), launcher.as_deref());
    Ok(plain(template.id, "cmake", argv))
}

fn cmake_build(template: &Template, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    let build_dir = one(template, args, "build_dir", folder)?.unwrap_or_else(|| CMAKE_BUILD_DIR.into());
    let mut argv: Vec<String> = vec!["--build".into(), build_dir];
    if template.id == "cmake-build" {
        if let Some(config) = one(template, args, "config", folder)? {
            argv.extend(["--config".into(), config]);
        }
    }
    // U21: cmake takes each target after one `--target`; Zerus adds the flag
    // itself, so a target is never read as another option.
    let targets = many(template, args, "targets", folder)?;
    if !targets.is_empty() {
        argv.push("--target".into());
        argv.extend(targets);
    }
    Ok(plain(template.id, "cmake", argv))
}

/// Task 85: where a run's test report goes: one file per template under the
/// folder's `.zerus-team/results`, removed before each run.
pub(crate) fn report_path(folder: &Path, template: &str) -> Option<(PathBuf, super::run_results::Format)> {
    use super::run_results::Format;
    let results = folder.join(".zerus-team").join("results");
    match template {
        "ctest" => Some((results.join("ctest.xml"), Format::Junit)),
        "vstest" => Some((results.join("vstest.trx"), Format::Trx)),
        // Task #239: vitest's own JUnit report, one suite per test file.
        "web-test" => Some((results.join("vitest.xml"), Format::Junit)),
        _ => project_report(folder, template),
    }
}

/// `a.b` as an anchored, escaped alternation for ctest -R.
fn anchored(names: &[String]) -> String {
    let escaped: Vec<String> = names
        .iter()
        .map(|name| name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':' || c == '@' { c.to_string() } else { format!("\\{c}") }).collect())
        .collect();
    format!("^({})$", escaped.join("|"))
}

fn ctest(template: &Template, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    let build_dir = one(template, args, "build_dir", folder)?.unwrap_or_else(|| CMAKE_BUILD_DIR.into());
    ensure!(folder.join(&build_dir).is_dir(), "The build folder {build_dir} is not in your folder: configure and build first");
    let tests = many(template, args, "tests", folder)?;
    let regex = one(template, args, "regex", folder)?;
    ensure!(tests.is_empty() || regex.is_none(), "Name the tests or a regex, not both");
    let (report, _) = report_path(folder, "ctest").context("the ctest report")?;
    let mut argv: Vec<String> = vec!["--test-dir".into(), build_dir, "--output-on-failure".into(), "--output-junit".into(), report.display().to_string()];
    if let Some(config) = one(template, args, "config", folder)? {
        argv.extend(["-C".into(), config]);
    }
    if !tests.is_empty() {
        argv.extend(["-R".into(), anchored(&tests)]);
    } else if let Some(regex) = regex {
        ensure!(regex.chars().count() <= 2000 && !regex.contains('\n'), "A ctest regex is one line of at most 2000 characters");
        argv.extend(["-R".into(), regex]);
    }
    Ok(plain(template.id, "ctest", argv))
}

fn msbuild_build(template: &Template, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    let project = many(template, args, "project", folder)?;
    ensure!(project.len() == 1, "The template's \"project\" is one .sln or project file in your folder");
    let project = project[0].clone();
    let extension = Path::new(&project).extension().and_then(|extension| extension.to_str()).unwrap_or_default().to_ascii_lowercase();
    ensure!(matches!(extension.as_str(), "sln" | "slnx" | "vcxproj" | "csproj" | "vbproj" | "fsproj" | "proj"), "MSBuild builds a .sln or a project file, not {project}");
    let mut argv: Vec<String> = vec![project, "/m".into(), "/nologo".into(), "/v:minimal".into()];
    if let Some(config) = one(template, args, "config", folder)? {
        argv.push(format!("/p:Configuration={config}"));
    }
    if let Some(platform) = one(template, args, "platform", folder)? {
        ensure!(platform.chars().count() <= 40 && platform.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_' || c == '-'), "An MSBuild platform is letters, digits, spaces, _ and - (got \"{platform}\")");
        argv.push(format!("/p:Platform={platform}"));
    }
    let targets = many(template, args, "targets", folder)?;
    if !targets.is_empty() {
        argv.push(format!("/t:{}", targets.join(";")));
    }
    Ok(plain(template.id, &msbuild_program(), argv))
}

fn vstest(template: &Template, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    let tests = many(template, args, "tests", folder)?;
    ensure!(!tests.is_empty(), "The template needs \"tests\": the test assemblies to run");
    for test in &tests {
        let extension = Path::new(test).extension().and_then(|extension| extension.to_str()).unwrap_or_default().to_ascii_lowercase();
        ensure!(matches!(extension.as_str(), "dll" | "exe"), "vstest runs test assemblies (.dll, .exe), not {test}");
    }
    let (report, _) = report_path(folder, "vstest").context("the vstest report")?;
    let mut argv = tests;
    argv.push(format!("/Logger:trx;LogFileName={}", report.display()));
    if let Some(filter) = one(template, args, "filter", folder)? {
        ensure!(filter.chars().count() <= 2000 && !filter.contains('\n'), "A test case filter is one line of at most 2000 characters");
        argv.push(format!("/TestCaseFilter:{filter}"));
    }
    Ok(plain(template.id, &vstest_program(), argv))
}

/// Task 85: the MSBuild the templates run, for the broker's version line.
pub(crate) fn msbuild_path() -> String {
    msbuild_program()
}

/// Task #239: the web project's vitest run. The project (default the member's
/// folder, `client` for this repository) must carry package.json; its own
/// node_modules run the tests, and a project without them installs first
/// through the worker (`npm ci`, or `npm install` without a lock file), so a
/// sandboxed member never needs npm. The run writes a JUnit report: one
/// suite per test file, each failure by name.
fn web_test(template: &Template, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    let project = one(template, args, "project", folder)?
        .map(|project| project.replace('\\', "/").trim_end_matches('/').to_owned())
        .filter(|project| project != ".")
        // Task #16 (team ede724c4): the default the template's summary names:
        // a folder without its own package.json runs `client/` when that is
        // the web project (this repository). It refused "The web project has
        // no package.json (<folder>)" instead.
        .or_else(|| (!folder.join("package.json").is_file() && folder.join("client").join("package.json").is_file()).then(|| "client".to_owned()));
    let dir = project.as_ref().map_or_else(|| folder.to_path_buf(), |project| folder.join(project));
    if let Some(project) = &project {
        let real = std::fs::canonicalize(&dir).with_context(|| format!("The template's project {project} is not a folder in your folder"))?;
        let base = std::fs::canonicalize(folder).context("Your folder cannot be read")?;
        ensure!(real.starts_with(&base), "The template's project {project} leads outside your folder");
    }
    ensure!(dir.join("package.json").is_file(), "The web project has no package.json ({})", dir.display());
    let (report, _) = report_path(folder, template.id).context("the vitest report")?;
    if let Some(parent) = report.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let (program, mut argv) = match local_vitest(&dir) {
        // The project's own vitest, run by node: no npm lookup, no PATH, and
        // the same binary the member's own editor uses.
        Some(script) => ("node".to_owned(), vec![script.display().to_string()]),
        // A project whose vitest comes from a parent node_modules (a workspace)
        // resolves it through npx without installing anything.
        #[cfg(windows)]
        None => ("cmd".to_owned(), vec!["/C".into(), "npx".to_owned(), "--no-install".to_owned(), "vitest".to_owned()]),
        #[cfg(not(windows))]
        None => ("npx".to_owned(), vec!["--no-install".to_owned(), "vitest".to_owned()]),
    };
    argv.push("run".to_owned());
    argv.push("--reporter=default".to_owned());
    argv.push("--reporter=junit".to_owned());
    argv.push(format!("--outputFile.junit={}", report.display()));
    for path in many(template, args, "paths", folder)? {
        argv.push(folder.join(&path).display().to_string());
    }
    if dir.join("node_modules").is_dir() {
        return Ok(Plan { template: template.id.to_owned(), program, argv, commit: None, more: Vec::new(), cwd: project });
    }
    let (install, install_argv) = web_install(&dir);
    Ok(Plan { template: template.id.to_owned(), program: install, argv: install_argv, commit: None, more: vec![(program, argv)], cwd: project })
}

/// The vitest script of a project's own node_modules, or None when they do not
/// carry one (npx resolves it instead).
fn local_vitest(dir: &Path) -> Option<PathBuf> {
    let script = dir.join("node_modules").join("vitest").join("vitest.mjs");
    script.is_file().then_some(script)
}

/// The install a missing node_modules needs: `npm ci`, or `npm install` when
/// the project has no lock file.
fn web_install(dir: &Path) -> (String, Vec<String>) {
    let ci = if dir.join("package-lock.json").is_file() || dir.join("npm-shrinkwrap.json").is_file() { "ci" } else { "install" };
    let fixed = ["--no-audit", "--no-fund", "--loglevel", "error"];
    #[cfg(windows)]
    {
        // npm is a .cmd; cmd is what runs it, with fixed words only.
        let mut argv = vec!["/C".to_owned(), "npm".to_owned(), ci.to_owned()];
        argv.extend(fixed.iter().map(|word| (*word).to_owned()));
        ("cmd".to_owned(), argv)
    }
    #[cfg(not(windows))]
    {
        let mut argv = vec![ci.to_owned()];
        argv.extend(fixed.iter().map(|word| (*word).to_owned()));
        ("npm".to_owned(), argv)
    }
}

/// Visual Studio's MSBuild, as vswhere names it; `msbuild` on PATH otherwise.
fn msbuild_program() -> String {
    visual_studio_find(r"MSBuild\**\Bin\MSBuild.exe", "Microsoft.Component.MSBuild").unwrap_or_else(|| "msbuild".into())
}

/// Visual Studio's vstest.console; `vstest.console` on PATH otherwise.
fn vstest_program() -> String {
    visual_studio_find(r"**\vstest.console.exe", "Microsoft.VisualStudio.PackageGroup.TestTools.Core").unwrap_or_else(|| "vstest.console".into())
}

#[cfg(windows)]
fn visual_studio_find(pattern: &str, component: &str) -> Option<String> {
    let program_files = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let vswhere = Path::new(&program_files).join(r"Microsoft Visual Studio\Installer\vswhere.exe");
    let query = |requires: Option<&str>| -> Option<String> {
        let mut command = Command::new(&vswhere);
        command.args(["-latest", "-products", "*"]);
        if let Some(component) = requires {
            command.args(["-requires", component]);
        }
        let output = command.args(["-find", pattern]).output().ok()?;
        let found = String::from_utf8_lossy(&output.stdout).lines().map(str::trim).find(|line| !line.is_empty())?.to_owned();
        Path::new(&found).is_file().then_some(found)
    };
    query(Some(component)).or_else(|| query(None))
}

#[cfg(not(windows))]
fn visual_studio_find(_pattern: &str, _component: &str) -> Option<String> {
    None
}

/// Task 85: a `[[template]]` of the folder's tests file, or None when the
/// file names none with this id. One whose exact content the owner has not
/// accepted is refused: it would run outside the member's sandbox.
fn project_template(folder: &Path, id: &str) -> anyhow::Result<Option<crate::test_discipline::config::ProjectTemplate>> {
    let path = folder.join(crate::test_discipline::config::FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let loaded = crate::test_discipline::config::load(folder, None)?;
    let Some(found) = loaded.config.templates.into_iter().find(|template| template.id == id) else { return Ok(None) };
    let hash = found.content_hash();
    ensure!(
        accepted_project().contains(&hash),
        "The project template {id} of .zerus/tests.toml waits for the owner's acceptance: it runs outside your sandbox, so the owner accepts its exact content \
         (hgs team templates accept, content {hash}); a changed template waits again"
    );
    Ok(Some(found))
}

/// The content hashes of the project templates the owner accepted.
fn accepted_project() -> Vec<String> {
    read_settings()["accepted_project"].as_array().into_iter().flatten().filter_map(|hash| hash.as_str().map(str::to_owned)).collect()
}

/// Task 85: the owner accepts project templates by their content hash (at
/// most 200 kept, the newest last).
pub(crate) fn accept_project(hashes: &[String]) -> anyhow::Result<Vec<String>> {
    let mut accepted = accepted_project();
    for hash in hashes {
        ensure!(hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()), "A project template's content hash is 64 hex digits");
        if !accepted.contains(hash) {
            accepted.push(hash.clone());
        }
    }
    let excess = accepted.len().saturating_sub(200);
    accepted.drain(..excess);
    let mut settings = read_settings();
    settings["accepted_project"] = json!(accepted);
    write_settings(&settings)?;
    Ok(accepted)
}

/// Task 85: the project templates of a folder with their hashes and whether
/// the owner accepted each, for `hgs team templates`.
pub(crate) fn project_listed(folder: &Path) -> anyhow::Result<Vec<Value>> {
    if !folder.join(crate::test_discipline::config::FILE).is_file() {
        return Ok(Vec::new());
    }
    let loaded = crate::test_discipline::config::load(folder, None)?;
    let accepted = accepted_project();
    Ok(loaded
        .config
        .templates
        .iter()
        .map(|template| {
            let hash = template.content_hash();
            let mut item = serde_json::to_value(template).unwrap_or_default();
            item["hash"] = json!(hash);
            item["accepted"] = json!(accepted.contains(&hash));
            item["source"] = json!("project");
            item
        })
        .collect())
}

fn kind_of(name: &str) -> Option<Kind> {
    match name {
        "identifier" => Some(Kind::Identifier),
        "identifiers" => Some(Kind::Identifiers),
        "text" => Some(Kind::Text),
        "paths" => Some(Kind::Paths),
        "path" => Some(Kind::Path),
        _ => None,
    }
}

/// Task 85: a project template's own checks, the same as a built-in's: an id
/// that is no built-in, a fixed program, every placeholder declared with a
/// known type, bounded arguments and timeout.
pub(crate) fn check_project_template(template: &crate::test_discipline::config::ProjectTemplate) -> anyhow::Result<()> {
    check_identifier(&template.id, "id")?;
    ensure!(builtin(&template.id).is_none(), "Template {} is built in", template.id);
    // Task #58: a project template may not take a removed Qt template's id;
    // the project is told its replacement.
    if let Some(refusal) = removed(&template.id) {
        bail!("{refusal}");
    }
    ensure!(!template.program.trim().is_empty() && template.program.chars().count() <= 200, "The project template {} names its program", template.id);
    ensure!(!template.program.contains('{') && !template.program.contains('}'), "The project template {}'s program has no placeholders", template.id);
    ensure!(template.args.len() <= 64, "The project template {} has at most 64 arguments", template.id);
    ensure!(template.params.len() <= 32, "The project template {} has at most 32 parameters", template.id);
    let mut names = std::collections::BTreeSet::new();
    for param in &template.params {
        check_identifier(&param.name, "parameter")?;
        ensure!(kind_of(&param.kind).is_some(), "The parameter {} of {} is identifier, identifiers, text, path or paths (got {})", param.name, template.id, param.kind);
        ensure!(names.insert(param.name.as_str()), "The project template {} declares {} twice", template.id, param.name);
    }
    for arg in &template.args {
        ensure!(arg.chars().count() <= 400 && !arg.contains('\0'), "An argument of {} is too long", template.id);
        for placeholder in placeholders(arg) {
            let declared = template.params.iter().find(|param| param.name == placeholder);
            ensure!(declared.is_some(), "The project template {} uses {{{placeholder}}} without declaring it", template.id);
            let list = declared.is_some_and(|param| matches!(param.kind.as_str(), "identifiers" | "paths"));
            ensure!(!list || whole_placeholder(arg).is_some(), "The list {{{placeholder}}} of {} is a whole argument", template.id);
        }
    }
    if let Some(seconds) = template.timeout_secs {
        ensure!((1..=86_400).contains(&seconds), "The project template {}'s timeout_secs is 1-86400", template.id);
    }
    if let Some(results) = &template.results {
        ensure!(super::run_results::Format::parse(&results.format).is_some(), "The project template {}'s results format is junit or trx", template.id);
        check_path(&results.path, "results path")?;
    }
    Ok(())
}

/// Task 85: the argv of an accepted project template, every value checked by
/// its declared type; an absent optional value drops its whole argument, as a
/// built-in's does.
fn project_plan(template: &crate::test_discipline::config::ProjectTemplate, args: &Value, folder: &Path) -> anyhow::Result<Plan> {
    check_project_template(template)?;
    for (key, _) in args.as_object().into_iter().flatten() {
        ensure!(template.params.iter().any(|param| param.name == *key), "The template {} has no \"{key}\"", template.id);
    }
    let declared = |name: &str| template.params.iter().find(|param| param.name == name).and_then(|param| kind_of(&param.kind).map(|kind| (kind, param.required)));
    let mut argv = Vec::new();
    for raw in &template.args {
        if let Some(name) = whole_placeholder(raw) {
            let (kind, required) = declared(name).context("an undeclared placeholder")?;
            match typed_value(kind, required, args, name, folder)? {
                Some(Words::One(word)) => argv.push(word),
                Some(Words::Many(words)) => argv.extend(words),
                None => {}
            }
            continue;
        }
        let mut element = raw.clone();
        let mut keep = true;
        for placeholder in placeholders(raw) {
            let (kind, required) = declared(&placeholder).context("an undeclared placeholder")?;
            match typed_value(kind, required, args, &placeholder, folder)? {
                Some(Words::One(word)) => element = element.replace(&format!("{{{placeholder}}}"), &word),
                None => {
                    keep = false;
                    break;
                }
                Some(Words::Many(_)) => bail!("The template {} uses \"{{{placeholder}}}\" inside an argument, which needs one value", template.id),
            }
        }
        if keep {
            argv.push(element);
        }
    }
    // A required parameter no argument uses is still required.
    for param in template.params.iter().filter(|param| param.required) {
        ensure!(!args[param.name.as_str()].is_null(), "The template needs \"{}\"", param.name);
    }
    Ok(plain(&template.id, &template.program, argv))
}

/// Task 85: the report a project template declares, if any.
fn project_report(folder: &Path, id: &str) -> Option<(PathBuf, super::run_results::Format)> {
    if builtin(id).is_some() || !folder.join(crate::test_discipline::config::FILE).is_file() {
        return None;
    }
    let loaded = crate::test_discipline::config::load(folder, None).ok()?;
    let template = loaded.config.templates.into_iter().find(|template| template.id == id)?;
    let results = template.results?;
    Some((folder.join(&results.path), super::run_results::Format::parse(&results.format)?))
}

/// Owner, 2026-10-28 (QA #512): on Windows a Qt build needs the Visual Studio
/// environment; without it cl.exe finds no standard header (C1083) and the
/// members built through their own vcvars batch files, a permission each.
/// vswhere names the newest Visual Studio with the C++ tools; its
/// vcvars64.bat runs once per worker and the environment it sets is kept.
#[cfg(windows)]
fn msvc_environment() -> Option<Vec<(String, String)>> {
    static ENV: std::sync::OnceLock<Option<Vec<(String, String)>>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let program_files = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        let vswhere = Path::new(&program_files).join(r"Microsoft Visual Studio\Installer\vswhere.exe");
        // The Visual Studio of the Qt kit (msvc2022) first, then the newest
        // with the C++ tools.
        let find = |version: Option<&str>| -> Option<String> {
            let mut query = Command::new(&vswhere);
            if let Some(version) = version {
                query.args(["-version", version]);
            } else {
                query.arg("-latest");
            }
            let found = query
                .args(["-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"])
                .output()
                .ok()?;
            let install = String::from_utf8_lossy(&found.stdout).lines().next()?.trim().to_owned();
            (!install.is_empty()).then_some(install)
        };
        let install = find(Some("[17.0,18.0)")).or_else(|| find(None))?;
        let vcvars = Path::new(&install).join("VC").join("Auxiliary").join("Build").join("vcvars64.bat");
        if !vcvars.is_file() {
            return None;
        }
        // cmd parses its own quotes: the line goes as it is, not re-quoted.
        use std::os::windows::process::CommandExt;
        let output = Command::new("cmd").raw_arg(format!("/d /s /c \"\"{}\" >nul 2>&1 && set\"", vcvars.display())).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let env = parse_set_output(&String::from_utf8_lossy(&output.stdout));
        env.iter().any(|(key, _)| key.eq_ignore_ascii_case("INCLUDE")).then_some(env)
    })
    .clone()
}

/// `set` prints NAME=value lines; a name never starts with '='.
fn parse_set_output(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| !key.is_empty() && !key.contains(char::is_whitespace))
        .map(|(key, value)| (key.to_owned(), value.trim_end_matches('\r').to_owned()))
        .collect()
}

/// Task 174: the environment the test queue gives a queued suite: the build
/// cache for cargo; for a CMake or CTest command also what the CMake templates
/// give (the Visual Studio tools on Windows, the C++ cache), so a merged
/// commit's suites build and run as the members' do.
pub(crate) fn queue_environment(project: &Path, command: &str) -> Vec<(String, String)> {
    let mut env = cache_use("cargo-test", project).env;
    let cmake = command
        .split(|ch: char| ch.is_whitespace() || matches!(ch, '&' | ';' | '|' | '(' | ')' | '"'))
        .any(|word| matches!(word.to_ascii_lowercase().as_str(), "ctest" | "cmake" | "ctest.exe" | "cmake.exe"));
    if !cmake {
        return env;
    }
    #[cfg(windows)]
    if let Some(msvc) = msvc_environment() {
        env.extend(msvc);
    }
    env.extend(cache_use("cmake-build", project).env);
    env
}

/// The environment a template needs beyond the worker's own (owner,
/// 2026-10-28): the Visual Studio tools for a C++ configure and build on
/// Windows, Gradle's home in the folder.
fn template_environment(plan: &Plan, folder: &Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    #[cfg(windows)]
    if matches!(plan.template.as_str(), "cmake-configure" | "cmake-build" | "ctest" | "msbuild-build" | "vstest") {
        if let Some(msvc) = msvc_environment() {
            env.extend(msvc);
        }
    }
    if plan.template == "ue-test" {
        let ue = folder.join(".zerus-team").join("ue");
        let _ = std::fs::create_dir_all(ue.join("Saved"));
        let _ = std::fs::create_dir_all(ue.join("DDC"));
        env.push(("UE-LocalDataCachePath".into(), ue.join("DDC").display().to_string()));
    }
    if matches!(plan.template.as_str(), "py-test" | "cython-build") {
        let python = folder.join(".zerus-team").join("python");
        let _ = std::fs::create_dir_all(python.join("pip-cache"));
        let _ = std::fs::create_dir_all(python.join("pycache"));
        let _ = std::fs::create_dir_all(python.join("pytest-cache"));
        env.push(("PIP_CACHE_DIR".into(), python.join("pip-cache").display().to_string()));
        env.push(("PYTHONPYCACHEPREFIX".into(), python.join("pycache").display().to_string()));
        // Absolute, so a project run in a subfolder (task 153) keeps it in the member's folder.
        env.push(("PYTEST_ADDOPTS".into(), format!("-o cache_dir={}", python.join("pytest-cache").display())));
    }
    if plan.template == "android-test" {
        env.push(("GRADLE_USER_HOME".into(), folder.join(".zerus-team").join("gradle").join("home").display().to_string()));
    }
    // Task #280: every app path goes through these templates, and each names
    // its action and 273's development identity in the environment.
    if let Some(action) = plan.template.strip_prefix("tauri-") {
        if matches!(action, "build" | "dev" | "launch") {
            env.push(("ZERUS_TAURI_ACTION".into(), action.to_owned()));
            env.push(("ZERUS_TAURI_IDENTITY".into(), "development".into()));
        }
    }
    // Task #239: the web project's npm cache lives in the member's folder, so a
    // sandboxed member never reads or writes the person's own cache (the
    // EPERM ui3 met) and two members never share one download.
    if plan.template == "web-test" {
        let npm = folder.join(".zerus-team").join("npm");
        let _ = std::fs::create_dir_all(&npm);
        env.push(("npm_config_cache".into(), npm.display().to_string()));
        env.push(("npm_config_fund".into(), "false".into()));
        env.push(("npm_config_audit".into(), "false".into()));
        env.push(("CI".into(), "1".into()));
    }
    env
}

fn whole_placeholder(raw: &str) -> Option<&str> {
    raw.strip_prefix('{')?.strip_suffix('}')
}

fn placeholders(raw: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        found.push(after[..end].to_owned());
        rest = &after[end + 1..];
    }
    found
}

/// The outcome of one run: an exit code (None when it timed out or could not
/// start), the last 4000 characters of its output and how long it took.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Outcome {
    pub exit: Option<i32>,
    pub tail: String,
    /// #717: the failing test names the output named, so a nonzero exit without
    /// a readable tail still says what failed.
    pub failures: Vec<String>,
    /// #717: a bounded excerpt around the first failure, kept in the record.
    pub excerpt: String,
    pub ms: u64,
    pub timed_out: bool,
    pub error: String,
    /// Task 85: one result per test from the run's report (CTest's JUnit,
    /// VSTest's TRX, a project template's declared report); None without one.
    pub report: Option<super::run_results::Report>,
    /// Task #34 (team ede724c4): what a cargo test output said of its test
    /// binaries, read from the whole output before the tail cut it; None for
    /// an output with no cargo test line.
    pub tests: Option<CargoTests>,
}

/// Task #34: a cargo test output's binaries and their results.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CargoTests {
    /// "Running ..." and "Doc-tests ..." lines: the test binaries started.
    pub binaries: usize,
    /// "test result:" lines: the binaries that reported.
    pub results: usize,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub filtered: u64,
    /// The last binary started, to name one that never reported.
    pub last_binary: String,
}

impl CargoTests {
    /// One line for the end of the tail, where a reader looks.
    pub(crate) fn summary(&self) -> String {
        format!(
            "Tests: {} passed, {} failed, {} ignored, {} filtered out, from {} of {} test binaries",
            self.passed, self.failed, self.ignored, self.filtered, self.results, self.binaries
        )
    }

    /// Why a run that exited 0 did not test anything, if it did not: no test
    /// matched (or ran), or a binary started and never reported (the output
    /// was cut or the binary stopped).
    pub(crate) fn no_success(&self) -> Option<String> {
        if self.results < self.binaries {
            let missing = self.binaries - self.results;
            return Some(format!(
                "{missing} of {} test binaries reported no result (the last started: {}): the output was cut or the binary stopped",
                self.binaries,
                if self.last_binary.is_empty() { "unknown" } else { self.last_binary.as_str() }
            ));
        }
        if self.results == 0 {
            return Some("the output ended before any test binary reported".into());
        }
        if self.passed + self.failed == 0 {
            return Some(if self.filtered > 0 {
                format!("no test matched the filter: 0 ran, {} filtered out", self.filtered)
            } else {
                format!("no test ran: 0 passed, 0 failed{}", if self.ignored > 0 { format!(", {} ignored", self.ignored) } else { String::new() })
            });
        }
        None
    }
}

/// Task #34: the cargo test lines of an output, or None when it has none.
pub(crate) fn cargo_tests(output: &str) -> Option<CargoTests> {
    let mut tests = CargoTests::default();
    let mut seen = false;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(binary) = trimmed.strip_prefix("Running ").or_else(|| trimmed.strip_prefix("Doc-tests ")) {
            // cargo's "Running unittests src\main.rs (target\debug\deps\hgs-....exe)".
            if trimmed.starts_with("Doc-tests ") || (binary.ends_with(')') && binary.split_once(" (").is_some_and(|(_, path)| path.contains("deps"))) {
                tests.binaries += 1;
                tests.last_binary = binary.split(" (").next().unwrap_or(binary).trim().to_owned();
                seen = true;
            }
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("test result: ") else { continue };
        seen = true;
        tests.results += 1;
        for part in rest.split(';') {
            let words: Vec<&str> = part.split_whitespace().collect();
            let number = |at: usize| words.get(at).and_then(|word| word.parse::<u64>().ok()).unwrap_or(0);
            // "ok. 3 passed", " 0 failed", " 1 ignored", " 0 measured", " 9 filtered out"
            let (count, label) = match words.as_slice() {
                [_, _, label, ..] if words[0].ends_with('.') => (number(1), *label),
                [_, label, ..] => (number(0), *label),
                _ => continue,
            };
            match label {
                "passed" => tests.passed += count,
                "failed" => tests.failed += count,
                "ignored" => tests.ignored += count,
                "filtered" => tests.filtered += count,
                _ => {}
            }
        }
    }
    seen.then_some(tests)
}

/// #717: the failing test names an output names, and a bounded excerpt around
/// the first one. Qt's `FAIL!`, cargo's `test ... FAILED`, pytest's `FAILED`
/// and CTest's `(Failed)` lines are read; an unknown format yields no names,
/// never a guess.
pub(crate) fn failures_in(output: &str) -> (Vec<String>, String) {
    let lines: Vec<&str> = output.lines().collect();
    let mut failures: Vec<String> = Vec::new();
    let mut first = None;
    for (index, line) in lines.iter().enumerate() {
        let Some(name) = failing_test(line) else { continue };
        if first.is_none() {
            first = Some(index);
        }
        if failures.len() < 20 && !failures.contains(&name) {
            failures.push(name);
        }
    }
    let excerpt = first
        .map(|index| {
            let start = index.saturating_sub(6);
            let end = (index + 12).min(lines.len());
            last_chars(&lines[start..end].join("\n"), FAILURE_EXCERPT_BYTES)
        })
        .unwrap_or_default();
    (failures, excerpt)
}

/// #717: how much of the output around a failure a record keeps.
pub(crate) const FAILURE_EXCERPT_BYTES: usize = 4000;

fn failing_test(line: &str) -> Option<String> {
    let trimmed = line.trim();
    // Qt Test: "FAIL!  : SlowEventLog::a_case() ..."
    if let Some(rest) = trimmed.strip_prefix("FAIL!") {
        let rest = rest.trim_start_matches([':', ' ']).trim();
        return clean_test_name(rest.split_whitespace().next().unwrap_or_default());
    }
    // cargo: "test some::case ... FAILED"
    if let Some(rest) = trimmed.strip_prefix("test ") {
        if let Some(name) = rest.strip_suffix(" ... FAILED") {
            return clean_test_name(name);
        }
    }
    // pytest: "FAILED path/test_x.py::test_case - assertion"
    if let Some(rest) = trimmed.strip_prefix("FAILED ") {
        return clean_test_name(rest.split(" - ").next().unwrap_or(rest));
    }
    // CTest: "  12 - name (Failed)"
    if let Some(before) = trimmed.strip_suffix("(Failed)") {
        let before = before.trim();
        return clean_test_name(before.split(" - ").last().unwrap_or(before));
    }
    // Gradle: "> Task :app:testDebugUnitTest FAILED", or a test name.
    if let Some(before) = trimmed.strip_suffix(" FAILED") {
        if let Some(task) = before.strip_prefix("> Task ") {
            return clean_test_name(task);
        }
    }
    None
}

fn clean_test_name(name: &str) -> Option<String> {
    let name = name.trim().trim_matches([':', '"', '\'']);
    (!name.is_empty() && name.chars().count() <= 200 && !name.chars().any(char::is_control)).then(|| name.to_owned())
}

/// Runs a validated plan in the member's folder, outside the member's sandbox
/// (the worker is not confined): TEMP inside the folder, bounded
/// time and output (U21). The whole process tree ends with the run (C2).
pub(crate) fn execute(plan: &Plan, folder: &Path, timeout: Duration, env: &[(&str, String)]) -> Outcome {
    execute_as(plan, folder, timeout, env, &super::isolation::Person::current())
}

/// `execute` with the person's own folders given, as a test gives synthetic ones.
pub(crate) fn execute_as(plan: &Plan, folder: &Path, timeout: Duration, env: &[(&str, String)], person: &super::isolation::Person) -> Outcome {
    if plan.template=="artifact-test" && plan.argv.first().is_some_and(|value|value=="zerus-profile") {
        return match super::artifacts::standalone(&plan.argv[1],&plan.argv[2],folder,timeout){Ok(outcome)=>outcome,Err(error)=>Outcome{error:error.to_string(),..Default::default()}};
    }
    let temp = folder.join(".zerus-team").join("tmp");
    let _ = std::fs::create_dir_all(&temp);
    // U62 (#720): a test runs with its own home, data, temporary and XDG
    // folders in a run folder (the project's .zerus/tests.toml).
    let isolated = super::isolation::prepare(&plan.template, &plan.program, folder, person);
    if isolated.is_some() {
        super::isolation::quiet_crashes();
    }
    let base = template_environment(plan, folder);
    let cache = cache_use(&plan.template, folder);
    // Every command of the run (#741) gets the same folder
    // and environment.
    let build = |program: &str, argv: &[String]| {
        let mut command = Command::new(program);
        command
            .args(argv)
            .current_dir(plan.cwd.as_ref().map_or_else(|| folder.to_path_buf(), |cwd| folder.join(cwd)))
            .env("TEMP", &temp)
            .env("TMP", &temp)
            // The worker's own build dir is Zerus', never the member's.
            .env_remove("CARGO_TARGET_DIR");
        if let Some(run) = &isolated {
            for name in &run.remove {
                command.env_remove(name);
            }
            for (key, value) in &run.env {
                command.env(key, value);
            }
        }
        for (key, value) in base.iter().chain(cache.env.iter()) {
            command.env(key, value);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        command
    };
    let mut command = build(&plan.program, &plan.argv);
    let started = Instant::now();
    // Task 85: a report from an earlier run is never read as this run's.
    let report = report_path(folder, &plan.template);
    if let Some((path, _)) = &report {
        let _ = std::fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    // Task 210: UnrealEditor-Cmd writes the run to its own log; an earlier
    // run's log is never read as this run's.
    let unreal_log = (plan.template == "ue-test").then(|| folder.join(".zerus-team").join("ue").join("Unreal.log"));
    if let Some(log) = &unreal_log {
        let _ = std::fs::remove_file(log);
    }
    let stdin = plan.commit.as_ref().map(|payload| serde_json::to_vec(payload).unwrap_or_default());
    let before = isolated.as_ref().map(super::isolation::Run::fingerprint);
    let attempt = |command: &mut Command| {
        let mut outcome = run_tree(command, timeout.saturating_sub(started.elapsed()).max(Duration::from_secs(1)), stdin.clone());
        for (program, argv) in &plan.more {
            let left = timeout.saturating_sub(started.elapsed());
            if outcome.timed_out || left.is_zero() {
                outcome.timed_out = true;
                break;
            }
            let step = run_tree(&mut build(program, argv), left, None);
            outcome = joined(outcome, step, program);
        }
        outcome
    };
    // Task 320: a cargo build at the worker's level never builds through a
    // shared sccache server a sandboxed process started in its restricted token.
    let cargo = matches!(plan.template.as_str(), "cargo-build" | "cargo-test" | "cargo-clippy");
    let restarted_before = cargo && super::super::build_cache_server::ensure_unrestricted();
    let mut outcome = attempt(&mut command);
    // Task #35: one more run for a compiler that real-time scanning stopped.
    if let Some(signature) = scanning_failure(&plan.template, &outcome) {
        if !timeout.saturating_sub(started.elapsed()).is_zero() {
            let first = outcome.exit;
            outcome = attempt(&mut build(&plan.program, &plan.argv));
            outcome.tail = last_chars(
                &format!("{}\n{}", outcome.tail, scanning_note(signature, folder, first, outcome.exit)),
                TAIL_BYTES,
            );
        }
    }
    // Task 261: a cargo build the shared sccache server could not write (it
    // was started in a restricted token) runs once more after the worker
    // restarted the server.
    if cargo
        && super::super::build_cache_server::worth_restart(outcome.exit, &format!("{}\n{}", outcome.tail, outcome.excerpt))
        && !timeout.saturating_sub(started.elapsed()).is_zero()
        && super::super::build_cache_server::restart().is_ok()
    {
        outcome = attempt(&mut build(&plan.program, &plan.argv));
        // At the end: a long output's tail keeps it, and the dispatcher reads it.
        outcome.tail = last_chars(&format!("{}\n{}", outcome.tail, super::super::build_cache_server::RESTART_NOTE), TAIL_BYTES);
    }
    if restarted_before {
        outcome.tail = last_chars(&format!("{}\n{}", outcome.tail, super::super::build_cache_server::RESTARTED_BEFORE_NOTE), TAIL_BYTES);
    }
    outcome.ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // Task #34 (team ede724c4): a cargo test that exited 0 without one test
    // binary started tested nothing either.
    if plan.template == "cargo-test" && outcome.exit == Some(0) && !outcome.timed_out && outcome.tests.is_none() {
        outcome.exit = Some(1);
        outcome.error = "the output ended before any test binary ran: no test ran".into();
        outcome.tail = last_chars(&format!("{}\n{}", outcome.tail, outcome.error), TAIL_BYTES);
    }
    if !cache.note.is_empty() {
        outcome.tail = last_chars(&format!("{}
{}", cache.note, outcome.tail), TAIL_BYTES);
    }
    if let (Some(run), Some(before)) = (&isolated, before) {
        checked(&mut outcome, run, &before, folder);
    }
    // Task 210: a failed ue-test says why from the log the editor wrote, since
    // its own output goes to the file, not the pipe.
    if let Some(log) = &unreal_log {
        if outcome.exit != Some(0) {
            if let Ok(text) = std::fs::read_to_string(log) {
                outcome.tail = last_chars(&format!("{}\n== Unreal.log\n{}", outcome.tail, last_chars(&text, TAIL_BYTES)), TAIL_BYTES);
            }
        }
    }
    if let Some((path, format)) = report {
        let read = super::run_results::read(&path, format);
        // The report names the failing tests when the text did not.
        if outcome.failures.is_empty() {
            outcome.failures = read.failing(20);
        }
        outcome.report = Some(read);
    }
    // Task #18: a run refused because its programs started at Low integrity
    // (a Low-labeled build folder, an ordinary TEMP) says so once, with the
    // owner's step.
    if outcome.exit != Some(0) && !outcome.timed_out {
        let run_temp = isolated
            .as_ref()
            .map_or_else(|| temp.clone(), |run| super::environment::run_temp(&run.env));
        let cwd = plan.cwd.as_ref().map_or_else(|| folder.to_path_buf(), |cwd| folder.join(cwd));
        let text = format!("{}\n{}\n{}", outcome.tail, outcome.excerpt, outcome.error);
        let folders = super::environment::build_folders(&cwd, &plan.argv.join(" "), &text);
        if let Some(message) = super::environment::diagnose(&text, &folders, Some(&run_temp)) {
            outcome.tail = last_chars(&format!("{}\n{message}", outcome.tail), TAIL_BYTES);
            outcome.error = message;
        }
    }
    outcome
}

/// Task #35: the builds Zerus runs again once after a scanning failure. Never
/// a commit, a merge or anything else whose second run would change more.
const RETRIED_ON_SCANNING: &[&str] = &["cargo-build", "cargo-test", "cargo-clippy", "cmake-configure", "cmake-build", "ctest", "msbuild-build"];

/// Task #35: the two compiler failures real-time scanning gives a build in a
/// folder it scans (team 33fbafeb, a DeepSeek member's clone without the
/// Defender exclusion the earlier team's folders had): MSVC's C1083 "Cannot
/// open compiler generated file ... Permission denied" while the scanner holds
/// the new object file, and D8050 "cannot execute ... c1.dll: failed to get
/// command line into debug records". Exactly these; any other failure is the
/// build's own and runs once.
fn scanning_failure(template: &str, outcome: &Outcome) -> Option<&'static str> {
    if outcome.exit == Some(0) || outcome.timed_out || !RETRIED_ON_SCANNING.contains(&template) {
        return None;
    }
    let text = format!("{}\n{}", outcome.excerpt, outcome.tail);
    text.lines().find_map(|line| {
        if line.contains("C1083") && line.contains("Cannot open compiler generated file") && line.contains("Permission denied") {
            Some("C1083")
        } else if line.contains("D8050") && line.contains("failed to get command line into debug records") {
            Some("D8050")
        } else {
            None
        }
    })
}

/// What the run says after its second try: the signature, the folder and
/// what to exclude from scanning.
fn scanning_note(signature: &str, folder: &Path, first: Option<i32>, second: Option<i32>) -> String {
    let result = if second == Some(0) { "the second run passed" } else { "the second run failed as well" };
    format!(
        "Zerus ran this build again once: the first run stopped with MSVC {signature} (exit {}), the failure real-time \
         scanning (Microsoft Defender) gives a compiler in a folder it scans; {result}. The likely cause is scanning of {}: \
         excluding its build outputs (.ci-build, target) from real-time scanning stops it.",
        first.map_or_else(|| "none".to_owned(), |code| code.to_string()),
        folder.display()
    )
}

/// #741: one run of several commands: the first failure's exit and error
/// stand, every failure is named, and the tail keeps the latest output.
fn joined(mut outcome: Outcome, step: Outcome, program: &str) -> Outcome {
    if outcome.exit == Some(0) || (outcome.exit.is_none() && outcome.error.is_empty()) {
        outcome.exit = step.exit;
    }
    if outcome.error.is_empty() {
        outcome.error = step.error;
    }
    outcome.failures.extend(step.failures);
    if outcome.excerpt.is_empty() {
        outcome.excerpt = step.excerpt;
    }
    outcome.timed_out |= step.timed_out;
    let name = Path::new(program).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    outcome.tail = last_chars(&format!("{}\n== {name}\n{}", outcome.tail, step.tail), TAIL_BYTES);
    outcome
}

/// U62 (#720): the second line. A run whose trace reached the person's own
/// paths fails, with the paths named; a crash is noted in the run folder.
fn checked(outcome: &mut Outcome, run: &super::isolation::Run, before: &super::isolation::Fingerprint, folder: &Path) {
    let mut lines = Vec::new();
    if let Some(note) = &run.note {
        lines.push(note.clone());
    }
    if super::isolation::crashed(outcome.exit) && !run.folder.as_os_str().is_empty() {
        let code = outcome.exit.map(|code| format!("{:#x}", code as u32)).unwrap_or_default();
        let _ = std::fs::write(run.folder.join("crash.txt"), format!("exit {code}\n{}", outcome.tail));
        lines.push(format!("The test crashed (exit {code}); its note is in {}", run.folder.display()));
    }
    let changes = run.changes(before, folder);
    if !changes.is_empty() {
        lines.push(format!("Isolation: the run touched the person's own paths: {}", changes.join("; ")));
        if outcome.exit == Some(0) {
            outcome.exit = Some(1);
        }
        if outcome.error.is_empty() {
            outcome.error = "the run touched the person's own paths".into();
        }
    }
    if !lines.is_empty() {
        if !outcome.tail.is_empty() && !outcome.tail.ends_with('\n') {
            outcome.tail.push('\n');
        }
        outcome.tail.push_str(&lines.join("\n"));
    }
}

/// How long a pipe reader may take after the tree ended: a process that escaped
/// the job cannot hold the worker (C2).
const JOIN_BOUND: Duration = Duration::from_secs(5);

/// C2: one command outside the member's sandbox. The child and everything it
/// starts live in one job (Windows) or process group (Unix), so a timeout stops
/// the whole tree, and whatever a finished command left behind is ended before
/// the output pipes are joined - a lingering grandchild can neither keep the
/// worker waiting nor outlive the run. The joins are bounded too, for a process
/// that escaped the tree.
pub(crate) fn run_tree(command: &mut Command, timeout: Duration, stdin: Option<Vec<u8>>) -> Outcome {
    let started = Instant::now();
    let program = command.get_program().to_string_lossy().into_owned();
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    #[cfg(windows)]
    let spawned = crate::platform::spawn(command);
    #[cfg(unix)]
    let spawned = crate::platform::spawn_group(command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => return Outcome { error: format!("could not start {program}: {error}"), ..Default::default() },
    };
    if let (Some(payload), Some(mut pipe)) = (stdin, child.stdin.take()) {
        use std::io::Write;
        let _ = pipe.write_all(&payload);
    }
    // Drain both streams without holding the child: a full pipe would stop it.
    let mut out = child.stdout.take().map(bounded);
    let mut err = child.stderr.take().map(bounded);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                timed_out = true;
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    // C2: the tree ends here, whether the command timed out or finished: a
    // descendant it left holding the pipes is stopped, so the readers return.
    #[cfg(windows)]
    child.terminate();
    #[cfg(unix)]
    crate::platform::kill_process_group(&mut child);
    let _ = child.kill();
    let _ = child.wait();
    let stdout = join_bounded(out.take());
    let stderr = join_bounded(err.take());
    let mut tail = stdout;
    if !stderr.trim().is_empty() {
        if !tail.is_empty() && !tail.ends_with('\n') {
            tail.push('\n');
        }
        tail.push_str(&stderr);
    }
    let (failures, excerpt) = failures_in(&tail);
    let mut exit = status.and_then(|status| status.code()).filter(|_| !timed_out);
    let mut error = if timed_out { format!("the command did not finish in {} s and was stopped", timeout.as_secs()) } else { String::new() };
    // Task #34 (team ede724c4): a cargo test run is read whole, before the
    // tail cuts it (stdout, where cargo prints the results, comes before
    // stderr's warnings and "Running" lines, so a long build left only those).
    // An exit 0 that tested nothing is a failure naming why, and the counts
    // end the tail.
    let tests = cargo_tests(&tail);
    if let Some(found) = &tests {
        if exit == Some(0) {
            if let Some(why) = found.no_success() {
                exit = Some(1);
                error = why;
            }
        }
        if !tail.is_empty() && !tail.ends_with('\n') {
            tail.push('\n');
        }
        tail.push_str(&found.summary());
        if !error.is_empty() && exit == Some(1) {
            tail.push('\n');
            tail.push_str(&error);
        }
    }
    Outcome {
        exit,
        tail: last_chars(&tail, TAIL_BYTES),
        failures,
        excerpt,
        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        timed_out,
        error,
        report: None,
        tests,
    }
}

/// #717: a suite's next time bound from its recorded duration: no record keeps
/// the template's own bound; a record gets `factor` times its last duration plus
/// `grace_secs`, never below `floor_secs` or above `ceiling_secs`. The queue
/// passes the configured values, so no policy or repository value is built in.
#[allow(dead_code)] // #717's queue passes its configured values once it lands.
pub(crate) fn recorded_bound(template_secs: u64, recorded_ms: Option<u64>, factor: u32, grace_secs: u64, floor_secs: u64, ceiling_secs: u64) -> u64 {
    let Some(ms) = recorded_ms.filter(|ms| *ms > 0) else { return template_secs };
    let bound = (ms / 1000).saturating_mul(u64::from(factor.max(1))).saturating_add(grace_secs);
    bound.clamp(floor_secs.max(1), ceiling_secs.max(floor_secs.max(1)))
}

/// C2: waits at most `JOIN_BOUND` for a drained stream. A reader still blocked
/// after the tree ended stays detached; its text is not worth the worker.
fn join_bounded(handle: Option<std::thread::JoinHandle<String>>) -> String {
    let Some(handle) = handle else { return String::new() };
    let until = Instant::now() + JOIN_BOUND;
    while !handle.is_finished() {
        if Instant::now() >= until {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.join().ok().unwrap_or_default()
}

fn bounded(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    kept.extend_from_slice(&buffer[..read]);
                    if kept.len() > OUTPUT_BYTES {
                        let excess = kept.len() - OUTPUT_BYTES;
                        kept.drain(..excess);
                    }
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}

/// The last `chars` characters, starting at a line boundary when there is one.
pub(crate) fn last_chars(text: &str, chars: usize) -> String {
    let clean = text.replace("\r\n", "\n");
    let total = clean.chars().count();
    if total <= chars {
        return clean;
    }
    let skip = total - chars;
    let tail: String = clean.chars().skip(skip).collect();
    match tail.find('\n') {
        Some(at) => tail[at + 1..].to_owned(),
        None => tail,
    }
}

/// `Path::lexists`: the path is there, even as a broken link.
trait Lexists {
    fn lexists(&self) -> bool;
}

impl Lexists for PathBuf {
    fn lexists(&self) -> bool {
        std::fs::symlink_metadata(self).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn every_builtin_has_a_type_for_each_placeholder_it_uses() {
        for template in BUILTIN {
            for raw in std::iter::once(&template.program).chain(template.args.iter()) {
                for placeholder in placeholders(raw) {
                    assert!(template.params.iter().any(|param| param.name == placeholder), "{}: {placeholder}", template.id);
                }
            }
            assert!(!template.summary.is_empty() && template.timeout_secs > 0, "{}", template.id);
        }
        // The set the owner asked for, by id.
        let ids: Vec<&str> = BUILTIN.iter().map(|template| template.id).collect();
        // Task 85: the CMake and MSBuild templates of any project after them; task 86: the Python and Cython ones; task 87: Unreal.
        assert_eq!(ids, ["artifact-test", "cargo-build", "cargo-test", "cargo-clippy", "py-test", "cython-build", "tauri-build", "tauri-dev", "tauri-launch", "android-test", "release-candidate",
                         "clean-build", "git-merge-lead", "git-commit", "ue-build", "ue-test", "cmake-configure", "cmake-build", "ctest", "msbuild-build", "vstest", "web-test"]);
    }

    // Run by hand on a Windows machine with Visual Studio (cargo test -- --ignored).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn the_visual_studio_environment_is_found_here() {
        let env = msvc_environment().expect("vswhere and vcvars64.bat");
        assert!(env.iter().any(|(key, value)| key.eq_ignore_ascii_case("INCLUDE") && !value.is_empty()));
    }

    #[test]
    fn clean_build_removes_only_build_outputs() {
        let dir = temp();
        std::fs::create_dir_all(dir.path().join("mobile").join("android").join("app").join("build")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let planned = plan("clean-build", &json!({"paths":["mobile/android/app/build"]}), dir.path()).unwrap();
        assert_eq!(planned.argv, ["-E", "rm", "-rf", "mobile/android/app/build"]);
        let error = plan("clean-build", &json!({"paths":["src"]}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("only build outputs"), "{error}");
        assert!(plan("clean-build", &json!({"paths":["../build"]}), dir.path()).is_err());
        assert!(plan("clean-build", &json!({"paths":["missing/build"]}), dir.path()).is_err());
    }

    #[test]
    fn templates_bring_the_environment_their_tools_need() {
        // Owner, 2026-10-28 (QA #512): the C++ templates on Windows had no
        // Visual Studio environment, and the Android tests had no template, so
        // QA asked a permission for each run.
        assert_eq!(parse_set_output("INCLUDE=C:/VC/include\r\nPath=C:/bin\r\n=C:=C:/\r\nbad line\r\n"),
                   vec![("INCLUDE".into(), "C:/VC/include".into()), ("Path".into(), "C:/bin".into())]);
        let dir = temp();
        // The Gradle wrapper is the folder's own; its home and cache stay in the folder.
        let android = dir.path().join("mobile").join("android");
        std::fs::create_dir_all(&android).unwrap();
        std::fs::write(android.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" }), "").unwrap();
        let planned = plan("android-test", &json!({}), dir.path()).unwrap();
        assert!(planned.argv.contains(&":app:testDebugUnitTest".to_owned()));
        assert!(planned.argv.iter().any(|arg| arg.starts_with(&dir.path().join(".zerus-team").display().to_string())));
        let env = template_environment(&planned, dir.path());
        assert!(env.iter().any(|(key, value)| key == "GRADLE_USER_HOME" && value.contains(".zerus-team")));
    }

    #[test]
    fn an_unknown_template_is_refused() {
        let dir = temp();
        let error = plan("cargo-deploy", &json!({}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("Unknown template"), "{error}");
    }

    #[test]
    fn a_value_outside_its_type_is_refused() {
        let dir = temp();
        for (id, args) in [
            ("cargo-build", json!({"package":"pkg; rm -rf /"})),
            ("cargo-build", json!({"package":"--all"})),
            ("cargo-build", json!({"package":"../elsewhere"})),
            ("cargo-test", json!({"filter":42})),
            ("cargo-test", json!({"bin":"a b"})),
            ("cargo-clippy", json!({"toolchain":"1.99.1 --offline"})),
            ("cmake-build", json!({"targets":["zerus","a b"]})),
            ("git-commit", json!({"message":"ok","paths":["../outside.txt"]})),
        ] {
            assert!(plan(id, &args, dir.path()).is_err(), "{id} {args} must be refused");
        }
        // And the types that are valid pass.
        assert_eq!(plan("cargo-build", &json!({"package":"hgs"}), dir.path()).unwrap().argv, ["build", "--locked", "--package=hgs"]);
        assert_eq!(plan("cmake-build", &json!({"targets":["zerus"]}), dir.path()).unwrap().argv, ["--build", "build", "--target", "zerus"]);
        // An absent filter adds no argument at all.
        assert_eq!(plan("cargo-build", &json!({}), dir.path()).unwrap().argv, ["build", "--locked"]);
    }

    /// Task 213: the release candidate takes the fork identity as two explicit
    /// inputs (the workflow takes them from its tag and run number), so a local
    /// leg embeds and packages the same build and feed.
    #[test]
    fn the_release_candidate_takes_the_fork_identity() {
        let dir = temp();
        let feed = "https://github.com/WhiteWh/zerus/releases/latest/download/zerus-fork-feed.json";
        let full = plan("release-candidate", &json!({"commit":"abc1234","fork_build":"41","fork_feed":feed}), dir.path()).unwrap();
        assert_eq!(
            full.argv,
            ["scripts/release-candidate.py", "--commit=abc1234", "--fork-build=41", "--fork-feed=https://github.com/WhiteWh/zerus/releases/latest/download/zerus-fork-feed.json"]
        );
        // Both are optional: an experimental leg (neither) passes no flag, and
        // one alone is the script's own refusal (both or neither).
        assert_eq!(plan("release-candidate", &json!({}), dir.path()).unwrap().argv, ["scripts/release-candidate.py"]);
        assert_eq!(
            plan("release-candidate", &json!({"fork_build":"41"}), dir.path()).unwrap().argv,
            ["scripts/release-candidate.py", "--fork-build=41"]
        );
        assert!(plan("release-candidate", &json!({"fork_build":"41 42"}), dir.path()).is_err(), "a build number is one word");
    }

    #[test]
    fn a_cargo_check_is_expressed_exactly() {
        // N2: the checks every hand-out names are expressible: a test-name
        // filter with a binary, and the pinned clippy toolchain.
        let dir = temp();
        assert_eq!(
            plan("cargo-test", &json!({"bin":"hgs","filter":"teams"}), dir.path()).unwrap().argv,
            ["test", "--locked", "--bin=hgs", "teams"]
        );
        assert_eq!(
            plan("cargo-clippy", &json!({"toolchain":"1.99.1"}), dir.path()).unwrap().argv,
            ["+1.99.1", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"]
        );
        assert_eq!(
            plan("cargo-test", &json!({"toolchain":"1.99.1","bin":"hgs","filter":"teams"}), dir.path()).unwrap().argv,
            ["+1.99.1", "test", "--locked", "--bin=hgs", "teams"]
        );
        // A package still narrows the run when the workspace has several.
        assert_eq!(
            plan("cargo-test", &json!({"package":"hgs","filter":"guide"}), dir.path()).unwrap().argv,
            ["test", "--locked", "--package=hgs", "guide"]
        );
    }

    /// #741: the run hgs test affected names goes through the templates: a
    /// cargo set with its filters and skips, and another crate by its manifest.
    #[test]
    fn an_affected_set_runs_through_the_test_templates() {
        let dir = temp();
        let built = plan(
            "cargo-test",
            &json!({"bin":"hgs","filters":["state::teams::","tests::a_case"],"skips":["state::teams::ops::"]}),
            dir.path(),
        )
        .unwrap();
        assert_eq!(built.argv, ["test", "--locked", "--bin=hgs", "--", "state::teams::", "tests::a_case", "--skip", "state::teams::ops::"]);
        std::fs::create_dir_all(dir.path().join("crates/replica")).unwrap();
        std::fs::write(dir.path().join("crates/replica/Cargo.toml"), "").unwrap();
        let built = plan("cargo-test", &json!({"manifest":["crates/replica/Cargo.toml"]}), dir.path()).unwrap();
        assert_eq!(built.argv, ["test", "--locked", "--manifest-path", "crates/replica/Cargo.toml"]);
        assert!(plan("cargo-test", &json!({"manifest":["crates/replica"]}), dir.path()).is_err());
        assert!(plan("cargo-test", &json!({"filters":["a; b"]}), dir.path()).is_err());
        // Hundreds of filters fit; a thousand do not.
        let many: Vec<String> = (0..300).map(|index| format!("m{index}::")).collect();
        assert_eq!(plan("cargo-test", &json!({"filters":many}), dir.path()).unwrap().argv.len(), 303);
        let too_many: Vec<String> = (0..600).map(|index| format!("m{index}::")).collect();
        assert!(plan("cargo-test", &json!({"filters":too_many}), dir.path()).is_err());
    }

    /// #741: the commands of one run go in turn; the first failure stands.
    #[test]
    fn a_run_of_several_commands_keeps_the_first_failure() {
        let dir = temp();
        let mut failing = shell_plan("exit /b 3", "exit 3");
        let second = shell_plan("echo second", "echo second");
        failing.more = vec![(second.program.clone(), second.argv.clone())];
        let outcome = execute(&failing, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(3));
        assert!(outcome.tail.contains("second"), "{}", outcome.tail);
        let mut good = shell_plan("echo first", "echo first");
        good.more = vec![(second.program.clone(), second.argv.clone())];
        let outcome = execute(&good, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(0));
        assert!(outcome.tail.contains("first") && outcome.tail.contains("second"), "{}", outcome.tail);
    }

    #[test]
    fn a_commit_message_is_any_text() {
        // N3: the message goes as JSON to Zerus' commit code and as one argv
        // element to git, never to a shell: ( ) ' " ! ? * and newlines are fine.
        let dir = temp();
        let message = "Fork: the run is off the tick (C1)\n\nIt said \"done\"! (really?) *yes* -- it's fine";
        let built = plan("git-commit", &json!({"message":message}), dir.path()).unwrap();
        assert_eq!(built.commit.as_ref().unwrap()["message"], message);
        // 4000 characters are allowed, 4001 are not; NUL never, and not blank.
        assert!(plan("git-commit", &json!({"message":"x".repeat(4000)}), dir.path()).is_ok());
        assert!(plan("git-commit", &json!({"message":"x".repeat(4001)}), dir.path()).is_err());
        assert!(plan("git-commit", &json!({"message":"a\u{0}b"}), dir.path()).is_err());
        assert!(plan("git-commit", &json!({"message":"   "}), dir.path()).is_err());
    }

    #[test]
    fn a_grandchild_holding_the_output_does_not_hold_the_run_and_is_ended() {
        // C2: the direct child exits at once, but the grandchild it started
        // keeps the output pipe open and would write a marker a few seconds
        // later. The run returns at once and the whole tree ended with it, so
        // the marker never appears: a detached grandchild is not left behind.
        let dir = temp();
        let marker = dir.path().join("late.txt");
        #[cfg(windows)]
        let plan = {
            let script = dir.path().join("late.bat");
            std::fs::write(&script, format!("@ping -n 4 127.0.0.1 >nul\r\n@echo late>\"{}\"\r\n", marker.display())).unwrap();
            Plan {
                template: "test".into(),
                program: "cmd".into(),
                argv: vec!["/c".into(), format!("start /b {}", script.display())],
                commit: None,
                more: Vec::new(), cwd: None,
            }
        };
        #[cfg(unix)]
        let plan = Plan {
            template: "test".into(),
            program: "sh".into(),
            argv: vec!["-c".into(), format!("(sleep 3; echo late > '{}') &", marker.display())],
            commit: None,
            more: Vec::new(), cwd: None,
        };
        let started = Instant::now();
        let outcome = execute(&plan, dir.path(), Duration::from_secs(120), &[]);
        assert!(!outcome.timed_out, "{outcome:?}");
        assert_eq!(outcome.exit, Some(0), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(30), "the run waited for the grandchild: {:?}", started.elapsed());
        std::thread::sleep(Duration::from_secs(5));
        assert!(!marker.exists(), "the grandchild outlived the run and wrote {}", marker.display());
    }

    #[test]
    fn a_path_outside_the_folder_is_refused() {
        let dir = temp();
        std::fs::write(dir.path().join("inside.txt"), b"x").unwrap();
        let error = plan("git-commit", &json!({"message":"m","paths":["outside.txt"]}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("does not exist in your folder"), "{error}");
        assert!(plan("git-commit", &json!({"message":"m","paths":["inside.txt"]}), dir.path()).is_ok());
    }

    fn shell_plan(windows: &str, unix: &str) -> Plan {
        let (program, flag, command) = if cfg!(windows) { ("cmd", "/c", windows) } else { ("sh", "-c", unix) };
        Plan { template: "test".into(), program: program.into(), argv: vec![flag.into(), command.into()], commit: None, more: Vec::new(), cwd: None }
    }

    /// Task #34 (team ede724c4): a cargo test output is read whole. A filter
    /// that matches no test, a binary that started and never reported, or no
    /// result at all is no success, named; a run that tested is one.
    #[test]
    fn a_cargo_test_output_says_whether_it_tested_anything() {
        let running = "     Running unittests src\\main.rs (target\\debug\\deps\\hgs-290eb8275e13a9c3.exe)\n";
        let none = cargo_tests(&format!("{running}\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4363 filtered out; finished in 0.02s\n")).unwrap();
        assert_eq!((none.binaries, none.results, none.filtered), (1, 1, 4363));
        assert_eq!(none.no_success().as_deref(), Some("no test matched the filter: 0 ran, 4363 filtered out"));
        let ran = cargo_tests(&format!("{running}test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 4360 filtered out; finished in 1.20s\n")).unwrap();
        assert_eq!((ran.passed, ran.failed, ran.ignored, ran.filtered), (3, 0, 1, 4360));
        assert_eq!(ran.no_success(), None);
        assert_eq!(ran.summary(), "Tests: 3 passed, 0 failed, 1 ignored, 4360 filtered out, from 1 of 1 test binaries");
        // Several binaries: the filter matched in one of them only.
        let one_of_two = cargo_tests(&format!("{running}test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.1s\n     Running tests\\voice_recovery_fixture.rs (target\\debug\\deps\\voice_recovery_fixture-5317950c3d6d713d.exe)\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s\n")).unwrap();
        assert_eq!(one_of_two.no_success(), None);
        // The output ends after a binary started: cut, or the binary stopped.
        let cut = cargo_tests(&format!("{running}test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s\n     Running tests\\voice_recovery_fixture.rs (target\\debug\\deps\\voice_recovery_fixture-5317950c3d6d713d.exe)\n")).unwrap();
        assert_eq!(cut.no_success().as_deref(), Some("1 of 2 test binaries reported no result (the last started: tests\\voice_recovery_fixture.rs): the output was cut or the binary stopped"));
        assert!(cargo_tests(running).unwrap().no_success().unwrap().starts_with("1 of 1 test binaries reported no result"));
        // A failed run counts its tests too; other outputs are not cargo tests.
        let failed = cargo_tests("test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2s\n").unwrap();
        assert_eq!((failed.passed, failed.failed), (4, 1));
        assert!(cargo_tests("   Compiling hgs v1.58.0\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 3s\n").is_none());
        assert!(cargo_tests("Running migration (step 2 of 3)\n").is_none(), "a program's own Running line is no test binary");
    }

    /// Task #34: through the run itself. cargo writes the results to stdout and
    /// its "Running" lines to stderr; an exit 0 that tested nothing fails,
    /// named, and the counts end the tail whatever the output before them.
    #[test]
    fn a_run_that_tested_nothing_fails_and_its_counts_end_the_tail() {
        let run = |windows: &str, unix: &str| {
            let plan = shell_plan(windows, unix);
            run_tree(Command::new(&plan.program).args(&plan.argv), Duration::from_secs(60), None)
        };
        // A filter that matches no test (the lead's own "ok. 0 passed").
        let none = run(
            "echo running 0 tests&& echo test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4363 filtered out; finished in 0.01s&& echo      Running unittests src\\main.rs (target\\debug\\deps\\hgs-1.exe) 1>&2",
            "echo 'running 0 tests'; echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4363 filtered out; finished in 0.01s'; echo '     Running unittests src/main.rs (target/debug/deps/hgs-1)' >&2",
        );
        assert_eq!(none.exit, Some(1), "{none:?}");
        assert_eq!(none.error, "no test matched the filter: 0 ran, 4363 filtered out");
        assert!(none.tail.trim_end().ends_with("no test matched the filter: 0 ran, 4363 filtered out"), "{}", none.tail);
        assert!(none.tail.contains("Tests: 0 passed, 0 failed, 0 ignored, 4363 filtered out, from 1 of 1 test binaries"), "{}", none.tail);
        // The output ends after "Running": nothing reported (dev1's two runs).
        let cut = run(
            "echo      Running unittests src\\main.rs (target\\debug\\deps\\hgs-1.exe) 1>&2",
            "echo '     Running unittests src/main.rs (target/debug/deps/hgs-1)' >&2",
        );
        assert_eq!(cut.exit, Some(1), "{cut:?}");
        assert!(cut.error.contains("reported no result") && cut.error.contains("src") && cut.error.contains("main.rs"), "{}", cut.error);
        // A run that tested passes, with its counts at the end of the tail.
        let ran = run(
            "echo test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.01s&& echo      Running unittests src\\main.rs (target\\debug\\deps\\hgs-1.exe) 1>&2",
            "echo 'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.01s'; echo '     Running unittests src/main.rs (target/debug/deps/hgs-1)' >&2",
        );
        assert_eq!(ran.exit, Some(0), "{ran:?}");
        assert!(ran.tail.trim_end().ends_with("Tests: 2 passed, 0 failed, 0 ignored, 9 filtered out, from 1 of 1 test binaries"), "{}", ran.tail);
        // Any other command is not judged.
        let other = run("echo hello", "echo hello");
        assert_eq!((other.exit, other.tests.is_none()), (Some(0), true));
    }

    /// Task #34: the cargo-test template that exited 0 with no test binary
    /// started (the build's output only) fails, named.
    #[test]
    fn a_cargo_test_template_without_a_test_binary_fails() {
        let dir = temp();
        let mut plan = shell_plan("echo    Compiling hgs v1.58.0 1>&2", "echo '   Compiling hgs v1.58.0' >&2");
        plan.template = "cargo-test".into();
        let outcome = execute(&plan, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(1), "{outcome:?}");
        assert_eq!(outcome.error, "the output ended before any test binary ran: no test ran");
        let mut other = shell_plan("echo built", "echo built");
        other.template = "cargo-build".into();
        assert_eq!(execute(&other, dir.path(), Duration::from_secs(60), &[]).exit, Some(0), "another template is not judged");
    }

    /// Task #35: team 33fbafeb's qt-build in a DeepSeek member's clone stopped
    /// with C1083 'Permission denied' on an object file while real-time
    /// scanning held it, and a cargo test with D8050. Such a build runs once
    /// more and names the folder; a build's own error runs once, and a commit
    /// is never run twice.
    #[test]
    fn a_build_stopped_by_scanning_runs_once_more_and_names_the_folder() {
        let dir = temp();
        // Fails with the scanner's signature on the first run only.
        // Task #61: printf, not echo: dash's echo reads the message's backslashes
        // as escapes (\c in C:\VC\c1.dll ends its output), so the scanner's
        // signature never reached the tail on Linux.
        let flaky = |template: &str, line: &str| {
            let (windows, unix) = (
                format!("if exist ran.txt (echo built & exit 0) else (echo ran> ran.txt & echo {line} & exit 2)"),
                format!("if [ -f ran.txt ]; then echo built; exit 0; else echo ran > ran.txt; printf '%s\\n' \"{line}\"; exit 2; fi"),
            );
            let mut plan = shell_plan(&windows, &unix);
            plan.template = template.into();
            plan
        };
        let c1083 = r"D:\work\build\x.cpp : fatal error C1083: Cannot open compiler generated file: 'x.cpp.obj': Permission denied";
        let outcome = execute(&flaky("cmake-build", c1083), dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(0), "{outcome:?}");
        assert!(outcome.tail.contains("built"), "{}", outcome.tail);
        assert!(outcome.tail.contains("MSVC C1083") && outcome.tail.contains("the second run passed"), "{}", outcome.tail);
        assert!(outcome.tail.contains(&dir.path().display().to_string()), "the folder is named: {}", outcome.tail);
        std::fs::remove_file(dir.path().join("ran.txt")).unwrap();
        let d8050 = r"cl : Command line error D8050 : cannot execute 'C:\VC\c1.dll': failed to get command line into debug records";
        // Task #34: the passing cargo test reports its tests, as cargo does
        // (results on stdout, "Running" on stderr); one that tests nothing fails.
        let cargo = {
            let (windows, unix) = (
                format!("if exist ran.txt (echo test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out& echo      Running unittests src\\main.rs ^(target\\debug\\deps\\hgs-1.exe^) 1>&2 & exit 0) else (echo ran> ran.txt & echo {d8050} & exit 2)"),
                format!("if [ -f ran.txt ]; then echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'; echo '     Running unittests src/main.rs (target/debug/deps/hgs-1)' >&2; exit 0; else echo ran > ran.txt; printf '%s\\n' \"{d8050}\"; exit 2; fi"),
            );
            let mut plan = shell_plan(&windows, &unix);
            plan.template = "cargo-test".into();
            plan
        };
        let outcome = execute(&cargo, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(0), "{outcome:?}");
        assert!(outcome.tail.contains("MSVC D8050"), "{}", outcome.tail);
        std::fs::remove_file(dir.path().join("ran.txt")).unwrap();
        // A build's own error, and the same text in a commit, run once.
        let counted = |template: &str, line: &str| {
            let (windows, unix) = (
                format!("echo x>> runs.txt & echo {line} & exit 2"),
                format!("echo x >> runs.txt; echo \"{line}\"; exit 2"),
            );
            let mut plan = shell_plan(&windows, &unix);
            plan.template = template.into();
            plan
        };
        let runs = || std::fs::read_to_string(dir.path().join("runs.txt")).map(|text| text.lines().count()).unwrap_or(0);
        let own = execute(&counted("cargo-test", "src\\lib.rs(3): error C2065: 'x': undeclared identifier"), dir.path(), Duration::from_secs(60), &[]);
        assert_eq!((own.exit, runs()), (Some(2), 1), "{own:?}");
        assert!(!own.tail.contains("ran this build again"), "{}", own.tail);
        std::fs::remove_file(dir.path().join("runs.txt")).unwrap();
        let commit = execute(&counted("git-commit", c1083), dir.path(), Duration::from_secs(60), &[]);
        assert_eq!((commit.exit, runs()), (Some(2), 1), "a commit never runs twice: {commit:?}");
    }

    #[test]
    fn a_run_in_a_temp_folder_reports_its_exit_and_output() {
        let dir = temp();
        let echo = shell_plan("echo hello", "echo hello");
        let outcome = execute(&echo, dir.path(), Duration::from_secs(30), &[]);
        assert_eq!(outcome.exit, Some(0), "{outcome:?}");
        assert!(outcome.tail.contains("hello"), "{}", outcome.tail);
        assert!(!outcome.timed_out);
        let failed = shell_plan("exit 7", "exit 7");
        assert_eq!(execute(&failed, dir.path(), Duration::from_secs(30), &[]).exit, Some(7));
        // The template runs in the member's folder with TEMP inside it.
        let folder = shell_plan("echo %TEMP%", "echo \"$TEMP\"");
        let outcome = execute(&folder, dir.path(), Duration::from_secs(30), &[]);
        assert!(outcome.tail.contains(".zerus-team"), "{}", outcome.tail);
    }

    #[test]
    fn a_run_is_bounded_in_time_and_keeps_only_its_tail() {
        let dir = temp();
        // A program that never finishes within the timeout.
        let plan = shell_plan("ping -n 30 127.0.0.1", "sleep 30");
        let outcome = execute(&plan, dir.path(), Duration::from_secs(1), &[]);
        assert!(outcome.timed_out, "{outcome:?}");
        assert!(outcome.exit.is_none());
        assert!(outcome.error.contains("did not finish"));
        // A big output keeps its last 4000 characters.
        let plan = shell_plan("type big.txt", "cat big.txt");
        std::fs::write(dir.path().join("big.txt"), "x".repeat(200_000) + "\nEND").unwrap();
        let outcome = execute(&plan, dir.path(), Duration::from_secs(30), &[]);
        assert_eq!(outcome.exit, Some(0));
        assert!(outcome.tail.chars().count() <= 4000 && outcome.tail.ends_with("END"), "{}", outcome.tail.chars().count());
        assert_eq!(last_chars("a\nb\nc", 2), "c");
    }

    #[test]
    fn the_off_switch_and_the_owners_additions_are_read_by_the_listing() {
        let dir = temp();
        settings_override(Some(dir.path().join("templates.json")));
        assert!(listed()["items"].as_array().unwrap().iter().all(|item| item["source"] == "builtin"));
        // The owner adds one in Zerus state; it is listed as an addition.
        set_additions(&[json!({"id":"make","program":"make","args":["-j2"]})]).unwrap();
        let items = listed();
        assert!(items["items"].as_array().unwrap().iter().any(|item| item["id"] == "make" && item["source"] == "owner"), "{items}");
        // An addition cannot shadow a built-in one.
        assert!(set_additions(&[json!({"id":"cargo-build","program":"make","args":[]})]).is_err());
        assert!(plan("cargo-build", &json!({"package":"hgs"}), dir.path()).is_ok());
        assert!(addition("cargo-build").is_none());
        settings_override(None);
    }

    #[test]
    fn a_templates_file_in_the_members_folder_is_ignored() {
        // U21, attempt 2: the folder is the member's, so a file there is not a
        // template source; the owner's state is (set_additions above).
        let dir = temp();
        settings_override(Some(dir.path().join("templates.json")));
        std::fs::create_dir_all(dir.path().join(".zerus-team")).unwrap();
        let marker = dir.path().join("marker.txt");
        std::fs::write(
            dir.path().join(".zerus-team").join("templates.json"),
            serde_json::to_vec(&json!({"templates":[{"id":"pwn","program":"cmd","args":["/c",format!("echo x > \"{}\"", marker.display())]}]})).unwrap(),
        )
        .unwrap();
        let error = plan("pwn", &json!({}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("Unknown template"), "{error}");
        assert!(!marker.exists());
        assert!(listed()["items"].as_array().unwrap().iter().all(|item| item["id"] != "pwn"));
        settings_override(None);
    }

    #[test]
    fn the_global_switch_turns_the_builtin_set_off() {
        let dir = temp();
        settings_override(Some(dir.path().join("templates.json")));
        assert!(enabled());
        assert!(plan("cargo-build", &json!({}), dir.path()).is_ok());
        set_enabled(false).unwrap();
        assert!(!enabled());
        let error = plan("cargo-build", &json!({}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("turned off"), "{error}");
        let listed = listed();
        assert_eq!((listed["enabled"].as_bool(), listed["items"].as_array().map(Vec::len)), (Some(false), Some(0)));
        set_enabled(true).unwrap();
        assert!(plan("cargo-build", &json!({}), dir.path()).is_ok());
        settings_override(None);
    }

    #[test]
    fn an_owner_added_template_runs_its_fixed_argv() {
        let dir = temp();
        settings_override(Some(dir.path().join("templates.json")));
        let (program, flag) = if cfg!(windows) { ("cmd", "/c") } else { ("sh", "-c") };
        set_additions(&[json!({"id":"say","program":program,"args":[flag,"echo hello"]})]).unwrap();
        let built = plan("say", &json!({}), dir.path()).unwrap();
        assert_eq!((built.program.as_str(), built.argv.as_slice()), (program, [flag.to_owned(), "echo hello".to_owned()].as_slice()));
        let outcome = execute(&built, dir.path(), Duration::from_secs(30), &[]);
        assert_eq!(outcome.exit, Some(0));
        assert_eq!(outcome.tail.trim(), "hello");
        // No placeholders in an addition: an argument is refused.
        assert!(plan("say", &json!({"x":"y"}), dir.path()).is_err());
        settings_override(None);
    }

    #[test]
    fn a_run_that_fails_records_the_failing_test_names() {
        // #717: the record of a failed run carries what failed, not just the tail.
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("cmd");
            command.args(["/D", "/S", "/C", "echo FAIL!  : SlowEventLog::a_case() & exit /b 1"]);
            command
        } else {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "echo 'FAIL!  : SlowEventLog::a_case()'; exit 1"]);
            command
        };
        let outcome = run_tree(&mut command, Duration::from_secs(30), None);
        assert_eq!(outcome.exit, Some(1), "{outcome:?}");
        assert_eq!(outcome.failures, ["SlowEventLog::a_case()"], "{outcome:?}");
        assert!(outcome.excerpt.contains("FAIL!  : SlowEventLog::a_case()"), "{outcome:?}");
    }

    #[test]
    fn a_failed_run_names_its_tests_and_keeps_the_context_around_them() {
        // #717: Qt, cargo, pytest and CTest failure lines name the tests.
        let qt = "********* Start testing of SlowEventLog *********\nPASS   : SlowEventLog::initTestCase()\nFAIL!  : SlowEventLog::a_slow_case() Compared values are not the same\n   Actual (1): 1\n   Expected (2): 2\nFAIL!  : SlowEventLog::b_case() timeout\nTotals: 2 failed\n";
        let (names, excerpt) = failures_in(qt);
        assert_eq!(names, ["SlowEventLog::a_slow_case()", "SlowEventLog::b_case()"]);
        assert!(excerpt.contains("FAIL!  : SlowEventLog::a_slow_case()") && excerpt.contains("Expected (2): 2"), "{excerpt}");
        assert!(excerpt.chars().count() <= FAILURE_EXCERPT_BYTES);
        let (names, _) = failures_in("test teams::mod::a_case ... FAILED\ntest teams::mod::b_case ... ok\n");
        assert_eq!(names, ["teams::mod::a_case"]);
        let (names, _) = failures_in("FAILED tests/test_api.py::test_page - assert 1 == 2\n");
        assert_eq!(names, ["tests/test_api.py::test_page"]);
        let (names, _) = failures_in("The following tests FAILED:\n\t12 - test_sloweventlog (Failed)\n");
        assert_eq!(names, ["test_sloweventlog"]);
        assert_eq!(failures_in("all good\n"), (Vec::new(), String::new()));
    }

    #[test]
    fn a_suite_bound_follows_its_recorded_duration_within_the_configured_limits() {
        // #717: no record keeps the template's own bound; a record scales with
        // the configured factor, grace, floor and ceiling.
        assert_eq!(recorded_bound(1800, None, 2, 60, 60, 7200), 1800);
        assert_eq!(recorded_bound(1800, Some(0), 2, 60, 60, 7200), 1800);
        assert_eq!(recorded_bound(1800, Some(300_000), 2, 60, 60, 7200), 660);
        assert_eq!(recorded_bound(1800, Some(10_000), 2, 60, 120, 7200), 120, "the floor");
        assert_eq!(recorded_bound(1800, Some(3_600_000), 2, 60, 60, 7200), 7200, "the ceiling");
    }

    fn env_of(used: &CacheUse, key: &str) -> Option<String> {
        used.env.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone())
    }

    fn project(folder: &Path, text: &str) {
        std::fs::create_dir_all(folder.join(".zerus")).unwrap();
        std::fs::write(folder.join(".zerus").join("tests.toml"), text).unwrap();
    }

    /// Task #239: a web project's vitest runs from its own node_modules, writes
    /// the JUnit report the counts and failures are read from, and installs
    /// through the worker first when node_modules are missing.
    #[test]
    fn a_web_project_runs_its_own_vitest_and_reports_each_file() {
        let dir = temp();
        let app = dir.path().join("client");
        std::fs::create_dir_all(app.join("node_modules").join("vitest")).unwrap();
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(app.join("package.json"), r#"{"scripts":{"test":"vitest"}}"#).unwrap();
        std::fs::write(app.join("package-lock.json"), "{}").unwrap();
        std::fs::write(app.join("src").join("app.test.ts"), "// fake").unwrap();
        // A fake vitest: it writes the report the template asks for and says
        // what it ran, so a test never needs a real web toolchain.
        std::fs::write(
            app.join("node_modules").join("vitest").join("vitest.mjs"),
            r#"
import fs from 'node:fs';
const out = process.argv.find((arg) => arg.startsWith('--outputFile.junit='));
const text = '<?xml version="1.0"?><testsuites><testsuite name="src/app.test.ts" tests="3" failures="1">' +
  '<testcase name="adds" classname="src/app.test.ts"/>' +
  '<testcase name="fails" classname="src/app.test.ts"><failure message="expected 1 to be 2"/></testcase>' +
  '<testcase name="skips" classname="src/app.test.ts"><skipped/></testcase></testsuite></testsuites>';
if (out) fs.writeFileSync(out.slice(out.indexOf('=') + 1), text);
console.log(' Test Files  1 failed | 1 passed (2)');
console.log(' FAIL  src/app.test.ts > fails');
"#,
        )
        .unwrap();
        let first = plan("web-test", &json!({"project":"client","paths":["client/src/app.test.ts"]}), dir.path()).unwrap();
        assert_eq!((first.program.as_str(), first.cwd.as_deref()), ("node", Some("client")), "{first:?}");
        // Task #16 (team ede724c4): without a project the template runs the
        // folder's client/ web project, as its summary says.
        let default = plan("web-test", &json!({"paths":["client/src/app.test.ts"]}), dir.path()).unwrap();
        assert_eq!((default.program.as_str(), default.cwd.as_deref()), ("node", Some("client")), "{default:?}");
        let report = dir.path().join(".zerus-team").join("results").join("vitest.xml");
        assert!(first.argv.iter().any(|arg| arg == "--reporter=junit"), "{first:?}");
        assert!(first.argv.iter().any(|arg| *arg == format!("--outputFile.junit={}", report.display())), "{first:?}");
        assert!(first.argv.iter().any(|arg| arg.ends_with("app.test.ts")), "{first:?}");
        let outcome = execute(&first, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(0), "{outcome:?}");
        assert!(outcome.tail.contains("Test Files"), "{}", outcome.tail);
        let read = outcome.report.expect("the report is read");
        assert_eq!((read.passed, read.failed, read.skipped), (1, 1, 1), "{read:?}");
        assert!(read.results.iter().all(|result| result.suite == "src/app.test.ts"), "{read:?}");
        assert!(outcome.failures.iter().any(|name| name.contains("fails")), "{:?}", outcome.failures);
        assert_eq!(report_path(dir.path(), "web-test"), Some((report.clone(), super::super::run_results::Format::Junit)));
        // Two files, one failing: the run's exit, its counts and the failing
        // name come back per file, not as one blob.
        std::fs::write(
            app.join("node_modules").join("vitest").join("vitest.mjs"),
            r#"
import fs from 'node:fs';
const out = process.argv.find((arg) => arg.startsWith('--outputFile.junit='));
const text = '<?xml version="1.0"?><testsuites>' +
  '<testsuite name="src/app.test.ts" tests="2" failures="1">' +
  '<testcase name="adds" classname="src/app.test.ts"/>' +
  '<testcase name="fails" classname="src/app.test.ts"><failure message="expected 1 to be 2"/></testcase>' +
  '</testsuite>' +
  '<testsuite name="src/util.test.ts" tests="1" failures="0"><testcase name="parses" classname="src/util.test.ts"/></testsuite>' +
  '</testsuites>';
if (out) fs.writeFileSync(out.slice(out.indexOf('=') + 1), text);
console.log(' Test Files  1 failed | 1 passed (2)');
console.log(' FAIL  src/app.test.ts > fails');
process.exit(1);
"#,
        )
        .unwrap();
        let failed = execute(&first, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(failed.exit, Some(1), "{failed:?}");
        let read = failed.report.expect("the report is read");
        assert_eq!((read.passed, read.failed), (2, 1), "{read:?}");
        let files: std::collections::BTreeSet<&str> = read.results.iter().map(|result| result.suite.as_str()).collect();
        assert_eq!(files.len(), 2, "one suite per file: {read:?}");
        assert!(failed.failures.iter().any(|name| name.contains("fails")), "{:?}", failed.failures);
        // Without node_modules the worker installs them first, then runs the
        // same tests: the request is npm ci with the vitest run behind it.
        std::fs::remove_dir_all(app.join("node_modules")).unwrap();
        let install = plan("web-test", &json!({"project":"client"}), dir.path()).unwrap();
        assert_eq!(install.more.len(), 1, "{install:?}");
        assert!(install.more[0].1.iter().any(|arg| arg == "--reporter=junit"), "{install:?}");
        assert!(install.argv.contains(&"ci".to_owned()), "{install:?}");
        // Task #61: Windows runs npm through cmd (cmd /C npm ci), other systems
        // run npm itself (npm ci).
        assert!(install.program == "npm" || install.argv.iter().any(|arg| arg == "npm"), "{install:?}");
        // A project that leads outside the member's folder is refused.
        let outside = temp();
        let error = plan("web-test", &json!({"project": outside.path().display().to_string()}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("stays inside your folder") || error.contains("leads outside"), "{error}");
        // A folder with no package.json is named, never run.
        let empty = temp();
        std::fs::create_dir_all(empty.path().join("client")).unwrap();
        let error = plan("web-test", &json!({"project":"client"}), empty.path()).unwrap_err().to_string();
        assert!(error.contains("package.json"), "{error}");
    }

    /// Task #58 (owner, 2026-11-10): Qt goes from Zerus completely. A stored
    /// run, a project's tests.toml or a request that still names a Qt
    /// template is refused with its replacement, never run.
    #[test]
    fn a_removed_qt_template_is_refused_with_its_replacement() {
        let dir = temp();
        for (old, new) in [("qt-configure", "cmake-configure"), ("qt-build", "cmake-build"), ("qt-test", "ctest")] {
            assert!(builtin(old).is_none(), "{old} is no built-in");
            let error = plan(old, &json!({}), dir.path()).unwrap_err().to_string();
            assert!(error.starts_with(&format!("The {old} template was removed with the Qt desktop: use {new}")), "{error}");
            assert!(builtin(new).is_some(), "{new} replaces it");
        }
        assert_eq!(removed("cargo-test"), None);
        // A project's .zerus/tests.toml that still names one is told the same.
        let project = crate::test_discipline::config::ProjectTemplate { id: "qt-test".into(), program: "tests.exe".into(), ..Default::default() };
        let error = check_project_template(&project).unwrap_err().to_string();
        assert!(error.contains("use ctest"), "{error}");
    }

    #[test]
    fn a_project_without_a_build_cache_builds_as_before() {
        let dir = temp();
        settings_override(Some(dir.path().join("templates.json")));
        let folder = dir.path().join("member");
        std::fs::create_dir_all(&folder).unwrap();
        assert_eq!(build_cache(&folder), None);
        for template in ["cargo-build", "cargo-test", "cargo-clippy", "cmake-configure", "cmake-build", "ctest"] {
            assert_eq!(cache_use(template, &folder), CacheUse::default(), "{template}");
        }
        // Task #58: a CMake project of the member's own; Qt is gone (no Qt
        // lookup, which failed on the CI runner with "Qt was not found for tray/").
        std::fs::write(folder.join("CMakeLists.txt"), "project(example)\n").unwrap();
        let configure = plan("cmake-configure", &json!({}), &folder).unwrap();
        assert!(!configure.argv.iter().any(|arg| arg.contains("LAUNCHER")), "{:?}", configure.argv);
        project(&folder, "[build_cache]\nenabled = false\n");
        assert_eq!(build_cache(&folder), None);
        // Another section of the file alone does not turn it on.
        project(&folder, "[providers]\ncargo = true\n");
        assert_eq!(build_cache(&folder), None);
        settings_override(None);
    }

    // On: cargo runs through sccache and a CMake build through ccache, both in
    // one shared folder and bounded; ccache hashes paths relative to the
    // member's folder so worktrees share entries; each sccache server is the
    // run's own.
    #[test]
    fn a_project_build_cache_wraps_rustc_and_the_cmake_compilers_in_one_bounded_folder() {
        let dir = temp();
        settings_override(Some(dir.path().join("templates.json")));
        let shared = dir.path().join("shared-cache");
        let folder = dir.path().join("member");
        std::fs::create_dir_all(&folder).unwrap();
        project(&folder, &format!("[build_cache]\nenabled = true\nsize = \"5g\"\ndir = '{}'\n", shared.display()));
        let tools = std::collections::HashMap::from([("sccache".to_owned(), dir.path().join("sccache")), ("ccache".to_owned(), dir.path().join("ccache"))]);
        tools_override(Some(tools));
        assert_eq!(build_cache(&folder), Some(BuildCache { dir: shared.clone(), size: "5G".into(), rust: true, cxx: true }));
        let cargo = cache_use("cargo-test", &folder);
        assert_eq!(env_of(&cargo, "RUSTC_WRAPPER"), Some(dir.path().join("sccache").display().to_string()));
        assert_eq!(env_of(&cargo, "SCCACHE_DIR"), Some(shared.join("sccache").display().to_string()));
        assert_eq!(env_of(&cargo, "SCCACHE_CACHE_SIZE").as_deref(), Some("5G"));
        let port: u16 = env_of(&cargo, "SCCACHE_SERVER_PORT").unwrap().parse().unwrap();
        assert!(port > 0);
        assert!(cargo.launcher.is_none() && cargo.note.is_empty());
        let cmake = cache_use("cmake-build", &folder);
        assert_eq!(cmake.launcher, Some(dir.path().join("ccache").display().to_string()));
        assert_eq!(env_of(&cmake, "CCACHE_DIR"), Some(shared.join("ccache").display().to_string()));
        assert_eq!(env_of(&cmake, "CCACHE_MAXSIZE").as_deref(), Some("5G"));
        assert_eq!(env_of(&cmake, "CCACHE_BASEDIR"), Some(folder.display().to_string()));
        assert!(env_of(&cmake, "RUSTC_WRAPPER").is_none());
        // The CMake configure names the launcher for both compilers.
        std::fs::write(folder.join("CMakeLists.txt"), "project(example)\n").unwrap();
        let configure = plan("cmake-configure", &json!({}), &folder).unwrap();
        let launcher = dir.path().join("ccache").display().to_string();
        assert!(configure.argv.contains(&format!("-DCMAKE_CXX_COMPILER_LAUNCHER={launcher}")), "{:?}", configure.argv);
        assert!(configure.argv.contains(&format!("-DCMAKE_C_COMPILER_LAUNCHER={launcher}")), "{:?}", configure.argv);
        // A test run, a git route or a commit gets nothing of it.
        for template in ["ctest", "git-merge-lead", "git-commit", "android-test"] {
            assert_eq!(cache_use(template, &folder), CacheUse::default(), "{template}");
        }
        // Only the C++ compilers: cargo builds as before.
        project(&folder, &format!("[build_cache]\nenabled = true\ncompilers = [\"cxx\"]\ndir = '{}'\n", shared.display()));
        assert_eq!(cache_use("cargo-build", &folder), CacheUse::default());
        assert!(cache_use("cmake-build", &folder).launcher.is_some_and(|launcher| !launcher.is_empty()));
        // The default folder is one in Zerus state, shared by every worktree.
        project(&folder, "[build_cache]\nenabled = true\n");
        assert_eq!(build_cache(&folder).map(|cache| (cache.dir, cache.size)), Some((root().join("build-cache"), "20G".to_owned())));
        tools_override(None);
        settings_override(None);
    }

    // The owner's `build_cache` in Settings > Teams overrides the project's
    // keys; a malformed value turns the cache off instead of guessing; a
    // missing tool builds without it and says so in the run.
    #[test]
    fn the_owner_overrides_the_project_and_a_missing_tool_is_named() {
        let dir = temp();
        settings_override(Some(dir.path().join("templates.json")));
        let folder = dir.path().join("member");
        std::fs::create_dir_all(&folder).unwrap();
        let shared = dir.path().join("shared-cache");
        project(&folder, &format!("[build_cache]\nenabled = true\nsize = \"5G\"\ndir = '{}'\n", shared.display()));
        write_settings(&json!({"enabled":true,"build_cache":{"size":"2G"}})).unwrap();
        assert_eq!(build_cache(&folder).map(|cache| cache.size), Some("2G".to_owned()));
        write_settings(&json!({"enabled":true,"build_cache":{"enabled":false}})).unwrap();
        assert_eq!(build_cache(&folder), None);
        // The owner may turn it on for a project without the section.
        write_settings(&json!({"enabled":true,"build_cache":{"enabled":true,"dir":shared.display().to_string()}})).unwrap();
        project(&folder, "");
        assert_eq!(build_cache(&folder).map(|cache| cache.dir), Some(shared.clone()));
        write_settings(&json!({"enabled":true})).unwrap();
        for bad in ["size = \"lots\"", "size = \"0G\"", "size = \"5GB\"", "dir = \"relative/cache\"", "compilers = []"] {
            project(&folder, &format!("[build_cache]\nenabled = true\n{bad}\n"));
            assert_eq!(build_cache(&folder), None, "{bad}");
        }
        project(&folder, &format!("[build_cache]\nenabled = true\ndir = '{}'\n", shared.display()));
        tools_override(Some(std::collections::HashMap::new()));
        let cargo = cache_use("cargo-build", &folder);
        assert!(cargo.env.is_empty() && cargo.note.contains("sccache was not found"), "{cargo:?}");
        let cmake = cache_use("cmake-configure", &folder);
        assert!(cmake.env.is_empty() && cmake.note.contains("ccache was not found"), "{cmake:?}");
        // The launcher a build folder had is cleared, not left pointing at a missing tool.
        assert_eq!(cmake.launcher.as_deref(), Some(""));
        tools_override(None);
        settings_override(None);
    }

    /// #742: a cache tool a run misses is offered to the owner, once, and the
    /// run's note says so instead of leaving the member to install it.
    #[test]
    fn a_missing_cache_tool_is_offered_to_the_owner() {
        let dir = temp();
        let state = temp();
        crate::state::with_test_state_root(state.path(), || {
            settings_override(Some(dir.path().join("templates.json")));
            project(dir.path(), &format!("[build_cache]\nenabled = true\ndir = '{}'\n", dir.path().join("cache").display()));
            tools_override(Some(std::collections::HashMap::new()));
            let cargo = cache_use("cargo-build", dir.path());
            assert!(cargo.note.contains("Zerus offers it to the owner in Waiting for you; never install it yourself"), "{cargo:?}");
            let cmake = cache_use("cmake-configure", dir.path());
            assert!(cmake.note.contains("ccache was not found") && cmake.note.contains("Waiting for you"), "{cmake:?}");
            cache_use("cargo-test", dir.path());
            let items = crate::test_discipline::tools::waiting_items();
            for tool in ["sccache", "ccache"] {
                // Offered unless this computer has it after all.
                if crate::test_discipline::tools::find_program(tool).is_none() {
                    assert_eq!(items.iter().filter(|item| item["tool"] == tool).count(), 1, "{items:?}");
                }
            }
            tools_override(None);
            settings_override(None);
        });
    }

    // A run with the cache on and its tools missing still runs and exits as
    // its command does; the result names the missing tool first.
    #[test]
    fn a_missing_cache_tool_never_fails_a_run() {
        let dir = temp();
        settings_override(Some(dir.path().join("templates.json")));
        project(dir.path(), &format!("[build_cache]\nenabled = true\ndir = '{}'\n", dir.path().join("cache").display()));
        tools_override(Some(std::collections::HashMap::new()));
        let mut run = shell_plan("echo built", "echo built");
        run.template = "cargo-build".into();
        let outcome = execute(&run, dir.path(), Duration::from_secs(30), &[]);
        assert_eq!(outcome.exit, Some(0), "{outcome:?}");
        assert!(outcome.tail.starts_with("build cache: sccache was not found") && outcome.tail.contains("built"), "{}", outcome.tail);
        // A CMake build names its missing ccache the same way.
        run.template = "cmake-build".into();
        let outcome = execute(&run, dir.path(), Duration::from_secs(30), &[]);
        assert_eq!(outcome.exit, Some(0), "{outcome:?}");
        assert!(outcome.tail.starts_with("build cache: ccache was not found"), "{}", outcome.tail);
        tools_override(None);
        settings_override(None);
    }

    /// Task 85: any CMake project configures, builds and tests through the
    /// generic templates; every value is checked by its type.
    #[test]
    fn cmake_templates_build_any_project_with_checked_values() {
        let dir = temp();
        std::fs::create_dir_all(dir.path().join("engine")).unwrap();
        std::fs::write(dir.path().join("engine").join("CMakeLists.txt"), "project(x)\n").unwrap();
        let configure = plan("cmake-configure", &json!({"source":"engine","build_dir":"out/engine","generator":"Visual Studio 17 2022","config":"Debug"}), dir.path()).unwrap();
        assert_eq!(configure.argv, ["-S", "engine", "-B", "out/engine", "-G", "Visual Studio 17 2022", "-DCMAKE_BUILD_TYPE=Debug"]);
        let preset = plan("cmake-configure", &json!({"source":"engine","preset":"ci"}), dir.path()).unwrap();
        assert_eq!(preset.argv, ["-S", "engine", "-B", "build", "--preset", "ci"]);
        // No CMakeLists, a folder outside, the sources as the build folder, an unknown key.
        assert!(plan("cmake-configure", &json!({}), dir.path()).unwrap_err().to_string().contains("no CMakeLists.txt"));
        for args in [json!({"source":"engine","build_dir":"../out"}), json!({"source":"engine","build_dir":"C:/out"}), json!({"source":"engine","build_dir":"/tmp/out"}),
                     json!({"source":"engine","build_dir":"engine"}), json!({"source":"engine","defines":"X=1"}), json!({"source":"engine","config":"Debug; rm"})] {
            assert!(plan("cmake-configure", &args, dir.path()).is_err(), "{args}");
        }
        let build = plan("cmake-build", &json!({"build_dir":"out/engine","config":"Debug","targets":["core","tests"]}), dir.path()).unwrap();
        assert_eq!(build.argv, ["--build", "out/engine", "--config", "Debug", "--target", "core", "tests"]);
        assert_eq!(plan("cmake-build", &json!({}), dir.path()).unwrap().argv, ["--build", "build"]);
        // ctest: the tests by name as an anchored, escaped -R; a regex as given; not both.
        std::fs::create_dir_all(dir.path().join("out").join("engine")).unwrap();
        let tests = plan("ctest", &json!({"build_dir":"out/engine","config":"Debug","tests":["adds","math.divides"]}), dir.path()).unwrap();
        let report = dir.path().join(".zerus-team").join("results").join("ctest.xml").display().to_string();
        assert_eq!(tests.argv, ["--test-dir", "out/engine", "--output-on-failure", "--output-junit", report.as_str(), "-C", "Debug", "-R", "^(adds|math\\.divides)$"]);
        let regex = plan("ctest", &json!({"build_dir":"out/engine","regex":"^unit_"}), dir.path()).unwrap();
        assert_eq!(regex.argv[regex.argv.len() - 2..], ["-R", "^unit_"]);
        assert!(plan("ctest", &json!({"build_dir":"out/engine","tests":["a"],"regex":"b"}), dir.path()).unwrap_err().to_string().contains("not both"));
        assert!(plan("ctest", &json!({}), dir.path()).unwrap_err().to_string().contains("configure and build first"));
        assert_eq!(report_path(dir.path(), "ctest").unwrap().1, super::super::run_results::Format::Junit);
    }

    /// Task 85: MSBuild builds a solution or project of the folder; VSTest
    /// runs test assemblies into a TRX report.
    #[test]
    fn msbuild_and_vstest_take_only_their_files_and_checked_values() {
        let dir = temp();
        std::fs::write(dir.path().join("Fixture.sln"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        let build = plan("msbuild-build", &json!({"project":"Fixture.sln","config":"Release","platform":"Any CPU","targets":["Core","Tests:Rebuild"]}), dir.path()).unwrap();
        assert_eq!(build.argv, ["Fixture.sln", "/m", "/nologo", "/v:minimal", "/p:Configuration=Release", "/p:Platform=Any CPU", "/t:Core;Tests:Rebuild"]);
        assert!(build.program.to_lowercase().contains("msbuild"), "{}", build.program);
        assert!(plan("msbuild-build", &json!({"project":"notes.txt"}), dir.path()).unwrap_err().to_string().contains(".sln or a project"));
        assert!(plan("msbuild-build", &json!({"project":"Missing.sln"}), dir.path()).is_err());
        assert!(plan("msbuild-build", &json!({"project":"Fixture.sln","platform":"x64;/t:Clean"}), dir.path()).is_err());
        assert!(plan("msbuild-build", &json!({}), dir.path()).is_err());
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin").join("Tests.dll"), "").unwrap();
        let tests = plan("vstest", &json!({"tests":["bin/Tests.dll"],"filter":"TestCategory=Unit"}), dir.path()).unwrap();
        assert_eq!(tests.argv[0], "bin/Tests.dll");
        assert!(tests.argv[1].starts_with("/Logger:trx;LogFileName=") && tests.argv[1].ends_with("vstest.trx"), "{:?}", tests.argv);
        assert_eq!(tests.argv[2], "/TestCaseFilter:TestCategory=Unit");
        assert!(plan("vstest", &json!({"tests":["notes.txt"]}), dir.path()).is_err());
    }

    fn project_file(dir: &Path, templates: &str) {
        std::fs::create_dir_all(dir.join(".zerus")).unwrap();
        std::fs::write(dir.join(".zerus").join("tests.toml"), format!("version = 1\n{templates}")).unwrap();
    }

    const PROJECT_TEMPLATE: &str = r#"
[[template]]
id = "make-target"
summary = "make one target"
program = "make"
args = ["-C", "src", "{targets}", "JOBS={jobs}", "--file={makefile}"]
timeout_secs = 900
results = { format = "junit", path = "out/report.xml" }

[[template.params]]
name = "targets"
kind = "identifiers"
required = true

[[template.params]]
name = "jobs"
kind = "identifier"

[[template.params]]
name = "makefile"
kind = "path"
"#;

    /// Task 85: a project's [[template]] is typed like a built-in, refused
    /// until the owner accepts its exact content, and waits again when it
    /// changes.
    #[test]
    fn a_project_template_is_typed_and_runs_only_once_the_owner_accepted_it() {
        let dir = temp();
        let state = temp();
        settings_override(Some(state.path().join("templates.json")));
        let _restore = scopeguard(|| settings_override(None));
        project_file(dir.path(), PROJECT_TEMPLATE);
        std::fs::write(dir.path().join("Makefile"), "").unwrap();
        let error = plan("make-target", &json!({"targets":["all"]}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("waits for the owner's acceptance"), "{error}");
        let listed = project_listed(dir.path()).unwrap();
        assert_eq!((listed.len(), listed[0]["accepted"].clone()), (1, json!(false)));
        let hash = listed[0]["hash"].as_str().unwrap().to_owned();
        accept_project(std::slice::from_ref(&hash)).unwrap();
        // Typed like a built-in: a list spliced whole, an absent optional value
        // drops its whole argument, a wrong value or key is refused.
        let planned = plan("make-target", &json!({"targets":["core","tests"],"jobs":"8"}), dir.path()).unwrap();
        assert_eq!((planned.program.as_str(), planned.argv.clone()), ("make", ["-C", "src", "core", "tests", "JOBS=8"].map(str::to_owned).to_vec()));
        let with_file = plan("make-target", &json!({"targets":["core"],"makefile":"Makefile"}), dir.path()).unwrap();
        assert_eq!(with_file.argv.last().unwrap(), "--file=Makefile");
        assert!(plan("make-target", &json!({}), dir.path()).unwrap_err().to_string().contains("targets"));
        assert!(plan("make-target", &json!({"targets":["a; rm -rf /"]}), dir.path()).is_err());
        assert!(plan("make-target", &json!({"targets":["a"],"other":"x"}), dir.path()).unwrap_err().to_string().contains("has no \"other\""));
        assert_eq!(timeout_in("make-target", dir.path()), 900);
        assert_eq!(report_path(dir.path(), "make-target"), Some((dir.path().join("out/report.xml"), super::super::run_results::Format::Junit)));
        // A changed template waits again.
        project_file(dir.path(), &PROJECT_TEMPLATE.replace("program = \"make\"", "program = \"gmake\""));
        assert!(plan("make-target", &json!({"targets":["core"]}), dir.path()).unwrap_err().to_string().contains("waits for the owner's acceptance"));
        // A built-in id is never a project's.
        assert!(plan("cmake-build", &json!({}), dir.path()).is_ok());
    }

    /// Task 85: a project template is checked as a built-in is, before it can
    /// ever be accepted.
    #[test]
    fn a_project_template_is_validated_like_a_builtin() {
        let base = crate::test_discipline::config::ProjectTemplate {
            id: "lint".into(),
            program: "eslint".into(),
            args: vec!["{paths}".into()],
            params: vec![crate::test_discipline::config::TemplateParam { name: "paths".into(), kind: "paths".into(), required: false }],
            ..Default::default()
        };
        check_project_template(&base).unwrap();
        let bad = |change: &dyn Fn(&mut crate::test_discipline::config::ProjectTemplate)| {
            let mut template = base.clone();
            change(&mut template);
            check_project_template(&template).unwrap_err().to_string()
        };
        assert!(bad(&|t| t.id = "cargo-test".into()).contains("built in"));
        assert!(bad(&|t| t.program = "{tool}".into()).contains("no placeholders"));
        assert!(bad(&|t| t.args = vec!["{missing}".into()]).contains("without declaring"));
        assert!(bad(&|t| t.args = vec!["--files={paths}".into()]).contains("whole argument"));
        assert!(bad(&|t| t.params[0].kind = "shell".into()).contains("identifier, identifiers, text, path or paths"));
        assert!(bad(&|t| t.timeout_secs = Some(0)).contains("1-86400"));
        assert!(bad(&|t| t.id = "a b".into()).contains("letters, digits"));
        // In a tests file, an invalid one makes the file refuse to load, so it
        // is never listed for acceptance.
        let dir = temp();
        project_file(dir.path(), "[[template]]\nid = \"x\"\nprogram = \"make\"\nargs = [\"{nope}\"]\n");
        assert!(crate::test_discipline::config::load(dir.path(), None).unwrap_err().to_string().contains("without declaring"));
        // [build_broker] no longer makes the file unreadable (task 84).
        project_file(dir.path(), "[build_broker]\nenabled = true\nmode = \"snapshot\"\n");
        assert!(crate::test_discipline::config::load(dir.path(), None).is_ok());
    }

    struct Guard<F: FnMut()>(F);
    impl<F: FnMut()> Drop for Guard<F> {
        fn drop(&mut self) {
            (self.0)();
        }
    }
    fn scopeguard<F: FnMut()>(f: F) -> Guard<F> {
        Guard(f)
    }

    /// The C compiler this machine has for the fixture build, or None.
    fn c_toolchain() -> bool {
        let cmake = Command::new("cmake").arg("--version").output().is_ok_and(|output| output.status.success());
        if !cmake {
            return false;
        }
        #[cfg(windows)]
        {
            msvc_environment().is_some()
        }
        #[cfg(not(windows))]
        {
            Command::new("cc").arg("--version").output().is_ok_and(|output| output.status.success())
        }
    }

    /// Task 85 (acceptance): a CMake project that is not Zerus configures,
    /// builds and tests through the templates, and its CTest report gives one
    /// result per test. Without CMake and a C compiler here it says so and
    /// checks nothing else.
    #[test]
    fn a_plain_cmake_project_builds_and_tests_through_the_templates() {
        if !c_toolchain() {
            eprintln!("skipped: no cmake or no C compiler on this machine");
            return;
        }
        let dir = temp();
        std::fs::write(dir.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.21)\nproject(fixture C)\nenable_testing()\nadd_executable(fixture main.c)\n\
             add_test(NAME passes COMMAND fixture 0)\nadd_test(NAME fails COMMAND fixture 1)\n").unwrap();
        std::fs::write(dir.path().join("main.c"), "#include <stdlib.h>\nint main(int argc, char **argv) { return argc > 1 ? atoi(argv[1]) : 0; }\n").unwrap();
        let timeout = Duration::from_secs(600);
        let run = |id: &str, args: Value| {
            let planned = plan(id, &args, dir.path()).unwrap();
            execute(&planned, dir.path(), timeout, &[])
        };
        let configured = run("cmake-configure", json!({"config":"Debug"}));
        assert_eq!(configured.exit, Some(0), "{}", configured.tail);
        let built = run("cmake-build", json!({"config":"Debug"}));
        assert_eq!(built.exit, Some(0), "{}", built.tail);
        let tested = run("ctest", json!({"config":"Debug"}));
        assert_ne!(tested.exit, Some(0), "one test fails: {}", tested.tail);
        let report = tested.report.expect("the CTest report");
        assert_eq!((report.passed, report.failed), (1, 1), "{report:?}");
        assert_eq!(tested.failures, ["fails"]);
        let only = run("ctest", json!({"config":"Debug","tests":["passes"]}));
        assert_eq!(only.exit, Some(0), "{}", only.tail);
        assert_eq!(only.report.unwrap().total(), 1);
    }

    /// Task 85 (acceptance, Windows): an MSBuild solution builds through
    /// msbuild-build. The fixture's solution is CMake's Visual Studio
    /// generator output, so no project file is written by hand.
    #[cfg(windows)]
    #[test]
    fn an_msbuild_solution_builds_on_windows() {
        if !c_toolchain() || visual_studio_find(r"MSBuild\**\Bin\MSBuild.exe", "Microsoft.Component.MSBuild").is_none() {
            eprintln!("skipped: no cmake, Visual Studio or MSBuild on this machine");
            return;
        }
        // The generator names the installed Visual Studio: a runner image may
        // carry VS 2026 (18) and no 2022 (17).
        let Some(generator) = visual_studio_generator() else {
            eprintln!("skipped: vswhere names no Visual Studio version");
            return;
        };
        let dir = temp();
        std::fs::write(dir.path().join("CMakeLists.txt"), "cmake_minimum_required(VERSION 3.21)\nproject(fixture C)\nadd_executable(fixture main.c)\n").unwrap();
        std::fs::write(dir.path().join("main.c"), "int main(void) { return 0; }\n").unwrap();
        let timeout = Duration::from_secs(600);
        let configure = plan("cmake-configure", &json!({"build_dir":"vs","generator":generator}), dir.path()).unwrap();
        let configured = execute(&configure, dir.path(), timeout, &[]);
        assert_eq!(configured.exit, Some(0), "{}", configured.tail);
        // CMake writes fixture.slnx for Visual Studio 2026 and fixture.sln before it.
        let solution = ["vs/fixture.slnx", "vs/fixture.sln"].into_iter().find(|name| dir.path().join(name).is_file()).expect("CMake wrote no solution");
        let build = plan("msbuild-build", &json!({"project":solution,"config":"Debug","platform":"x64"}), dir.path()).unwrap();
        let built = execute(&build, dir.path(), timeout, &[]);
        assert_eq!(built.exit, Some(0), "{}", built.tail);
        assert!(dir.path().join("vs").join("Debug").join("fixture.exe").is_file());
    }

    /// CMake's generator for the newest installed Visual Studio that this
    /// CMake knows: vswhere's major versions, named as CMake names them
    /// ("Visual Studio 17 2022", "Visual Studio 18 2026"), and kept only when
    /// `cmake --help` lists that generator.
    #[cfg(windows)]
    fn visual_studio_generator() -> Option<String> {
        let program_files = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        let vswhere = Path::new(&program_files).join(r"Microsoft Visual Studio\Installer\vswhere.exe");
        let output = Command::new(&vswhere).args(["-products", "*", "-property", "installationVersion"]).output().ok()?;
        let mut majors: Vec<u32> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().split('.').next()?.parse().ok())
            .collect();
        majors.sort_unstable_by(|a, b| b.cmp(a));
        let help = Command::new("cmake").arg("--help").output().ok()?;
        let known = String::from_utf8_lossy(&help.stdout).into_owned();
        majors.into_iter().find_map(|major| {
            let year = match major {
                16 => 2019,
                17 => 2022,
                18 => 2026,
                _ => return None,
            };
            let name = format!("Visual Studio {major} {year}");
            known.contains(&name).then_some(name)
        })
    }

    #[cfg(not(windows))]
    fn visual_studio_generator() -> Option<String> {
        None
    }

    #[test]
    fn tauri_templates_carry_the_development_identity_and_never_the_switch() {
        let dir = temp();
        for (template, action) in [("tauri-build", "build"), ("tauri-dev", "dev"), ("tauri-launch", "launch")] {
            let planned = plan(template, &json!({}), dir.path()).unwrap();
            assert_eq!(planned.program, "python", "{template}");
            assert_eq!(planned.argv, ["scripts/tauri_template.py", action], "{template}");
            assert!(!planned.argv.iter().any(|arg| arg.contains("switch")), "{template}");
            let env = template_environment(&planned, dir.path());
            assert!(env.contains(&("ZERUS_TAURI_ACTION".to_owned(), action.to_owned())), "{template}: {env:?}");
            assert!(env.contains(&("ZERUS_TAURI_IDENTITY".to_owned(), "development".to_owned())), "{template}: {env:?}");
        }
        // Only the release candidate/package leg names the switch identity.
        assert!(builtin("release-candidate").is_some());
        assert!(builtin("tauri-build").is_some());
    }

    #[test]
    fn python_and_cython_templates_take_typed_arguments() {
        let dir = temp();
        std::fs::create_dir_all(dir.path().join(".venv/Scripts")).unwrap();
        std::fs::write(dir.path().join(".venv/Scripts/python.exe"), "").unwrap();
        std::fs::write(dir.path().join("a.pyx"), "cimport helper\n").unwrap();
        let py = plan("py-test", &json!({"filter":"slow and not x","tests":["test_a.py::test_x"]}), dir.path()).unwrap();
        assert!(py.program.contains(".venv"));
        assert!(py.argv.contains(&"-k=slow and not x".to_owned()));
        assert!(py.argv.contains(&"test_a.py::test_x".to_owned()));
        assert!(py.argv.contains(&"--junitxml=.zerus-team/python/junit.xml".to_owned()));
        assert!(!py.argv.iter().any(|arg| arg.starts_with("-m=")));
        // Task 137: the test files an affected set chose, as paths.
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("tests/test_demo.py"), "").unwrap();
        let files = plan("py-test", &json!({"paths":["tests/test_demo.py"]}), dir.path()).unwrap();
        assert!(files.argv.contains(&"tests/test_demo.py".to_owned()), "{:?}", files.argv);
        assert!(plan("py-test", &json!({"paths":["tests/missing.py"]}), dir.path()).is_err());
        let cy = plan("cython-build", &json!({"sources":["a.pyx"]}), dir.path()).unwrap();
        assert_eq!(cy.argv, ["setup.py", "build_ext", "--inplace", "a.pyx"]);
        let uvdir = temp();
        std::fs::write(uvdir.path().join("uv.lock"), "").unwrap();
        let uv = plan("py-test", &json!({}), uvdir.path()).unwrap();
        assert_eq!(uv.program, "uv");
        assert_eq!(&uv.argv[..2], ["run", "python"]);
        let empty = temp();
        assert!(plan("py-test", &json!({}), empty.path()).is_err());
    }

    /// Task 153: a Python project in a subfolder (a monorepo, a provider's
    /// root): both templates take its environment and setup.py, run there,
    /// and keep every path the member's.
    #[test]
    fn a_nested_python_project_runs_through_both_templates_inside_the_folder() {
        let dir = temp();
        let project = dir.path().join("services").join("app");
        std::fs::create_dir_all(project.join(".venv/Scripts")).unwrap();
        std::fs::write(project.join(".venv/Scripts/python.exe"), "").unwrap();
        std::fs::create_dir_all(project.join("tests")).unwrap();
        std::fs::write(project.join("tests/test_app.py"), "").unwrap();
        std::fs::write(project.join("setup.py"), "").unwrap();
        std::fs::write(project.join("ext.pyx"), "").unwrap();
        // Without the project the member's root has no environment.
        let refused = plan("py-test", &json!({"paths":["services/app/tests/test_app.py"]}), dir.path()).unwrap_err().to_string();
        assert!(refused.contains("No project Python environment"), "{refused}");
        let py = plan("py-test", &json!({"project":"services/app","paths":["services/app/tests/test_app.py"],"tests":["test_app.py::test_one"]}), dir.path()).unwrap();
        assert_eq!(py.cwd.as_deref(), Some("services/app"));
        assert!(Path::new(&py.program).is_absolute() && Path::new(&py.program).starts_with(&project), "{}", py.program);
        let junit = format!("--junitxml={}", dir.path().join(".zerus-team/python/junit.xml").display());
        assert!(py.argv.contains(&junit), "{:?}", py.argv);
        assert!(py.argv.contains(&dir.path().join("services/app/tests/test_app.py").display().to_string()), "{:?}", py.argv);
        // A node id names the test from the project's folder, as pytest reads it there.
        assert!(py.argv.contains(&"test_app.py::test_one".to_owned()));
        let cy = plan("cython-build", &json!({"project":"services/app","sources":["services/app/ext.pyx"]}), dir.path()).unwrap();
        assert_eq!(cy.cwd.as_deref(), Some("services/app"));
        assert!(Path::new(&cy.program).starts_with(&project));
        assert_eq!(&cy.argv[..3], ["setup.py", "build_ext", "--inplace"]);
        assert_eq!(cy.argv[3], dir.path().join("services/app/ext.pyx").display().to_string());
        // The member's root and "." are the folder itself, as before.
        std::fs::create_dir_all(dir.path().join(".venv/Scripts")).unwrap();
        std::fs::write(dir.path().join(".venv/Scripts/python.exe"), "").unwrap();
        let root = plan("py-test", &json!({"project":"."}), dir.path()).unwrap();
        assert_eq!((root.cwd, root.program.as_str()), (None, ".venv/Scripts/python.exe"));
        assert!(root.argv.contains(&"--junitxml=.zerus-team/python/junit.xml".to_owned()));
        // Confined to the member's folder.
        for outside in ["../app", "/tmp/app", "C:/app", "services/missing", "services/app/setup.py"] {
            assert!(plan("cython-build", &json!({"project": outside}), dir.path()).is_err(), "{outside}");
        }
        #[cfg(unix)]
        {
            let elsewhere = temp();
            std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("linked")).unwrap();
            let error = plan("cython-build", &json!({"project":"linked"}), dir.path()).unwrap_err().to_string();
            assert!(error.contains("leads outside your folder"), "{error}");
        }
    }

    /// Task 153: a plan's folder is where its commands run.
    #[test]
    fn a_plan_runs_in_its_project_folder() {
        let dir = temp();
        std::fs::create_dir_all(dir.path().join("services").join("app")).unwrap();
        let mut plan = shell_plan("cd", "pwd");
        plan.cwd = Some("services/app".into());
        let outcome = execute(&plan, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(0), "{}", outcome.tail);
        let tail = outcome.tail.replace('\\', "/");
        assert!(tail.trim_end().ends_with("services/app"), "{tail}");
    }

    /// Task 87/210: the project and the three `.zerus-team` paths are absolute:
    /// Build.bat and UnrealEditor-Cmd resolve after changing into the engine, so
    /// a member-relative path finds nothing.
    #[test]
    fn unreal_templates_pass_absolute_paths() {
        let dir = temp();
        std::fs::write(dir.path().join("Game.uproject"), "{}").unwrap();
        let project = dir.path().join("Game.uproject").display().to_string();
        let build = plan("ue-build", &json!({"engine":"UE_5.5","project":"Game.uproject","target":"Game","platform":"Win64","config":"Development"}), dir.path()).unwrap();
        assert!(build.program.ends_with("Engine/Build/BatchFiles/Build.bat"));
        assert!(build.argv.contains(&format!("-Project={project}")), "{:?}", build.argv);
        let expected: Vec<String> = vec![project.clone(), "Game".into(), "Win64".into(), "Development".into(), format!("-Project={project}")];
        assert_eq!(build.argv, expected);
        let engine = fake_engine("UnrealEditor-Cmd.exe");
        let root = engine.path().to_string_lossy().into_owned();
        let test = plan("ue-test", &json!({"engine":root,"project":"Game.uproject","filters":"Project.Game"}), dir.path()).unwrap();
        assert!(test.program.ends_with("Engine/Binaries/Win64/UnrealEditor-Cmd.exe"), "{}", test.program);
        assert_eq!(test.argv[0], project);
        assert!(test.argv.contains(&"-ExecCmds=Automation RunTests Project.Game;Quit".to_owned()));
        assert!(test.argv.contains(&"-nullrhi".to_owned()) && test.argv.contains(&"-unattended".to_owned()));
        let ue = dir.path().join(".zerus-team").join("ue");
        assert!(test.argv.contains(&format!("-saveddir={}", ue.join("Saved").display())), "{:?}", test.argv);
        assert!(test.argv.contains(&format!("-abslog={}", ue.join("Unreal.log").display())), "{:?}", test.argv);
        assert!(test.argv.contains(&format!("-ReportExportPath={}", dir.path().join(".zerus-team").join("results").join("unreal").display())), "{:?}", test.argv);
        assert!(plan("ue-build", &json!({"engine":"UE_5.5","project":"Game.uproject","target":"Game"}), dir.path()).is_ok());
        // A shell-style path is refused as before.
        let error = plan("ue-build", &json!({"engine":"UE_5.5","project":"../Game.uproject","target":"Game"}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("stays inside your folder"), "{error}");
    }

    /// Task 210: UnrealEditor-Cmd writes its run to `-abslog`, so a failed
    /// ue-test reports that log's tail; a passing one never shows a stale log.
    #[test]
    fn a_failed_ue_test_reports_its_unreal_log() {
        let dir = temp();
        let ue = dir.path().join(".zerus-team").join("ue");
        std::fs::create_dir_all(&ue).unwrap();
        std::fs::write(ue.join("Unreal.log"), "STALE-LOG-LINE\n").unwrap();
        let mut passing = shell_plan("exit 0", "exit 0");
        passing.template = "ue-test".into();
        let outcome = execute(&passing, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(0), "{outcome:?}");
        assert!(!outcome.tail.contains("STALE-LOG-LINE"), "{}", outcome.tail);
        let (windows, unix) = (
            "echo LogAutomation: Error: Test Game.Core failed > .zerus-team\\ue\\Unreal.log & exit /b 1",
            "echo 'LogAutomation: Error: Test Game.Core failed' > .zerus-team/ue/Unreal.log; exit 1",
        );
        let mut failing = shell_plan(windows, unix);
        failing.template = "ue-test".into();
        let outcome = execute(&failing, dir.path(), Duration::from_secs(60), &[]);
        assert_eq!(outcome.exit, Some(1), "{outcome:?}");
        assert!(outcome.tail.contains("Test Game.Core failed") && outcome.tail.contains("Unreal.log"), "{}", outcome.tail);
    }

    /// An engine folder holding the named editor.
    fn fake_engine(editor: &str) -> tempfile::TempDir {
        let engine = tempfile::tempdir().unwrap();
        let bin = engine.path().join("Engine").join("Binaries").join("Win64");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(editor), "").unwrap();
        engine
    }

    /// Task 90: -ExecCmds takes one automation filter, and the engine holds the
    /// editor (UE4Editor-Cmd.exe for a 4.x engine).
    #[test]
    fn ue_test_refuses_a_filter_that_adds_commands_and_an_engine_without_its_editor() {
        let dir = temp();
        std::fs::write(dir.path().join("Game.uproject"), "{}").unwrap();
        let engine = fake_engine("UnrealEditor-Cmd.exe");
        let root = engine.path().to_string_lossy().into_owned();
        for filters in ["Project.Game;Quit", "Project.Game\nquit", "A\rB"] {
            let error = plan("ue-test", &json!({"engine":root,"project":"Game.uproject","filters":filters}), dir.path()).unwrap_err().to_string();
            assert!(error.contains("one automation filter"), "{filters:?}: {error}");
        }
        assert!(plan("ue-test", &json!({"engine":root,"project":"Game.uproject","filters":"Project.Game"}), dir.path()).is_ok());
        let empty = tempfile::tempdir().unwrap();
        let error = plan("ue-test", &json!({"engine":empty.path().to_string_lossy(),"project":"Game.uproject"}), dir.path()).unwrap_err().to_string();
        assert!(error.contains("UnrealEditor-Cmd.exe") && error.contains("UE4Editor-Cmd.exe"), "{error}");
        let four = fake_engine("UE4Editor-Cmd.exe");
        let plan = plan("ue-test", &json!({"engine":four.path().to_string_lossy(),"project":"Game.uproject"}), dir.path()).unwrap();
        assert!(plan.program.ends_with("Engine/Binaries/Win64/UE4Editor-Cmd.exe"), "{}", plan.program);
    }

    /// reg.exe's own output, as printed on a machine with Unreal 5.3 installed
    /// by the launcher and a source build registered by its GUID.
    #[test]
    fn reg_exe_output_is_read_without_its_type_token_and_with_the_path_whole() {
        let launcher = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\EpicGames\\Unreal Engine\\5.3\r\n    InstalledDirectory    REG_SZ    D:\\EGS\\UE_5.3\r\n\r\n";
        assert_eq!(parse_reg_value(launcher, "InstalledDirectory").as_deref(), Some("D:\\EGS\\UE_5.3"));
        let spaced = "HKEY_LOCAL_MACHINE\\SOFTWARE\\EpicGames\\Unreal Engine\\5.3\r\n    InstalledDirectory    REG_SZ    C:\\Program Files\\Epic Games\\UE_5.3\r\n";
        assert_eq!(parse_reg_value(spaced, "InstalledDirectory").as_deref(), Some("C:\\Program Files\\Epic Games\\UE_5.3"));
        let source = "HKEY_CURRENT_USER\\Software\\Epic Games\\Unreal Engine\\Builds\r\n    {A1B2-C3}    REG_SZ    D:\\Source\\UE\r\n    {OTHER}    REG_SZ    E:\\Other\r\n";
        assert_eq!(parse_reg_value(source, "{A1B2-C3}").as_deref(), Some("D:\\Source\\UE"));
        assert_eq!(parse_reg_value("    InstalledDirectoryX    REG_SZ    D:\\X\r\n", "InstalledDirectory"), None);
        assert_eq!(parse_reg_value("    InstalledDirectory    REG_DWORD    0x1\r\n", "InstalledDirectory"), None);
        assert_eq!(parse_reg_value("ERROR: The system was unable to find the specified registry key or value.\r\n", "InstalledDirectory"), None);
    }

    /// Against the real registry with reg.exe: every launcher engine this computer
    /// lists gives its directory as plain data (no type token), and the ones still
    /// installed give a folder that exists (a version may stay registered after its
    /// folder is gone). Nothing to check where none is registered.
    #[cfg(windows)]
    #[test]
    fn the_registry_of_this_computer_gives_its_launcher_engines() {
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
        let output = Command::new(Path::new(&system).join("System32").join("reg.exe")).args(["query", "HKLM\\SOFTWARE\\EpicGames\\Unreal Engine"]).output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let versions: Vec<&str> = text.lines().filter_map(|line| line.trim().rsplit('\\').next()).filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')).collect();
        let (mut found, mut present) = (0, 0);
        for version in versions {
            let key = format!("SOFTWARE\\EpicGames\\Unreal Engine\\{version}");
            if let Some(dir) = SystemRegistry.value("HKLM", &key, "InstalledDirectory") {
                assert!(!dir.starts_with("REG_") && !dir.starts_with(char::is_whitespace), "{version}: {dir}");
                found += 1;
                present += usize::from(Path::new(&dir).is_dir());
            }
        }
        assert!(found == 0 || present > 0, "no registered engine folder exists ({found} registered)");
    }

    struct FakeRegistry(std::collections::BTreeMap<(String,String,String),String>);
    impl EngineRegistry for FakeRegistry {
        fn value(&self, root: &str, key: &str, name: &str) -> Option<String> {
            self.0.get(&(root.to_owned(), key.to_owned(), name.to_owned())).cloned()
        }
    }

    #[test]
    fn unreal_engine_comes_from_the_association_or_the_override() {
        let dir = temp();
        let project = dir.path().join("Game.uproject");
        std::fs::write(&project, r#"{"EngineAssociation":"5.5"}"#).unwrap();
        let launcher = FakeRegistry([(("HKLM".into(), "SOFTWARE\\EpicGames\\Unreal Engine\\5.5".into(), "InstalledDirectory".into()), "C:/Epic/UE_5.5".into())].into());
        assert_eq!(unreal_engine(&project, None, &launcher).unwrap(), "C:/Epic/UE_5.5");
        assert_eq!(unreal_engine(&project, Some("D:/UE"), &launcher).unwrap(), "D:/UE");
        std::fs::write(&project, r#"{"EngineAssociation":"{abc}"}"#).unwrap();
        let source = FakeRegistry([(("HKCU".into(), "Software\\Epic Games\\Unreal Engine\\Builds".into(), "{abc}".into()), "D:/Source/UE".into())].into());
        assert_eq!(unreal_engine(&project, None, &source).unwrap(), "D:/Source/UE");
        let error = unreal_engine(&project, None, &launcher).unwrap_err().to_string();
        assert!(error.contains("HKCU") && error.contains("ZERUS_UE_ENGINE"), "{error}");
    }
}




