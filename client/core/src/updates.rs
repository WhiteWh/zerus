//! Update catalogs and installation rules, ported from `UpdateController`.
//! All parsing and comparisons are pure; network, process and filesystem work
//! belongs to the Tauri update service.

use crate::text::Text;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;
use time::format_description::well_known::Iso8601;
use time::OffsetDateTime;

pub const CATALOG_LIMIT: usize = 256 * 1024;
pub const INSTALLER_RECORD_LIMIT: usize = 4096;
const PACKAGES: [&str; 4] = [
    "zerus",
    "zerus-git",
    "zerus-ade-bin",
    "zerus-ade-nightly-bin",
];
const CASKS: [&str; 3] = ["zerus", "zerus@nightly", "zerus@dev"];
const WINGET: &str = "ufna.Zerus";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Nightly,
    Dev,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
            Self::Dev => "dev",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stable" => Some(Self::Stable),
            "nightly" => Some(Self::Nightly),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    pub valid: bool,
    pub version: String,
    pub release_url: String,
    pub packages: BTreeMap<String, String>,
    pub release_id: u64,
    pub build_number: u64,
    pub source_commit: String,
    pub source_url: String,
    pub package_url: String,
    pub macos_zip_url: String,
    pub macos_dmg_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub kind: String,
    pub package: String,
    pub version: String,
    pub arch: bool,
    pub helper: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationGuide {
    pub ownership: Text,
    /// The update command line with its kind; the copy action copies the bare command.
    pub command: Option<Text>,
    pub copy_name: Option<Text>,
    pub instructions: Option<Text>,
}

fn integer(value: &Value, low: u64, high: u64) -> Option<u64> {
    let number = value.as_f64()?;
    (number.is_finite() && number.fract() == 0.0 && number >= low as f64 && number <= high as f64)
        .then_some(number as u64)
}

fn hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    [8, 4, 4, 4, 12].as_slice()
        == parts
            .iter()
            .map(|part| part.len())
            .collect::<Vec<_>>()
            .as_slice()
        && parts.iter().all(|part| {
            part.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
}

fn safe(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 180
        && text
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'+' | b'-'))
        && !text.contains("..")
}

fn stable_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()))
}

fn nightly_parts(text: &str) -> Option<(String, u64)> {
    let (base, run) = text.split_once(".n")?;
    if run.is_empty() || run.len() > 15 || run.bytes().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    let (revision, commit) = base.split_once(".g")?;
    let (product, revision) = revision.split_once(".r")?;
    (stable_version(product)
        && !revision.is_empty()
        && revision.bytes().all(|c| c.is_ascii_digit())
        && commit.len() == 7
        && commit
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
    .then_some((product.to_owned(), run.parse().ok()?))
}

fn dev_parts(text: &str) -> Option<(String, u64)> {
    let (base, number) = text.split_once("-dev.")?;
    (stable_version(base)
        && !number.is_empty()
        && number.len() <= 15
        && number
            .bytes()
            .next()
            .is_some_and(|c| (b'1'..=b'9').contains(&c))
        && number.bytes().all(|c| c.is_ascii_digit()))
    .then_some((base.to_owned(), number.parse().ok()?))
}

pub fn release_channel(version: &str) -> Option<Channel> {
    if dev_parts(version).is_some() {
        Some(Channel::Dev)
    } else if nightly_parts(version).is_some() {
        Some(Channel::Nightly)
    } else if stable_version(version) {
        Some(Channel::Stable)
    } else {
        None
    }
}

fn timestamp(text: &str) -> bool {
    if text.len() > 40
        || !(text.ends_with('Z')
            || (text.len() >= 6
                && matches!(text.as_bytes()[text.len() - 6], b'+' | b'-')
                && text.as_bytes()[text.len() - 3] == b':'))
    {
        return false;
    }
    OffsetDateTime::parse(text, &Iso8601::DEFAULT).is_ok()
}

fn object<'a>(raw: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    raw.get(key)?.as_object()
}

