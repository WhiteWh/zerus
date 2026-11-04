//! Task Scheduler COM runs only in the original, bounded same-image worker.
use super::{ABI, CAP};
use crate::desktop::windows_registration_policy::{
    self as policy, Action, Plan, Scheduler, Settings, Shape, Task, Trigger,
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    fs::OpenOptions,
    io::Write,
    os::windows::io::{FromRawHandle, OwnedHandle},
    ptr,
    time::Instant,
};
use windows::{
    core::{Interface, BSTR},
    Win32::{
        Foundation::VARIANT_BOOL,
        System::{Com::*, TaskScheduler::*, Variant::VARIANT},
    },
};
use windows_sys::Win32::{
    Foundation::{LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            ConvertStringSidToSidW,
        },
        GetAce, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, IsValidAcl,
        IsValidSecurityDescriptor, IsValidSid, LookupAccountNameW, ACCESS_ALLOWED_ACE, ACL, PSID,
    },
    System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
};

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}
fn text(value: &BSTR) -> Result<String> {
    ensure!(value.len() <= 32768, "Task property exceeds its bound.");
    String::from_utf16(value).context("Task property is not Unicode.")
}
macro_rules! get {
    ($object:expr, $method:ident, $kind:ty) => {{
        let mut value = <$kind>::default();
        // COM owns the interface; the local output is initialized and live.
        unsafe { $object.$method(&mut value) }?;
        value
    }};
}
macro_rules! string {
    ($object:expr, $method:ident) => {
        text(&get!($object, $method, BSTR))?
    };
}
macro_rules! boolean {
    ($object:expr, $method:ident) => {
        get!($object, $method, VARIANT_BOOL).as_bool()
    };
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        // This is the worker's original main thread, before any COM object.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        let result = Self;
        unsafe {
            CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            )?;
        }
        Ok(result)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct Lock {
    _handle: OwnedHandle,
    raw: windows_sys::Win32::Foundation::HANDLE,
}
impl Lock {
    fn acquire(sid: &str) -> Result<Self> {
        let raw = unsafe {
            CreateMutexW(
                ptr::null(),
                0,
                wide(&format!(r"Global\hgs-desktop-registration-{sid}")).as_ptr(),
            )
        };
        ensure!(!raw.is_null(), "Registration lock unavailable.");
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let state = unsafe { WaitForSingleObject(raw, 2000) };
        if state != WAIT_OBJECT_0 {
            if state == WAIT_ABANDONED {
                unsafe { ReleaseMutex(raw) };
            }
            bail!("Registration lock not confirmed; inspect status before retrying.");
        }
        Ok(Self {
            _handle: handle,
            raw,
        })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.raw) };
    }
}

