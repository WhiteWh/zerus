//! The `zerus-file` URI scheme over draft attachments and the private
//! attachment cache.
//!
//! `zerus-file://localhost/draft/{handle}/{list}/{index}` serves an
//! attachment of a composer in memory while its attachment list is the one
//! the view named; `.../cache/{digest}/{name}` serves a copy in the private
//! cache, only when `attachments::cached_at` accepts it (a regular file inside
//! the canonical root, nonempty, at most 10 MiB) and no link leads to it.
//! Windows webviews ask `http://zerus-file.localhost/...` instead. Only the
//! main webview gets answers, and only to GET. Range requests answer 206, as
//! media elements seek with them; a cached copy is read only as far as the
//! range needs. Bytes reach the webview only this way or as raw IPC bodies,
//! never as JSON.
//!
//! The path is percent-decoded once, before any check: the webview's
//! `convertFileSrc` encodes it whole. A response names the type `sniff`
//! finds in the bytes, never one a webview would run as a page.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::http::{header, response::Builder, Method, Request, Response, StatusCode};
use zerus_client_core::attachments;

use super::{lock, Composers, Spawn};

const APP_ORIGIN: &str = if cfg!(windows) {
    "http://tauri.localhost"
} else {
    "tauri://localhost"
};

/// Copies another owner still needs, by path (a failed message's files that
/// Retry sends again).
pub type Keep = Arc<dyn Fn() -> HashSet<PathBuf> + Send + Sync>;

/// The most copies pinned at once: the webview pins only what it shows, so
/// this bound is never reached in use; past it a pin is ignored, and a
/// thumbnail whose copy went is read once more.
const PIN_LIMIT: usize = 1024;

/// The private attachment cache: `{root}/{sha256}/{name}`, as
/// `attachments::store_at` writes it.
pub struct Cache {
    root: PathBuf,
    /// The size `prune_later` keeps the cache under.
    limit: u64,
    /// A prune runs on the blocking pool.
    pruning: AtomicBool,
    /// A store arrived during the prune: one more runs after it.
    again: AtomicBool,
    /// Copies the webview shows now, by canonical path and how many views
    /// show each: a prune never removes them.
    pins: Mutex<HashMap<PathBuf, usize>>,
    /// Copies another owner keeps, asked at every prune.
    extra: Mutex<Option<Keep>>,
}

pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Cache {
    /// Playback gets one bounded raw copy from an already fetched private
    /// cache entry. No renderer-supplied filesystem path is opened.
    pub fn recording(&self, path: &str, video: bool) -> Result<Vec<u8>, String> {
        let read = self
            .shown(path)
            .and_then(|file| open_copy(&file))
            .and_then(|(file, _)| read_copy(file, None));
        let Some((mime, bytes)) = read else {
            return Err("Recording is unavailable.".into());
        };
        let supported = if video {
            mime == "video/mp4"
        } else {
            matches!(
                mime.as_str(),
                "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave"
            )
        };
        if !supported {
            return Err("Recording is unavailable.".into());
        }
        Ok(bytes)
    }

    pub fn new(root: PathBuf) -> Arc<Self> {
        Self::with_limit(root, attachments::CACHE_LIMIT)
    }

    /// A cache bounded to `limit` bytes.
    pub fn with_limit(root: PathBuf, limit: u64) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&root);
        Arc::new(Cache {
            root: canonical(&root),
            limit,
            pruning: AtomicBool::new(false),
            again: AtomicBool::new(false),
            pins: Mutex::new(HashMap::new()),
            extra: Mutex::new(None),
        })
    }

    /// One more view shows the copy at `path`: no prune removes it until
    /// every view that pinned it unpins it.
    pub fn pin(&self, path: &Path) {
        let path = canonical(path);
        let mut pins = lock(&self.pins);
        if pins.len() < PIN_LIMIT || pins.contains_key(&path) {
            *pins.entry(path).or_default() += 1;
        }
    }

    /// A view no longer shows the copy at `path`.
    pub fn unpin(&self, path: &Path) {
        let path = canonical(path);
        let mut pins = lock(&self.pins);
        if let Some(count) = pins.get_mut(&path) {
            *count -= 1;
            if *count == 0 {
                pins.remove(&path);
            }
        }
    }

    /// A reloaded webview shows nothing yet.
    pub fn clear_pins(&self) {
        lock(&self.pins).clear();
    }

    /// The copies another owner keeps, asked at every prune from now on.
    pub fn set_extra(&self, extra: Keep) {
        *lock(&self.extra) = Some(extra);
    }

    /// The copy a view names as `cache/{digest}/{name}`, when it is one.
    pub fn shown(&self, path: &str) -> Option<PathBuf> {
        self.copy(path.strip_prefix("cache/")?)
    }

    /// Where a view's `cache/{digest}/{name}` would be, even when the copy
    /// is gone (to unpin it).
    pub fn shown_at(&self, path: &str) -> Option<PathBuf> {
        let (digest, name) = path.strip_prefix("cache/")?.split_once('/')?;
        let digest_ok = digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        (digest_ok && plain_name(name)).then(|| self.root.join(digest).join(name))
    }

    /// One prune after a store, on the blocking pool; a store during a prune
    /// runs exactly one more. A copy survives while it is pinned, or named
    /// by `extra` or by the extra the cache was given.
    pub fn prune_later(self: &Arc<Self>, spawn: &Spawn, extra: Keep) {
        // Asked for before the claim: a prune that ends between the two
        // still sees it.
        self.again.store(true, Ordering::SeqCst);
        if self.pruning.swap(true, Ordering::SeqCst) {
            return;
        }
        let cache = Arc::clone(self);
        spawn(Box::new(move || loop {
            cache.again.store(false, Ordering::SeqCst);
            let mut kept: HashSet<PathBuf> = extra().iter().map(|path| canonical(path)).collect();
            let more = lock(&cache.extra).clone();
            if let Some(more) = more {
                kept.extend(more().iter().map(|path| canonical(path)));
            }
            let pins: HashSet<PathBuf> = lock(&cache.pins).keys().cloned().collect();
            attachments::prune(&cache.root, cache.limit, |path| {
                let path = canonical(path);
                pins.contains(&path) || kept.contains(&path)
            });
            if cache.again.load(Ordering::SeqCst) {
                continue;
            }
            cache.pruning.store(false, Ordering::SeqCst);
            // A store between the check and the release saw a prune running
            // and left it to this one: run once more, unless another job
            // took over meanwhile.
            if !cache.again.load(Ordering::SeqCst) || cache.pruning.swap(true, Ordering::SeqCst) {
                break;
            }
        }));
    }

    /// The canonical root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The copy `relative` names, decoded: exactly `{digest}/{name}`, a
    /// digest folder and a file that are neither links, which
    /// `attachments::cached_at` accepts. So no `..`, absolute path, drive,
    /// link or second folder ever leads anywhere else.
    fn copy(&self, relative: &str) -> Option<PathBuf> {
        let (digest, name) = relative.split_once('/')?;
        let digest_ok = digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !digest_ok || !plain_name(name) {
            return None;
        }
        let folder = self.root().join(digest);
        let file = folder.join(name);
        // The root is canonical; `symlink_metadata` never follows a link.
        let real_folder = std::fs::symlink_metadata(&folder).is_ok_and(|m| m.is_dir());
        let real_file = std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file());
        if !real_folder || !real_file {
            return None;
        }
        attachments::cached_at(self.root(), &file)
    }
}