fn string(raw: &Map<String, Value>, key: &str) -> String {
    raw.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn parse_macos(raw: &Map<String, Value>, version: &str, prefix: &str, feed: &mut Feed) -> bool {
    let Some(mac) = raw.get("macos") else {
        return true;
    };
    let Some(mac) = mac.as_object() else {
        return false;
    };
    let archive = format!("zerus-{version}-macos-arm64.zip");
    let image = format!("zerus-{version}-macos-arm64.dmg");
    if string(mac, "asset") != archive
        || string(mac, "minimum_system_version") != "14.0"
        || !matches(&string(mac, "team_id"), 10)
        || !stable_version(&string(mac, "qt_version"))
        || !uuid(&string(mac, "notarization_id"))
    {
        return false;
    }
    let has_dmg = mac.contains_key("dmg");
    if has_dmg {
        let Some(dmg) = object(mac, "dmg") else {
            return false;
        };
        if dmg.len() != 2 || string(dmg, "asset") != image || !uuid(&string(dmg, "notarization_id"))
        {
            return false;
        }
    }
    if mac.len() != if has_dmg { 7 } else { 6 } {
        return false;
    }
    let Some(downloads) = mac.get("downloads").and_then(Value::as_array) else {
        return false;
    };
    let names = if has_dmg {
        vec![archive.clone(), image.clone()]
    } else {
        vec![archive.clone()]
    };
    if downloads.len() != names.len() {
        return false;
    }
    for (at, row) in downloads.iter().enumerate() {
        let Some(row) = row.as_object() else {
            return false;
        };
        if row.len() != 4
            || string(row, "name") != names[at]
            || string(row, "url") != format!("{prefix}{}", names[at])
            || integer(
                row.get("size").unwrap_or(&Value::Null),
                1,
                512 * 1024 * 1024,
            )
            .is_none()
            || !hex(&string(row, "sha256"), 64)
        {
            return false;
        }
    }
    feed.macos_zip_url = format!("{prefix}{archive}");
    if has_dmg {
        feed.macos_dmg_url = format!("{prefix}{image}");
    }
    true
}

fn matches(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn package_version(text: &str) -> bool {
    let text = text.split_once(':').map_or(text, |(_, version)| version);
    let Some((prefix, release)) = text.rsplit_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.len() <= 120
        && prefix.bytes().next().is_some_and(|c| c.is_ascii_digit())
        && prefix
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'_' | b'~' | b':'))
        && !release.is_empty()
        && release
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()))
}

fn parse_dev(data: &[u8]) -> Feed {
    let mut feed = Feed::default();
    if data.len() > CATALOG_LIMIT {
        return feed;
    }
    let Ok(Value::Object(raw)) = serde_json::from_slice::<Value>(data) else {
        return feed;
    };
    if integer(raw.get("schemaVersion").unwrap_or(&Value::Null), 2, 2).is_none()
        || string(&raw, "product") != "zerus"
        || string(&raw, "platform") != "desktop"
        || string(&raw, "channel") != "dev"
        || string(&raw, "channelTag") != "dev"
        || raw.contains_key("sourceDirty")
        || integer(
            raw.get("buildId").unwrap_or(&Value::Null),
            1,
            9_007_199_254_740_991,
        )
        .is_none()
        || integer(
            raw.get("buildNumber").unwrap_or(&Value::Null),
            1,
            9_007_199_254_740_991,
        )
        .is_none()
    {
        return feed;
    }
    let id = integer(raw.get("buildId").unwrap_or(&Value::Null), 1, u64::MAX).unwrap();
    let number = integer(raw.get("buildNumber").unwrap_or(&Value::Null), 1, u64::MAX).unwrap();
    let commit = string(&raw, "sourceCommit");
    let version = string(&raw, "versionName");
    if !hex(&commit, 40)
        || version.len() > 120
        || !(stable_version(&version) || dev_parts(&version).is_some())
        || string(&raw, "buildUrl") != format!("https://github.com/ufna/zerus/actions/runs/{id}")
        || !timestamp(&string(&raw, "publishedAt"))
    {
        return feed;
    }
    let Some(artifacts) = raw.get("artifacts").and_then(Value::as_array) else {
        return feed;
    };
    if artifacts.is_empty() || artifacts.len() > 16 {
        return feed;
    }
    let mut seen = Vec::new();
    let mut sources = 0;
    let mut packages = 0;
    for value in artifacts {
        let Some(item) = value.as_object() else {
            return feed;
        };
        let name = string(item, "name");
        if !safe(&name)
            || seen.contains(&name)
            || string(item, "url") != format!("https://zerus.dev/downloads/dev-desktop-{id}/{name}")
            || !hex(&string(item, "sha256"), 64)
            || integer(
                item.get("size").unwrap_or(&Value::Null),
                1,
                512 * 1024 * 1024,
            )
            .is_none()
        {
            return feed;
        }
        seen.push(name.clone());
        match string(item, "kind").as_str() {
            "source" => {
                if string(item, "os") != "any"
                    || string(item, "arch") != "source"
                    || !name.ends_with("-source.tar.gz")
                {
                    return feed;
                }
                feed.source_url = string(item, "url");
                sources += 1;
            }
            "arch-package" => {
                if string(item, "os") != "linux"
                    || string(item, "arch") != "x86_64"
                    || !name.ends_with(".pkg.tar.zst")
                {
                    return feed;
                }
                feed.package_url = string(item, "url");
                packages += 1;
            }
            _ => return feed,
        }
    }
    if sources != 1 || packages != 1 {
        return feed;
    }
    if let Some((_, suffix)) = dev_parts(&version) {
        if suffix != number {
            return feed;
        }
    }
    if raw.contains_key("macos")
        && (dev_parts(&version).is_none()
            || !object(&raw, "macos").is_some_and(|mac| mac.contains_key("dmg")))
    {
        return feed;
    }
    if !parse_macos(
        &raw,
        &version,
        &format!("https://zerus.dev/downloads/dev-desktop-{id}/"),
        &mut feed,
    ) {
        return Feed::default();
    }
    feed.valid = true;
    feed.release_id = id;
    feed.build_number = number;
    feed.source_commit = commit;
    feed.release_url = string(&raw, "buildUrl");
    feed.version = if dev_parts(&version).is_some() {
        version
    } else {
        format!("{version}-dev.{number}")
    };
    feed
}

