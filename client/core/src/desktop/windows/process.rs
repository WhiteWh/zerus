//! Strict authorization reads from retained process objects, never a Toolhelp
//! parent-zero fallback. Unknown ancestry remains unavailable.
use super::{birth, owned, user};
use std::{
    io,
    mem::{offset_of, size_of},
    os::windows::io::{AsRawHandle, OwnedHandle},
    sync::{Arc, OnceLock},
    time::Instant,
};
use windows_sys::{
    Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
    },
    Win32::{
        Foundation::*,
        Globalization::{CompareStringOrdinal, CSTR_EQUAL},
        System::{
            Diagnostics::Debug::{CheckRemoteDebuggerPresent, ReadProcessMemory},
            Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW},
            Memory::*,
            Threading::*,
        },
        UI::Shell::CommandLineToArgvW,
    },
};

pub const ENV_BYTES: usize = 65536;
const LIMIT: usize = 32;
fn require(ok: bool) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other("frontend process evidence unavailable"))
    }
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Basic {
    exit: i32,
    peb: usize,
    affinity: usize,
    priority: i32,
    pid: usize,
    parent: usize,
}
fn basic(process: HANDLE, pid: u32) -> io::Result<Basic> {
    let mut value = Basic::default();
    let mut used = 0;
    // PROCESS_BASIC_INFORMATION layout from the Windows native API. No process
    // memory is dereferenced in this call. Returned pointers are read separately.
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            ProcessBasicInformation,
            (&mut value as *mut Basic).cast(),
            size_of::<Basic>() as u32,
            &mut used,
        )
    };
    require(
        status >= 0
            && used as usize == size_of::<Basic>()
            && value.pid == pid as usize
            && value.parent <= u32::MAX as usize,
    )?;
    Ok(value)
}
fn context(stage: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{stage}: {error}"))
}
fn image(process: HANDLE) -> io::Result<String> {
    let mut bytes = vec![0u16; 32768];
    let mut count = bytes.len() as u32;
    require(
        unsafe { QueryFullProcessImageNameW(process, 0, bytes.as_mut_ptr(), &mut count) } != 0
            && count > 0
            && count as usize <= bytes.len(),
    )?;
    String::from_utf16(&bytes[..count as usize]).map_err(|_| io::ErrorKind::InvalidData.into())
}

