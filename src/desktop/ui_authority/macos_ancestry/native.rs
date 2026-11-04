use super::*;
use std::{
    fs::OpenOptions,
    mem::{size_of, MaybeUninit},
    os::unix::{fs::MetadataExt, fs::OpenOptionsExt},
};

// XNU proc_info_private.h declares this structure as API and asserts 56 bytes.
// The public PIDINFO operation permits this flavor without same-UID privilege:
// https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/sys/proc_info_private.h
const PROC_PIDUNIQIDENTIFIERINFO: i32 = 17;
#[repr(C)]
struct NativeUnique {
    executable: [u8; 16],
    birth: u64,
    parent_birth: u64,
    version: i32,
    _original_parent_version: i32,
    _reserved: [u64; 2],
}
const _: [(); 56] = [(); size_of::<NativeUnique>()];

fn info<T>(pid: u32, flavor: i32) -> Result<T> {
    let mut value = MaybeUninit::<T>::uninit();
    // SAFETY: exactly sized native output, read only after a complete result.
    let used = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            flavor,
            0,
            value.as_mut_ptr().cast(),
            size_of::<T>() as i32,
        )
    };
    let error = std::io::Error::last_os_error();
    ensure!(
        used == size_of::<T>() as i32,
        "macOS ancestry metadata unavailable (pid {pid}, flavor {flavor}, bytes {used}, errno {:?})",
        error.raw_os_error()
    );
    // SAFETY: the native call initialized the complete declared structure.
    Ok(unsafe { value.assume_init() })
}

fn system_image(image: &str) -> Result<Option<SystemImage>> {
    if !matches!(image, "/usr/bin/login" | "/sbin/launchd") {
        return Ok(None);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(image)?;
    let metadata = file.metadata()?;
    Ok(Some(SystemImage {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode(),
        len: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    }))
}

fn snapshot(pid: u32) -> Result<Snapshot> {
    let short: libc::proc_bsdshortinfo = info(pid, libc::PROC_PIDT_SHORTBSDINFO)?;
    let unique: NativeUnique = info(pid, PROC_PIDUNIQIDENTIFIERINFO)?;
    let mut path = [0u8; 4096];
    // SAFETY: bounded writable native path buffer for this PID.
    let used =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    ensure!(
        used > 0 && (used as usize) < path.len() && path[used as usize] == 0,
        "macOS ancestry native image unavailable (pid {pid})"
    );
    let image = std::str::from_utf8(&path[..used as usize])?.to_owned();
    // SAFETY: read-only native session lookup; no caller-supplied session ID.
    let session = unsafe { libc::getsid(pid as i32) };
    ensure!(
        short.pbsi_pid == pid && session > 0,
        "macOS ancestry session unavailable"
    );
    let protected = system_image(&image)?;
    Ok(Snapshot {
        pid,
        parent: short.pbsi_ppid,
        group: short.pbsi_pgid,
        session: session as u32,
        status: short.pbsi_status,
        command: short.pbsi_comm.map(|v| v as u8),
        flags: short.pbsi_flags,
        uids: [short.pbsi_uid, short.pbsi_ruid, short.pbsi_svuid],
        gids: [short.pbsi_gid, short.pbsi_rgid, short.pbsi_svgid],
        unique: Unique {
            executable: unique.executable,
            birth: unique.birth,
            parent_birth: unique.parent_birth,
            version: unique.version,
        },
        image,
        system_image: protected,
    })
}

pub(in crate::desktop::ui_authority) fn inspect(
    pid: u32,
    until: Instant,
) -> Result<Vec<super::super::Process>> {
    let owned = walk(pid, unsafe { libc::geteuid() }, until, snapshot, |value| {
        let process = super::super::Process::read(value.pid)?;
        ensure!(
            process.identity.parent == value.parent && process.executable == value.image,
            "macOS own-user ancestry identity changed"
        );
        ensure!(
            !super::super::agent_environment(value.pid)?
                && !super::super::agent_image(&process.executable),
            "agent-descended frontend request refused"
        );
        ensure!(process.current(), "frontend ancestry changed");
        Ok(process)
    })?;
    ensure!(
        Instant::now() < until && owned.iter().all(super::super::Process::current),
        "macOS own-user ancestry changed"
    );
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_native_own_and_system_init_metadata_have_the_declared_identities() {
        let pid = std::process::id();
        let owner = unsafe { libc::geteuid() };
        let own = snapshot(pid).unwrap();
        assert_eq!(classify(&own, owner).unwrap(), Role::Own);
        let process = super::super::super::Process::read(pid).unwrap();
        assert_eq!(process.identity.parent, own.parent);
        assert_eq!(process.executable, own.image);
        assert!(process.current());
        let current = snapshot(pid).unwrap();
        assert_eq!(classify(&current, owner).unwrap(), Role::Own);
        assert!(own.same_identity(&current));
        let init = snapshot(1).unwrap();
        assert_eq!(classify(&init, owner).unwrap(), Role::Init);
        let current = snapshot(1).unwrap();
        assert_eq!(classify(&current, owner).unwrap(), Role::Init);
        assert!(init.same_identity(&current));
    }

    #[test]
    fn unavailable_native_identity_is_an_error_never_an_ancestry_boundary() {
        assert!(snapshot(u32::MAX).is_err());
    }
}