pub fn parse_feed(data: &[u8], channel: Channel) -> Feed {
    if channel == Channel::Dev {
        return parse_dev(data);
    }
    let mut feed = Feed::default();
    if data.len() > CATALOG_LIMIT {
        return feed;
    }
    let Ok(Value::Object(raw)) = serde_json::from_slice::<Value>(data) else {
        return feed;
    };
    if integer(raw.get("schemaVersion").unwrap_or(&Value::Null), 1, 1).is_none()
        || string(&raw, "product") != "zerus"
        || string(&raw, "platform") != "desktop"
        || string(&raw, "channel") != channel.as_str()
        || raw.contains_key("sourceDirty")
        || integer(
            raw.get("releaseId").unwrap_or(&Value::Null),
            1,
            9_007_199_254_740_991,
        )
        .is_none()
    {
        return feed;
    }
    let version = string(&raw, "versionName");
    let release = string(&raw, "releaseUrl");
    if version.len() > 120
        || !hex(&string(&raw, "sourceCommit"), 40)
        || (channel == Channel::Stable && !stable_version(&version))
        || (channel == Channel::Nightly && nightly_parts(&version).is_none())
    {
        return feed;
    }
    let Some(tag) = release.strip_prefix("https://github.com/ufna/zerus/releases/tag/") else {
        return feed;
    };
    if !safe(tag)
        || (channel == Channel::Stable && tag != format!("v{version}"))
        || (channel == Channel::Nightly && !tag.starts_with("nightly-"))
    {
        return feed;
    }
    if channel == Channel::Nightly
        && (tag.len() <= 8 || tag[8..].bytes().any(|c| !c.is_ascii_digit()))
    {
        return feed;
    }
    if !timestamp(&string(&raw, "publishedAt")) {
        return feed;
    }
    let Some(artifacts) = raw.get("artifacts").and_then(Value::as_array) else {
        return feed;
    };
    if artifacts.is_empty() || artifacts.len() > 16 {
        return feed;
    }
    let mut seen = Vec::new();
    let mut package = false;
    for value in artifacts {
        let Some(artifact) = value.as_object() else {
            return feed;
        };
        let name = string(artifact, "name");
        if !safe(&name)
            || seen.contains(&name)
            || string(artifact, "url") != format!("https://zerus.dev/downloads/{tag}/{name}")
            || !hex(&string(artifact, "sha256"), 64)
            || integer(
                artifact.get("size").unwrap_or(&Value::Null),
                1,
                512 * 1024 * 1024,
            )
            .is_none()
        {
            return feed;
        }
        seen.push(name.clone());
        match string(artifact, "kind").as_str() {
            "source" => {
                if string(artifact, "os") != "any"
                    || string(artifact, "arch") != "source"
                    || !name.ends_with("-source.tar.gz")
                {
                    return feed;
                }
            }
            "arch-package" => {
                if string(artifact, "os") != "linux"
                    || string(artifact, "arch") != "x86_64"
                    || !name.ends_with(".pkg.tar.zst")
                {
                    return feed;
                }
                package = true;
            }
            "linux-archive" => {
                if string(artifact, "os") != "linux"
                    || string(artifact, "arch") != "x86_64"
                    || !name.ends_with("-arch-x86_64.tar.gz")
                {
                    return feed;
                }
            }
            _ => return feed,
        }
    }
    if !package {
        return feed;
    }
    if let Some(packages) = raw.get("packages") {
        let Some(packages) = packages.as_object() else {
            return feed;
        };
        for (name, version) in packages {
            if !PACKAGES.contains(&name.as_str()) || !version.as_str().is_some_and(package_version)
            {
                return feed;
            }
        }
        feed.packages = packages
            .iter()
            .map(|(name, version)| (name.clone(), version.as_str().unwrap().to_owned()))
            .collect();
    }
    if !parse_macos(
        &raw,
        &version,
        &format!("https://github.com/ufna/zerus/releases/download/{tag}/"),
        &mut feed,
    ) {
        return Feed::default();
    }
    feed.version = version;
    feed.release_url = release;
    feed.source_commit = string(&raw, "sourceCommit");
    feed.valid = true;
    feed.release_id = integer(raw.get("releaseId").unwrap_or(&Value::Null), 1, u64::MAX).unwrap();
    feed
}

