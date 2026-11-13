//! A live DeepSeek host recognized by its own command line (zerus-ppd26.21).
//!
//! hgs starts a host with one fixed shape (`state/dsh.rs`'s `host_command`):
//! `--profile hgs [--from-default-profile web] --patch
//! <state>/dsh/hosts/<id>/hgs.patch.json --no-open --host 127.0.0.1 --port 0`,
//! where `<id>` is the host folder's sixteen lowercase hex digits. A process
//! whose argument vector carries exactly that ordered shape is a host hgs
//! started, whether or not its records survive; a descendant can delete the
//! records but cannot rewrite an ancestor's argument vector.
//!
//! Only the argument vector is read: `/proc/<pid>/cmdline` on Linux and the
//! argv part of `KERN_PROCARGS2` on macOS, never environments. Other
//! platforms keep their previous behaviour and read nothing here.
use std::path::{Component, Path};

/// The number of hex digits of `host_dir`'s agent-home digest.
const HOST_ID_LEN: usize = 16;

/// Whether `tokens` is the argument vector of a host started under `state`:
/// the exact shape above, immediately after whatever launched `dsh` (a node
/// or shim prefix is allowed, and no token before the shape is inspected).
pub(super) fn host(tokens: &[Vec<u8>], state: &Path) -> bool {
    let expected = state.join("dsh").join("hosts");
    for start in 0..tokens.len() {
        if shape(&tokens[start..], &expected) {
            return true;
        }
    }
    false
}

/// The exact ordered shape, beginning at `tokens[0]`.
fn shape(tokens: &[Vec<u8>], hosts: &Path) -> bool {
    let Some(at) = step(tokens, 0, b"--profile") else {
        return false;
    };
    let Some(at) = step(tokens, at, b"hgs") else {
        return false;
    };
    // The writer adds this pair only while the profile does not exist yet.
    let at = if tokens
        .get(at)
        .is_some_and(|token| token == b"--from-default-profile")
    {
        match step(tokens, at + 1, b"web") {
            Some(next) => next,
            None => return false,
        }
    } else {
        at
    };
    let Some(at) = step(tokens, at, b"--patch") else {
        return false;
    };
    let Some(patch) = tokens.get(at) else {
        return false;
    };
    if !patched(patch, hosts) {
        return false;
    }
    let Some(at) = step(tokens, at + 1, b"--no-open") else {
        return false;
    };
    let Some(at) = step(tokens, at, b"--host") else {
        return false;
    };
    let Some(at) = step(tokens, at, b"127.0.0.1") else {
        return false;
    };
    let Some(at) = step(tokens, at, b"--port") else {
        return false;
    };
    let Some(at) = step(tokens, at, b"0") else {
        return false;
    };
    // The shape ends the host's arguments; a later argument could only be a
    // different command line that happens to embed these tokens.
    at == tokens.len()
}

/// `at` when `tokens[at]` is exactly `value`, else `None`.
fn step(tokens: &[Vec<u8>], at: usize, value: &[u8]) -> Option<usize> {
    tokens
        .get(at)
        .filter(|token| token.as_slice() == value)
        .map(|_| at + 1)
}

/// The patch path under this state's `dsh/hosts`: one host folder named with
/// the writer's sixteen lowercase hex digits, and the writer's file name.
fn patched(token: &[u8], hosts: &Path) -> bool {
    let Ok(text) = std::str::from_utf8(token) else {
        return false;
    };
    let path = Path::new(text);
    let components: Vec<Component> = path.components().collect();
    let root: Vec<Component> = hosts.components().collect();
    if components.len() != root.len() + 2 {
        return false;
    }
    if components[..root.len()] != root[..] {
        return false;
    }
    let Component::Normal(id) = components[root.len()] else {
        return false;
    };
    let Some(id) = id.to_str() else {
        return false;
    };
    if id.len() != HOST_ID_LEN
        || !id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return false;
    }
    components[root.len() + 1] == Component::Normal(std::ffi::OsStr::new("hgs.patch.json"))
}

/// The argument vector of `pid`, or `None` when it cannot be read: an
/// unreadable vector is not evidence either way.
#[cfg(target_os = "linux")]
pub(super) fn read(pid: u32) -> Option<Vec<Vec<u8>>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/cmdline"))
        .ok()?
        .take(65_537)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 65_536 {
        return None;
    }
    Some(tokens(&bytes))
}

/// `KERN_PROCARGS2`'s argv part: argc, the exec path, then exactly argc
/// arguments. The environment follows and is never read.
#[cfg(target_os = "macos")]
pub(super) fn read(pid: u32) -> Option<Vec<Vec<u8>>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query with a null buffer, as sysctl(3) documents.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    if size == 0 || size > 256 * 1024 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    // SAFETY: the buffer is valid for `size` bytes and sysctl writes at most that.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    buffer.truncate(size);
    let argc = i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?);
    if argc <= 0 || argc > 64 {
        return None;
    }
    let mut rest = &buffer[4..];
    // The executable's path, then any NUL padding before the first argument.
    let end = rest.iter().position(|byte| *byte == 0)?;
    rest = &rest[end + 1..];
    rest = &rest[rest.iter().take_while(|byte| **byte == 0).count()..];
    let mut tokens = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        let end = rest.iter().position(|byte| *byte == 0)?;
        tokens.push(rest[..end].to_vec());
        rest = &rest[end + 1..];
    }
    Some(tokens)
}

/// Windows starts no DeepSeek host, so there is nothing to recognize.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn read(_pid: u32) -> Option<Vec<Vec<u8>>> {
    None
}

fn tokens(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

#[cfg(test)]
mod tests;
