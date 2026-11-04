//! Folder paths in the form of the computer they belong to, ported from the
//! Qt client's `MachinePlatform.h`.
//!
//! The local computer may be Windows; peers are POSIX, and the swarm catalog
//! accepts nothing else. A [`Style`] argument rather than `cfg`, so both forms
//! are tested on every platform. POSIX answers are exactly Qt's
//! (`QDir::cleanPath`, `==`, `section('/', -1)`); Windows stores Qt's form
//! (`C:/work`, `//server/share/dir`), compares with either separator and
//! ignoring case, and shows native separators.

use serde::{Deserialize, Serialize};

/// The form of a computer's folder paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Posix,
    Windows,
}

impl Default for Style {
    /// The platform this client was built for.
    fn default() -> Self {
        build()
    }
}

/// The platform this client was built for.
pub fn build() -> Style {
    if cfg!(windows) {
        Style::Windows
    } else {
        Style::Posix
    }
}

/// A peer is POSIX. The local computer follows its `metrics.platform`
/// (`windows`, `linux`, `macos`); before it reports one it is the platform
/// this client was built for.
pub fn of(platform: Option<&str>, local: bool) -> Style {
    if !local {
        return Style::Posix;
    }
    match platform {
        None | Some("") => build(),
        Some("windows") => Style::Windows,
        Some(_) => Style::Posix,
    }
}

fn separator(c: char) -> bool {
    c == '/' || c == '\\'
}

/// "X:\" or "X:/".
fn drive(path: &[char]) -> bool {
    path.len() >= 3 && path[1] == ':' && separator(path[2]) && path[0].is_ascii_alphabetic()
}

/// The end of "\\server" (two separators, then a name), or 0.
fn server(path: &[char]) -> usize {
    if path.len() < 3 || !separator(path[0]) || !separator(path[1]) || separator(path[2]) {
        return 0;
    }
    let mut end = 3;
    while end < path.len() && !separator(path[end]) {
        end += 1;
    }
    end
}

/// The end of "\\server\share", or 0 without a share name.
fn share(path: &[char]) -> usize {
    let from = server(path) + 1;
    if from == 1 || from >= path.len() || separator(path[from]) {
        return 0;
    }
    let mut end = from;
    while end < path.len() && !separator(path[end]) {
        end += 1;
    }
    end
}

/// "\\?\" and "\\.\" name verbatim paths and devices, not a share.
fn device(path: &[char]) -> bool {
    server(path) == 3 && (path[2] == '?' || path[2] == '.')
}

fn chars(path: &str) -> Vec<char> {
    path.chars().collect()
}

fn slashes(path: &str) -> String {
    path.replace('\\', "/")
}

/// A drive or UNC path in either separator form (also "\\?\" and "\\.\"
/// paths): a Windows folder, which the swarm catalog must never receive.
pub fn windows_shaped(path: &str) -> bool {
    let path = chars(path);
    drive(&path) || share(&path) > 0
}

/// POSIX: a leading '/' (`QDir::isAbsolutePath` also takes Qt's ':'
/// resources, which only [`within`] asks). Windows: a drive root ("C:\",
/// "c:/") or a share ("\\server\share", "//server/share"); "\work", "C:work"
/// and "\\?\" paths are not folders of a computer.
pub fn absolute(path: &str, style: Style) -> bool {
    match style {
        Style::Posix => path.starts_with('/'),
        Style::Windows => {
            let path = chars(path);
            drive(&path) || (share(&path) > 0 && !device(&path))
        }
    }
}

/// Resolves `.` and `..` by names after `root` characters of `text`. A `..`
/// beyond the start is kept when `keep_parents`, or dropped (a Windows root).
fn names(text: &[char], keep_parents: bool) -> Vec<String> {
    let rest: String = text.iter().collect();
    let mut names: Vec<String> = Vec::new();
    for name in rest.split('/').filter(|name| !name.is_empty()) {
        match name {
            "." => {}
            ".." if names.last().is_some_and(|last| last != "..") => {
                names.pop();
            }
            ".." if !keep_parents => {}
            _ => names.push(name.to_owned()),
        }
    }
    names
}

