//! The composer's attachments, ported from the Qt client: the limits and
//! texts, `[Image #n]` and `[File #n]` references, reading a chosen file or a
//! pasted image, and the private content-addressed cache that a sent message's
//! files are kept in for retry and viewing.
//!
//! Every function that touches the disk blocks; callers run it off the UI
//! thread.
//!
//! Reference: `tray/src/MessageComposer.cpp` (`attachFile`, `addAttachment`,
//! `removeAttachment`, the pasted image), `tray/src/AttachmentViewer.h`
//! (`AttachmentFiles::storeAt`, `cachedAt`, `prepareAsync`, `readAsync`) and
//! `tray/src/HgsClient.h` (the limits).
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub mod image;

pub const MAX_ATTACHMENTS: usize = 8;
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 20 * 1024 * 1024;
/// A pasted image must stay below this many pixels.
pub const MAX_IMAGE_PIXELS: u64 = 32_000_000;

/// One attached file. It serializes as an attachment of `hgs send`'s
/// standard input: `{name, mime, data_base64, reference}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    #[serde(
        rename = "data_base64",
        serialize_with = "to_base64",
        deserialize_with = "from_base64"
    )]
    pub bytes: Arc<[u8]>,
    /// `[Image #n]` or `[File #n]` in the message text; empty for files a
    /// message names nowhere, and then left out as Qt does.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reference: String,
}

fn to_base64<S: Serializer>(bytes: &Arc<[u8]>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&STANDARD.encode(bytes))
}

fn from_base64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Arc<[u8]>, D::Error> {
    let text = String::deserialize(deserializer)?;
    STANDARD
        .decode(text)
        .map(Arc::from)
        .map_err(serde::de::Error::custom)
}

/// What the composer lists for an attachment, without its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub name: String,
    pub mime: String,
    pub reference: String,
    pub bytes: u64,
}

impl Attachment {
    pub fn info(&self) -> AttachmentInfo {
        AttachmentInfo {
            name: self.name.clone(),
            mime: self.mime.clone(),
            reference: self.reference.clone(),
            bytes: self.bytes.len() as u64,
        }
    }
}

/// A file read for attaching, before the draft accepts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRead {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// A selection in the composer's text, in UTF-16 code units as a webview's
/// `selectionStart` and `selectionEnd` count them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}

/// The result of attaching: the reference inserted into the text and the
/// cursor after it, in UTF-16 code units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Added {
    pub reference: String,
    pub cursor: usize,
}

/// The text and attachments of one composer. It stays in Rust: it has no
/// serde, so its bytes never cross the IPC; the webview gets `infos()`, and
/// the draft store keeps its own record with the bytes in separate files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub next_attachment_number: u32,
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            text: String::new(),
            attachments: Vec::new(),
            next_attachment_number: 1,
        }
    }
}

impl Draft {
    /// Attach a file, as Qt's `addAttachment`: the limits first, then the next
    /// reference the text does not already contain, inserted over the
    /// selection with a space on each side where the neighbour is not one.
    pub fn add(
        &mut self,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
        selection: Selection,
    ) -> Result<Added, String> {
        let total = bytes.len()
            + self
                .attachments
                .iter()
                .map(|attachment| attachment.bytes.len())
                .sum::<usize>();
        if self.attachments.len() >= MAX_ATTACHMENTS {
            return Err("Attach up to 8 files per message.".into());
        }
        if bytes.is_empty() {
            return Err("The attachment is empty.".into());
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES || total > MAX_TOTAL_BYTES {
            return Err("Attachments: 10 MiB per file, 20 MiB per message.".into());
        }
        let kind = if mime.starts_with("image/") {
            "Image"
        } else {
            "File"
        };
        let reference = loop {
            let reference = format!("[{kind} #{}]", self.next_attachment_number);
            self.next_attachment_number += 1;
            if !self.text.contains(&reference) {
                break reference;
            }
        };
        self.attachments.push(Attachment {
            name: name.to_owned(),
            mime: mime.to_owned(),
            bytes: Arc::from(bytes),
            reference: reference.clone(),
        });
        let start = byte_index(&self.text, selection.start.min(selection.end));
        let end = byte_index(&self.text, selection.start.max(selection.end));
        let leading = self.text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace());
        let trailing = self.text[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_whitespace());
        let inserted = format!(
            "{}{reference}{}",
            if leading { " " } else { "" },
            if trailing { " " } else { "" }
        );
        let cursor = utf16_len(&self.text[..start]) + utf16_len(&inserted);
        self.text.replace_range(start..end, &inserted);
        Ok(Added { reference, cursor })
    }

