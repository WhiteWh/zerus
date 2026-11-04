//! Service-side selection and the original unreaped Child only. There is no
//! caller-provided PID/requirement or factory for serialized observations.
use super::*;
use objc2::rc::Retained;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use std::process::Child;

pub struct ServiceSelection {
    original: Snapshot,
}
impl std::fmt::Debug for ServiceSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceSelection { .. }")
    }
}

/// The service may run outside the app, but its ACTUAL running signature and
/// native CDHash must match the bundled hgs in the selected genuine sealed pair.
/// The bundle is supplied only by maintained installer selection, never scanning.
/// Off actor/UI threads: retain the actual worker across timeout/cancellation.
pub fn select_for_service(bundle: &Path) -> Result<ServiceSelection, Refusal> {
    snapshot_from(Some(bundle)).map(|original| ServiceSelection { original })
}

/// A point-in-time native observation, not an exec fence, lease or permission.
pub struct ChildObservation<'a> {
    _selection: &'a ServiceSelection,
    _child: &'a Child,
    _birth: Birth,
}
impl std::fmt::Debug for ChildObservation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChildObservation { .. }")
    }
}

impl ServiceSelection {
    /// A sealed data capability refuses old apps BEFORE launching an unknown
    /// entry flag. It grants no channel, writer lease or posting authority.
    pub fn supports_notification_host(&self) -> Result<bool, Refusal> {
        use std::io::Read;
        self.revalidate()?;
        let file = std::fs::File::open(self.original.bundle.join("Contents/Info.plist"))
            .map_err(|_| Refusal::Metadata)?;
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| Refusal::Metadata)?;
        if bytes.len() > 65536 {
            return Err(Refusal::Metadata);
        }
        let plist = plist::Value::from_reader(std::io::Cursor::new(bytes))
            .map_err(|_| Refusal::Metadata)?;
        let supported = plist
            .as_dictionary()
            .and_then(|d| d.get(crate::desktop::notifications::CAPABILITY_KEY))
            .and_then(plist::Value::as_string)
            == Some(crate::desktop::notifications::ABI);
        self.revalidate()?;
        Ok(supported)
    }
    /// Only the exact native-validated selection's executable, for the factory.
    /// This path alone is not authority; revalidate before each queued dispatch.
    pub fn host_executable(&self) -> &Path {
        &self.original.app.executable
    }
    pub fn revalidate(&self) -> Result<(), Refusal> {
        same_generation(&self.original, &snapshot_from(Some(&self.original.bundle))?)
    }
    /// Borrow the factory's actual Child exclusively: try_wait's cached exit
    /// status rejects reaped handles even if their old PID has since been reused.
    /// A finished child may be reaped here; its status stays cached in Child.
    pub fn observe_original_child<'a>(
        &'a self,
        child: &'a mut Child,
    ) -> Result<ChildObservation<'a>, Refusal> {
        autoreleasepool(|_| {
            self.revalidate()?;
            let before = birth(child)?;
            verify_child_code(child, &self.original.app)?;
            if birth(child)? != before {
                return Err(Refusal::Child);
            }
            // A second fresh guest lookup avoids relying on a cached SecCode
            // object across exit/exec. Actual dispatch still rechecks currentness.
            verify_child_code(child, &self.original.app)?;
            if birth(child)? != before {
                return Err(Refusal::Child);
            }
            self.revalidate()?;
            Ok(ChildObservation {
                _selection: self,
                _child: child,
                _birth: before,
            })
        })
    }
}

#[derive(PartialEq, Eq)]
struct Birth {
    pid: u32,
    parent: u32,
    uid: u32,
    real_uid: u32,
    seconds: u64,
    microseconds: u64,
}
fn birth(child: &mut Child) -> Result<Birth, Refusal> {
    if child.try_wait().map_err(|_| Refusal::Child)?.is_some() {
        return Err(Refusal::Child);
    }
    let pid = child.id();
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(Refusal::Child);
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: exact public PROC_PIDTBSDINFO layout and initialized bounded output.
    let read = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read != size
        || info.pbi_pid != pid
        || info.pbi_ppid != std::process::id()
        || info.pbi_uid != unsafe { libc::geteuid() }
        || info.pbi_ruid != unsafe { libc::getuid() }
        || info.pbi_status == 5
        || info.pbi_start_tvsec == 0
        || info.pbi_start_tvusec >= 1_000_000
    {
        return Err(Refusal::Child);
    }
    Ok(Birth {
        pid,
        parent: info.pbi_ppid,
        uid: info.pbi_uid,
        real_uid: info.pbi_ruid,
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

fn verify_child_code(child: &Child, expected: &Signature) -> Result<(), Refusal> {
    // Public kSecGuestAttributePid is used ONLY for the actual retained Child,
    // after its direct-parent/birth/UID and unreaped state have been checked.
    let key =
        unsafe { (kSecGuestAttributePid as *const NSString).as_ref() }.ok_or(Refusal::Metadata)?;
    let pid = NSNumber::numberWithInt(child.id() as i32);
    let attributes = NSDictionary::from_slices(&[key], &[&*pid]);
    let mut output = std::ptr::null_mut();
    let code = unsafe {
        let status = super::super::SecCodeCopyGuestWithAttributes(
            std::ptr::null(),
            Retained::as_ptr(&attributes).cast(),
            0,
            &mut output,
        );
        Cf::checked(status, output, super::super::SecCodeGetTypeID())
    }?;
    let requirement =
        super::super::requirement(&expected.requirement).map_err(|_| Refusal::Signature)?;
    live_valid(&code, requirement.as_ptr())?;
    let info = information(code.ptr())?;
    if info_path(&info)? != expected.executable
        || info_unique(&info)? != expected.unique
        || info_text(&info, unsafe { kSecCodeInfoIdentifier })? != expected.identifier
        || info_text(&info, unsafe { kSecCodeInfoTeamIdentifier })? != expected.team
    {
        return Err(Refusal::Child);
    }
    live_valid(&code, requirement.as_ptr())
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecGuestAttributePid: *const c_void;
}

#[cfg(test)]
mod tests;