/// The stored form. POSIX: `QDir::cleanPath` (a `..` at the root stays, as
/// Qt keeps it). Windows: what `QDir::cleanPath` gives in a Windows build
/// ('/' separators, no trailing one except in "X:/", "//server/share" keeps
/// its two), except that `..` never leaves the root.
pub fn clean(path: &str, style: Style) -> String {
    if path.is_empty() {
        return String::new();
    }
    match style {
        Style::Posix => {
            let text = chars(path);
            let rooted = text[0] == '/';
            let rest = names(&text, true).join("/");
            if rooted {
                format!("/{rest}")
            } else if rest.is_empty() {
                ".".into()
            } else {
                rest
            }
        }
        Style::Windows => {
            let text = chars(&slashes(path));
            // Kept as typed: "X:/", "//server/share" (or "//server" without
            // a share), "/".
            let mut root = if drive(&text) { 3 } else { share(&text) };
            if root == 0 {
                root = server(&text);
            }
            if root == 0 && text[0] == '/' {
                root = 1;
            }
            let rest = names(&text[root..], root == 0).join("/");
            if root == 0 {
                return if rest.is_empty() { ".".into() } else { rest };
            }
            let prefix: String = text[..root].iter().collect();
            if rest.is_empty() || prefix.ends_with('/') {
                prefix + &rest
            } else {
                format!("{prefix}/{rest}")
            }
        }
    }
}

/// Qt's case-insensitive comparison folds each character on its own; a
/// character whose lower case is longer than one stays as it is.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

/// A normalized comparison key, for bounded folder metadata indexes only.
/// It conveys no computer ownership or filesystem authority.
pub(crate) fn comparison_key(path: &str, style: Style) -> String {
    let cleaned = clean(path, style);
    match style {
        Style::Posix => cleaned,
        Style::Windows => cleaned.chars().map(fold).collect(),
    }
}
fn equal_folded(a: &str, b: &str) -> bool {
    a.chars().map(fold).eq(b.chars().map(fold))
}

fn starts_folded(text: &str, prefix: &str) -> bool {
    let mut text = text.chars().map(fold);
    prefix.chars().map(fold).all(|c| text.next() == Some(c))
}

/// Windows compares with either separator and ignoring case; POSIX compares
/// exactly.
pub fn same(a: &str, b: &str, style: Style) -> bool {
    match style {
        Style::Posix => a == b,
        Style::Windows => equal_folded(&slashes(a), &slashes(b)),
    }
}

/// Whether `path` is `folder` or inside it, by whole names: C:/workshop is
/// not in C:/work.
pub fn within(path: &str, folder: &str, style: Style) -> bool {
    match style {
        Style::Posix => {
            (folder.starts_with('/') || folder.starts_with(':'))
                && (path == folder
                    || if folder == "/" {
                        path.starts_with('/')
                    } else {
                        path.strip_prefix(folder)
                            .is_some_and(|rest| rest.starts_with('/'))
                    })
        }
        Style::Windows => {
            if !absolute(folder, style) {
                return false;
            }
            let target = slashes(path);
            let base = slashes(folder);
            let prefix = if base.ends_with('/') {
                base.clone()
            } else {
                format!("{base}/")
            };
            equal_folded(&target, &base) || starts_folded(&target, &prefix)
        }
    }
}

/// The last name; empty for a root ("/", "C:/", "//server/share") and after
/// a trailing separator, as `section('/', -1)` gives on POSIX.
pub fn leaf(path: &str, style: Style) -> String {
    let text = match style {
        Style::Posix => path.to_owned(),
        Style::Windows => {
            let text = slashes(path);
            let end = share(&chars(&text));
            if end > 0 && end == text.chars().count() {
                return String::new();
            }
            text
        }
    };
    text.rsplit('/').next().unwrap_or_default().to_owned()
}

/// The form shown to the user: backslashes on Windows, whatever the
/// client's platform.
pub fn native(path: &str, style: Style) -> String {
    match style {
        Style::Posix => path.to_owned(),
        Style::Windows => path.replace('/', "\\"),
    }
}

/// `QFileInfo(path).path()` and `fileName()`, by the folder's style: the
/// parent ("/" for "/repo", "C:/" for "C:/repo") and the last name.
pub fn split(path: &str, style: Style) -> (String, String) {
    let path = match style {
        Style::Posix => path.to_owned(),
        Style::Windows => slashes(path),
    };
    match path.rfind('/') {
        None => (".".into(), path),
        Some(at) => {
            // The root keeps its separator: "/" and, on Windows, "X:/".
            let root = at == 0 || (style == Style::Windows && at == 2 && drive(&chars(&path)));
            let parent = if root { &path[..=at] } else { &path[..at] };
            (parent.to_owned(), path[at + 1..].to_owned())
        }
    }
}

/// `QDir(parent).filePath(name)`: one '/' between them.
pub fn join(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}