struct Alloc(*mut std::ffi::c_void);
impl Drop for Alloc {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}
fn sid_string(sid: PSID) -> Result<String> {
    ensure!(
        !sid.is_null() && unsafe { IsValidSid(sid) } != 0,
        "Task SID is invalid."
    );
    let mut pointer = ptr::null_mut();
    ensure!(
        unsafe { ConvertSidToStringSidW(sid, &mut pointer) } != 0,
        "Task SID is unreadable."
    );
    let _allocation = Alloc(pointer.cast());
    let length = (0..=184)
        .find(|&i| unsafe { *pointer.add(i) } == 0)
        .context("Task SID exceeds its bound.")?;
    String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, length) })
        .context("Task SID is invalid.")
}
fn resolve_sid(value: &str) -> Result<String> {
    ensure!(
        !value.is_empty() && value.encode_utf16().count() <= 256,
        "Missing task user identity."
    );
    if value.starts_with("S-1-") {
        let mut pointer = ptr::null_mut();
        ensure!(
            unsafe { ConvertStringSidToSidW(wide(value).as_ptr(), &mut pointer) } != 0,
            "Task SID is invalid."
        );
        let _allocation = Alloc(pointer);
        return sid_string(pointer);
    }
    let name = wide(value);
    let (mut sid_size, mut domain_size, mut usage) = (0, 0, 0);
    unsafe {
        LookupAccountNameW(
            ptr::null(),
            name.as_ptr(),
            ptr::null_mut(),
            &mut sid_size,
            ptr::null_mut(),
            &mut domain_size,
            &mut usage,
        )
    };
    ensure!(
        sid_size > 0 && sid_size <= 4096 && domain_size <= 4096,
        "Task user identity is unavailable."
    );
    let mut sid = vec![0u32; (sid_size as usize).div_ceil(4)];
    let mut domain = vec![0u16; domain_size as usize];
    ensure!(
        unsafe {
            LookupAccountNameW(
                ptr::null(),
                name.as_ptr(),
                sid.as_mut_ptr().cast(),
                &mut sid_size,
                domain.as_mut_ptr(),
                &mut domain_size,
                &mut usage,
            )
        } != 0,
        "Task user identity is unavailable."
    );
    sid_string(sid.as_mut_ptr().cast())
}
fn security(task: &IRegisteredTask) -> Result<(String, Option<Vec<policy::Ace>>)> {
    let sddl = text(&unsafe { task.GetSecurityDescriptor(5) }?)?;
    let mut descriptor = ptr::null_mut();
    ensure!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(&sddl).as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } != 0,
        "Task security descriptor unavailable."
    );
    let _allocation = Alloc(descriptor);
    ensure!(
        unsafe { IsValidSecurityDescriptor(descriptor) } != 0,
        "Task security descriptor is invalid."
    );
    let mut owner = ptr::null_mut();
    let mut defaulted = 0;
    ensure!(
        unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) } != 0,
        "Task owner unavailable."
    );
    let owner = sid_string(owner)?;
    let mut acl: *mut ACL = ptr::null_mut();
    let mut present = 0;
    ensure!(
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            != 0,
        "Task DACL unavailable."
    );
    if present == 0 || acl.is_null() {
        return Ok((owner, None));
    }
    ensure!(unsafe { IsValidAcl(acl) } != 0, "Task DACL is invalid.");
    let count = unsafe { (*acl).AceCount };
    ensure!(count <= 16, "Task DACL exceeds its bound.");
    let mut entries = Vec::new();
    for index in 0..u32::from(count) {
        let mut ace = ptr::null_mut();
        ensure!(
            unsafe { GetAce(acl, index, &mut ace) } != 0 && !ace.is_null(),
            "Task DACL is unreadable."
        );
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        ensure!(
            ace.Header.AceType == 0 && ace.Header.AceSize >= 16,
            "Task DACL has an unsupported entry."
        );
        // The SID is variable-sized. Bound its declared subauthority array
        // inside this validated ACE before any SID API follows that pointer.
        let sid = (&ace.SidStart as *const u32).cast::<u8>();
        let subauthorities = unsafe { *sid.add(1) } as usize;
        ensure!(
            subauthorities <= 15 && 16 + subauthorities * 4 <= usize::from(ace.Header.AceSize),
            "Task ACE SID exceeds its bound."
        );
        entries.push(policy::Ace {
            allowed: true,
            sid: sid_string((&ace.SidStart as *const u32).cast_mut().cast())?,
        });
    }
    Ok((owner, Some(entries)))
}

fn shape(definition: &ITaskDefinition) -> Result<Shape> {
    let info = unsafe { definition.RegistrationInfo() }?;
    let principal = unsafe { definition.Principal() }?;
    let actions = unsafe { definition.Actions() }?;
    let triggers = unsafe { definition.Triggers() }?;
    ensure!(
        get!(actions, Count, i32) == 1 && get!(triggers, Count, i32) == 1,
        "Task has extra actions or triggers; it was kept."
    );
    let action = unsafe { actions.get_Item(1) }?;
    let action_kind = get!(action, Type, TASK_ACTION_TYPE).0;
    ensure!(action_kind == 0, "Task has a foreign action; it was kept.");
    let action: IExecAction = action.cast()?;
    let trigger = unsafe { triggers.get_Item(1) }?;
    let trigger_kind = get!(trigger, Type, TASK_TRIGGER_TYPE2).0;
    ensure!(
        trigger_kind == 9,
        "Task has a foreign trigger; it was kept."
    );
    let logon: ILogonTrigger = trigger.cast()?;
    let repetition = unsafe { trigger.Repetition() }?;
    let settings = unsafe { definition.Settings() }?;
    Ok(Shape {
        description: string!(info, Description),
        uri: string!(info, URI),
        user: resolve_sid(&string!(principal, UserId))?,
        group: string!(principal, GroupId),
        principal: string!(principal, Id),
        context: string!(actions, Context),
        logon: get!(principal, LogonType, TASK_LOGON_TYPE).0,
        run_level: get!(principal, RunLevel, TASK_RUNLEVEL_TYPE).0,
        action: Action {
            kind: action_kind,
            path: string!(action, Path),
            arguments: string!(action, Arguments),
            directory: string!(action, WorkingDirectory),
        },
        trigger: Trigger {
            kind: trigger_kind,
            user: resolve_sid(&string!(logon, UserId))?,
            enabled: boolean!(trigger, Enabled),
            start: string!(trigger, StartBoundary),
            end: string!(trigger, EndBoundary),
            delay: string!(logon, Delay),
            interval: string!(repetition, Interval),
            duration: string!(repetition, Duration),
        },
        settings: Settings {
            enabled: boolean!(settings, Enabled),
            hidden: boolean!(settings, Hidden),
            instances: get!(settings, MultipleInstances, TASK_INSTANCES_POLICY).0,
            execution: string!(settings, ExecutionTimeLimit),
            restart: string!(settings, RestartInterval),
            retries: get!(settings, RestartCount, i32),
            disallow_battery: boolean!(settings, DisallowStartIfOnBatteries),
            stop_battery: boolean!(settings, StopIfGoingOnBatteries),
            available: boolean!(settings, StartWhenAvailable),
            idle: boolean!(settings, RunOnlyIfIdle),
            network: boolean!(settings, RunOnlyIfNetworkAvailable),
            wake: boolean!(settings, WakeToRun),
        },
    })
}

