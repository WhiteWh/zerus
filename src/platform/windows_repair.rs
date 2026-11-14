//! Making an entry this user owns private again, in place.
//!
//! A folder below the profile normally inherits an owner-only ACL, but not
//! always: a folder on a second drive inherits Authenticated Users Modify,
//! and Codex's Windows sandbox grants its own group read access on
//! `~/.local`, which every folder below inherits. Zerus then refused its own
//! state ("must be private") with no way out but icacls. `repair_private`
//! replaces such an entry's DACL with a protected one that grants only this
//! user, as Unix makes a folder 0700, and only when this user owns the entry.
use super::{file_information, process::current_user, verify_private, Private};
use crate::platform::PrivateRepair;
use std::fs;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    GetAce, GetLengthSid, IsWellKnownSid, WinBuiltinAdministratorsSid, WinCreatorOwnerRightsSid, WinLocalSystemSid,
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
    INHERIT_ONLY_ACE, NO_INHERITANCE, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    READ_CONTROL, WRITE_DAC,
};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
struct Acl(*mut ACL);
impl Drop for Acl {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0 as *mut _);
        }
    }
}

/// Makes the entry at `path` private when this user owns it and it is only
/// too open: its DACL becomes protected and grants this user alone. It never
/// follows a link; the content and the owner stay. Another owner, a reparse
/// point, another kind of entry or a second link is refused and the entry is
/// left as it is.
pub fn repair_private(path: &Path, kind: Private) -> io::Result<PrivateRepair> {
    let open = |access: u32| {
        fs::OpenOptions::new()
            .access_mode(access)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    };
    // Looked at first through a handle that changes nothing.
    let entry = open(READ_CONTROL | FILE_READ_ATTRIBUTES)?;
    let info = file_information(&entry)?;
    let attributes = info.dwFileAttributes;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(PrivateRepair::Refused("it is a link or reparse point".into()));
    }
    let directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    match kind {
        Private::Directory if !directory => return Ok(PrivateRepair::Refused("it is not a folder".into())),
        Private::File if directory => return Ok(PrivateRepair::Refused("it is a folder, not a file".into())),
        Private::File if info.nNumberOfLinks != 1 => {
            return Ok(PrivateRepair::Refused(format!("the file has {} links", info.nNumberOfLinks)));
        }
        _ => {}
    }
    let user = current_user().ok_or_else(|| io::Error::other("this user's identity is unavailable"))?;
    let (owner, open_aces) = security(entry.as_raw_handle() as HANDLE, user)?;
    match owner {
        Some(owner) if owner == user => {}
        Some(owner) => return Ok(PrivateRepair::Refused(format!("it is owned by {}", account_label(&owner)))),
        None => return Ok(PrivateRepair::Refused("it has no owner".into())),
    }
    if open_aces.is_empty() {
        return Ok(PrivateRepair::Private);
    }
    // Changed through a second handle that names the same file: never one
    // renamed into its place meanwhile.
    let writer = match open(READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES) {
        Ok(writer) => writer,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Ok(PrivateRepair::Refused(format!("this user cannot change its permissions ({error})")));
        }
        Err(error) => return Err(error),
    };
    let same = file_information(&writer)?;
    if (same.dwVolumeSerialNumber, same.nFileIndexHigh, same.nFileIndexLow)
        != (info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow)
        || same.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Ok(PrivateRepair::Refused("it changed while it was checked".into()));
    }
    let acl = owner_only_acl(user, kind == Private::Directory)?;
    let code = unsafe {
        SetSecurityInfo(
            writer.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl.0,
            std::ptr::null(),
        )
    };
    if code != ERROR_SUCCESS {
        let error = io::Error::from_raw_os_error(code as i32);
        return Ok(PrivateRepair::Refused(format!("its permissions could not be changed ({error})")));
    }
    drop(writer);
    drop(entry);
    if !verify_private(path, kind)? {
        return Ok(PrivateRepair::Refused("it is still open after the repair".into()));
    }
    Ok(PrivateRepair::Repaired(open_aces))
}

/// An ACL with one entry: this user's FullControl, which a folder's new files
/// and folders inherit when `container`.
fn owner_only_acl(user: &[u8], container: bool) -> io::Result<Acl> {
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: if container { SUB_CONTAINERS_AND_OBJECTS_INHERIT } else { NO_INHERITANCE },
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: user.as_ptr() as *mut u16,
            ..Default::default()
        },
    };
    let mut acl: *mut ACL = std::ptr::null_mut();
    let code = unsafe { SetEntriesInAclW(1, &access, std::ptr::null(), &mut acl) };
    if code != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    Ok(Acl(acl))
}

