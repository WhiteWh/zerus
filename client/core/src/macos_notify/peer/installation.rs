//! Read-only native paired-bundle selection and live code observations.
//! These observations do not launch code or grant a service lease. Negative
//! bare/ad-hoc tests are not evidence of a genuine signed runtime positive.
use std::{
    ffi::c_void,
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::{Component, Path, PathBuf},
    ptr::NonNull,
};

use objc2::rc::autoreleasepool;

mod service;
pub use service::{select_for_service, ChildObservation, ServiceSelection};

const APP_ID: &str = "com.hgdev.hgs-tray";
const APPLE: &str = "anchor apple generic";
const PATH_BYTES: usize = 4096;
const TEXT_BYTES: usize = 16 * 1024;
// Security SDK SecStaticCode.h. Never skip executable or resource validation.
const STRICT: u32 = (1 << 0) | (1 << 3) | (1 << 4) | (1 << 7) | (1 << 12);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    UnsupportedLayout,
    Metadata,
    Signature,
    SelfChanged,
    GenerationChanged,
    Child,
    Peer(super::Refusal),
}

/// Expectations selected together from this process's own verified bundle.
/// There is no requirement extraction, Clone, serde or caller input.
pub struct Selection {
    original: Snapshot,
}
impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Selection { .. }")
    }
}

/// A borrowed, point-in-time observation; not a current lease or a permit.
pub struct Observation<'a> {
    _selection: &'a Selection,
    _peer: super::Observation<'a>,
}
impl std::fmt::Debug for Observation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Observation { .. }")
    }
}

/// Off UI/actor threads only. Security is synchronous; a future host must retain
/// its actual worker/admission past cancellation, timeouts or a lost receiver.
/// Bare/source/ad-hoc builds refuse. No installed-app, PATH or environment scan.
pub fn select_own() -> Result<Selection, Refusal> {
    snapshot().map(|original| Selection { original })
}
impl Selection {
    /// CLI routing only, after actual running-self and sealed-pair verification.
    /// A caller must not treat this path as a lease or native effect permission.
    pub fn service_executable(&self) -> &Path {
        &self.original.service.executable
    }
    /// Cheap CLI routing freshness after a connection's full select_own. This
    /// does not repeat Security verification and is NOT native permission or a
    /// substitute for observe_service/observe_app/full factory revalidation.
    pub fn service_executable_if_unchanged(&self) -> Result<&Path, Refusal> {
        if generation(&self.original.bundle)? != self.original.generation {
            return Err(Refusal::GenerationChanged);
        }
        Ok(&self.original.service.executable)
    }
    pub fn observe_service<'a>(
        &'a self,
        connection: &'a UnixStream,
    ) -> Result<Observation<'a>, Refusal> {
        self.observe(connection, &self.original.service.requirement)
    }
    pub fn observe_app<'a>(
        &'a self,
        connection: &'a UnixStream,
    ) -> Result<Observation<'a>, Refusal> {
        self.observe(connection, &self.original.app.requirement)
    }
    fn observe<'a>(
        &'a self,
        connection: &'a UnixStream,
        requirement: &str,
    ) -> Result<Observation<'a>, Refusal> {
        same_generation(&self.original, &snapshot()?)?;
        let peer = super::observe(connection, requirement).map_err(Refusal::Peer)?;
        same_generation(&self.original, &snapshot()?)?;
        Ok(Observation {
            _selection: self,
            _peer: peer,
        })
    }
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    bundle: PathBuf,
    generation: Vec<Node>,
    app: Signature,
    service: Signature,
    self_executable: PathBuf,
}
#[derive(PartialEq, Eq)]
struct Signature {
    identifier: String,
    team: String,
    executable: PathBuf,
    unique: Vec<u8>,
    requirement: String,
}
#[derive(PartialEq, Eq)]
struct Node {
    path: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    content: Option<ContentStamp>,
}
#[derive(PartialEq, Eq)]
struct ContentStamp {
    bytes: u64,
    changed: Option<(i64, i64)>,
    modified: (i64, i64),
}

