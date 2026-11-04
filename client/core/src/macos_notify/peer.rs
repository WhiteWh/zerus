//! Read-only, point-in-time observations on a host-retained Mac connection.
//!
//! The requirement MUST come from G1's pinned trusted installed selection,
//! never peer input. Parsing a requirement does not authenticate its source.
//! G1 owns that trust, current connection/challenge binding and revalidation.
//! An observation is neither a verified peer nor a lease/native-job permit.
//!
//! Calls are synchronous and may block in Security. Run off UI/actor threads;
//! the host must retain its connection, actual worker and admission through
//! completion, including past a bounded outer wait or dropped receiver. This
//! helper spawns no worker, opens no connection and implements no timeout/drain.
use std::{
    ffi::c_void, marker::PhantomData, os::fd::AsRawFd, os::unix::net::UnixStream, ptr::NonNull,
};

use objc2::rc::{autoreleasepool, Retained};
use objc2_foundation::{NSData, NSDictionary, NSString};
use sha2::{Digest, Sha256};

pub mod installation;

pub const REQUIREMENT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    RequirementSize,
    RequirementNul,
    RequirementInvalid,
    AuditUnavailable,
    AuditMalformed,
    LiveCodeUnavailable,
    LiveCodeRejected,
    PeerChanged,
}

/// Opaque evidence for the supplied requirement and retained connection only.
/// The borrow is not a promise that peer code cannot exec or exit afterward.
pub struct Observation<'connection> {
    requirement_sha256: [u8; 32],
    _connection: PhantomData<&'connection UnixStream>,
}

impl Observation<'_> {
    pub fn requirement_sha256(&self) -> [u8; 32] {
        self.requirement_sha256
    }
}

impl std::fmt::Debug for Observation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Observation { .. }")
    }
}

// Public Darwin audit_token_t layout; interpret PID fields only through BSM.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct AuditToken {
    values: [u32; 8],
}
const _: () = assert!(std::mem::size_of::<AuditToken>() == 32);

#[link(name = "bsm")]
unsafe extern "C" {
    fn audit_token_to_pid(token: AuditToken) -> libc::pid_t;
    fn audit_token_to_pidversion(token: AuditToken) -> libc::c_int;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecGuestAttributeAudit: *const c_void;
    fn SecRequirementCreateWithString(
        text: *const c_void,
        flags: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn SecCodeCopyGuestWithAttributes(
        host: *const c_void,
        attributes: *const c_void,
        flags: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn SecCodeCheckValidity(code: *const c_void, flags: u32, requirement: *const c_void) -> i32;
    fn SecCodeGetTypeID() -> usize;
    fn SecRequirementGetTypeID() -> usize;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
    // Native-word CFTypeID on supported 64-bit Macs, matching qt_lock.rs.
    fn CFGetTypeID(value: *const c_void) -> usize;
}

/// Own one Create/Copy return locally; no Security object leaves this module.
struct OwnedCf {
    pointer: NonNull<c_void>,
    #[cfg(test)]
    _count: tests::Resource,
}
impl OwnedCf {
    /// SAFETY: output is null or an owned CF Create/Copy object.
    unsafe fn checked(
        status: i32,
        output: *mut c_void,
        expected_type: usize,
        refusal: Refusal,
    ) -> Result<Self, Refusal> {
        let value = NonNull::new(output).map(|pointer| Self {
            pointer,
            #[cfg(test)]
            _count: tests::Resource::new(),
        });
        if status != 0 {
            return Err(refusal);
        }
        let value = value.ok_or(refusal)?;
        // SAFETY: a non-null owned CF object, checked before any typed use.
        if unsafe { CFGetTypeID(value.as_ptr()) } != expected_type {
            return Err(refusal);
        }
        Ok(value)
    }
    fn as_ptr(&self) -> *const c_void {
        self.pointer.as_ptr()
    }
}
impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: exactly one release of our Create/Copy ownership.
        unsafe { CFRelease(self.as_ptr()) };
    }
}