pub fn is_arch_linux(data: &[u8]) -> bool {
    if data.len() > 8192 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return false;
    };
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, mut value)) = line.split_once('=') else {
            continue;
        };
        if values.contains_key(key) {
            return false;
        }
        if !matches!(key, "ID" | "ID_LIKE") {
            continue;
        }
        if value.starts_with('"') || value.starts_with('\'') {
            if value.len() < 2 || !value.ends_with(&value[..1]) {
                return false;
            }
            value = &value[1..value.len() - 1];
        }
        if value.is_empty()
            || value.split(' ').any(|part| {
                part.is_empty()
                    || part.bytes().any(|c| {
                        !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
                    })
            })
        {
            return false;
        }
        values.insert(key, value);
    }
    values.get("ID") == Some(&"arch")
        || values
            .get("ID_LIKE")
            .is_some_and(|value| value.split(' ').any(|part| part == "arch"))
}

pub fn update_command(installation: &Installation, channel: Channel) -> String {
    if installation.kind == "portable" {
        return if installation.package == WINGET && channel == Channel::Stable {
            "winget upgrade --id ufna.Zerus --exact".to_owned()
        } else {
            String::new()
        };
    }
    if installation.kind == "homebrew-cask" {
        let wanted = match channel {
            Channel::Stable => "zerus",
            Channel::Nightly => "zerus@nightly",
            Channel::Dev => "zerus@dev",
        };
        if !CASKS.contains(&installation.package.as_str()) {
            return String::new();
        }
        return if wanted == installation.package {
            format!("brew upgrade --cask {wanted}")
        } else {
            format!(
                "brew uninstall --cask {} && brew install --cask ufna/zerus/{wanted}",
                installation.package
            )
        };
    }
    if channel == Channel::Dev || !matches!(installation.helper.as_str(), "yay" | "paru") {
        return String::new();
    }
    if installation.kind == "package" {
        if !PACKAGES.contains(&installation.package.as_str()) {
            return String::new();
        }
        return if installation.package == "zerus-git" {
            format!("{} -Syu --devel", installation.helper)
        } else {
            format!("{} -Syu {}", installation.helper, installation.package)
        };
    }
    if !installation.arch || installation.kind == "checking" {
        return String::new();
    }
    format!(
        "{} -Syu {}",
        installation.helper,
        if channel == Channel::Nightly {
            "zerus-ade-nightly-bin"
        } else {
            "zerus-ade-bin"
        }
    )
}

pub fn installation_guide(
    installation: &Installation,
    channel: Channel,
    feed: &Feed,
    app_version: &str,
) -> InstallationGuide {
    let version = || installation.version.clone();
    let mut guide = InstallationGuide {
        ownership: match installation.kind.as_str() {
        "package" => crate::text!("Package: %1\nInstalled package: %2", installation.package.clone(), if installation.version.is_empty() { crate::text!("unconfirmed") } else { crate::text::Text::verbatim(version()) }),
        "homebrew-cask" => crate::text!("Homebrew cask: %1\nInstalled version: %2", installation.package.clone(), version()),
        "macos-bundle" => crate::text!("macOS app installed from a download\nInstalled version: %1", version()),
        "source" => crate::text!("Local build in a Git checkout; update and rebuild deliberately."),
        "checking" => crate::text!("Checking installation ownership…"),
        "portable" if installation.package == WINGET => crate::text!("winget package %1\nPortable Windows build %2", WINGET, app_version.to_owned()),
        "portable" => crate::text!("Portable Windows build %1. Download newer builds from the release page.", app_version.to_owned()),
        "installer" => crate::text!("Windows build %1, installed for this user by the Zerus installer. It does not update itself yet; download newer builds from the release page.", app_version.to_owned()),
        _ => crate::text!("Local / manually installed build; package ownership unconfirmed."),
        }, ..Default::default()
    };
    if installation.arch {
        guide.ownership = crate::text!(
            "%1\n%2",
            guide.ownership,
            crate::text!("System: Arch-based Linux")
        );
    }
    if macos_installation(installation) {
        if !feed.valid {
            guide.instructions = Some(crate::text!(
                "Check for updates to see the available macOS build and update instructions."
            ));
            return guide;
        }
        if feed.macos_zip_url.is_empty() {
            guide.instructions = Some(crate::text!(
                "No macOS build is published in this channel yet. Choose another channel."
            ));
            return guide;
        }
        if installation.kind == "macos-bundle" {
            let first = if feed.macos_dmg_url.is_empty() {
                crate::text!("Download and extract the ZIP.")
            } else {
                crate::text!("Download and open the DMG.")
            };
            guide.instructions = Some(crate::text!("%1 %2", first, crate::text!("Quit Zerus from its menu, move the new Zerus app into Applications and confirm replacing it, then open Zerus again. Your agents and tmux sessions keep running.")));
            return guide;
        }
    }
    let value = update_command(installation, channel);
    if installation.kind == "homebrew-cask" {
        if value.is_empty() {
            return guide;
        }
        guide.command = Some(crate::text!("Homebrew: %1", value));
        guide.copy_name = Some(crate::text!("Copy Homebrew command"));
        guide.instructions = Some(crate::text!("Run this in Terminal. Homebrew replaces Zerus.app without quitting Zerus, agents or tmux; then choose Restart to finish update."));
        return guide;
    }
    if windows_build(installation) {
        if installation.package != WINGET {
            return guide;
        }
        if value.is_empty() {
            guide.instructions = Some(crate::text!("winget carries stable releases only. Choose the stable channel for its update command."));
            return guide;
        }
        guide.command = Some(crate::text!("winget: %1", value));
        guide.copy_name = Some(crate::text!("Copy winget command"));
        guide.instructions = Some(crate::text!("Quit Zerus from its notification area menu, then run this in PowerShell or Command Prompt. winget lists a release once its manifest is accepted, which can take a while after GitHub. Agents and local sessions keep running."));
        return guide;
    }
    let optional = installation.kind != "package";
    if !value.is_empty() {
        guide.command = Some(if optional {
            crate::text!("Optional AUR installation: %1", value)
        } else {
            crate::text!("Full system update: %1", value)
        });
        guide.copy_name = Some(if optional {
            crate::text!("Copy optional installation command")
        } else {
            crate::text!("Copy update command")
        });
        guide.instructions = Some(if optional {
            crate::text!("This installs an AUR package; the current local build remains separate. Review the full system upgrade and your executable path.")
        } else {
            crate::text!("Run this through your AUR helper and review the full system upgrade before confirming.")
        });
    } else if installation.arch {
        guide.instructions = Some(crate::text!("No supported update command is available for this installation. See the installation instructions."));
    }
    guide
}

