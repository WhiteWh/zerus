//! The ptrace-inaccessible per-user service manager is a kernel-identified ancestry
//! boundary. Names, command lines and arbitrary unreadable ancestors are not.
use crate::platform;
use std::{fs, io::Read};

const CAP: u64 = 16 * 1024;

pub(super) fn boundary(pid: u32, uid: u32) -> bool {
    let Some(before) = platform::process_identity(pid) else {
        return false;
    };
    if before.parent != 1 {
        return false;
    }
    let image = fs::read_link(format!("/proc/{pid}/exe"));
    let status = bounded(pid, "status");
    let cgroup = bounded(pid, "cgroup");
    matches(
        image.as_ref().err().and_then(|e| e.raw_os_error()),
        uid,
        status.as_deref(),
        cgroup.as_deref(),
    ) && platform::process_identity(pid).as_ref() == Some(&before)
}

fn bounded(pid: u32, name: &str) -> Option<String> {
    let mut text = String::new();
    fs::File::open(format!("/proc/{pid}/{name}"))
        .ok()?
        .take(CAP + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= CAP).then_some(text)
}

fn matches(errno: Option<i32>, uid: u32, status: Option<&str>, cgroup: Option<&str>) -> bool {
    if !matches!(errno, Some(libc::EACCES | libc::EPERM)) {
        return false;
    }
    let Some(status) = status else { return false };
    if !super::tracer_absent(status.as_bytes()) {
        return false;
    }
    let mut parent = status.lines().filter_map(|l| l.strip_prefix("PPid:"));
    if parent.next().map(str::trim) != Some("1") || parent.next().is_some() {
        return false;
    }
    let mut ids = status.lines().filter_map(|l| l.strip_prefix("Uid:"));
    let Some(ids_line) = ids.next() else {
        return false;
    };
    let fields: Vec<_> = ids_line.split_whitespace().collect();
    if ids.next().is_some()
        || fields.len() != 4
        || fields.iter().any(|s| s.parse::<u32>() != Ok(uid))
    {
        return false;
    }
    cgroup
        == Some(format!("0::/user.slice/user-{uid}.slice/user@{uid}.service/init.scope\n").as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    const STATUS: &str =
        "Name:\tuntrusted-name\nPPid:\t1\nUid:\t1000\t1000\t1000\t1000\nTracerPid:\t0\n";
    const GROUP: &str = concat!(
        "0::/user.slice/user-1000.slice/user",
        "@",
        "1000.service/init.scope\n"
    );

    #[test]
    fn only_exact_unreadable_kernel_manager_ends_the_walk() {
        for errno in [libc::EACCES, libc::EPERM] {
            assert!(matches(Some(errno), 1000, Some(STATUS), Some(GROUP)));
        }
        for errno in [None, Some(libc::ENOENT), Some(libc::EIO)] {
            // A readable manager still follows the ordinary image/env checks.
            assert!(!matches(errno, 1000, Some(STATUS), Some(GROUP)));
        }
        for status in [
            STATUS.replace("TracerPid:\t0", "TracerPid:\t42"),
            STATUS.replace("TracerPid:\t0\n", ""),
        ] {
            assert!(!matches(
                Some(libc::EACCES),
                1000,
                Some(&status),
                Some(GROUP)
            ));
        }
        for status in [
            None,
            Some(""),
            Some("PPid: 1\nUid: 1000 1000 1000\n"),
            Some("PPid: 2\nUid: 1000 1000 1000 1000\n"),
            Some("PPid: 1\nUid: 1000 0 1000 1000\n"),
            Some("PPid: 1\nUid: 1000 1000 0 1000\n"),
            Some("PPid: 1\nUid: 1000 1000 1000 0\n"),
            Some("PPid: 1\nUid: 1000 1000 1000 1000\nUid: 1000 1000 1000 1000\n"),
            Some("PPid: 1\nPPid: 1\nUid: 1000 1000 1000 1000\n"),
        ] {
            let status = status.map(|text| format!("{text}TracerPid:\t0\n"));
            assert!(!matches(
                Some(libc::EACCES),
                1000,
                status.as_deref(),
                Some(GROUP)
            ));
        }
        assert!(!matches(
            Some(libc::EACCES),
            1001,
            Some(STATUS),
            Some(GROUP)
        ));
        for group in [
            None,
            Some(""),
            Some(concat!(
                "1:name=systemd:/user.slice/user-1000.slice/user",
                "@",
                "1000.service/init.scope\n"
            )),
            Some(concat!(
                "0::/user.slice/user-1000.slice/user",
                "@",
                "1000.service/app.slice/example.service\n"
            )),
            Some(concat!(
                "0::/user.slice/user-1001.slice/user",
                "@",
                "1001.service/init.scope\n"
            )),
            Some(concat!(
                "0::/user.slice/user-1000.slice/user",
                "@",
                "1000.service/init.scope/child\n"
            )),
        ] {
            assert!(!matches(Some(libc::EACCES), 1000, Some(STATUS), group));
        }
    }
}