/// The entry's owner and the accesses its DACL grants anyone but this user,
/// SYSTEM, Administrators and OWNER RIGHTS.
fn security(handle: HANDLE, user: &[u8]) -> io::Result<(Option<Vec<u8>>, Vec<String>)> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let code = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let _descriptor = Descriptor(descriptor);
    let owner = if owner.is_null() { None } else { Some(unsafe { sid_bytes(owner) }.to_vec()) };
    Ok((owner, unsafe { open_aces(dacl, user) }?))
}

/// # Safety
/// `dacl` is null or points to a valid ACL.
unsafe fn open_aces(dacl: *mut ACL, user: &[u8]) -> io::Result<Vec<String>> {
    if dacl.is_null() {
        return Ok(vec!["Everyone (no DACL)".into()]);
    }
    let mut open = Vec::new();
    for index in 0..u32::from((*dacl).AceCount) {
        let mut ace = std::ptr::null_mut();
        if GetAce(dacl, index, &mut ace) == 0 {
            return Err(io::Error::last_os_error());
        }
        let header = &*(ace as *const ACE_HEADER);
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 || header.AceType == ACCESS_DENIED_ACE_TYPE {
            continue;
        }
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            open.push(format!("an access entry of type {}", header.AceType));
            continue;
        }
        let sid = std::ptr::addr_of!((*(ace as *const ACCESS_ALLOWED_ACE)).SidStart) as PSID;
        let trusted = sid_bytes(sid) == user
            || IsWellKnownSid(sid, WinLocalSystemSid) != 0
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
            || IsWellKnownSid(sid, WinCreatorOwnerRightsSid) != 0;
        if !trusted {
            let label = account_label(sid_bytes(sid));
            if !open.contains(&label) {
                open.push(label);
            }
        }
    }
    Ok(open)
}

/// `DOMAIN\name (S-1-...)`, or the SID alone when no account is found.
fn account_label(sid: &[u8]) -> String {
    use windows_sys::Win32::Security::{LookupAccountSidW, SID_NAME_USE};
    let text = sid_string(sid);
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut kind: SID_NAME_USE = 0;
    let found = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid.as_ptr() as PSID,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    } != 0;
    if !found {
        return text;
    }
    let name = String::from_utf16_lossy(&name[..name_len as usize]);
    let domain = String::from_utf16_lossy(&domain[..domain_len as usize]);
    if domain.is_empty() { format!("{name} ({text})") } else { format!("{domain}\\{name} ({text})") }
}

fn sid_string(sid: &[u8]) -> String {
    let revision = sid.first().copied().unwrap_or(1);
    let count = sid.get(1).copied().unwrap_or(0) as usize;
    let authority = (2..8).fold(0u64, |value, index| (value << 8) | u64::from(sid.get(index).copied().unwrap_or(0)));
    let subs: Vec<String> = (0..count)
        .map(|index| {
            let at = 8 + index * 4;
            let bytes = [0, 1, 2, 3].map(|offset| sid.get(at + offset).copied().unwrap_or(0));
            u32::from_le_bytes(bytes).to_string()
        })
        .collect();
    format!("S-{revision}-{authority}-{}", subs.join("-"))
}

/// # Safety
/// `sid` points to a valid SID.
unsafe fn sid_bytes<'a>(sid: PSID) -> &'a [u8] {
    std::slice::from_raw_parts(sid as *const u8, GetLengthSid(sid) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_and_file_this_user_owns_are_made_private_and_a_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("state");
        fs::create_dir(&folder).unwrap();
        let file = folder.join("record.json");
        fs::write(&file, b"{}").unwrap();
        for (path, kind) in [(&folder, Private::Directory), (&file, Private::File)] {
            match repair_private(path, kind).unwrap() {
                PrivateRepair::Private | PrivateRepair::Repaired(_) => {}
                refused => panic!("{path:?}: {refused:?}"),
            }
            assert!(verify_private(path, kind).unwrap(), "{path:?}");
            assert_eq!(repair_private(path, kind).unwrap(), PrivateRepair::Private, "{path:?} again");
        }
        assert!(matches!(repair_private(&folder, Private::File).unwrap(), PrivateRepair::Refused(_)));
        assert!(matches!(repair_private(&file, Private::Directory).unwrap(), PrivateRepair::Refused(_)));
    }
}