pub fn running_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        ""
    }
}

pub fn installation_instructions_url(installation: &Installation, platform: &str) -> String {
    let fragment = match platform {
        "linux" => {
            if installation.arch {
                "arch-linux"
            } else {
                "local-installation"
            }
        }
        "macos" => {
            if installation.kind == "macos-bundle" {
                "macos-dmg"
            } else {
                "macos-homebrew"
            }
        }
        "windows" => "windows",
        _ => "",
    };
    format!(
        "https://github.com/ufna/zerus/blob/main/docs/installation.md{}",
        if fragment.is_empty() {
            String::new()
        } else {
            format!("#{fragment}")
        }
    )
}

pub fn plain_stable_is_newer(installed: &str, released: &str) -> bool {
    if !stable_version(installed) || !stable_version(released) {
        return false;
    }
    installed
        .split('.')
        .zip(released.split('.'))
        .map(|(local, available)| {
            let local = local.trim_start_matches('0');
            let available = available.trim_start_matches('0');
            local
                .len()
                .cmp(&available.len())
                .then_with(|| local.cmp(available))
        })
        .find(|order| !order.is_eq())
        .unwrap_or(std::cmp::Ordering::Equal)
        .is_lt()
}

pub fn winget_package(executable: &str) -> String {
    let native = executable.replace('\\', "/");
    let parts: Vec<&str> = native.split('/').filter(|part| !part.is_empty()).collect();
    for index in 2..parts.len() {
        if !parts[index - 2].eq_ignore_ascii_case("WinGet")
            || !parts[index - 1].eq_ignore_ascii_case("Packages")
        {
            continue;
        }
        let prefix = format!("{WINGET}_");
        if parts[index].len() > prefix.len()
            && parts[index]
                .to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
        {
            return WINGET.into();
        }
    }
    String::new()
}

pub fn installer_record(data: &[u8]) -> bool {
    if data.is_empty() || data.len() > INSTALLER_RECORD_LIMIT {
        return false;
    }
    let Ok(Value::Object(record)) = serde_json::from_slice::<Value>(data) else {
        return false;
    };
    integer(record.get("schema").unwrap_or(&Value::Null), 1, 1).is_some()
        && string(&record, "kind") == "installer"
        && string(&record, "product") == "Zerus"
}

