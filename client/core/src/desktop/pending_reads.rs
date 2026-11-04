//! Bounded frozen-read payloads. These values describe data, never producer
//! authentication, writer drain, service ownership or host acknowledgement.
use crate::readmarks::ReadMark;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SOURCE_BYTES: usize = 16 << 20;
pub const HELP_MARKER: &str = "Desktop pending read import ABI: desktop-pending-reads-v1";
pub const SOURCE_ROWS: usize = 50_000;
pub const PAGE_ROWS: usize = 64;
// Leave room inside the existing 64 KiB control line for native evidence.
pub const PAGE_BYTES: usize = 48 << 10;
const MARK_BYTES: usize = 32 << 10;
pub type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub sha256: String,
    pub bytes: usize,
    pub rows: usize,
}
impl Source {
    pub fn valid(&self) -> bool {
        super::digest(&self.sha256)
            && (2..=SOURCE_BYTES).contains(&self.bytes)
            && self.rows <= SOURCE_ROWS
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub source: Source,
    pub after: usize,
    pub marks: Vec<ReadMark>,
}
impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingReadPage")
            .field("source", &self.source)
            .field("after", &self.after)
            .field("rows", &self.marks.len())
            .finish()
    }
}
impl Page {
    pub fn valid(&self) -> bool {
        self.source.valid()
            && self.marks.len() <= PAGE_ROWS
            && self.after <= self.source.rows
            && self.marks.len() <= self.source.rows - self.after
            && (!self.marks.is_empty() || self.source.rows == 0)
            && self.marks.iter().all(valid_mark)
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= PAGE_BYTES)
    }
    pub fn through(&self) -> usize {
        self.after.saturating_add(self.marks.len())
    }
}

/// Owned only by the off-thread capture/import job. Complete validation precedes
/// its first page, so a bad final entry cannot import a valid prefix. The source
/// file and its unknown fields are never rewritten by this projection.
pub struct Capture {
    source: Source,
    marks: Vec<ReadMark>,
}
impl Capture {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > SOURCE_BYTES {
            return Err("pending_read_source_too_large");
        }
        let stream = crate::migration::sources::next_pending_reads(bytes)
            .map_err(|_| "invalid_pending_read_source")?;
        let mut marks = Vec::new();
        for page in stream.into_chunks() {
            let mut page = page.map_err(|_| "invalid_pending_read_source")?;
            let entries = page
                .get_mut("entries")
                .and_then(serde_json::Value::as_array_mut)
                .map(std::mem::take)
                .ok_or("invalid_pending_read_source")?;
            for mut entry in entries {
                let value = entry
                    .as_object_mut()
                    .and_then(|object| object.remove("read_mark"))
                    .ok_or("invalid_pending_read_source")?;
                let mut mark: ReadMark =
                    serde_json::from_value(value).map_err(|_| "invalid_pending_read_source")?;
                // These are the existing queue's unset-part semantics.
                mark.reply_id = mark.reply_id.take().filter(|part| !part.is_empty());
                mark.attention_signature = mark
                    .attention_signature
                    .take()
                    .filter(|part| !part.is_empty());
                if !valid_mark(&mark) || marks.len() == SOURCE_ROWS {
                    return Err("invalid_pending_read_source");
                }
                marks.push(mark);
            }
        }
        Ok(Self {
            source: Source {
                sha256: format!("{:x}", Sha256::digest(bytes)),
                bytes: bytes.len(),
                rows: marks.len(),
            },
            marks,
        })
    }
    pub fn source(&self) -> &Source {
        &self.source
    }
    pub fn matches(&self, page: &Page) -> bool {
        page.valid()
            && page.source == self.source
            && self.marks.get(page.after..page.through()) == Some(page.marks.as_slice())
    }
    pub fn page(&self, after: usize) -> Result<Page> {
        if after > self.marks.len() || (after == self.marks.len() && after != 0) {
            return Err("invalid_pending_read_cursor");
        }
        let mut page = Page {
            source: self.source.clone(),
            after,
            marks: Vec::new(),
        };
        let mut size = serde_json::to_vec(&page)
            .map_err(|_| "invalid_pending_read_source")?
            .len();
        for mark in self.marks[after..].iter().take(PAGE_ROWS) {
            let added = serde_json::to_vec(mark)
                .map_err(|_| "invalid_pending_read_source")?
                .len()
                + usize::from(!page.marks.is_empty());
            if size + added > PAGE_BYTES {
                break;
            }
            size += added;
            page.marks.push(mark.clone());
        }
        if !page.valid() {
            return Err("invalid_pending_read_page");
        }
        Ok(page)
    }
}

/// Same target bounds as the service read ledger. Nothing is truncated into a
/// different selector, conversation, reply or attention identity.
pub fn valid_mark(mark: &ReadMark) -> bool {
    let text =
        |s: &str, limit| !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control);
    (mark.computer.is_empty()
        || (text(&mark.computer, 255)
            && !mark.computer.starts_with(['-', '@'])
            && !mark.computer.chars().any(char::is_whitespace)))
        && text(&mark.name, 1024)
        && !mark.name.starts_with(['-', '@'])
        && text(&mark.run_id, 1024)
        && text(&mark.conversation_id, 1024)
        && [&mark.reply_id, &mark.attention_signature]
            .into_iter()
            .flatten()
            .all(|part| text(part, 8192))
        && !mark.is_empty()
        && serde_json::to_vec(mark).is_ok_and(|bytes| bytes.len() <= MARK_BYTES)
}

/// Stable across source-file formatting, capture, page boundaries, reconnects
/// and service epochs. An exact prior receipt cannot resurrect a settled mark.
pub fn intent_id(configuration: &str, mark: &ReadMark) -> Result<String> {
    if !super::digest(configuration) || !valid_mark(mark) {
        return Err("invalid_pending_read_identity");
    }
    let mut hash = Sha256::new();
    hash.update(b"zerus-pending-read-intent-v1\0");
    hash.update(
        serde_json::to_vec(&(
            configuration,
            &mark.computer,
            &mark.name,
            &mark.run_id,
            &mark.conversation_id,
            &mark.reply_id,
            &mark.attention_signature,
        ))
        .map_err(|_| "invalid_pending_read_identity")?,
    );
    let digest = hash.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod tests;