fn same_generation(before: &Snapshot, after: &Snapshot) -> Result<(), Refusal> {
    if before == after {
        Ok(())
    } else {
        Err(Refusal::GenerationChanged)
    }
}
fn layout(executable: &Path) -> Result<PathBuf, Refusal> {
    let valid = executable.is_absolute()
        && executable.as_os_str().len() <= PATH_BYTES
        && executable.components().count() <= 64
        && executable
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && matches!(
            executable.file_name().and_then(|s| s.to_str()),
            Some("hgs" | "hgs-tray")
        );
    let macos = executable.parent().ok_or(Refusal::UnsupportedLayout)?;
    let contents = macos.parent().ok_or(Refusal::UnsupportedLayout)?;
    let bundle = contents.parent().ok_or(Refusal::UnsupportedLayout)?;
    if !valid
        || macos.file_name().is_none_or(|s| s != "MacOS")
        || contents.file_name().is_none_or(|s| s != "Contents")
        || bundle.extension().is_none_or(|s| s != "app")
    {
        return Err(Refusal::UnsupportedLayout);
    }
    Ok(bundle.into())
}

fn generation(bundle: &Path) -> Result<Vec<Node>, Refusal> {
    let mut paths: Vec<PathBuf> = bundle.ancestors().map(Path::to_path_buf).collect();
    paths.reverse();
    for leaf in [
        "Contents",
        "Contents/MacOS",
        "Contents/MacOS/hgs-tray",
        "Contents/MacOS/hgs",
        "Contents/Info.plist",
        "Contents/_CodeSignature",
        "Contents/_CodeSignature/CodeResources",
    ] {
        paths.push(bundle.join(leaf));
    }
    paths
        .into_iter()
        .map(|path| {
            let m = std::fs::symlink_metadata(&path).map_err(|_| Refusal::Metadata)?;
            if m.file_type().is_symlink() || !(m.is_dir() || m.is_file()) {
                return Err(Refusal::UnsupportedLayout);
            }
            // The fixed code/plist/seal leaves must be ordinary files, and
            // fixed directories must be directories. Never follow a link.
            let file = [
                "Contents/MacOS/hgs",
                "Contents/MacOS/hgs-tray",
                "Contents/Info.plist",
                "Contents/_CodeSignature/CodeResources",
            ]
            .iter()
            .any(|leaf| path == bundle.join(leaf));
            if file == m.is_dir() {
                return Err(Refusal::UnsupportedLayout);
            }
            Ok(Node {
                device: m.dev(),
                inode: m.ino(),
                mode: m.mode(),
                uid: m.uid(),
                // Parent directory churn is unrelated to this sealed pair.
                // Preserve every ancestor's identity, ownership, mode and
                // no-link checks, without recording its contents' timestamps.
                content: path.starts_with(bundle).then(|| ContentStamp {
                    bytes: m.len(),
                    // LaunchServices may update lastuseddate xattrs when an
                    // app is opened. Directory ctime is not sealed content.
                    // The bundle's mtime/size still detect membership changes;
                    // fixed code, plist and seal nodes keep their full stamps.
                    changed: (path != bundle).then(|| (m.ctime(), m.ctime_nsec())),
                    modified: (m.mtime(), m.mtime_nsec()),
                }),
                path,
            })
        })
        .collect()
}

fn pair(bundle: &Path, app: &Signature, service: &Signature) -> Result<(), Refusal> {
    if app.identifier != APP_ID
        || app.team != service.team
        || app.executable != bundle.join("Contents/MacOS/hgs-tray")
        || service.executable != bundle.join("Contents/MacOS/hgs")
        || app.unique == service.unique
    {
        return Err(Refusal::Signature);
    }
    Ok(())
}

fn snapshot() -> Result<Snapshot, Refusal> {
    snapshot_from(None)
}