    /// Remove an attachment and every mention of its reference, ignoring case
    /// as Qt's document search does. Returns the removed attachment.
    pub fn remove(&mut self, index: usize) -> Option<Attachment> {
        if index >= self.attachments.len() {
            return None;
        }
        let attachment = self.attachments.remove(index);
        if !attachment.reference.is_empty() {
            self.text = remove_ignoring_ascii_case(&self.text, &attachment.reference);
        }
        Some(attachment)
    }

    pub fn infos(&self) -> Vec<AttachmentInfo> {
        self.attachments.iter().map(Attachment::info).collect()
    }
}

/// The byte index of a UTF-16 offset, clamped to the text and rounded down to
/// a character boundary.
fn byte_index(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        units += c.len_utf16();
        if units > utf16 {
            return index;
        }
    }
    text.len()
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `needle` is ASCII, so a byte match never splits a character.
fn remove_ignoring_ascii_case(text: &str, needle: &str) -> String {
    let (haystack, needle) = (text.as_bytes(), needle.as_bytes());
    let mut kept = Vec::with_capacity(haystack.len());
    let mut index = 0;
    while index < haystack.len() {
        if haystack[index..].len() >= needle.len()
            && haystack[index..index + needle.len()].eq_ignore_ascii_case(needle)
        {
            index += needle.len();
        } else {
            kept.push(haystack[index]);
            index += 1;
        }
    }
    String::from_utf8(kept).expect("an ASCII needle keeps UTF-8 intact")
}

/// The MIME type of the content, as Qt's `QMimeDatabase::mimeTypeForData`
/// decides it: known signatures first, then `text/plain` when the first 128
/// bytes hold no control characters other than tab, line feed and carriage
/// return (or start with a UTF-16 byte order mark).
pub fn sniff(bytes: &[u8]) -> String {
    if let Some(kind) = infer::get(bytes) {
        return kind.mime_type().to_owned();
    }
    let text = bytes.starts_with(&[0xfe, 0xff])
        || bytes.starts_with(&[0xff, 0xfe])
        || bytes
            .iter()
            .take(128)
            .all(|&b| b >= 32 || b == b'\t' || b == b'\n' || b == b'\r');
    if text {
        "text/plain"
    } else {
        "application/octet-stream"
    }
    .to_owned()
}

/// An I/O error as a sentence, without Rust's `(os error N)` suffix.
fn describe(error: &std::io::Error) -> String {
    let text = error.to_string();
    match text.find(" (os error ") {
        Some(index) => text[..index].to_owned(),
        None => text,
    }
}

/// Read a chosen file, as Qt's `attachFile`. It reads at most 10 MiB and one
/// byte more, so a file that grew meanwhile fails the draft's limit.
pub fn read_file(path: &Path) -> Result<FileRead, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let too_large = || "Choose a file of at most 10 MiB.".to_owned();
    let metadata = fs::metadata(path).map_err(|_| too_large())?;
    if !metadata.is_file() || metadata.len() > MAX_ATTACHMENT_BYTES as u64 {
        return Err(too_large());
    }
    let file =
        File::open(path).map_err(|error| format!("Cannot read {name}: {}", describe(&error)))?;
    let mut bytes = Vec::new();
    file.take(MAX_ATTACHMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read {name}: {}", describe(&error)))?;
    Ok(FileRead {
        mime: sniff(&bytes),
        name,
        bytes,
    })
}