pub fn installer_record_path(path: &Path) -> bool {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(INSTALLER_RECORD_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .is_ok()
        && installer_record(&bytes)
}

pub fn stable_has_windows_build(version: &str) -> bool {
    plain_stable_is_newer("0.42.2", version)
}

pub fn portable_update_available(app_version: &str, feed: &Feed) -> bool {
    if !feed.valid || feed.build_number > 0 {
        return false;
    }
    if stable_version(&feed.version) {
        return stable_has_windows_build(&feed.version)
            && plain_stable_is_newer(app_version, &feed.version);
    }
    nightly_parts(&feed.version)
        .is_some_and(|(product, _)| plain_stable_is_newer(app_version, &product))
}

pub fn dev_build_is_newer(feed: &Feed, installed_build: u64, installed_commit: &str) -> bool {
    feed.valid
        && feed.build_number > 0
        && feed.source_commit != installed_commit
        && (installed_build == 0 || feed.build_number > installed_build)
}

pub fn automatic_interval(channel: Channel) -> u64 {
    if channel == Channel::Dev {
        15 * 60
    } else {
        24 * 60 * 60
    }
}

pub fn is_update_token(token: &str) -> bool {
    let Some((prefix, number)) = token.rsplit_once(':') else {
        return false;
    };
    matches!(
        prefix,
        "zerus-update:v1:stable" | "zerus-update:v1:nightly" | "zerus-update:v2:dev"
    ) && !number.is_empty()
        && number.len() <= 16
        && number
            .bytes()
            .next()
            .is_some_and(|c| (b'1'..=b'9').contains(&c))
        && number.bytes().all(|c| c.is_ascii_digit())
}

pub fn update_token(channel: Channel, feed: &Feed, available: bool) -> String {
    if !available || !feed.valid {
        return String::new();
    }
    let schema = if channel == Channel::Dev { "v2" } else { "v1" };
    format!(
        "zerus-update:{schema}:{}:{}",
        channel.as_str(),
        feed.release_id
    )
}

pub fn download_url(installation: &Installation, feed: &Feed, platform: &str) -> String {
    if !feed.valid || windows_build(installation) {
        return String::new();
    }
    if platform == "macos" && !feed.macos_zip_url.is_empty() {
        return if feed.macos_dmg_url.is_empty() {
            feed.macos_zip_url.clone()
        } else {
            feed.macos_dmg_url.clone()
        };
    }
    if feed.build_number > 0 && !macos_installation(installation) {
        return if installation.arch {
            feed.package_url.clone()
        } else {
            feed.source_url.clone()
        };
    }
    String::new()
}

pub fn macos_installation(installation: &Installation) -> bool {
    matches!(installation.kind.as_str(), "homebrew-cask" | "macos-bundle")
}

pub fn windows_build(installation: &Installation) -> bool {
    matches!(installation.kind.as_str(), "portable" | "installer")
}

pub fn macos_update_available(installation: &Installation, feed: &Feed) -> bool {
    if !feed.valid || feed.macos_zip_url.is_empty() || !macos_installation(installation) {
        return false;
    }
    let Some(channel) = release_channel(&feed.version) else {
        return false;
    };
    if Some(channel) != release_channel(&installation.version) {
        return false;
    }
    match channel {
        Channel::Nightly => nightly_is_newer(&installation.version, &feed.version),
        Channel::Dev => dev_parts(&feed.version)
            .zip(dev_parts(&installation.version))
            .is_some_and(|(available, local)| available.1 > local.1),
        Channel::Stable => plain_stable_is_newer(&installation.version, &feed.version),
    }
}

pub fn nightly_is_newer(installed: &str, released: &str) -> bool {
    nightly_parts(installed)
        .zip(nightly_parts(released))
        .is_some_and(|(local, available)| available.1 > local.1)
}

pub fn prefers_nightly(installation: &Installation) -> bool {
    installation.package == "zerus-ade-nightly-bin"
        || (installation.kind == "homebrew-cask" && installation.package == "zerus@nightly")
}

pub fn preferred_channel(installation: &Installation) -> Option<Channel> {
    if macos_installation(installation) {
        return release_channel(&installation.version);
    }
    Some(if prefers_nightly(installation) {
        Channel::Nightly
    } else {
        Channel::Stable
    })
}

pub fn comparison_message(
    installation: &Installation,
    feed: &Feed,
    app_version: &str,
    comparison: i32,
    compared: bool,
    platform: &str,
) -> Text {
    if !feed.valid {
        return crate::text!("No verified release catalog yet.");
    }
    if windows_build(installation) {
        if feed.build_number > 0 {
            return crate::text!("Dev builds do not include a Windows build yet. Choose the stable or nightly channel to hear about newer Windows builds.");
        }
        if stable_version(&feed.version) {
            if portable_update_available(app_version, feed) {
                return if installation.package == WINGET {
                    crate::text!(
                        "Zerus %1 is available. Quit Zerus, then update it with winget.",
                        feed.version.clone()
                    )
                } else {
                    crate::text!(
                        "Zerus %1 is available. Download its Windows zip from the release page.",
                        feed.version.clone()
                    )
                };
            }
            if !stable_has_windows_build(&feed.version)
                && plain_stable_is_newer(app_version, &feed.version)
            {
                return crate::text!("Zerus %1 has no Windows build; stable releases after it carry one. Until then, choose the nightly channel to hear about newer Windows builds.", feed.version.clone());
            }
            return if installation.kind == "installer" {
                crate::text!("This Windows build does not update itself yet, and no newer stable release is published.")
            } else {
                crate::text!("Portable Windows build: there is no self-update, and no newer stable release is published.")
            };
        }
        if portable_update_available(app_version, feed) {
            return crate::text!(
                "Zerus nightly %1 is available. Download its Windows zip from the release page.",
                feed.version.clone()
            );
        }
        return if installation.kind == "installer" {
            crate::text!("This Windows build does not update itself yet, and nightly builds of the same version cannot be compared. The release page has the newest Windows zip.")
        } else {
            crate::text!("Portable Windows build: there is no self-update, and nightly builds of the same version cannot be compared. The release page has the newest Windows zip.")
        };
    }
    if macos_installation(installation) {
        if feed.macos_zip_url.is_empty() {
            return crate::text!(
                "No macOS build is published in this channel yet. Choose another channel."
            );
        }
        let channel = release_channel(&feed.version);
        let installed = release_channel(&installation.version);
        if channel != installed {
            return if installation.kind == "homebrew-cask" {
                crate::text!(
                    "This installation follows %1 builds. To switch to %2, run: %3",
                    installed.map_or("", Channel::as_str),
                    channel.map_or("", Channel::as_str),
                    update_command(installation, channel.unwrap_or_default())
                )
            } else {
                crate::text!("This installation follows %1 builds. Download the %2 macOS app to switch channels.", installed.map_or("", Channel::as_str), channel.map_or("", Channel::as_str))
            };
        }
        if !macos_update_available(installation, feed) {
            return crate::text!("Installed macOS build is at least as new as this release.");
        }
        return if installation.kind == "homebrew-cask" {
            crate::text!(
                "Update available. Run %1; the Homebrew tap may still be propagating.",
                update_command(installation, channel.unwrap_or_default())
            )
        } else {
            crate::text!(
                "Update available. Download the macOS app and replace Zerus in Applications."
            )
        };
    }
    if feed.build_number > 0 {
        return crate::text!("Verified main build %1 is published. Use its source or platform package to update this installation.", feed.build_number.to_string());
    }
    if platform == "macos" {
        if feed.macos_zip_url.is_empty() {
            return crate::text!("No macOS build is published in this channel yet. This local build can be updated from source.");
        }
        let cask = if nightly_parts(&feed.version).is_some() {
            "brew install --cask ufna/zerus/zerus@nightly"
        } else {
            "brew install --cask ufna/zerus/zerus"
        };
        if plain_stable_is_newer(app_version, &feed.version) {
            return crate::text!("Update available for this local build. Rebuild it, or install the Homebrew cask: %1", cask);
        }
        return crate::text!("Local build: release freshness is unconfirmed. Rebuild deliberately, or install the Homebrew cask: %1", cask);
    }
    if installation.kind == "package" {
        if installation.package == "zerus-git" {
            return crate::text!("VCS package: check main through your AUR helper with --devel.");
        }
        if !compared {
            return crate::text!("No matching package version comparison available.");
        }
        return if comparison > 0 {
            crate::text!("Update available. AUR publication may still be propagating.")
        } else {
            crate::text!("Installed package is at least as new as this release.")
        };
    }
    if plain_stable_is_newer(app_version, &feed.version) {
        return crate::text!("Update available for this local build.");
    }
    crate::text!("Local build: release freshness is unconfirmed. Update or rebuild deliberately.")
}

fn bundle_values(info_plist: &Path) -> BTreeMap<String, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    let Ok(file) = std::fs::File::open(info_plist) else {
        return BTreeMap::new();
    };
    if file.take(256 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 256 * 1024 {
        return BTreeMap::new();
    }
    let Ok(value) = plist::from_reader::<_, plist::Value>(std::io::Cursor::new(bytes)) else {
        return BTreeMap::new();
    };
    value
        .as_dictionary()
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.as_string().unwrap_or("").to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn bundle_directory(executable: &Path) -> Option<&Path> {
    let contents = executable.parent()?.parent()?;
    (executable.file_name()? == "hgs-tray"
        && executable.parent()?.file_name()? == "MacOS"
        && contents.file_name()? == "Contents")
        .then_some(contents)
}

fn product_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    (2..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()))
}