/// The handle pins the original kernel object, including while its parent PID
/// is inspected. Other-user boundaries need no environment or VM-read rights.
pub struct Original {
    process: OwnedHandle,
    pub pid: u32,
    pub birth: u64,
    pub parent: u32,
    pub image: String,
    pub own: bool,
    peb: usize,
    sid: Vec<u8>,
}
impl Original {
    pub fn open(pid: u32) -> io::Result<Self> {
        require(pid != 0)?;
        let limited = owned(unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        })
        .map_err(|e| context("open original process", e))?;
        let before = birth(limited.as_raw_handle()).ok_or(io::ErrorKind::PermissionDenied)?;
        let sid = user(limited.as_raw_handle()).map_err(|e| context("read original SID", e))?;
        let own = sid == user(unsafe { GetCurrentProcess() })?;
        let process = if own {
            let full = owned(unsafe {
                OpenProcess(
                    PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            })
            .map_err(|e| context("open same-user process memory", e))?;
            require(
                birth(full.as_raw_handle()).as_deref() == Some(before.as_str())
                    && user(full.as_raw_handle())? == sid,
            )?;
            full
        } else {
            limited
        };
        Self::from_handle(
            pid,
            process,
            before.parse().map_err(|_| io::ErrorKind::InvalidData)?,
            sid,
            own,
        )
    }
    fn from_handle(
        pid: u32,
        process: OwnedHandle,
        created: u64,
        sid: Vec<u8>,
        own: bool,
    ) -> io::Result<Self> {
        let value = if own {
            Some(
                basic(process.as_raw_handle(), pid)
                    .map_err(|e| context("read original parent", e))?,
            )
        } else {
            None
        };
        let result = Self {
            pid,
            birth: created,
            parent: value.map_or(0, |v| v.parent as u32),
            image: image(process.as_raw_handle()).map_err(|e| context("read original image", e))?,
            own,
            peb: value.map_or(0, |v| v.peb),
            sid,
            process,
        };
        result.check_current()?;
        Ok(result)
    }
    pub fn current(&self) -> bool {
        self.check_current().is_ok()
    }
    fn check_current(&self) -> io::Result<()> {
        let wait = unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) };
        require(wait == WAIT_TIMEOUT).map_err(|e| context("original process is not live", e))?;
        require(
            birth(self.process.as_raw_handle()).as_deref() == Some(self.birth.to_string().as_str()),
        )
        .map_err(|e| context("original process birth changed", e))?;
        require(image(self.process.as_raw_handle())? == self.image)
            .map_err(|e| context("original process image changed", e))?;
        require(user(self.process.as_raw_handle())? == self.sid)
            .map_err(|e| context("original process SID changed", e))?;
        if self.own {
            let value = basic(self.process.as_raw_handle(), self.pid)
                .map_err(|e| context("original process parent unavailable", e))?;
            require(value.parent == self.parent as usize && value.peb == self.peb)
                .map_err(|e| context("original process parent changed", e))?;
        }
        Ok(())
    }
    fn command_line(&self) -> io::Result<Vec<u16>> {
        require(self.own && self.current())?;
        let mut size = 0;
        unsafe {
            NtQueryInformationProcess(
                self.process.as_raw_handle(),
                ProcessCommandLineInformation,
                std::ptr::null_mut(),
                0,
                &mut size,
            )
        };
        require((size_of::<UNICODE_STRING>()..=ENV_BYTES + 32).contains(&(size as usize)))?;
        let mut data = vec![0u64; (size as usize).div_ceil(8)];
        let cap = size;
        require(
            unsafe {
                NtQueryInformationProcess(
                    self.process.as_raw_handle(),
                    ProcessCommandLineInformation,
                    data.as_mut_ptr().cast(),
                    cap,
                    &mut size,
                )
            } >= 0
                && size <= cap,
        )?;
        let value = unsafe { &*data.as_ptr().cast::<UNICODE_STRING>() };
        let start = data.as_ptr() as usize;
        let address = value.Buffer as usize;
        let length = value.Length as usize;
        require(
            length.is_multiple_of(2)
                && length > 0
                && length <= value.MaximumLength as usize
                && address >= start + size_of::<UNICODE_STRING>()
                && address
                    .checked_add(length)
                    .is_some_and(|end| end <= start + size as usize),
        )?;
        let line = unsafe { std::slice::from_raw_parts(value.Buffer, length / 2) }.to_vec();
        require(!line.contains(&0) && self.current())?;
        Ok(line)
    }
    pub fn arguments(&self) -> io::Result<Vec<String>> {
        let mut line = self.command_line()?;
        line.push(0);
        let mut count = 0;
        let argv = unsafe { CommandLineToArgvW(line.as_ptr(), &mut count) };
        if argv.is_null() {
            return Err(io::Error::last_os_error());
        }
        struct Local(*mut core::ffi::c_void);
        impl Drop for Local {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let _allocation = Local(argv.cast());
        require((1..=4096).contains(&count))?;
        let mut output = Vec::new();
        for arg in unsafe { std::slice::from_raw_parts(argv, count as usize) } {
            let mut length = 0;
            while length < ENV_BYTES / 2 && unsafe { *arg.add(length) } != 0 {
                length += 1;
            }
            require(length < ENV_BYTES / 2)?;
            output.push(
                String::from_utf16(unsafe { std::slice::from_raw_parts(*arg, length) })
                    .map_err(|_| io::ErrorKind::InvalidData)?,
            );
        }
        require(self.current())?;
        Ok(output)
    }
    fn read(&self, address: usize, bytes: &mut [u8]) -> io::Result<()> {
        require(address != 0 && !bytes.is_empty())?;
        let mut count = 0;
        require(
            unsafe {
                ReadProcessMemory(
                    self.process.as_raw_handle(),
                    address as *const _,
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                    &mut count,
                )
            } != 0
                && count == bytes.len(),
        )
    }
    fn pointer(&self, address: usize) -> io::Result<usize> {
        let mut bytes = [0u8; size_of::<usize>()];
        self.read(address, &mut bytes)?;
        Ok(usize::from_ne_bytes(bytes))
    }
    /// Only the native pointer-width layout is supported. WOW64 or unreadable
    /// parameter layouts explicitly refuse, rather than guessing clean state.
    pub fn environment(&self, until: Instant) -> io::Result<Vec<u16>> {
        static LAYOUT: OnceLock<()> = OnceLock::new();
        require(Instant::now() < until)?;
        // Verify this native extension against the Win32 API in our own
        // process. Never wait on another request's initialization or cache a
        // transient failed read as clean evidence.
        if LAYOUT.get().is_none() {
            require(layout_matches(until)?)?;
            let _ = LAYOUT.set(());
        }
        self.environment_inner(until)
    }
    fn environment_inner(&self, until: Instant) -> io::Result<Vec<u16>> {
        require(self.own && self.current() && Instant::now() < until)?;
        let mut wow64 = 0;
        require(
            unsafe { IsWow64Process(self.process.as_raw_handle(), &mut wow64) } != 0 && wow64 == 0,
        )?;
        let mut debugged = 0;
        require(
            unsafe { CheckRemoteDebuggerPresent(self.process.as_raw_handle(), &mut debugged) } != 0
                && debugged == 0,
        )?;
        let params = self.pointer(
            self.peb
                .checked_add(offset_of!(PEB, ProcessParameters))
                .ok_or(io::ErrorKind::InvalidData)?,
        )?;
        // The native RTL_USER_PROCESS_PARAMETERS prefix ends at CommandLine;
        // its next native-width field is Environment. Validate the prefix's
        // command-line pointer against a kernel query before reading that field.
        let mut command = vec![0u8; size_of::<UNICODE_STRING>()];
        self.read(
            params
                .checked_add(offset_of!(RTL_USER_PROCESS_PARAMETERS, CommandLine))
                .ok_or(io::ErrorKind::InvalidData)?,
            &mut command,
        )?;
        let length = u16::from_ne_bytes(
            command[..2]
                .try_into()
                .map_err(|_| io::ErrorKind::InvalidData)?,
        ) as usize;
        let offset = offset_of!(UNICODE_STRING, Buffer);
        let pointer = usize::from_ne_bytes(
            command[offset..offset + size_of::<usize>()]
                .try_into()
                .map_err(|_| io::ErrorKind::InvalidData)?,
        );
        require(length > 0 && length.is_multiple_of(2) && length <= ENV_BYTES)?;
        let mut remote = vec![0u8; length];
        self.read(pointer, &mut remote)?;
        let remote: Vec<u16> = remote
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| u16::from_ne_bytes([v[0], v[1]]))
            .collect();
        require(remote == self.command_line()?)?;
        let environment = self.pointer(
            params
                .checked_add(size_of::<RTL_USER_PROCESS_PARAMETERS>())
                .ok_or(io::ErrorKind::InvalidData)?,
        )?;
        require(environment != 0 && environment.is_multiple_of(2))?;
        let mut output = Vec::new();
        let mut address = environment;
        while output.len() < ENV_BYTES / 2 && Instant::now() < until {
            let mut region = MEMORY_BASIC_INFORMATION::default();
            require(
                unsafe {
                    VirtualQueryEx(
                        self.process.as_raw_handle(),
                        address as *const _,
                        &mut region,
                        size_of::<MEMORY_BASIC_INFORMATION>(),
                    )
                } == size_of::<MEMORY_BASIC_INFORMATION>()
                    && region.State == MEM_COMMIT
                    && region.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0,
            )?;
            let end = (region.BaseAddress as usize)
                .checked_add(region.RegionSize)
                .ok_or(io::ErrorKind::InvalidData)?;
            require((region.BaseAddress as usize) <= address && end > address)?;
            let count = (end - address).min(4096).min(ENV_BYTES - output.len() * 2) & !1;
            require(count >= 2)?;
            let mut bytes = vec![0u8; count];
            self.read(address, &mut bytes)?;
            for pair in bytes.as_chunks::<2>().0 {
                let word = u16::from_ne_bytes([pair[0], pair[1]]);
                if word == 0 && output.last() == Some(&0) {
                    output.push(0);
                    require(
                        self.pointer(self.peb + offset_of!(PEB, ProcessParameters))? == params
                            && self.pointer(params + size_of::<RTL_USER_PROCESS_PARAMETERS>())?
                                == environment
                            && self.current()
                            && Instant::now() < until,
                    )?;
                    return Ok(output);
                }
                output.push(word);
            }
            address = address
                .checked_add(count)
                .ok_or(io::ErrorKind::InvalidData)?;
        }
        Err(io::Error::other("frontend environment exceeds its bound"))
    }
}