/// One file name: no separator of any platform, no drive or stream colon,
/// and not a copy `store_at` is still writing. (An empty name, `.` and `..`
/// name a folder, which `copy` never takes for a file.)
fn plain_name(name: &str) -> bool {
    !name.contains(['/', '\\', ':']) && !name.starts_with(".partial-")
}

/// Percent-decodes a URI path once. A `%` without two hex digits after it,
/// a control character (NUL included) or bytes that are not UTF-8 make the
/// path invalid.
fn decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let digit = |at: usize| char::from(*bytes.get(at)?).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = if bytes[i] == b'%' {
            let value = digit(i + 1)? * 16 + digit(i + 2)?;
            i += 3;
            value as u8
        } else {
            i += 1;
            bytes[i - 1]
        };
        if byte.is_ascii_control() {
            return None;
        }
        out.push(byte);
    }
    String::from_utf8(out).ok()
}

/// What a request's `Range` asks of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wanted {
    /// No range, an `If-Range`, or a unit other than bytes (RFC 9110 §14.2).
    Whole,
    /// Bytes `start..=end`, the end clamped to the last byte.
    Part(usize, usize),
    /// 416: not one satisfiable byte range.
    Unsatisfiable,
}

/// The `Range` of a request for `len` bytes. An `If-Range` never matches,
/// since these answers carry no validator, so the whole file answers then.
/// Two `Range` fields, or one that is not text, are unsatisfiable.
fn wanted(headers: &tauri::http::HeaderMap, len: usize) -> Wanted {
    let ranges: Vec<_> = headers.get_all(header::RANGE).iter().collect();
    match ranges.as_slice() {
        [] => Wanted::Whole,
        _ if headers.contains_key(header::IF_RANGE) => Wanted::Whole,
        [one] => one
            .to_str()
            .map_or(Wanted::Unsatisfiable, |spec| range(spec, len)),
        _ => Wanted::Unsatisfiable,
    }
}

/// One `Range` field: a unit other than `bytes` is ignored (RFC 9110
/// §14.2); a field without a unit token is unsatisfiable, and so is a
/// bytes range `byte_range` refuses.
fn range(spec: &str, len: usize) -> Wanted {
    let Some((unit, set)) = spec.trim().split_once('=') else {
        return Wanted::Unsatisfiable;
    };
    let token = !unit.is_empty()
        && unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    if !token {
        Wanted::Unsatisfiable
    } else if !unit.eq_ignore_ascii_case("bytes") {
        Wanted::Whole
    } else {
        byte_range(set, len).map_or(Wanted::Unsatisfiable, |(start, end)| {
            Wanted::Part(start, end)
        })
    }
}

/// One `a-b`, `a-` or `-n` byte range of `len` bytes, its end clamped to the
/// last byte. None for anything else: several ranges, a number with a sign,
/// space or other character, a start past the last byte or after the end,
/// an empty suffix, or an empty file.
fn byte_range(set: &str, len: usize) -> Option<(usize, usize)> {
    let (first, last) = set.trim().split_once('-')?;
    // Digits only; a number too large for `usize` is past any file.
    let number = |text: &str| {
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse::<usize>().unwrap_or(usize::MAX))
    };
    let last_byte = len.checked_sub(1)?;
    let (start, end) = match (first.is_empty(), last.is_empty()) {
        (false, false) => (number(first)?, number(last)?),
        (false, true) => (number(first)?, last_byte),
        // `-0` starts past the last byte, so it is refused below.
        (true, false) => (len - number(last)?.min(len), last_byte),
        (true, true) => return None,
    };
    (start <= end && start <= last_byte).then_some((start, end.min(last_byte)))
}

/// The type a response names for `bytes`.
fn served_type(bytes: &[u8]) -> String {
    safe_type(attachments::sniff(bytes))
}

/// A sniffed type as served: images, audio and video keep theirs (any XML
/// aside, SVG's `image/svg+xml` included), any other text is plain text, HTML
/// and XML included, and the rest an opaque stream. Nothing served is a page
/// or a script.
fn safe_type(sniffed: String) -> String {
    let media = ["image/", "audio/", "video/"]
        .iter()
        .any(|kind| sniffed.starts_with(kind));
    if media && !sniffed.contains("xml") {
        sniffed
    } else if sniffed.starts_with("text/") {
        "text/plain".into()
    } else {
        "application/octet-stream".into()
    }
}

/// Headers of every answer: never stored, never sniffed into another type,
/// run as nothing if opened as a page, and loadable by the app's pages under
/// their `Cross-Origin-Embedder-Policy: require-corp`.
fn guarded(builder: Builder) -> Builder {
    builder
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; sandbox",
        )
        .header("cross-origin-resource-policy", "cross-origin")
}

/// The response, or a bare 500 should the builder refuse a header (it never
/// does: every value here is ASCII), so a request is always answered.
fn finish(builder: Builder, body: Vec<u8>) -> Response<Vec<u8>> {
    builder
        .header(header::CONTENT_LENGTH, body.len())
        .body(body)
        .unwrap_or_else(|_| {
            let mut response = Response::new(Vec::new());
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        })
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    finish(guarded(Response::builder()).status(code), Vec::new())
}

fn unsatisfiable(len: usize) -> Response<Vec<u8>> {
    finish(
        guarded(Response::builder())
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}")),
        Vec::new(),
    )
}

/// `{handle}/{list}/{index}`, each plain decimal digits.
pub(super) fn draft_numbers(rest: &str) -> Option<[u64; 3]> {
    let mut parts = rest.split('/').map(|part| {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u64>().ok())
            .flatten()
    });
    let numbers = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(numbers)
}

/// `draft/{handle}/{list}/{index}`: the attachment of a composer in memory,
/// while its list is still the one the view named. No hashing: an attachment
/// list that changed answers 404 for its old paths. A draft that sends lists
/// no attachments (Qt's `rebuildAttachments`, `MessageComposer.cpp:1017`),
/// so its paths answer nothing until it is back.
fn draft_file(composers: &Composers, rest: &str) -> Option<Arc<[u8]>> {
    let [handle, list, index] = draft_numbers(rest)?;
    let hub = composers.lock();
    let composer = hub
        .composers
        .values()
        .find(|c| c.handle == handle && c.attachments_rev == list && !c.sending)?;
    let attachment = composer
        .draft
        .attachments
        .get(usize::try_from(index).ok()?)?;
    Some(Arc::clone(&attachment.bytes))
}

/// The bytes a type is sniffed from.
const HEAD_BYTES: usize = 64 * 1024;

/// A cached copy, opened once, and its size, taken from the open handle: a
/// regular file of at most 10 MiB.
fn open_copy(file: &Path) -> Option<(File, usize)> {
    let handle = File::open(file).ok()?;
    let metadata = handle.metadata().ok()?;
    let len = usize::try_from(metadata.len()).ok()?;
    (metadata.is_file() && len <= attachments::MAX_ATTACHMENT_BYTES).then_some((handle, len))
}