fn audit(connection: &UnixStream) -> Result<AuditToken, Refusal> {
    let mut token = AuditToken { values: [0; 8] };
    let mut length = std::mem::size_of::<AuditToken>() as libc::socklen_t;
    // SAFETY: live borrowed FD and correctly sized writable token/length.
    let status = unsafe {
        libc::getsockopt(
            connection.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            (&mut token as *mut AuditToken).cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(Refusal::AuditUnavailable);
    }
    check_audit(token, length)
}

fn check_audit(token: AuditToken, length: libc::socklen_t) -> Result<AuditToken, Refusal> {
    if length as usize != std::mem::size_of::<AuditToken>() {
        return Err(Refusal::AuditMalformed);
    }
    // SAFETY: checked public audit_token_t passed by value to BSM accessors.
    check_lifetime(unsafe { audit_token_to_pid(token) }, unsafe {
        audit_token_to_pidversion(token)
    })?;
    Ok(token)
}

fn check_lifetime(pid: libc::pid_t, version: libc::c_int) -> Result<(), Refusal> {
    if pid <= 0 || version == 0 {
        Err(Refusal::AuditMalformed)
    } else {
        Ok(())
    }
}

fn requirement(text: &str) -> Result<OwnedCf, Refusal> {
    let text = NSString::from_str(text);
    let mut output = std::ptr::null_mut();
    // SAFETY: NSString/CFString are toll-free bridged; default flags are zero.
    let status =
        unsafe { SecRequirementCreateWithString(Retained::as_ptr(&text).cast(), 0, &mut output) };
    // SAFETY: the initialized Create out pointer is null or owned.
    unsafe {
        OwnedCf::checked(
            status,
            output,
            SecRequirementGetTypeID(),
            Refusal::RequirementInvalid,
        )
    }
}

struct Attributes {
    dictionary: Retained<NSDictionary<NSString, NSData>>,
    #[cfg(test)]
    _count: tests::Resource,
}
impl Attributes {
    fn new(token: &AuditToken) -> Result<Self, Refusal> {
        // SAFETY: static Security CFString/NSString toll-free bridge. Refuse null.
        let key = unsafe { (kSecGuestAttributeAudit as *const NSString).as_ref() }
            .ok_or(Refusal::LiveCodeUnavailable)?;
        // SAFETY: token is initialized, 32 bytes, with no padding; NSData copies.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (token as *const AuditToken).cast(),
                std::mem::size_of::<AuditToken>(),
            )
        };
        let data = NSData::with_bytes(bytes);
        let dictionary = NSDictionary::from_slices(&[key], &[&*data]);
        Ok(Self {
            dictionary,
            #[cfg(test)]
            _count: tests::Resource::new(),
        })
    }
}

fn live_check(attributes: &Attributes, requirement: &OwnedCf) -> Result<(), Refusal> {
    let mut output = std::ptr::null_mut();
    // SAFETY: null selects the kernel host; a retained NSDictionary/CFDictionary
    // contains the exact copied audit data, and output storage is initialized.
    let status = unsafe {
        SecCodeCopyGuestWithAttributes(
            std::ptr::null(),
            Retained::as_ptr(&attributes.dictionary).cast(),
            0,
            &mut output,
        )
    };
    // SAFETY: the initialized Copy out pointer is null or owned.
    let code = unsafe {
        OwnedCf::checked(
            status,
            output,
            SecCodeGetTypeID(),
            Refusal::LiveCodeUnavailable,
        )
    }?;
    // SAFETY: live locally owned SecCode and SecRequirement, default flags.
    if unsafe { SecCodeCheckValidity(code.as_ptr(), 0, requirement.as_ptr()) } != 0 {
        return Err(Refusal::LiveCodeRejected);
    }
    Ok(())
}

/// Observe live code against the exact caller-pinned requirement. No PID/path
/// fallback, peer lookup by claimed identity, reusable permit or native effect.
pub fn observe<'connection>(
    connection: &'connection UnixStream,
    expected_requirement: &str,
) -> Result<Observation<'connection>, Refusal> {
    observe_inner(connection, expected_requirement, || {}, audit)
}

fn observe_inner<'connection>(
    connection: &'connection UnixStream,
    text: &str,
    after_check: impl FnOnce(),
    audit_again: impl FnOnce(&UnixStream) -> Result<AuditToken, Refusal>,
) -> Result<Observation<'connection>, Refusal> {
    if text.is_empty() || text.len() > REQUIREMENT_BYTES {
        return Err(Refusal::RequirementSize);
    }
    if text.contains('\0') {
        return Err(Refusal::RequirementNul);
    }
    autoreleasepool(|_| {
        let requirement = requirement(text)?;
        let before = audit(connection)?;
        let attributes = Attributes::new(&before)?;
        live_check(&attributes, &requirement)?;
        after_check();
        if audit_again(connection)? != before {
            return Err(Refusal::PeerChanged);
        }
        // A fresh live lookup after the second token read narrows the exit race.
        // Even this is point-in-time evidence, never an exec fence or a lease.
        live_check(&attributes, &requirement)?;
        Ok(Observation {
            requirement_sha256: Sha256::digest(text.as_bytes()).into(),
            _connection: PhantomData,
        })
    })
}

#[cfg(test)]
mod tests;