pub fn bundle_release(info_plist: &Path) -> String {
    let values = bundle_values(info_plist);
    let release = values.get("ZerusReleaseVersion").map_or("", String::as_str);
    if release_channel(release).is_some() {
        return release.to_owned();
    }
    values
        .get("CFBundleShortVersionString")
        .filter(|version| product_version(version))
        .cloned()
        .unwrap_or_default()
}

pub fn packaged_bundle_release(info_plist: &Path) -> String {
    bundle_values(info_plist)
        .get("ZerusReleaseVersion")
        .filter(|release| release_channel(release).is_some())
        .cloned()
        .unwrap_or_default()
}

pub fn macos_bundle_installation(executable: &Path) -> Installation {
    let mut installation = Installation::default();
    let Some(contents) = bundle_directory(executable) else {
        return installation;
    };
    let Some(bundle) = contents.parent() else {
        return installation;
    };
    if bundle.file_name().is_none_or(|name| name != "Zerus.app") {
        return installation;
    }
    let values = bundle_values(&contents.join("Info.plist"));
    let text = |key: &str| values.get(key).map_or("", String::as_str);
    let version = text("CFBundleShortVersionString");
    let release = text("ZerusReleaseVersion");
    if text("CFBundleIdentifier") != "com.hgdev.hgs-tray"
        || text("CFBundleExecutable") != "hgs-tray"
        || text("CFBundleName") != "Zerus"
        || !stable_version(version)
        || release_channel(release).is_none()
    {
        return installation;
    }
    let base = release
        .split_once("-dev.")
        .map_or(release, |(base, _)| base);
    if release != version
        && !release.starts_with(&format!("{version}.r"))
        && !(dev_parts(release).is_some() && plain_stable_is_newer(version, base))
    {
        return installation;
    }
    installation.kind = "macos-bundle".into();
    installation.version = release.to_owned();
    installation
}