struct Native {
    folder: ITaskFolder,
}
impl Scheduler for Native {
    fn read(&mut self, name: &str) -> Result<Option<Task>> {
        let task = match unsafe { self.folder.GetTask(&BSTR::from(name)) } {
            Ok(task) => task,
            Err(error) if error.code().0 == 0x80070002u32 as i32 => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let shape = shape(&unsafe { task.Definition() }?)?;
        let (owner, acl) = security(&task)?;
        Ok(Some(Task {
            shape,
            owner,
            acl,
            enabled: unsafe { task.Enabled() }?.as_bool(),
            state: unsafe { task.State() }?.0,
        }))
    }
    fn create_only(&mut self, plan: &Plan) -> Result<()> {
        let sddl = format!(
            "O:{}D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{})",
            plan.sid, plan.sid
        );
        // Create-only and ignore registration triggers. Never update or Run.
        unsafe {
            self.folder.RegisterTask(
                &BSTR::from(&plan.name),
                &BSTR::from(&plan.definition),
                TASK_CREATE.0 | TASK_IGNORE_REGISTRATION_TRIGGERS.0,
                &VARIANT::from(plan.sid.as_str()),
                &VARIANT::default(),
                TASK_LOGON_INTERACTIVE_TOKEN,
                &VARIANT::from(sddl.as_str()),
            )
        }?;
        Ok(())
    }
    fn delete(&mut self, name: &str) -> Result<()> {
        unsafe { self.folder.DeleteTask(&BSTR::from(name), 0) }?;
        Ok(())
    }
}
struct Trace {
    file: Option<std::fs::File>,
    start: Instant,
}
impl Trace {
    fn new() -> Self {
        Self {
            file: std::env::var_os("HGS_DESKTOP_REGISTRATION_TRACE_FILE")
                .filter(|v| !v.is_empty())
                .and_then(|p| OpenOptions::new().append(true).open(p).ok()),
            start: Instant::now(),
        }
    }
    fn phase(&mut self, name: &str) {
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{name} {}", self.start.elapsed().as_millis());
        }
    }
}
fn operation(args: &[String], trace: &mut Trace) -> Result<Value> {
    ensure!(args.len() == 1, "Invalid native registration arguments.");
    let length = args[0]
        .parse::<usize>()
        .context("Invalid native registration length.")?;
    ensure!(
        length > 0 && length <= CAP && length.to_string() == args[0],
        "Invalid native registration length."
    );
    trace.phase("opened");
    let request = policy::read_frame(&mut std::io::stdin().lock(), length)?;
    trace.phase("bytes");
    trace.phase("input");
    let sid = crate::host::token::user_sid()?;
    let plan = Plan::parse(&request, &sid)?;
    trace.phase("sid");
    let _lock = Lock::acquire(&sid)?;
    trace.phase("mutex");
    let _apartment = Apartment::new()?;
    let service: ITaskService =
        unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }?;
    trace.phase("created");
    unsafe {
        service.Connect(
            &VARIANT::default(),
            &VARIANT::default(),
            &VARIANT::default(),
            &VARIANT::default(),
        )
    }?;
    trace.phase("connected");
    let wanted = plan.shape(crate::host::spawn::command_line(&plan.argv[1..])?);
    // Validate the supplied XML's actual COM properties before any registration.
    let definition = unsafe { service.NewTask(0) }?;
    unsafe { definition.SetXmlText(&BSTR::from(&plan.definition)) }?;
    ensure!(
        shape(&definition)? == wanted,
        "Registration definition differs from its selected command."
    );
    let folder = unsafe { service.GetFolder(&BSTR::from("\\")) }?;
    trace.phase("folder");
    let reply = policy::perform(&plan, &wanted, &mut Native { folder })?;
    trace.phase("done");
    Ok(reply)
}
pub(super) fn serve(args: &[String]) -> i32 {
    let mut trace = Trace::new();
    trace.phase("main");
    let result = operation(args, &mut trace);
    let (code, value) = match result {
        Ok(reply) => (0, json!({"abi":ABI,"reply":reply})),
        Err(error) => (1, json!({"abi":ABI,"error":format!("{error:#}")})),
    };
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    if serde_json::to_writer(&mut stdout, &value).is_err()
        || stdout.write_all(b"\n").is_err()
        || stdout.flush().is_err()
    {
        return 1;
    }
    code
}