fn snapshot_from(service_bundle: Option<&Path>) -> Result<Snapshot, Refusal> {
    autoreleasepool(|_| {
        let own = self_code()?;
        let anchor = super::requirement(APPLE).map_err(|_| Refusal::Signature)?;
        // A static path/DR is never used as a substitute for running validity.
        live_valid(&own, anchor.as_ptr())?;
        let info = information(own.ptr())?;
        let executable = info_path(&info)?;
        let bundle = if let Some(bundle) = service_bundle {
            // Only the service factory supplies its maintained installed
            // selection. No directory, PATH, plist or environment discovery.
            layout(&bundle.join("Contents/MacOS/hgs"))?
        } else {
            layout(&executable)?
        };
        let before = generation(&bundle)?;
        let app = static_signature(&bundle, anchor.as_ptr())?;
        let service = static_signature(&bundle.join("Contents/MacOS/hgs"), anchor.as_ptr())?;
        pair(&bundle, &app, &service)?;
        let selected = if service_bundle.is_some() {
            &service
        } else if executable == app.executable {
            &app
        } else if executable == service.executable {
            &service
        } else {
            return Err(Refusal::SelfChanged);
        };
        let exact = super::requirement(&selected.requirement).map_err(|_| Refusal::Signature)?;
        live_valid(&own, exact.as_ptr())?;
        let after_info = information(own.ptr())?;
        if info_path(&after_info)? != executable
            || info_unique(&after_info)? != selected.unique
            || info_text(&after_info, unsafe { kSecCodeInfoIdentifier })? != selected.identifier
            || info_text(&after_info, unsafe { kSecCodeInfoTeamIdentifier })? != selected.team
        {
            return Err(Refusal::SelfChanged);
        }
        let after = generation(&bundle)?;
        if before != after {
            return Err(Refusal::GenerationChanged);
        }
        live_valid(&own, exact.as_ptr())?;
        Ok(Snapshot {
            bundle,
            generation: after,
            app,
            service,
            self_executable: executable,
        })
    })
}