pub fn homebrew_installation(executable: &Path, prefixes: &[&Path]) -> Installation {
    let mut result = Installation::default();
    let Some(contents) = bundle_directory(executable) else {
        return result;
    };
    let Some(bundle) = contents.parent() else {
        return result;
    };
    if bundle.file_name().is_none_or(|name| name != "Zerus.app") {
        return result;
    }
    let Ok(canonical_bundle) = bundle.canonicalize() else {
        return result;
    };
    for prefix in prefixes {
        let mut owners = Vec::new();
        let mut versions = Vec::new();
        for (cask, channel) in [
            ("zerus", Channel::Stable),
            ("zerus@nightly", Channel::Nightly),
            ("zerus@dev", Channel::Dev),
        ] {
            let room = prefix.join("Caskroom").join(cask);
            if !room.is_dir() {
                continue;
            }
            owners.push(cask);
            let Ok(entries) = std::fs::read_dir(room) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && release_channel(&entry.file_name().to_string_lossy()) == Some(channel)
                {
                    versions.push(entry.file_name());
                }
            }
        }
        if owners.is_empty() {
            continue;
        }
        if owners.len() == 1 && versions.len() == 1 {
            let link = prefix
                .join("Caskroom")
                .join(owners[0])
                .join(&versions[0])
                .join("Zerus.app");
            if link
                .symlink_metadata()
                .is_ok_and(|meta| meta.file_type().is_symlink())
                && link
                    .canonicalize()
                    .is_ok_and(|path| path == canonical_bundle)
            {
                result.kind = "homebrew-cask".into();
                result.package = owners[0].into();
                result.version = versions[0].to_string_lossy().into_owned();
            }
        }
        return result;
    }
    result
}

pub fn bundle_replaced(executable: &Path, started_release: &str) -> bool {
    if executable
        .parent()
        .is_none_or(|parent| parent.file_name().is_none_or(|name| name != "MacOS"))
        || started_release.is_empty()
    {
        return false;
    }
    let Some(contents) = executable.parent().and_then(Path::parent) else {
        return false;
    };
    let current = bundle_release(&contents.join("Info.plist"));
    !current.is_empty() && current != started_release
}

pub fn packaged_bundle_replaced(executable: &Path, started_release: &str) -> bool {
    if started_release.is_empty() {
        return false;
    }
    let Some(contents) = executable.parent().and_then(Path::parent) else {
        return false;
    };
    let current = packaged_bundle_release(&contents.join("Info.plist"));
    !current.is_empty() && current != started_release
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_and_versions_match_qt_patterns() {
        assert!(stable_version("0.42.3"));
        assert!(!stable_version("0.42.3.r1.gabcdef0.n1"));
        assert!(nightly_parts("0.38.0.r10.gaaaaaaa.n5").is_some());
        assert!(dev_parts("0.42.1-dev.50").is_some());
        assert!(dev_parts("0.42.1-dev.050").is_none());
        assert_eq!(release_channel("0.42.1-dev.50"), Some(Channel::Dev));
        assert_eq!(
            release_channel("0.38.0.r10.gaaaaaaa.n5"),
            Some(Channel::Nightly)
        );
        assert!(plain_stable_is_newer("0.42.2", "0.42.3"));
        assert!(!plain_stable_is_newer("0.42.3", "0.42.2"));
        assert!(plain_stable_is_newer(
            "0.42.999999999999999999999999999999",
            "0.42.1000000000000000000000000000000"
        ));
        assert!(!plain_stable_is_newer("0.42.0003", "0.42.3"));
        assert!(nightly_is_newer(
            "0.38.0.r10.gaaaaaaa.n5",
            "0.38.0.r10.gaaaaaaa.n6"
        ));
        assert!(dev_build_is_newer(
            &Feed {
                valid: true,
                build_number: 50,
                source_commit: "a".repeat(40),
                ..Feed::default()
            },
            0,
            ""
        ));
    }

    #[test]
    fn os_release_matches_arch_only() {
        assert!(is_arch_linux(b"ID=arch\n"));
        assert!(is_arch_linux(b"ID=manjaro\nID_LIKE=arch\n"));
        assert!(!is_arch_linux(b"ID=ubuntu\n"));
        assert!(!is_arch_linux(b"ID=arch\nID=arch\n"));
    }
}