/// The bytes `part` names of an open copy, with its served type: the whole
/// file (at most 10 MiB, should it have grown since it was opened) typed
/// from its head, or only the range's bytes, typed from a head read first.
/// A copy that shrank so the range is short answers nothing.
fn read_copy(mut handle: File, part: Option<(usize, usize)>) -> Option<(String, Vec<u8>)> {
    let Some((start, end)) = part else {
        let bytes = read_at_most(&mut handle, attachments::MAX_ATTACHMENT_BYTES)?;
        return Some((served_type(&bytes[..bytes.len().min(HEAD_BYTES)]), bytes));
    };
    let mut head = Vec::new();
    (&mut handle)
        .take(HEAD_BYTES as u64)
        .read_to_end(&mut head)
        .ok()?;
    let count = end - start + 1;
    handle.seek(SeekFrom::Start(start as u64)).ok()?;
    let mut part = Vec::with_capacity(count);
    handle.take(count as u64).read_to_end(&mut part).ok()?;
    (part.len() == count).then(|| (served_type(&head), part))
}

/// All of `source` when it holds at most `limit` bytes. It reads at most
/// one byte past the limit, never the whole of a larger source.
fn read_at_most(source: impl Read, limit: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    source.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

/// A file of `len` bytes, whole or the one range the request names; `read`
/// gives the served type and the bytes of the whole file (None) or of the
/// range, or nothing when the file is gone.
fn answer(
    request: &Request<Vec<u8>>,
    len: usize,
    read: impl FnOnce(Option<(usize, usize)>) -> Option<(String, Vec<u8>)>,
) -> Response<Vec<u8>> {
    let part = match wanted(request.headers(), len) {
        Wanted::Whole => None,
        Wanted::Part(start, end) => Some((start, end)),
        Wanted::Unsatisfiable => return unsatisfiable(len),
    };
    let Some((mime, body)) = read(part) else {
        return status(StatusCode::NOT_FOUND);
    };
    let builder = guarded(Response::builder())
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes");
    match part {
        Some((start, end)) => finish(
            builder
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}")),
            body,
        ),
        None => finish(builder.status(StatusCode::OK), body),
    }
}

/// Answers one `zerus-file` request for webview `webview`.
pub fn respond(
    composers: &Composers,
    webview: &str,
    request: &Request<Vec<u8>>,
    poster: impl FnOnce(&str, &str, &str) -> Option<Arc<[u8]>>,
) -> Response<Vec<u8>> {
    if webview != crate::MAIN_WINDOW {
        return status(StatusCode::FORBIDDEN);
    }
    let mut origins = request.headers().get_all(header::ORIGIN).iter();
    let origin = origins.next();
    if origin.is_some_and(|value| value.to_str().ok() != Some(APP_ORIGIN))
        || origins.next().is_some()
    {
        return status(StatusCode::FORBIDDEN);
    }
    if request.method() != Method::GET {
        return finish(
            guarded(Response::builder())
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::ALLOW, "GET"),
            Vec::new(),
        );
    }
    let Some(path) = request.uri().path().strip_prefix('/').and_then(decode) else {
        return status(StatusCode::NOT_FOUND);
    };
    if path == "poster" {
        if origin.is_none() {
            return status(StatusCode::FORBIDDEN);
        }
        let Some((computer, key, message)) = poster_reference(request.uri().query()) else {
            return status(StatusCode::NOT_FOUND);
        };
        let Some(bytes) = poster(&computer, &key, &message) else {
            return status(StatusCode::NOT_FOUND);
        };
        if bytes.len() > zerus_client_core::video_posters::POSTER_BYTES || bytes.is_empty() {
            return status(StatusCode::NOT_FOUND);
        }
        let mut response = answer(request, bytes.len(), |part| {
            Some((
                "image/jpeg".into(),
                match part {
                    Some((start, end)) => bytes[start..=end].to_vec(),
                    None => bytes.to_vec(),
                },
            ))
        });
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            header::HeaderValue::from_static(APP_ORIGIN),
        );
        response
    } else if let Some(rest) = path.strip_prefix("draft/") {
        let Some(bytes) = draft_file(composers, rest) else {
            return status(StatusCode::NOT_FOUND);
        };
        answer(request, bytes.len(), |part| {
            let mime = served_type(&bytes[..bytes.len().min(HEAD_BYTES)]);
            Some(match part {
                Some((start, end)) => (mime, bytes[start..=end].to_vec()),
                None => (mime, bytes.to_vec()),
            })
        })
    } else if let Some(relative) = path.strip_prefix("cache/") {
        let copy = composers.cache().copy(relative).and_then(|file| {
            attachments::touch(&file);
            open_copy(&file)
        });
        let Some((handle, len)) = copy else {
            return status(StatusCode::NOT_FOUND);
        };
        answer(request, len, |part| read_copy(handle, part))
    } else {
        status(StatusCode::NOT_FOUND)
    }
}