fn layout_matches(until: Instant) -> io::Result<bool> {
    require(Instant::now() < until)?;
    let original = Original::open(std::process::id())?;
    let observed = original
        .environment_inner(until.min(Instant::now() + std::time::Duration::from_secs(1)))?;
    let raw = unsafe { GetEnvironmentStringsW() };
    require(!raw.is_null())?;
    struct Environment(*mut u16);
    impl Drop for Environment {
        fn drop(&mut self) {
            unsafe { FreeEnvironmentStringsW(self.0) };
        }
    }
    let _owned = Environment(raw);
    let mut expected = Vec::new();
    // Win32 owns this immutable double-NUL-terminated copy until it is freed.
    for offset in 0..ENV_BYTES / 2 {
        let word = unsafe { *raw.add(offset) };
        if word == 0 && expected.last() == Some(&0) {
            expected.push(0);
            require(Instant::now() < until)?;
            return Ok(expected == observed);
        }
        expected.push(word);
    }
    Err(io::Error::other("native environment exceeds its bound"))
}

fn equal_windows(left: &str, right: &str) -> io::Result<bool> {
    if left.is_ascii() && right.is_ascii() {
        return Ok(left.eq_ignore_ascii_case(right));
    }
    let left: Vec<_> = left.encode_utf16().collect();
    let right: Vec<_> = right.encode_utf16().collect();
    // Windows environment names use the OS ordinal uppercasing table, not a
    // locale or ASCII-only comparison. Failure is unavailable, never clean.
    let compared = unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    };
    require(compared != 0)?;
    Ok(compared == CSTR_EQUAL)
}
fn agent_name(name: &str) -> io::Result<bool> {
    for target in ["codex", "claude", "deepseek", "gemini", "opencode"] {
        if equal_windows(name, target)? || equal_windows(name, &format!("{target}.exe"))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn agent(environment: &[u16]) -> io::Result<bool> {
    require(environment.len() >= 2 && environment.ends_with(&[0, 0]))?;
    for entry in environment[..environment.len() - 1]
        .split(|c| *c == 0)
        .filter(|v| !v.is_empty())
    {
        let equal = entry
            .iter()
            .position(|c| *c == u16::from(b'='))
            .ok_or(io::ErrorKind::InvalidData)?;
        let key = String::from_utf16(&entry[..equal]).map_err(|_| io::ErrorKind::InvalidData)?;
        if equal_windows(&key, "HGS_SESSION")? || equal_windows(&key, "HGS_RUN_ID")? {
            return Ok(true);
        }
        // Search paths are ordinary configuration, not injection primitives.
        if entry.len() > equal + 1 {
            for injection in [
                "LD_PRELOAD",
                "LD_AUDIT",
                "DYLD_INSERT_LIBRARIES",
                "DYLD_FORCE_FLAT_NAMESPACE",
            ] {
                if equal_windows(&key, injection)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}
/// A minimal parent observation pins the kernel object. Creation identity is
/// checked before SID/image/environment inspection so a reused PID is never
/// adopted as the original ancestor.
struct ParentStamp {
    process: OwnedHandle,
    pid: u32,
    created: u64,
}
impl ParentStamp {
    fn read(pid: u32, process: OwnedHandle) -> io::Result<Self> {
        require(unsafe { GetProcessId(process.as_raw_handle()) } == pid)?;
        let created = birth(process.as_raw_handle())
            .ok_or_else(io::Error::last_os_error)?
            .parse()
            .map_err(|_| io::ErrorKind::InvalidData)?;
        Ok(Self {
            process,
            pid,
            created,
        })
    }
    fn unchanged(&self) -> bool {
        (unsafe { GetProcessId(self.process.as_raw_handle()) }) == self.pid
            && birth(self.process.as_raw_handle()).as_deref()
                == Some(self.created.to_string().as_str())
    }
    fn exit_code(&self) -> io::Result<u32> {
        let mut value = 0;
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut value) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(value)
    }
}
enum Boundary {
    Missing,
    Denied,
    Reused(ParentStamp),
    Exited(ParentStamp),
    Signaled(ParentStamp),
    Foreign(ParentStamp, Vec<u8>),
}
enum Parent {
    Same(Original),
    End(Boundary),
}
fn parent_open_error(error: io::Error) -> io::Result<Boundary> {
    match error.raw_os_error().map(|v| v as u32) {
        // The PID comes only from the held child's successful native query;
        // with this fixed access mask an invalid PID has no live process.
        Some(ERROR_INVALID_PARAMETER) => Ok(Boundary::Missing),
        // Owner-approved Unix parity: query-only access denial ends the
        // same-user walk. This is not a role/name/PID or caller-JSON allowlist.
        Some(ERROR_ACCESS_DENIED) => Ok(Boundary::Denied),
        _ => Err(error),
    }
}
fn parent(pid: u32, child_birth: u64, until: Instant) -> io::Result<Parent> {
    require(Instant::now() < until)?;
    let process = match owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }) {
        Ok(process) => process,
        Err(error) => return parent_open_error(error).map(Parent::End),
    };
    let observed = ParentStamp::read(pid, process)?;
    if observed.created >= child_birth {
        return Ok(Parent::End(Boundary::Reused(observed)));
    }
    // An exit value other than STILL_ACTIVE proves departure. A process may
    // exit with 259, so that value alone never proves liveness: same-user
    // parents also require the original synchronization handle below.
    if observed.exit_code()? != STILL_ACTIVE as u32 {
        return Ok(Parent::End(Boundary::Exited(observed)));
    }
    let sid = user(observed.process.as_raw_handle())?;
    if sid != user(unsafe { GetCurrentProcess() })? {
        return Ok(Parent::End(Boundary::Foreign(observed, sid)));
    }
    let full = match owned(unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    }) {
        Ok(full) => full,
        Err(error) => {
            // A same-user VM/query failure is not a foreign-principal proof.
            // Only an independently observed exit can end this race cleanly.
            return if observed.exit_code()? != STILL_ACTIVE as u32 {
                Ok(Parent::End(Boundary::Exited(observed)))
            } else {
                Err(error)
            };
        }
    };
    let full = ParentStamp::read(pid, full)?;
    require(
        full.created == observed.created
            && observed.unchanged()
            && user(full.process.as_raw_handle())? == sid,
    )?;
    match unsafe { WaitForSingleObject(full.process.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(Parent::End(Boundary::Signaled(full))),
        WAIT_TIMEOUT => {
            let result = Original::from_handle(pid, full.process, full.created, sid, true);
            match result {
                Ok(original) => Ok(Parent::Same(original)),
                Err(_) if observed.exit_code()? != STILL_ACTIVE as u32 => {
                    Ok(Parent::End(Boundary::Exited(observed)))
                }
                Err(error) => Err(error),
            }
        }
        _ => Err(io::Error::last_os_error()),
    }
}
impl Boundary {
    fn validate(&self, child: &Original, until: Instant) -> io::Result<()> {
        require(child.current() && Instant::now() < until)?;
        match self {
            Self::Missing | Self::Denied => {
                // No original parent handle exists for these two outcomes.
                // Re-query the held child's exact parent, without adopting a
                // newly readable same-user process into the accepted chain.
                require(matches!(
                    parent(child.parent, child.birth, until)?,
                    Parent::End(_)
                ))?;
            }
            Self::Reused(value) => require(
                value.pid == child.parent && value.unchanged() && value.created >= child.birth,
            )?,
            Self::Exited(value) => require(
                value.pid == child.parent
                    && value.unchanged()
                    && value.created < child.birth
                    && value.exit_code()? != STILL_ACTIVE as u32,
            )?,
            Self::Signaled(value) => require(
                value.pid == child.parent
                    && value.unchanged()
                    && value.created < child.birth
                    && unsafe { WaitForSingleObject(value.process.as_raw_handle(), 0) }
                        == WAIT_OBJECT_0,
            )?,
            Self::Foreign(value, sid) => require(
                value.pid == child.parent
                    && value.unchanged()
                    && value.created < child.birth
                    && user(value.process.as_raw_handle())? == *sid
                    && *sid != user(unsafe { GetCurrentProcess() })?,
            )?,
        }
        require(Instant::now() < until)
    }
}
pub struct Ancestry {
    chain: Vec<Arc<Original>>,
    boundary: Boundary,
}
impl Ancestry {
    /// Borrow the actual retained ancestors, excluding the inspected process.
    /// Selection still requires validate(); these references grant no role.
    pub fn ancestors(&self) -> impl Iterator<Item = &Original> {
        self.chain.iter().skip(1).map(Arc::as_ref)
    }

    pub fn inspect(pid: u32, until: Instant) -> io::Result<Self> {
        require(Instant::now() < until)?;
        let mut current =
            Original::open(pid).map_err(|e| context("initial ancestor metadata", e))?;
        require(current.own)?;
        let mut chain: Vec<Arc<Original>> = Vec::new();
        loop {
            require(
                Instant::now() < until
                    && current.own
                    && current.current()
                    && chain.len() < LIMIT
                    && !chain.iter().any(|p| p.pid == current.pid),
            )?;
            let name = std::path::Path::new(&current.image)
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or(io::ErrorKind::InvalidData)?;
            let environment = current
                .environment(until)
                .map_err(|e| context(&format!("ancestor environment at hop {}", chain.len()), e))?;
            if agent_name(name)? || agent(&environment)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "agent-descended frontend request refused",
                ));
            }
            let candidate = parent(current.parent, current.birth, until)?;
            chain.push(Arc::new(current));
            match candidate {
                Parent::Same(original) => current = original,
                Parent::End(boundary) => {
                    let result = Self { chain, boundary };
                    result.validate(until)?;
                    return Ok(result);
                }
            }
        }
    }
    pub fn validate(&self, until: Instant) -> io::Result<()> {
        self.validate_chain(&self.chain, until)
    }

    /// A read-only hint may outlive its original relay. Retain the same
    /// inspected objects, but require the exact original direct parent and
    /// revalidate every ancestor from that parent. Mutation admission continues
    /// to use validate(), including the original relay's liveness.
    pub fn validate_parent(&self, parent: &Original, until: Instant) -> io::Result<()> {
        let (relay, ancestors) = self.chain.split_first().ok_or(io::ErrorKind::InvalidData)?;
        let retained = ancestors.first().ok_or(io::ErrorKind::InvalidData)?;
        require(
            relay.parent == parent.pid
                && retained.pid == parent.pid
                && retained.birth == parent.birth
                && parent.current(),
        )?;
        self.validate_chain(ancestors, until)
    }

    fn validate_chain(&self, chain: &[Arc<Original>], until: Instant) -> io::Result<()> {
        let child = chain.last().ok_or(io::ErrorKind::InvalidData)?;
        self.boundary.validate(child, until)?;
        for original in chain {
            require(
                Instant::now() < until
                    && original.current()
                    && !agent(&original.environment(until)?)?,
            )?;
        }
        self.boundary.validate(child, until)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::windows::process::CommandExt,
        process::{Child, Command, Stdio},
        time::Duration,
    };
    struct Held(Child);
    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn read_line(stdout: &mut impl Read) -> Vec<u8> {
        let mut line = Vec::new();
        for _ in 0..32 {
            let mut byte = [0u8];
            match stdout.read(&mut byte) {
                Ok(1) => {
                    line.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                _ => break,
            }
        }
        line
    }
    fn child() -> Held {
        let mut child = Held(
            Command::new("cmd.exe")
                .creation_flags(CREATE_NO_WINDOW)
                .args(["/D", "/Q", "/C", "echo START& set /p HGS_TEST_WAIT="])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_RUN_ID")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut stdout = child.0.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let _ = send.send(read_line(&mut stdout));
        });
        let ready = receive.recv_timeout(Duration::from_secs(2));
        if ready.is_err() {
            drop(child);
            reader.join().unwrap();
            panic!("owned child did not become ready");
        }
        reader.join().unwrap();
        assert!(ready.unwrap().ends_with(b"START\r\n"));
        child
    }
    #[test]
    fn retained_ancestor_view_excludes_the_inspected_child_and_preserves_its_actual_parent() {
        let child = child();
        let until = Instant::now() + Duration::from_secs(2);
        let proof = Ancestry::inspect(child.0.id(), until).unwrap();
        let parent = Original::open(std::process::id()).unwrap();
        assert!(proof
            .ancestors()
            .all(|original| original.pid != child.0.id()));
        let first = proof.ancestors().next().unwrap();
        assert_eq!((first.pid, first.birth), (parent.pid, parent.birth));
        proof.validate(until).unwrap();
        drop(child);
        assert!(proof
            .validate(Instant::now() + Duration::from_secs(2))
            .is_err());
    }
    #[test]
    fn native_agent_parent_fixture() {
        let Some(mode) = std::env::var_os("ZERUS_ANCESTRY_FIXTURE") else {
            return;
        };
        let child = child();
        let until = Instant::now() + Duration::from_secs(2);
        let original = Original::open(child.0.id()).unwrap();
        assert!(
            !agent(&original.environment(until).unwrap()).unwrap(),
            "the child must have stripped the markers"
        );
        assert!(matches!(mode.to_str(), Some("name" | "tag")));
        let error = match Ancestry::inspect(child.0.id(), until) {
            Err(error) => error,
            Ok(_) => panic!("a readable same-user agent ancestor was admitted"),
        };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("agent-descended"));
    }
    struct FixtureDirectory(std::path::PathBuf);
    impl FixtureDirectory {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zerus-ancestry-{}-{nonce}-{next}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for FixtureDirectory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn run_agent_fixture(named: bool) {
        let directory = FixtureDirectory::new();
        let source = std::env::current_exe().unwrap();
        let program = if named {
            let copy = directory.0.join("codex.exe");
            std::fs::copy(source, &copy).unwrap();
            copy
        } else {
            source
        };
        let mut command = Command::new(program);
        command
            .creation_flags(CREATE_NO_WINDOW)
            .args([
                "--exact",
                "desktop::windows::process::tests::native_agent_parent_fixture",
                "--nocapture",
            ])
            .env_remove("HGS_SESSION")
            .env_remove("HGS_RUN_ID")
            .env("ZERUS_ANCESTRY_FIXTURE", if named { "name" } else { "tag" })
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !named {
            command.env("hGs_RuN_Id", "");
        }
        let mut fixture = Held(command.spawn().unwrap());
        let until = Instant::now() + Duration::from_secs(6);
        loop {
            if fixture.0.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < until,
                "owned agent fixture exceeded its deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let status = fixture.0.wait().unwrap();
        let mut output = String::new();
        fixture
            .0
            .stdout
            .take()
            .unwrap()
            .take(8192)
            .read_to_string(&mut output)
            .unwrap();
        fixture
            .0
            .stderr
            .take()
            .unwrap()
            .take(8192)
            .read_to_string(&mut output)
            .unwrap();
        drop(fixture);
        assert!(status.success(), "{output}");
    }
    #[test]
    fn native_readable_same_user_agent_name_and_stripped_child_marker_are_refused() {
        run_agent_fixture(true);
        run_agent_fixture(false);
    }
    #[test]
    fn parent_open_denial_is_distinct_from_other_query_errors() {
        assert!(matches!(
            parent_open_error(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32)).unwrap(),
            Boundary::Denied
        ));
        assert!(matches!(
            parent_open_error(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32))
                .unwrap(),
            Boundary::Missing
        ));
        for code in [ERROR_INVALID_HANDLE, ERROR_GEN_FAILURE, ERROR_NOT_SUPPORTED] {
            assert_eq!(
                parent_open_error(io::Error::from_raw_os_error(code as i32))
                    .err()
                    .unwrap()
                    .raw_os_error(),
                Some(code as i32)
            );
        }
        let child = child();
        let restricted =
            owned(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child.0.id()) }).unwrap();
        assert!(
            ParentStamp::read(child.0.id(), restricted).is_err(),
            "a successful open followed by a denied query is not a foreign boundary"
        );
    }
    #[test]
    fn native_later_created_parent_pid_is_retired_before_environment_inspection() {
        let older = child();
        let original = Original::open(older.0.id()).unwrap();
        let mut command = Command::new("cmd.exe");
        command
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/D", "/Q", "/C", "set /p HGS_TEST_WAIT="])
            .env("HGS_RUN_ID", "native-fixture")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for index in 0..3 {
            command.env(format!("ZERUS_FIXTURE_PAD{index}"), "λ".repeat(20000));
        }
        let candidate = Held(command.spawn().unwrap());
        let found = parent(
            candidate.0.id(),
            original.birth,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        let Parent::End(Boundary::Reused(stamp)) = found else {
            panic!("later-created process was inspected as an ancestor");
        };
        assert!(stamp.created >= original.birth && stamp.unchanged());
        assert_eq!(
            birth(candidate.0.as_raw_handle()).unwrap(),
            stamp.created.to_string()
        );
    }
    #[test]
    fn native_departed_parent_keeps_its_original_handle_identity() {
        let mut departed = child();
        let created = birth(departed.0.as_raw_handle()).unwrap();
        departed
            .0
            .stdin
            .take()
            .unwrap()
            .write_all(b"done\n")
            .unwrap();
        departed.0.wait().unwrap();
        let newer = child();
        let child_birth = Original::open(newer.0.id()).unwrap().birth;
        let found = parent(
            departed.0.id(),
            child_birth,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        match found {
            Parent::End(Boundary::Exited(stamp) | Boundary::Signaled(stamp)) => {
                assert_eq!(stamp.created.to_string(), created);
                assert!(stamp.unchanged() && stamp.created < child_birth);
            }
            Parent::End(Boundary::Missing) => (),
            _ => panic!("departed process was adopted as a live same-user parent"),
        }
    }
    fn environment(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0, 0]).collect()
    }
    #[test]
    fn windows_markers_are_case_insensitive_even_when_empty_and_paths_are_allowed() {
        assert!(agent(&environment("hGs_SeSsIoN=")).unwrap());
        assert!(agent(&environment("Hgs_Run_Id=example")).unwrap());
        assert!(!agent(&environment(
            "HGS_SESSION_OTHER=example\0LD_LIBRARY_PATH=example\0DYLD_LIBRARY_PATH=example"
        ))
        .unwrap());
        assert!(agent(&environment("LD_PRELOAD=example")).unwrap());
        assert!(agent(&environment("dyld_insert_libraries=example")).unwrap());
        assert!(agent(&[1, 0]).is_err());
        assert!(agent(&environment("invalid")).is_err());
    }
    #[test]
    fn native_original_child_keeps_birth_image_argv_environment_and_exit_identity() {
        let mut child = child();
        let original = Original::open(child.0.id()).unwrap();
        assert!(original.own && original.current());
        assert_eq!(original.parent, std::process::id());
        assert_eq!(
            birth(child.0.as_raw_handle()).unwrap(),
            original.birth.to_string()
        );
        let args = original.arguments().unwrap();
        assert_eq!(
            &args[1..],
            ["/D", "/Q", "/C", "echo START& set /p HGS_TEST_WAIT="]
        );
        assert!(!agent(
            &original
                .environment(Instant::now() + Duration::from_secs(2))
                .unwrap()
        )
        .unwrap());
        child.0.stdin.take().unwrap().write_all(b"done\n").unwrap();
        child.0.wait().unwrap();
        assert!(
            !original.current(),
            "an exited retained object is never a fresh PID"
        );
    }
    #[test]
    fn native_unreadable_and_expired_process_evidence_is_not_a_clean_root() {
        let child = child();
        let mut original = Original::open(child.0.id()).unwrap();
        assert!(original.environment(Instant::now()).is_err());
        original.process = owned(unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                child.0.id(),
            )
        })
        .unwrap();
        assert!(original
            .environment(Instant::now() + Duration::from_secs(2))
            .is_err());
        assert!(Original::open(0).is_err());
    }
    #[test]
    fn native_parent_hint_proof_keeps_original_ancestors_after_only_the_relay_exits() {
        let mut relay = child();
        let parent = Original::open(std::process::id()).unwrap();
        let proof =
            Ancestry::inspect(relay.0.id(), Instant::now() + Duration::from_secs(2)).unwrap();
        let wrong_parent = child();
        let wrong = Original::open(wrong_parent.0.id()).unwrap();
        assert!(proof
            .validate_parent(&wrong, Instant::now() + Duration::from_secs(2))
            .is_err());
        // Exit/reap only the original fixture relay. The hint's retained parent
        // and ancestry still exist; a mutation permit must fail on that exit.
        relay
            .0
            .stdin
            .take()
            .unwrap()
            .write_all(b"continue\n")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if relay.0.try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "original relay did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        let until = Instant::now() + Duration::from_secs(2);
        assert!(proof.validate(until).is_err());
        proof.validate_parent(&parent, until).unwrap();
        assert!(proof.validate_parent(&parent, Instant::now()).is_err());
    }
    #[test]
    fn native_clean_ancestry_ends_only_at_the_approved_unix_parity_boundaries() {
        let chain = Ancestry::inspect(std::process::id(), Instant::now() + Duration::from_secs(2))
            .unwrap_or_else(|error| {
                let until = Instant::now() + Duration::from_secs(2);
                let mut pid = std::process::id();
                for hop in 0..LIMIT {
                    if Instant::now() >= until { break; }
                    match Original::open(pid) {
                        Ok(original) => {
                            eprintln!("ancestry hop={hop} pid={pid} parent={} own={} current={} env_read={:?}",
                                original.parent, original.own, original.current(),
                                original.environment(until).map(|_| ()).map_err(|e| e.to_string()));
                            if !original.own || original.parent == 0 { break; }
                            pid = original.parent;
                        }
                        Err(e) => {
                            let observed = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) });
                            if let Ok(process) = observed {
                                let filename = image(process.as_raw_handle()).ok().and_then(|p|
                                    std::path::Path::new(&p).file_name().map(|s| s.to_string_lossy().into_owned()));
                                let wait = unsafe { WaitForSingleObject(process.as_raw_handle(), 0) };
                                eprintln!("ancestry hop={hop} image={filename:?} wait={wait} metadata={e}");
                            } else {
                                eprintln!("ancestry hop={hop} metadata={e}");
                            }
                            break;
                        }
                    }
                }
                panic!("clean native ancestry refused: {error}");
            });
        assert!(chain
            .chain
            .iter()
            .all(|ancestor| ancestor.own && ancestor.current()));
        assert!(!chain.chain.is_empty());
        assert!(chain.chain.len() <= LIMIT);
        chain
            .validate(Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert!(chain.validate(Instant::now()).is_err());
    }
    #[test]
    fn native_stale_birth_and_image_never_revalidate() {
        let child = child();
        let mut original = Original::open(child.0.id()).unwrap();
        let birth = original.birth;
        original.birth += 1;
        assert!(!original.current());
        original.birth = birth;
        original.image.push_str(".changed");
        assert!(!original.current());
    }

    #[test]
    fn native_environment_changed_after_admission_is_rechecked_at_use() {
        use std::{sync::mpsc, thread};
        let mut child=Held(Command::new("cmd.exe").creation_flags(CREATE_NO_WINDOW)
            .args(["/D","/Q","/C","echo START& set /p HGS_TEST_WAIT=& set HGS_RUN_ID=late-tag& echo READY& set /p HGS_TEST_WAIT="])
            .env_remove("HGS_SESSION").env_remove("HGS_RUN_ID")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap());
        let mut stdout = child.0.stdout.take().unwrap();
        let (send, receive) = mpsc::sync_channel(2);
        let reader = thread::spawn(move || {
            let _ = send.send(read_line(&mut stdout));
            let _ = send.send(read_line(&mut stdout));
        });
        let started = receive.recv_timeout(Duration::from_secs(2));
        if started.is_err() {
            drop(child);
            reader.join().unwrap();
            panic!("owned child did not become ready");
        }
        if !started.unwrap().ends_with(b"START\r\n") {
            drop(child);
            reader.join().unwrap();
            panic!("owned child sent an invalid readiness line");
        }
        let until = Instant::now() + Duration::from_secs(2);
        // On refused evidence, close the owned child before joining its reader.
        let evidence = Original::open(child.0.id()).and_then(|original| {
            Ancestry::inspect(child.0.id(), until).map(|admitted| (original, admitted))
        });
        let (original, admitted) = match evidence {
            Ok(evidence) => evidence,
            Err(error) => {
                drop(child);
                reader.join().unwrap();
                panic!("native ancestry admission refused: {error}");
            }
        };
        child
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"continue\n")
            .unwrap();
        let ready = receive.recv_timeout(Duration::from_secs(2));
        let tagged = original.environment(until).and_then(|bytes| agent(&bytes));
        let current = original.current();
        let result = admitted.validate(until);
        drop(child);
        reader.join().unwrap();
        assert!(ready.unwrap().ends_with(b"READY\r\n"));
        assert!(tagged.unwrap());
        assert!(current);
        assert!(
            result.is_err(),
            "a still-live process acquiring an agent marker must lose admission"
        );
    }
}