/// A pasted image, as PNG bytes from the clipboard. Its size is read from the
/// PNG header, never trusted from the caller.
pub fn pasted_image(png: Vec<u8>) -> Result<FileRead, String> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png.len() < 24 || !png.starts_with(SIGNATURE) || &png[12..16] != b"IHDR" {
        return Err("Could not read the pasted image.".into());
    }
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("Image is too large. Use an image below 32 megapixels.".into());
    }
    Ok(FileRead {
        name: "pasted-image.png".into(),
        mime: "image/png".into(),
        bytes: png,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A plain, portable file name for a cached copy, as `storeAt` makes it.
/// Letters, digits, `.`, `_`, space and `-` are kept, anything else becomes
/// `_`. The extension (up to 16 characters with its dot) is kept while the
/// stem shrinks to 180 UTF-16 units and 240 UTF-8 bytes. Trailing dots and
/// spaces go, as Windows drops them; Windows device names gain a `_` prefix,
/// and a `.lnk` name a `_` suffix so no shell treats it as a shortcut.
pub fn cache_name(name: &str) -> String {
    let file = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let base: Vec<char> = file
        .chars()
        .map(|c| {
            if c.is_alphabetic() || c.is_numeric() || matches!(c, '.' | '_' | ' ' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let units = |chars: &[char]| chars.iter().map(|c| c.len_utf16()).sum::<usize>();
    let bytes = |chars: &[char]| chars.iter().map(|c| c.len_utf8()).sum::<usize>();
    let split = match base.iter().rposition(|&c| c == '.') {
        Some(dot) if dot > 0 && units(&base[dot..]) <= 16 => dot,
        _ => base.len(),
    };
    let (mut stem, extension) = (base[..split].to_vec(), &base[split..]);
    while !stem.is_empty()
        && (units(&stem) + units(extension) > 180 || bytes(&stem) + bytes(extension) > 240)
    {
        stem.pop();
    }
    let mut result: String = stem.iter().chain(extension).collect();
    while result.ends_with('.') || result.ends_with(' ') {
        result.pop();
    }
    if result.is_empty() {
        result = "attachment".into();
    }
    if is_device_name(&result) {
        result.insert(0, '_');
    }
    if result.len() >= 4 && result[result.len() - 4..].eq_ignore_ascii_case(".lnk") {
        result.push('_');
    }
    result
}

/// `con`, `prn`, `aux`, `nul`, `com0`-`com9` or `lpt0`-`lpt9` (also with a
/// superscript digit), then spaces and an optional extension, in any case.
fn is_device_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let rest = ["con", "prn", "aux", "nul"]
        .iter()
        .find_map(|device| lower.strip_prefix(device))
        .or_else(|| {
            ["com", "lpt"].iter().find_map(|device| {
                let rest = lower.strip_prefix(device)?;
                let digit = rest.chars().next()?;
                (digit.is_ascii_digit() || matches!(digit, '¹' | '²' | '³'))
                    .then(|| &rest[digit.len_utf8()..])
            })
        });
    rest.is_some_and(|rest| {
        let rest = rest.trim_start_matches(' ');
        rest.is_empty() || rest.starts_with('.')
    })
}

#[cfg(unix)]
fn private(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn private(_path: &Path, _mode: u32) {}

/// Identical attachments share a path, so writers take turns.
static WRITERS: Mutex<()> = Mutex::new(());
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Keep a copy of an attachment in the private cache, as `storeAt`:
/// `{root}/{sha256}/{cache_name}`, folders owner-only and the file `0600`.
/// An identical plain file is reused; a link there is removed, never
/// followed. Returns `None` when the bytes are out of bounds or the copy
/// cannot be written.
pub fn store_at(root: &Path, name: &str, bytes: &[u8]) -> Option<PathBuf> {
    if bytes.is_empty() || bytes.len() > MAX_ATTACHMENT_BYTES {
        return None;
    }
    let digest = sha256_hex(bytes);
    fs::create_dir_all(root).ok()?;
    let canonical_root = fs::canonicalize(root).ok()?;
    let folder = root.join(&digest);
    fs::create_dir_all(&folder).ok()?;
    // The digest folder must be a real folder inside the root.
    if fs::canonicalize(&folder).ok()? != canonical_root.join(&digest) {
        return None;
    }
    private(root, 0o700);
    private(&folder, 0o700);
    let path = folder.join(cache_name(name));
    let _turn = WRITERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match fs::symlink_metadata(&path) {
        Ok(entry) if entry.file_type().is_symlink() => fs::remove_file(&path).ok()?,
        Ok(entry) if entry.is_file() && entry.len() == bytes.len() as u64 => {
            let mut existing = Vec::with_capacity(bytes.len());
            let same = File::open(&path)
                .and_then(|file| file.take(bytes.len() as u64 + 1).read_to_end(&mut existing))
                .is_ok()
                && existing == bytes;
            if same {
                touch(&path);
                return Some(path);
            }
        }
        _ => {}
    }
    let temporary = folder.join(format!(
        ".partial-{}-{}",
        std::process::id(),
        TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = File::create(&temporary)?;
        private(&temporary, 0o600);
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
        return None;
    }
    // Windows refuses the replacement while another process, such as a virus
    // scanner, the search indexer or a viewer, briefly holds the old copy.
    let attempts = if cfg!(windows) { 41 } else { 1 };
    for attempt in 0..attempts {
        if fs::rename(&temporary, &path).is_ok() {
            return Some(path);
        }
        if attempt + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    let _ = fs::remove_file(&temporary);
    None
}

/// The cache's size bound.
pub const CACHE_LIMIT: u64 = 512 * 1024 * 1024;

/// Marks a cached copy as used now, for the least-recently-used order.
pub fn touch(path: &Path) {
    if let Ok(file) = File::options().write(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// What a prune did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pruned {
    pub removed: usize,
    /// What the cache holds afterwards.
    pub bytes: u64,
}

/// Bounds the cache: removes the least recently used copies until it holds
/// at most `limit` bytes. A copy `pinned` names (open for preview, or a
/// failed message's file a retry still needs) is never removed; nor is
/// anything that is not a plain file in a digest folder. Run it off the UI
/// thread after storing.
pub fn prune(root: &Path, limit: u64, pinned: impl Fn(&Path) -> bool) -> Pruned {
    let mut copies = Vec::new();
    let mut total = 0;
    let Ok(folders) = fs::read_dir(root) else {
        return Pruned::default();
    };
    for folder in folders.flatten() {
        let name = folder.file_name().to_string_lossy().into_owned();
        let digest =
            name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !digest || !folder.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(files) = fs::read_dir(folder.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(metadata) = fs::symlink_metadata(file.path()) else {
                continue;
            };
            if !metadata.is_file() || file.file_name().to_string_lossy().starts_with(".partial-") {
                continue;
            }
            total += metadata.len();
            let used = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
            copies.push((used, metadata.len(), file.path()));
        }
    }
    copies.sort();
    let mut removed = 0;
    for (_, size, path) in copies {
        if total <= limit {
            break;
        }
        if pinned(&path) || fs::remove_file(&path).is_err() {
            continue;
        }
        total -= size;
        removed += 1;
        if let Some(folder) = path.parent() {
            let _ = fs::remove_dir(folder);
        }
    }
    Pruned {
        removed,
        bytes: total,
    }
}

/// A cached copy's canonical path, as `cachedAt`: only a regular file inside
/// the canonical root, nonempty and at most 10 MiB.
pub fn cached_at(root: &Path, local_path: &Path) -> Option<PathBuf> {
    let path = fs::canonicalize(local_path).ok()?;
    let root = fs::canonicalize(root).ok()?;
    let metadata = fs::metadata(&path).ok()?;
    (path.starts_with(&root)
        && path != root
        && metadata.is_file()
        && metadata.len() > 0
        && metadata.len() <= MAX_ATTACHMENT_BYTES as u64)
        .then_some(path)
}

/// A sent message's attachment, kept in the cache for retry and viewing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    pub reference: String,
    pub local_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<image::Size>,
}

/// Cache a message's attachments before it is sent, as `prepareAsync`.
pub fn prepare(root: &Path, attachments: &[Attachment]) -> Result<Vec<StoredFile>, String> {
    let failure =
        || "Could not save the attachments. Your draft was kept without sending.".to_owned();
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(failure());
    }
    let mut total = 0;
    for attachment in attachments {
        if attachment.bytes.is_empty() || attachment.bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(failure());
        }
        total += attachment.bytes.len();
        if total > MAX_TOTAL_BYTES {
            return Err(failure());
        }
    }
    attachments
        .iter()
        .map(|attachment| {
            let local_path =
                store_at(root, &attachment.name, &attachment.bytes).ok_or_else(failure)?;
            Ok(StoredFile {
                name: attachment.name.clone(),
                mime: attachment.mime.clone(),
                bytes: attachment.bytes.len() as u64,
                reference: attachment.reference.clone(),
                local_path,
                image_size: image::dimensions(&attachment.bytes),
            })
        })
        .collect()
}

/// Read a failed message's cached attachments again for a retry, as
/// `readAsync`: each must still be the cached file it was, by size and by the
/// digest its folder is named after.
pub fn read_stored(root: &Path, files: &[StoredFile]) -> Result<Vec<Attachment>, String> {
    let failure = || {
        "The saved attachment is unavailable or changed. Attach it again before sending.".to_owned()
    };
    if files.len() > MAX_ATTACHMENTS {
        return Err(failure());
    }
    let mut total = 0;
    let mut attachments = Vec::with_capacity(files.len());
    for stored in files {
        let path = cached_at(root, &stored.local_path).ok_or_else(failure)?;
        // Bound the allocation even if the file grows after its check.
        let limit = MAX_ATTACHMENT_BYTES.min(MAX_TOTAL_BYTES - total);
        let mut bytes = Vec::new();
        File::open(&path)
            .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
            .map_err(|_| failure())?;
        if bytes.is_empty() || bytes.len() > limit || bytes.len() as u64 != stored.bytes {
            return Err(failure());
        }
        total += bytes.len();
        let folder = path
            .parent()
            .and_then(Path::file_name)
            .ok_or_else(failure)?;
        if folder.to_string_lossy() != sha256_hex(&bytes) {
            return Err(failure());
        }
        attachments.push(Attachment {
            name: stored.name.clone(),
            mime: stored.mime.clone(),
            bytes: Arc::from(bytes),
            reference: stored.reference.clone(),
        });
    }
    Ok(attachments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str) -> Draft {
        Draft {
            text: text.into(),
            ..Draft::default()
        }
    }

    fn at(position: usize) -> Selection {
        Selection {
            start: position,
            end: position,
        }
    }

    #[test]
    fn limits_are_qt_texts() {
        let mut full = draft("");
        for _ in 0..8 {
            full.add("a.txt", "text/plain", vec![1], at(0)).unwrap();
        }
        assert_eq!(
            full.add("b.txt", "text/plain", vec![1], at(0)).unwrap_err(),
            "Attach up to 8 files per message."
        );
        assert_eq!(
            draft("")
                .add("e", "text/plain", Vec::new(), at(0))
                .unwrap_err(),
            "The attachment is empty."
        );
        let size = "Attachments: 10 MiB per file, 20 MiB per message.";
        assert_eq!(
            draft("")
                .add(
                    "big",
                    "text/plain",
                    vec![0; MAX_ATTACHMENT_BYTES + 1],
                    at(0)
                )
                .unwrap_err(),
            size
        );
        let mut two = draft("");
        two.add("a", "text/plain", vec![0; MAX_ATTACHMENT_BYTES], at(0))
            .unwrap();
        two.add("b", "text/plain", vec![0; MAX_ATTACHMENT_BYTES], at(0))
            .unwrap();
        assert_eq!(
            two.add("c", "text/plain", vec![0], at(0)).unwrap_err(),
            size
        );
    }

    #[test]
    fn references_skip_numbers_the_text_already_has_and_insert_with_spacing() {
        let mut d = draft("see [Image #1]");
        let added = d.add("shot.png", "image/png", vec![1], at(14)).unwrap();
        assert_eq!(added.reference, "[Image #2]");
        assert_eq!(d.text, "see [Image #1] [Image #2] ");
        assert_eq!(added.cursor, 26);
        assert_eq!(d.next_attachment_number, 3);
        // Spaces already on both sides: none added; a selection is replaced.
        let mut d = draft("a  b");
        let added = d.add("x.txt", "text/plain", vec![1], at(2)).unwrap();
        assert_eq!((d.text.as_str(), added.cursor), ("a [File #1] b", 11));
        let mut d = draft("abcdef");
        d.add("x", "text/plain", vec![1], Selection { start: 4, end: 2 })
            .unwrap();
        assert_eq!(d.text, "ab [File #1] ef");
        // Offsets count UTF-16 units: the emoji is two.
        let mut d = draft("😀x");
        let added = d.add("x", "text/plain", vec![1], at(2)).unwrap();
        assert_eq!(d.text, "😀 [File #1] x");
        assert_eq!(added.cursor, 13);
    }

    #[test]
    fn removing_drops_every_mention_ignoring_case() {
        let mut d = draft("");
        d.add("a.png", "image/png", vec![1], at(0)).unwrap();
        d.text.push_str("again [image #1] and [Image #1]");
        let removed = d.remove(0).unwrap();
        assert_eq!(removed.reference, "[Image #1]");
        assert_eq!(d.text, " again  and ");
        assert!(d.remove(0).is_none());
    }

    #[test]
    fn content_decides_the_type() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n0000"), "image/png");
        assert_eq!(sniff(b"%PDF-1.7"), "application/pdf");
        assert_eq!(sniff("plain text\twith tabs\n".as_bytes()), "text/plain");
        assert_eq!(sniff(b"\x00\x01binary"), "application/octet-stream");
        assert_eq!(sniff(b"\xff\xfeu\x00t\x00f\x00"), "text/plain");
    }

    #[test]
    fn pasted_images_are_bounded_by_their_header() {
        let png = |width: u32, height: u32| {
            let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
            bytes.extend(width.to_be_bytes());
            bytes.extend(height.to_be_bytes());
            bytes
        };
        let read = pasted_image(png(4000, 8000)).unwrap();
        assert_eq!(
            (read.name.as_str(), read.mime.as_str()),
            ("pasted-image.png", "image/png")
        );
        assert_eq!(
            pasted_image(png(4000, 8001)).unwrap_err(),
            "Image is too large. Use an image below 32 megapixels."
        );
        assert_eq!(
            pasted_image(b"GIF89a".to_vec()).unwrap_err(),
            "Could not read the pasted image."
        );
    }

    #[test]
    fn cache_names_are_plain_and_portable() {
        assert_eq!(
            cache_name("/home/example/report: final?.pdf"),
            "report_ final_.pdf"
        );
        assert_eq!(cache_name("C:\\Users\\example\\notes.txt"), "notes.txt");
        assert_eq!(cache_name("Отчёт 2.txt"), "Отчёт 2.txt");
        assert_eq!(cache_name("trailing. . "), "trailing");
        assert_eq!(cache_name(""), "attachment");
        assert_eq!(cache_name("..."), "attachment");
        assert_eq!(cache_name("CON"), "_CON");
        assert_eq!(cache_name("com1 .txt"), "_com1 .txt");
        assert_eq!(cache_name("lpt³.log"), "_lpt³.log");
        assert_eq!(cache_name("console.txt"), "console.txt");
        assert_eq!(cache_name("link.LNK"), "link.LNK_");
        // The extension survives shortening; the stem never splits a character.
        let long = format!("{}.markdown", "a".repeat(300));
        let short = cache_name(&long);
        assert_eq!(short.chars().count(), 180);
        assert!(short.ends_with(".markdown"));
        // A letter outside the BMP is two UTF-16 units and four UTF-8 bytes.
        let wide = cache_name(&format!("{}.txt", "𝒜".repeat(100)));
        assert_eq!((wide.len(), wide.chars().count()), (240, 63));
        assert!(wide.ends_with(".txt") && !wide.contains('_'));
        assert_eq!(cache_name("😀.txt"), "_.txt");
        // An overlong "extension" is part of the stem.
        assert_eq!(cache_name("a.abcdefghijklmnopq"), "a.abcdefghijklmnopq");
    }

    #[test]
    fn attachments_serialize_as_send_input() {
        let attachment = Attachment {
            name: "a.txt".into(),
            mime: "text/plain".into(),
            bytes: Arc::from(&b"hi"[..]),
            reference: "[File #1]".into(),
        };
        let value = serde_json::to_value(&attachment).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"name": "a.txt", "mime": "text/plain", "data_base64": "aGk=", "reference": "[File #1]"})
        );
        assert_eq!(
            serde_json::from_value::<Attachment>(value).unwrap(),
            attachment
        );
    }
}