fn poster_reference(query: Option<&str>) -> Option<(String, String, String)> {
    let query = query.filter(|query| query.len() <= 8192)?;
    let mut fields = HashMap::new();
    for part in query.split('&') {
        let (key, value) = part.split_once('=')?;
        if !matches!(key, "computer" | "key" | "message_id") || fields.contains_key(key) {
            return None;
        }
        fields.insert(key, decode(&value.replace('+', " "))?);
    }
    let (computer, key, message) = (
        fields.remove("computer")?,
        fields.remove("key")?,
        fields.remove("message_id")?,
    );
    zerus_client_core::video_posters::valid_reference(&computer, &key, &message)
        .then_some((computer, key, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::tests::{dir, hub, session, until, Scratch};
    use std::io::Write;
    use zerus_client_core::attachments::Selection;

    fn respond(c: &Composers, webview: &str, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
        super::respond(c, webview, request, |_, _, _| None)
    }

    #[test]
    fn poster_protocol_is_exact_bounded_raw_and_main_get_only() {
        use std::cell::Cell;
        let root = dir("poster-protocol");
        let (c, _) = hub(&root);
        let calls = Cell::new(0);
        let lookup = |computer: &str, key: &str, message: &str| {
            calls.set(calls.get() + 1);
            (computer == "example-peer" && key == "run:one" && message == "message-1")
                .then(|| Arc::<[u8]>::from([255, 216, 1, 2, 255, 217]))
        };
        let path = "poster?computer=example-peer&key=run%3Aone&message_id=message-1";
        let response = super::respond(&c, "main", &get(path, None), lookup);
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), &[255, 216, 1, 2, 255, 217]);
        assert_eq!(response.headers()["content-type"], "image/jpeg");
        assert_eq!(response.headers()["cache-control"], "no-store");
        let response = super::respond(&c, "main", &get(path, Some("bytes=2-3")), lookup);
        assert_eq!(response.status(), 206);
        assert_eq!(response.body(), &[1, 2]);
        assert_eq!(
            super::respond(&c, "native-ui", &get(path, None), lookup).status(),
            403
        );
        for invalid in [
            "poster?key=one",
            "poster?computer=example-peer&key=one&key=two&message_id=one",
            "poster?computer=example-peer&key=%00&message_id=one",
            "poster?computer=example-peer&key=one&message_id=one&path=private",
        ] {
            assert_eq!(
                super::respond(&c, "main", &get(invalid, None), lookup).status(),
                404
            );
        }
        let post = Request::builder()
            .method("POST")
            .uri(format!("zerus-file://localhost/{path}"))
            .body(Vec::new())
            .unwrap();
        assert_eq!(super::respond(&c, "main", &post, lookup).status(), 405);
        assert_eq!(calls.get(), 2, "refused requests must not read the cache");
        {
            let origin = APP_ORIGIN;
            let mut request = get(path, None);
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
            let response = super::respond(&c, "main", &request, lookup);
            assert_eq!(response.status(), 200);
            assert_eq!(
                response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
                origin
            );
        }
        let before = calls.get();
        for origin in [
            "https://example.invalid",
            "http://127.0.0.1:1420",
            "http://127.0.0.1:9988",
            "http://localhost:9988",
            "null",
            "tauri://localhost.invalid",
        ] {
            let mut request = get(path, None);
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
            for webview in ["main", "native-ui"] {
                let refused = super::respond(&c, webview, &request, lookup);
                assert_eq!(refused.status(), 403);
                assert!(refused.body().is_empty());
                assert!(!refused
                    .headers()
                    .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
            }
        }
        let mut missing = get(path, None);
        missing.headers_mut().remove(header::ORIGIN);
        assert_eq!(super::respond(&c, "main", &missing, lookup).status(), 403);
        assert_eq!(calls.get(), before);
        assert_eq!(
            super::respond(
                &c,
                "main",
                &get(
                    "poster?computer=other&key=run%3Aone&message_id=message-1",
                    None
                ),
                lookup
            )
            .status(),
            404
        );
    }

    #[test]
    fn playback_reads_only_bounded_recordings_in_the_private_cache() {
        let root = dir("recording-bytes");
        let cache = Cache::new(root.join("cache"));
        let pcm = [0, 0, 1, 0];
        let wav = zerus_client_core::voice::wav(&pcm);
        let file = attachments::store_at(cache.root(), "voice.wav", &wav).unwrap();
        let path = format!(
            "cache/{}",
            file.strip_prefix(cache.root())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        );
        assert_eq!(cache.recording(&path, false).unwrap(), wav);
        assert!(cache.recording(&path, true).is_err());
        assert!(cache
            .recording("cache/../../outside/voice.wav", false)
            .is_err());
        assert!(cache.recording(file.to_str().unwrap(), false).is_err());
        std::fs::write(&file, b"<html>not a recording</html>").unwrap();
        assert!(cache.recording(&path, false).is_err());
        let large = File::create(&file).unwrap();
        large
            .set_len(attachments::MAX_ATTACHMENT_BYTES as u64 + 1)
            .unwrap();
        assert!(cache.recording(&path, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn playback_refuses_a_replaced_cache_file() {
        let root = dir("recording-link");
        let cache = Cache::new(root.join("cache"));
        let wav = zerus_client_core::voice::wav(&[0, 0]);
        let file = attachments::store_at(cache.root(), "voice.wav", &wav).unwrap();
        let path = format!(
            "cache/{}",
            file.strip_prefix(cache.root())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        );
        let outside = root.join("outside.wav");
        std::fs::write(&outside, wav).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(outside, file).unwrap();
        assert!(cache.recording(&path, false).is_err());
    }

    fn get(path: &str, range: Option<&str>) -> Request<Vec<u8>> {
        let mut request = Request::builder().uri(format!("zerus-file://localhost/{path}"));
        if path.starts_with("poster?") {
            request = request.header(header::ORIGIN, APP_ORIGIN);
        }
        if let Some(range) = range {
            request = request.header("range", range);
        }
        request.body(Vec::new()).unwrap()
    }

    #[test]
    fn the_scheme_serves_draft_bytes_with_ranges_and_only_to_main() {
        let root = dir("scheme");
        let file = root.join("clip.bin");
        std::fs::write(&file, b"0123456789").unwrap();
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![file], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        let path = view.attachments[0].path.clone();
        let whole = respond(&c, "main", &get(&path, None));
        assert_eq!(whole.status(), 200);
        assert_eq!(whole.body(), b"0123456789");
        assert_eq!(whole.headers()["accept-ranges"], "bytes");
        assert_eq!(
            whole.headers()["cross-origin-resource-policy"],
            "cross-origin"
        );
        let part = respond(&c, "main", &get(&path, Some("bytes=2-5")));
        assert_eq!(part.status(), 206);
        assert_eq!(part.body(), b"2345");
        assert_eq!(part.headers()["content-range"], "bytes 2-5/10");
        assert_eq!(
            respond(&c, "main", &get(&path, Some("bytes=20-"))).status(),
            416
        );
        assert_eq!(respond(&c, "native-ui-1", &get(&path, None)).status(), 403);
        assert_eq!(respond(&c, "main", &get("draft/0/0/0", None)).status(), 404);
        assert_eq!(
            respond(&c, "main", &get("cache/../../etc/passwd", None)).status(),
            404
        );
        c.remove(view.handle, 0);
        assert_eq!(
            respond(&c, "main", &get(&path, None)).status(),
            404,
            "a removed file's path answers nothing"
        );
    }

    #[test]
    fn the_scheme_serves_only_cached_files_under_the_root() {
        let root = dir("cached");
        let (c, _) = hub(&root);
        let cached = zerus_client_core::attachments::store_at(
            c.cache().root(),
            "shot.png",
            b"\x89PNG\r\n\x1a\nrest",
        )
        .unwrap();
        let relative = cached
            .strip_prefix(c.cache().root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let response = respond(
            &c,
            "main",
            &get(&format!("cache/{}", urlencode(&relative)), None),
        );
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "image/png");
        std::fs::write(root.join("outside.txt"), b"secret").unwrap();
        assert_eq!(
            respond(&c, "main", &get("cache/..%2Foutside.txt", None)).status(),
            404
        );
    }

    /// A test's cache with one copy, `shot.png`, and `outside.txt` beside
    /// the cache: the hub, the copy's `{digest}/{name}` and the outside file.
    fn cached_shot(root: &Scratch) -> (Composers, String, PathBuf) {
        let (c, _) = hub(root);
        let cached =
            attachments::store_at(c.cache().root(), "shot.png", b"\x89PNG\r\n\x1a\nrest").unwrap();
        let relative = cached
            .strip_prefix(c.cache().root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let outside = root.join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();
        (c, relative, outside)
    }

    fn status_of(c: &Composers, path: &str) -> u16 {
        respond(c, "main", &get(path, None)).status().as_u16()
    }

    #[test]
    fn only_the_main_webview_is_answered() {
        let root = dir("labels");
        let (c, relative, _) = cached_shot(&root);
        let path = format!("cache/{relative}");
        assert_eq!(status_of(&c, &path), 200);
        for label in ["native-ui-1", "", "Main", "main ", "mainx"] {
            let response = respond(&c, label, &get(&path, None));
            assert_eq!(response.status(), 403, "{label:?}");
            assert!(response.body().is_empty());
        }
    }

    #[test]
    fn only_get_is_answered() {
        let root = dir("methods");
        let (c, relative, _) = cached_shot(&root);
        for method in [
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
        ] {
            let request = Request::builder()
                .method(method.clone())
                .uri(format!("zerus-file://localhost/cache/{relative}"))
                .body(Vec::new())
                .unwrap();
            let response = respond(&c, "main", &request);
            assert_eq!(response.status(), 405, "{method}");
            assert_eq!(response.headers()["allow"], "GET");
            assert!(response.body().is_empty());
        }
    }

    #[test]
    fn paths_are_decoded_once_and_strictly() {
        assert_eq!(decode("cache/%2e%2E%2fx").as_deref(), Some("cache/../x"));
        assert_eq!(decode("draft%2F1%2F2%2F0").as_deref(), Some("draft/1/2/0"));
        assert_eq!(decode("%C3%A9").as_deref(), Some("\u{e9}"));
        assert_eq!(
            decode("%252e%252e").as_deref(),
            Some("%2e%2e"),
            "decoded once"
        );
        for bad in [
            "%",
            "%2",
            "x%2",
            "%zz",
            "%+F",
            "%-1",
            "% 1",
            "%00",
            "a%0Ab",
            "%7F",
            "a\u{1}b",
            "%ff%fe",
            "%C3",
            "%C0%AE%C0%AE",
            "%C0%AF",
            "%C0%AE%C0%AE%C0%AFx",
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn cache_paths_never_leave_the_cache() {
        let root = dir("traversal");
        let (c, relative, outside) = cached_shot(&root);
        let (digest, _) = relative.split_once('/').unwrap();
        assert_eq!(status_of(&c, &format!("cache/{relative}")), 200);
        assert_eq!(
            status_of(&c, &format!("cache/{}", urlencode(&relative))),
            200
        );
        let absolute = outside.to_string_lossy().replace('\\', "/");
        let attempts = [
            "cache/../outside.txt".to_owned(),
            "cache/../../etc/passwd".to_owned(),
            "cache/..%2Foutside.txt".to_owned(),
            "cache/%2e%2e/outside.txt".to_owned(),
            "cache/%2E%2E%2Foutside.txt".to_owned(),
            "cache%2F..%2Foutside.txt".to_owned(),
            "cache/..%5Coutside.txt".to_owned(),
            format!("cache/{digest}/../../outside.txt"),
            format!("cache/{digest}/..%2F..%2Foutside.txt"),
            format!("cache/{digest}%5C..%5C..%5Coutside.txt"),
            format!("cache/{digest}/./shot.png"),
            format!("cache/./{relative}"),
            format!("cache/{digest}/.."),
            format!("cache/{digest}/."),
            format!("cache/{digest}//shot.png"),
            format!("cache/{digest}/shot.png/"),
            format!("cache/{digest}/shot.png%00"),
            format!("cache/{digest}/shot.png%00.txt"),
            format!("cache/{digest}%00/shot.png"),
            format!("cache/{digest}/shot.png:stream"),
            format!("cache/{}/shot.png", digest.to_uppercase()),
            format!("cache/{}/shot.png", &digest[1..]),
            "cache/%252e%252e/outside.txt".to_owned(),
            // Overlong UTF-8 for `.` and `/`, which some decoders take as such.
            "cache/%C0%AE%C0%AE/outside.txt".to_owned(),
            "cache/%C0%AE%C0%AE%C0%AFoutside.txt".to_owned(),
            format!("cache/{digest}%C0%AF%C0%AE%C0%AE%C0%AFoutside.txt"),
            format!("cache/{digest}%C0%AFshot.png"),
            // A `..` that leads back to a real copy.
            format!("cache/{digest}/../{digest}/shot.png"),
            format!("cache/{digest}/..%2F{digest}%2Fshot.png"),
            format!("cache/{absolute}"),
            format!("cache/{}", urlencode(&absolute)),
            format!("cache//{absolute}"),
            "cache/C:%5Coutside.txt".to_owned(),
            "cache/C:/outside.txt".to_owned(),
            "cache/%5C%5Cserver%5Cshare%5Cx".to_owned(),
            format!("cache/{digest}"),
            "cache/".to_owned(),
            "cache".to_owned(),
            "".to_owned(),
            format!("Cache/{relative}"),
            format!("/cache/{relative}"),
            format!("files/{relative}"),
        ];
        for attempt in &attempts {
            assert_eq!(status_of(&c, attempt), 404, "{attempt}");
        }
        // Real folders and files under the root that are not one digest
        // folder and its copy.
        let cache = c.cache();
        for folder in ["plain".to_owned(), digest.to_uppercase()] {
            // A case-insensitive file system (macOS) already has the upper-case
            // name as the digest folder: its real copy must still be refused.
            if cache.root().join(&folder).exists() {
                assert_eq!(
                    status_of(&c, &format!("cache/{folder}/shot.png")),
                    404,
                    "{folder}"
                );
                assert_eq!(cache.copy(&format!("{folder}/shot.png")), None, "{folder}");
                continue;
            }
            std::fs::create_dir(cache.root().join(&folder)).unwrap();
            std::fs::write(cache.root().join(&folder).join("x.txt"), b"not a copy").unwrap();
            assert_eq!(
                status_of(&c, &format!("cache/{folder}/x.txt")),
                404,
                "{folder}"
            );
            assert_eq!(cache.copy(&format!("{folder}/x.txt")), None, "{folder}");
        }
        std::fs::write(cache.root().join("loose.txt"), b"not a copy").unwrap();
        assert_eq!(status_of(&c, "cache/loose.txt"), 404);
        // Names that are files on Linux and macOS but a path or a stream on Windows.
        #[cfg(unix)]
        for name in ["a\\b", "a:b"] {
            std::fs::write(cache.root().join(digest).join(name), b"odd").unwrap();
            assert_eq!(cache.copy(&format!("{digest}/{name}")), None, "{name}");
            assert_eq!(
                status_of(&c, &format!("cache/{digest}/{}", urlencode(name))),
                404,
                "{name}"
            );
        }
        // The guard itself, below the scheme.
        assert!(cache.copy(&relative).is_some());
        for relative in [
            "../outside.txt".to_owned(),
            format!("{digest}/../shot.png"),
            format!("{digest}/../{digest}/shot.png"),
            format!("{digest}/sub/shot.png"),
            format!("{digest}/.partial-1-0"),
            format!("{digest}/a:b"),
            format!("{digest}/a\\b"),
            format!("/{relative}"),
            absolute.clone(),
        ] {
            assert_eq!(cache.copy(&relative), None, "{relative}");
        }
    }

    #[test]
    fn a_copy_still_being_written_is_not_served() {
        let root = dir("partial");
        let (c, relative, _) = cached_shot(&root);
        let (digest, _) = relative.split_once('/').unwrap();
        let partial = c.cache().root().join(digest).join(".partial-7-0");
        std::fs::write(&partial, b"half a fi").unwrap();
        assert_eq!(
            attachments::cached_at(c.cache().root(), &partial),
            Some(partial.clone())
        );
        assert_eq!(status_of(&c, &format!("cache/{digest}/.partial-7-0")), 404);
    }

    #[test]
    fn cached_copies_keep_the_cache_bounds() {
        let root = dir("bounds");
        let (c, relative, _) = cached_shot(&root);
        let (digest, _) = relative.split_once('/').unwrap();
        let folder = c.cache().root().join(digest);
        std::fs::write(folder.join("empty.bin"), b"").unwrap();
        std::fs::write(
            folder.join("large.bin"),
            vec![b'x'; attachments::MAX_ATTACHMENT_BYTES + 1],
        )
        .unwrap();
        std::fs::write(
            folder.join("limit.bin"),
            vec![b'x'; attachments::MAX_ATTACHMENT_BYTES],
        )
        .unwrap();
        std::fs::create_dir(folder.join("folder.png")).unwrap();
        assert_eq!(status_of(&c, &format!("cache/{digest}/empty.bin")), 404);
        assert_eq!(status_of(&c, &format!("cache/{digest}/large.bin")), 404);
        assert_eq!(status_of(&c, &format!("cache/{digest}/folder.png")), 404);
        let limit = respond(&c, "main", &get(&format!("cache/{digest}/limit.bin"), None));
        assert_eq!(limit.status(), 200);
        assert_eq!(limit.body().len(), attachments::MAX_ATTACHMENT_BYTES);
        // The size comes from the open handle, which must be a file within
        // the bound; the read itself stops at the limit, should a copy grow
        // after it was opened.
        assert!(open_copy(&folder.join("large.bin")).is_none());
        assert!(open_copy(&folder.join("folder.png")).is_none());
        assert_eq!(
            open_copy(&folder.join("limit.bin")).map(|(_, len)| len),
            Some(attachments::MAX_ATTACHMENT_BYTES)
        );
        let (handle, _) = open_copy(&folder.join("limit.bin")).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(folder.join("limit.bin"))
            .unwrap()
            .write_all(b"grown")
            .unwrap();
        assert_eq!(
            read_copy(handle, None),
            None,
            "a copy that grew past the bound after it was opened"
        );
        let mut larger = std::io::Cursor::new(vec![b'x'; 64]);
        assert_eq!(read_at_most(&mut larger, 16), None);
        assert_eq!(larger.position(), 17, "one byte past the limit, no more");
        assert_eq!(read_at_most(&b"0123"[..], 4).as_deref(), Some(&b"0123"[..]));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_never_leads_out_of_or_within_the_cache() {
        use std::os::unix::fs::symlink;
        let root = dir("links");
        let (c, relative, outside) = cached_shot(&root);
        let (digest, _) = relative.split_once('/').unwrap();
        let cache = c.cache().root().to_path_buf();
        // A file that is a link out of the cache.
        symlink(&outside, cache.join(digest).join("out.txt")).unwrap();
        // A digest folder that is a link to a folder outside.
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("shot.png"), b"\x89PNG\r\n\x1a\nother").unwrap();
        let linked = "b".repeat(64);
        symlink(&elsewhere, cache.join(&linked)).unwrap();
        // A link inside the cache to another copy in it.
        symlink(cache.join(&relative), cache.join(digest).join("again.png")).unwrap();
        // A digest folder that links to another digest folder.
        let twin = "c".repeat(64);
        symlink(cache.join(digest), cache.join(&twin)).unwrap();
        assert_eq!(status_of(&c, &format!("cache/{relative}")), 200);
        for path in [
            format!("cache/{digest}/out.txt"),
            format!("cache/{linked}/shot.png"),
            format!("cache/{digest}/again.png"),
            format!("cache/{twin}/shot.png"),
            // A link reached through `..`.
            format!("cache/{digest}/../{twin}/shot.png"),
            format!("cache/{twin}/../{digest}/shot.png"),
            format!("cache/{linked}/../{digest}/shot.png"),
        ] {
            assert_eq!(status_of(&c, &path), 404, "{path}");
        }
    }

    #[test]
    fn ranges_are_parsed_strictly() {
        let ten = 10;
        for (spec, wanted) in [
            ("bytes=2-5", (2, 5)),
            ("bytes=0-0", (0, 0)),
            ("bytes=9-9", (9, 9)),
            ("bytes=2-", (2, 9)),
            ("bytes=0-", (0, 9)),
            ("bytes=-3", (7, 9)),
            ("bytes=-10", (0, 9)),
            ("bytes=-20", (0, 9)),
            ("bytes=2-100", (2, 9)),
            ("bytes=0-99999999999999999999999999", (0, 9)),
            ("bytes=-99999999999999999999999999", (0, 9)),
            ("Bytes=2-5", (2, 5)),
            (" bytes= 2-5 ", (2, 5)),
        ] {
            assert_eq!(range(spec, ten), Wanted::Part(wanted.0, wanted.1), "{spec}");
        }
        // Another unit is ignored (RFC 9110 §14.2): the whole file answers.
        for spec in ["items=2-5", "Items=0-1", "none=", "x-unit=whatever"] {
            assert_eq!(range(spec, ten), Wanted::Whole, "{spec}");
        }
        for spec in [
            "bytes=10-",
            "bytes=20-",
            "bytes=10-10",
            "bytes=10-20",
            "bytes=5-2",
            "bytes=-0",
            "bytes=-",
            "bytes=",
            "bytes",
            "bytes=abc-5",
            "bytes=x-",
            "bytes=2-x",
            "bytes=+2-5",
            "bytes=2-+5",
            "bytes=-+3",
            "bytes=2 -5",
            "bytes=2- 5",
            "bytes=0x2-5",
            "bytes=0-1,4-5",
            "bytes=0-1, 4-5",
            "bytes=0-1,",
            "bytes=2-5-7",
            "bytes =2-5",
            " =2-5",
            "=2-5",
            "by tes=2-5",
            "bytes;=2-5",
            "2-5",
            "",
            "bytes=99999999999999999999999999-",
        ] {
            assert_eq!(range(spec, ten), Wanted::Unsatisfiable, "{spec}");
        }
        assert_eq!(
            range("bytes=0-", 0),
            Wanted::Unsatisfiable,
            "an empty file has no range"
        );
        assert_eq!(range("bytes=-1", 0), Wanted::Unsatisfiable);
        assert_eq!(range("bytes=0-0", 1), Wanted::Part(0, 0));
    }

    #[test]
    fn a_bad_or_unsatisfiable_range_answers_416() {
        let root = dir("ranges");
        let (c, _, _) = cached_shot(&root);
        let content = b"0123456789";
        let relative = attachments::store_at(c.cache().root(), "digits.txt", content)
            .unwrap()
            .strip_prefix(c.cache().root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let path = format!("cache/{relative}");
        let clamped = respond(&c, "main", &get(&path, Some("bytes=7-100")));
        assert_eq!(clamped.status(), 206);
        assert_eq!(clamped.body(), b"789");
        assert_eq!(clamped.headers()["content-range"], "bytes 7-9/10");
        assert_eq!(clamped.headers()["content-length"], "3");
        let suffix = respond(&c, "main", &get(&path, Some("bytes=-2")));
        assert_eq!(
            (suffix.status().as_u16(), suffix.body().as_slice()),
            (206, &b"89"[..])
        );
        for bad in [
            "bytes=10-",
            "bytes=5-2",
            "bytes=abc-5",
            "bytes=0-1,4-5",
            "bytes =0-1",
            "bytes=-0",
        ] {
            let response = respond(&c, "main", &get(&path, Some(bad)));
            assert_eq!(response.status(), 416, "{bad}");
            assert_eq!(response.headers()["content-range"], "bytes */10", "{bad}");
            assert!(response.body().is_empty());
        }
        let opaque = Request::builder()
            .uri(format!("zerus-file://localhost/{path}"))
            .header(
                "range",
                tauri::http::HeaderValue::from_bytes(b"bytes=\xff-1").unwrap(),
            )
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            respond(&c, "main", &opaque).status(),
            416,
            "a value that is not text"
        );
        let twice = Request::builder()
            .uri(format!("zerus-file://localhost/{path}"))
            .header("range", "bytes=0-1")
            .header("range", "bytes=2-3")
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            respond(&c, "main", &twice).status(),
            416,
            "two Range fields"
        );
        let validated = Request::builder()
            .uri(format!("zerus-file://localhost/{path}"))
            .header("range", "bytes=0-1")
            .header("if-range", "\"an-old-tag\"")
            .body(Vec::new())
            .unwrap();
        let whole = respond(&c, "main", &validated);
        assert_eq!(
            whole.status(),
            200,
            "an If-Range never matches: the whole file"
        );
        assert_eq!(whole.body(), content);
        assert_eq!(whole.headers()["content-length"], "10");
        // RFC 9110 §14.2: a unit the server does not know is ignored.
        for unknown in ["items=0-1", "lines=2-"] {
            let whole = respond(&c, "main", &get(&path, Some(unknown)));
            assert_eq!(whole.status(), 200, "{unknown}");
            assert_eq!(whole.body(), content, "{unknown}");
            assert!(whole.headers().get("content-range").is_none());
        }
    }

    #[test]
    fn a_range_reads_only_its_bytes_of_the_cached_copy() {
        let root = dir("range-reads");
        let (c, _, _) = cached_shot(&root);
        // A PNG longer than the 64 KiB head the type is sniffed from.
        let mut content = b"\x89PNG\r\n\x1a\n".to_vec();
        content.extend((0..200_000u32).map(|i| (i % 251) as u8));
        let cached = attachments::store_at(c.cache().root(), "long.png", &content).unwrap();
        let relative = cached
            .strip_prefix(c.cache().root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let path = format!("cache/{relative}");
        let len = content.len();
        for (spec, start, end) in [
            ("bytes=0-99", 0, 99),
            ("bytes=70000-70099", 70_000, 70_099),
            ("bytes=65530-65545", 65_530, 65_545),
            ("bytes=150000-", 150_000, len - 1),
            ("bytes=-1000", len - 1000, len - 1),
            ("bytes=199000-999999", 199_000, len - 1),
        ] {
            let part = respond(&c, "main", &get(&path, Some(spec)));
            assert_eq!(part.status(), 206, "{spec}");
            assert_eq!(part.body().as_slice(), &content[start..=end], "{spec}");
            assert_eq!(part.headers()["content-type"], "image/png", "{spec}");
            assert_eq!(
                part.headers()["content-range"],
                format!("bytes {start}-{end}/{len}").as_str()
            );
        }
        let whole = respond(&c, "main", &get(&path, None));
        assert_eq!(whole.body().as_slice(), content.as_slice());
        assert_eq!(whole.headers()["content-type"], "image/png");
        // A copy that shrank after it was opened answers nothing for a range
        // past its new end.
        let (handle, opened) = open_copy(&cached).unwrap();
        assert_eq!(opened, len);
        File::options()
            .write(true)
            .open(&cached)
            .unwrap()
            .set_len(1000)
            .unwrap();
        assert_eq!(read_copy(handle, Some((70_000, 70_099))), None);
    }

    #[test]
    fn draft_paths_name_a_live_composer_and_its_list() {
        let root = dir("draft-paths");
        let file = root.join("clip.png");
        std::fs::write(&file, b"\x89PNG\r\n\x1a\n0123456789").unwrap();
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![file], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        let (handle, list) = (
            view.handle,
            view.attachments[0]
                .path
                .split('/')
                .nth(2)
                .unwrap()
                .parse::<u64>()
                .unwrap(),
        );
        assert_eq!(view.attachments[0].path, format!("draft/{handle}/{list}/0"));
        let whole = respond(&c, "main", &get(&format!("draft/{handle}/{list}/0"), None));
        assert_eq!(whole.status(), 200);
        assert_eq!(whole.headers()["content-type"], "image/png");
        let part = respond(
            &c,
            "main",
            &get(&format!("draft/{handle}/{list}/0"), Some("bytes=8-11")),
        );
        assert_eq!(part.body(), b"0123");
        assert_eq!(part.headers()["content-type"], "image/png");
        // `convertFileSrc` encodes the path whole.
        assert_eq!(
            status_of(&c, &format!("draft%2F{handle}%2F{list}%2F0")),
            200
        );
        for path in [
            format!("draft/{}/{list}/0", handle + 1),
            format!("draft/{handle}/{}/0", list + 1),
            format!("draft/{handle}/{}/0", list - 1),
            format!("draft/{handle}/{list}/1"),
            format!("draft/{handle}/{list}/0/0"),
            format!("draft/{handle}/{list}/"),
            format!("draft/{handle}/{list}"),
            format!("draft/+{handle}/{list}/0"),
            format!("draft/{handle}/{list}/+0"),
            format!("draft/{handle}/{list}/-0"),
            format!("draft/{handle}/{list}/0%00"),
            format!("draft/{handle}/{list}/18446744073709551616"),
            format!("draft/%20{handle}/{list}/0"),
            format!("draft//{handle}/{list}/0"),
            format!("draft/{handle}/{list}/../0"),
            format!("Draft/{handle}/{list}/0"),
        ] {
            assert_eq!(status_of(&c, &path), 404, "{path}");
        }
        assert_eq!(draft_numbers("1/2/3"), Some([1, 2, 3]));
        assert_eq!(draft_numbers("01/2/3"), Some([1, 2, 3]));
        for bad in [
            "1/2",
            "1/2/3/4",
            "1/2/",
            "/1/2/3",
            "+1/2/3",
            "1/-2/3",
            "1/2/3 ",
            "a/2/3",
            "18446744073709551616/2/3",
        ] {
            assert_eq!(draft_numbers(bad), None, "{bad}");
        }
        // Qt lists no attachment while its draft sends: its paths answer nothing.
        for composer in c.lock().composers.values_mut() {
            composer.sending = true;
        }
        assert_eq!(status_of(&c, &format!("draft/{handle}/{list}/0")), 404);
        for composer in c.lock().composers.values_mut() {
            composer.sending = false;
        }
        assert_eq!(status_of(&c, &format!("draft/{handle}/{list}/0")), 200);
    }

    #[test]
    fn nothing_served_is_a_page() {
        let html = b"<!DOCTYPE html><html><body><script>document.title='x'</script></body></html>";
        let xml =
            b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"x()\"/>";
        // `sniff` names markup as such; the scheme never does.
        assert_eq!(attachments::sniff(html), "text/html");
        assert_eq!(attachments::sniff(xml), "text/xml");
        assert_eq!(served_type(html), "text/plain");
        assert_eq!(served_type(xml), "text/plain");
        assert_eq!(
            served_type(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            "text/plain"
        );
        assert_eq!(served_type(b"#!/bin/sh\necho hi\n"), "text/plain");
        assert_eq!(served_type(b"plain words"), "text/plain");
        assert_eq!(served_type(b"\x89PNG\r\n\x1a\nrest"), "image/png");
        assert_eq!(served_type(b"\xff\xd8\xff\xe0rest"), "image/jpeg");
        assert_eq!(served_type(b"OggS\x00\x02rest-of-a-page"), "audio/ogg");
        assert_eq!(served_type(b"%PDF-1.7\n"), "application/octet-stream");
        assert_eq!(served_type(b"PK\x03\x04rest"), "application/octet-stream");
        assert_eq!(served_type(&[0, 1, 2, 3]), "application/octet-stream");
        for (sniffed, served) in [
            ("image/svg+xml", "application/octet-stream"),
            ("image/x-sample+xml", "application/octet-stream"),
            ("application/xhtml+xml", "application/octet-stream"),
            ("application/javascript", "application/octet-stream"),
            ("text/html", "text/plain"),
            ("text/javascript", "text/plain"),
            ("video/webm", "video/webm"),
            ("audio/mpeg", "audio/mpeg"),
            ("image/gif", "image/gif"),
        ] {
            assert_eq!(safe_type(sniffed.into()), served, "{sniffed}");
        }

        let root = dir("pages");
        let (c, _, _) = cached_shot(&root);
        let page = attachments::store_at(c.cache().root(), "page.html", html).unwrap();
        let relative = page
            .strip_prefix(c.cache().root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let response = respond(&c, "main", &get(&format!("cache/{relative}"), None));
        assert_eq!(response.status(), 200);
        let headers = response.headers();
        assert_eq!(headers["content-type"], "text/plain");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(
            headers["content-security-policy"],
            "default-src 'none'; sandbox"
        );
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["cross-origin-resource-policy"], "cross-origin");
        // Refusals carry the same guards.
        let refused = respond(&c, "main", &get("cache/../outside.txt", None));
        assert_eq!(refused.headers()["x-content-type-options"], "nosniff");
        assert!(refused.headers().get("content-type").is_none());
    }

    fn urlencode(text: &str) -> String {
        text.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }

    #[test]
    fn a_store_during_a_prune_runs_exactly_one_more() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Mutex;
        let root = dir("prune-later");
        let cache = Cache::new(root.join("cache"));
        let jobs: Arc<Mutex<Vec<crate::composer::Job>>> = Arc::default();
        let queue = Arc::clone(&jobs);
        let spawn: Spawn = Arc::new(move |job| queue.lock().unwrap().push(job));
        let asked = Arc::new(AtomicUsize::new(0));
        let (counted, me, again) = (Arc::clone(&asked), Arc::clone(&cache), Arc::clone(&spawn));
        // The first prune meets a store of its own while it runs.
        let extra: Arc<dyn Fn() -> HashSet<PathBuf> + Send + Sync> = Arc::new(move || {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                me.prune_later(&again, Arc::new(HashSet::new));
            }
            HashSet::new()
        });
        cache.prune_later(&spawn, Arc::clone(&extra));
        cache.prune_later(&spawn, Arc::clone(&extra));
        assert_eq!(jobs.lock().unwrap().len(), 1, "one prune at a time");
        let job = jobs.lock().unwrap().pop().unwrap();
        job();
        assert_eq!(
            asked.load(Ordering::SeqCst),
            2,
            "the store during it ran one more"
        );
        assert!(jobs.lock().unwrap().is_empty());
        cache.prune_later(&spawn, extra);
        assert_eq!(jobs.lock().unwrap().len(), 1, "a later store prunes again");
    }

    #[test]
    fn a_prune_keeps_shown_copies_and_retry_files() {
        let root = dir("prune-pins");
        let cache = Cache::with_limit(root.join("cache"), 10);
        let keep = attachments::store_at(cache.root(), "open.png", &[1u8; 8]).unwrap();
        let retry = attachments::store_at(cache.root(), "retry.txt", &[2u8; 8]).unwrap();
        let old = attachments::store_at(cache.root(), "old.txt", &[3u8; 8]).unwrap();
        cache.pin(&keep);
        let kept = retry.clone();
        cache.set_extra(Arc::new(move || HashSet::from([kept.clone()])));
        let spawn: Spawn = Arc::new(|job| job());
        cache.prune_later(&spawn, Arc::new(HashSet::new));
        assert!(
            keep.exists() && retry.exists(),
            "pinned and Retry's copies survive"
        );
        assert!(!old.exists(), "the other copy went to stay under the bound");
        cache.unpin(&keep);
        cache.prune_later(&spawn, Arc::new(HashSet::new));
        assert!(!keep.exists(), "unpinned, it goes");
        assert!(retry.exists());
    }

    #[test]
    fn pins_are_counted_per_view_and_cleared_by_a_reload() {
        let root = dir("pin-counts");
        let cache = Cache::with_limit(root.join("cache"), 0);
        let shot = attachments::store_at(cache.root(), "shot.png", &[1u8; 8]).unwrap();
        let spawn: Spawn = Arc::new(|job| job());
        cache.pin(&shot);
        cache.pin(&shot);
        cache.unpin(&shot);
        cache.prune_later(&spawn, Arc::new(HashSet::new));
        assert!(shot.exists(), "another view still shows it");
        cache.clear_pins();
        cache.prune_later(&spawn, Arc::new(HashSet::new));
        assert!(!shot.exists(), "a reloaded webview shows nothing");
        // An unpin without a pin changes nothing.
        cache.unpin(&shot);
        assert!(lock(&cache.pins).is_empty());
    }

    #[test]
    fn pins_stay_bounded() {
        let root = dir("pin-limit");
        let cache = Cache::new(root.join("cache"));
        for i in 0..PIN_LIMIT + 10 {
            cache.pin(&cache.root().join(format!("{i:064x}")).join("shot.png"));
        }
        assert_eq!(lock(&cache.pins).len(), PIN_LIMIT);
        let first = cache.root().join(format!("{:064x}", 0)).join("shot.png");
        cache.pin(&first);
        assert_eq!(
            lock(&cache.pins)[&canonical(&first)],
            2,
            "a pinned copy still counts"
        );
    }

    #[test]
    fn a_view_names_only_copies_in_the_cache() {
        let root = dir("shown");
        let cache = Cache::new(root.join("cache"));
        let shot =
            attachments::store_at(cache.root(), "shot.png", b"\x89PNG\r\n\x1a\nrest").unwrap();
        let relative = shot
            .strip_prefix(cache.root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        assert_eq!(
            cache.shown(&format!("cache/{relative}")),
            Some(shot.clone())
        );
        assert_eq!(
            cache.shown_at(&format!("cache/{relative}")),
            Some(shot.clone())
        );
        let digest = relative.split_once('/').unwrap().0;
        let gone = format!("cache/{digest}/gone.png");
        assert_eq!(cache.shown(&gone), None, "a missing copy is not shown");
        assert_eq!(
            cache.shown_at(&gone),
            Some(cache.root().join(digest).join("gone.png"))
        );
        for bad in [
            "cache/../outside.txt",
            "draft/1/1/0",
            &relative,
            "cache/abc/shot.png",
            &format!("cache/{digest}/a/b"),
        ] {
            assert_eq!(cache.shown(bad), None, "{bad}");
            assert_eq!(cache.shown_at(bad), None, "{bad}");
        }
    }
}