// All Create/Copy returns are private RAII objects; no CF object escapes.
struct Cf(NonNull<c_void>);
impl Cf {
    unsafe fn checked(status: i32, p: *mut c_void, kind: usize) -> Result<Self, Refusal> {
        let owned = NonNull::new(p).map(Self);
        if status != 0 {
            return Err(Refusal::Metadata);
        }
        let owned = owned.ok_or(Refusal::Metadata)?;
        // SAFETY: initialized owned Create/Copy return, checked before use.
        if unsafe { CFGetTypeID(owned.ptr()) } != kind {
            return Err(Refusal::Metadata);
        }
        Ok(owned)
    }
    fn ptr(&self) -> *const c_void {
        self.0.as_ptr()
    }
}
impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: exactly one release of an owned Create/Copy object.
        unsafe { CFRelease(self.ptr()) }
    }
}
fn self_code() -> Result<Cf, Refusal> {
    let mut out = std::ptr::null_mut();
    // SAFETY: default flags and initialized owned Copy out pointer.
    unsafe {
        let status = SecCodeCopySelf(0, &mut out);
        Cf::checked(status, out, super::SecCodeGetTypeID())
    }
}
fn live_valid(code: &Cf, requirement: *const c_void) -> Result<(), Refusal> {
    // SAFETY: retained, type-checked running code and requirement.
    if unsafe { super::SecCodeCheckValidity(code.ptr(), 0, requirement) } != 0 {
        Err(Refusal::SelfChanged)
    } else {
        Ok(())
    }
}
fn information(code: *const c_void) -> Result<Cf, Refusal> {
    let mut out = std::ptr::null_mut();
    // SAFETY: owned checked running/static code, signing data only.
    unsafe {
        let status = SecCodeCopySigningInformation(code, 1 << 1, &mut out);
        Cf::checked(status, out, CFDictionaryGetTypeID())
    }
}
fn field(info: &Cf, key: *const c_void, kind: usize) -> Result<*const c_void, Refusal> {
    if key.is_null() {
        return Err(Refusal::Metadata);
    }
    // SAFETY: type-checked retained dictionary; keys are Security constants.
    let value = unsafe { CFDictionaryGetValue(info.ptr(), key) };
    if value.is_null() || unsafe { CFGetTypeID(value) } != kind {
        Err(Refusal::Metadata)
    } else {
        Ok(value)
    }
}
fn text(value: *const c_void) -> Result<String, Refusal> {
    // SAFETY: callers pass a checked CFString held by an owned dictionary/Cf.
    let length = unsafe { CFStringGetLength(value) };
    if length <= 0 || length as usize > TEXT_BYTES {
        return Err(Refusal::Metadata);
    }
    for index in 0..length {
        // A C-string conversion must never silently truncate secured metadata.
        if unsafe { CFStringGetCharacterAtIndex(value, index) } == 0 {
            return Err(Refusal::Metadata);
        }
    }
    let mut bytes = vec![0u8; TEXT_BYTES + 1];
    if unsafe {
        CFStringGetCString(
            value,
            bytes.as_mut_ptr().cast(),
            bytes.len() as isize,
            0x08000100,
        )
    } == 0
    {
        return Err(Refusal::Metadata);
    }
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or(Refusal::Metadata)?;
    String::from_utf8(bytes[..end].to_vec()).map_err(|_| Refusal::Metadata)
}
fn info_text(info: &Cf, key: *const c_void) -> Result<String, Refusal> {
    text(field(info, key, unsafe { CFStringGetTypeID() })?)
}
fn info_path(info: &Cf) -> Result<PathBuf, Refusal> {
    let url = field(info, unsafe { kSecCodeInfoMainExecutable }, unsafe {
        CFURLGetTypeID()
    })?;
    let mut bytes = [0u8; PATH_BYTES + 1];
    // SAFETY: checked CFURL and bounded writable buffer.
    if unsafe { CFURLGetFileSystemRepresentation(url, 1, bytes.as_mut_ptr(), bytes.len() as isize) }
        == 0
    {
        return Err(Refusal::Metadata);
    }
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or(Refusal::Metadata)?;
    let path = std::str::from_utf8(&bytes[..end]).map_err(|_| Refusal::Metadata)?;
    Ok(PathBuf::from(path))
}
fn info_unique(info: &Cf) -> Result<Vec<u8>, Refusal> {
    let value = field(info, unsafe { kSecCodeInfoUnique }, unsafe {
        CFDataGetTypeID()
    })?;
    // Conservative first subset: only the native selected 20-byte CDHash.
    if unsafe { CFDataGetLength(value) } != 20 {
        return Err(Refusal::Metadata);
    }
    let p = unsafe { CFDataGetBytePtr(value) };
    if p.is_null() {
        return Err(Refusal::Metadata);
    }
    // SAFETY: checked 20-byte CFData retained by info.
    Ok(unsafe { std::slice::from_raw_parts(p, 20) }.to_vec())
}
fn static_signature(path: &Path, anchor: *const c_void) -> Result<Signature, Refusal> {
    let bytes = path.as_os_str().as_encoded_bytes();
    // SAFETY: bounded existing absolute UTF-8 path bytes; Create owns return.
    let url = unsafe {
        let p = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            0,
        );
        Cf::checked(0, p, CFURLGetTypeID())
    }?;
    let mut out = std::ptr::null_mut();
    let code = unsafe {
        let status = SecStaticCodeCreateWithPath(url.ptr(), 0, &mut out);
        Cf::checked(status, out, SecStaticCodeGetTypeID())
    }?;
    // Fresh object on every check. Whole app resources, nested code and all
    // architectures are validated; no allow-network or skip-content flags.
    if unsafe { SecStaticCodeCheckValidity(code.ptr(), STRICT, anchor) } != 0 {
        return Err(Refusal::Signature);
    }
    let info = information(code.ptr())?;
    let flags = field(&info, unsafe { kSecCodeInfoFlags }, unsafe {
        CFNumberGetTypeID()
    })?;
    let mut bits: i64 = 0;
    if unsafe { CFNumberGetValue(flags, 4, (&mut bits as *mut i64).cast()) } == 0
        || !(0..=u32::MAX as i64).contains(&bits)
        || bits & 2 != 0
    {
        return Err(Refusal::Signature);
    }
    let cms = field(&info, unsafe { kSecCodeInfoCMS }, unsafe {
        CFDataGetTypeID()
    })?;
    let certs = field(&info, unsafe { kSecCodeInfoCertificates }, unsafe {
        CFArrayGetTypeID()
    })?;
    if !(1..=65536).contains(&unsafe { CFDataGetLength(cms) })
        || !(1..=16).contains(&unsafe { CFArrayGetCount(certs) })
    {
        return Err(Refusal::Signature);
    }
    let identifier = info_text(&info, unsafe { kSecCodeInfoIdentifier })?;
    let team = info_text(&info, unsafe { kSecCodeInfoTeamIdentifier })?;
    if identifier.len() > 255
        || team.len() != 10
        || !team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(Refusal::Signature);
    }
    let unique = info_unique(&info)?;
    let mut out = std::ptr::null_mut();
    let dr = unsafe {
        let status = SecCodeCopyDesignatedRequirement(code.ptr(), 0, &mut out);
        Cf::checked(status, out, super::SecRequirementGetTypeID())
    }?;
    let mut out = std::ptr::null_mut();
    let dr_text = unsafe {
        let status = SecRequirementCopyString(dr.ptr(), 0, &mut out);
        Cf::checked(status, out, CFStringGetTypeID())
    }?;
    let hash = unique
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let requirement = format!("({}) and cdhash H\"{hash}\"", text(dr_text.ptr())?);
    if requirement.len() > super::REQUIREMENT_BYTES {
        return Err(Refusal::Metadata);
    }
    // Prepare BOTH exact expectations before any peer input. The first check
    // validated every architecture against the genuine signer. This second
    // check pins only the already selected native architecture's code hash.
    let exact = super::requirement(&requirement).map_err(|_| Refusal::Signature)?;
    if unsafe { SecStaticCodeCheckValidity(code.ptr(), STRICT & !1, exact.as_ptr()) } != 0 {
        return Err(Refusal::Signature);
    }
    Ok(Signature {
        identifier,
        team,
        executable: info_path(&info)?,
        unique,
        requirement,
    })
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecCodeInfoMainExecutable: *const c_void;
    static kSecCodeInfoIdentifier: *const c_void;
    static kSecCodeInfoTeamIdentifier: *const c_void;
    static kSecCodeInfoUnique: *const c_void;
    static kSecCodeInfoFlags: *const c_void;
    static kSecCodeInfoCMS: *const c_void;
    static kSecCodeInfoCertificates: *const c_void;
    fn SecCodeCopySelf(flags: u32, out: *mut *mut c_void) -> i32;
    fn SecCodeCopySigningInformation(code: *const c_void, flags: u32, out: *mut *mut c_void)
        -> i32;
    fn SecStaticCodeCreateWithPath(url: *const c_void, flags: u32, out: *mut *mut c_void) -> i32;
    fn SecStaticCodeGetTypeID() -> usize;
    fn SecStaticCodeCheckValidity(code: *const c_void, flags: u32, req: *const c_void) -> i32;
    fn SecCodeCopyDesignatedRequirement(
        code: *const c_void,
        flags: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn SecRequirementCopyString(req: *const c_void, flags: u32, out: *mut *mut c_void) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(value: *const c_void) -> isize;
    fn CFStringGetCharacterAtIndex(value: *const c_void, index: isize) -> u16;
    fn CFStringGetCString(value: *const c_void, buffer: *mut i8, size: isize, encoding: u32) -> u8;
    fn CFURLGetTypeID() -> usize;
    fn CFURLGetFileSystemRepresentation(
        url: *const c_void,
        resolve: u8,
        p: *mut u8,
        size: isize,
    ) -> u8;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: *const c_void,
        p: *const u8,
        size: isize,
        directory: u8,
    ) -> *mut c_void;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: *const c_void, kind: isize, value: *mut c_void) -> u8;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: *const c_void) -> isize;
}

#[cfg(test)]
mod tests;
