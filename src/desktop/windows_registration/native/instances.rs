//! Original Task Scheduler instances and their independently verified service.
//! These objects are private native custody, never decoded request authority.
use super::*;
use crate::desktop::{protocol, store::Location, transport};
use std::{
    fs::File,
    io::{BufReader, Read, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};
use zerus_client_core::desktop::windows::process::Original;

mod image;

fn task(folder: &ITaskFolder, plan: &Plan, wanted: &Shape) -> Result<IRegisteredTask> {
    let task = unsafe { folder.GetTask(&BSTR::from(&plan.name)) }?;
    let (owner, acl) = security(&task)?;
    Task {
        shape: shape(&unsafe { task.Definition() }?)?,
        owner,
        acl,
        enabled: unsafe { task.Enabled() }?.as_bool(),
        state: unsafe { task.State() }?.0,
    }
    .validate(wanted, &plan.sid)?;
    ensure!(
        text(&unsafe { task.Path() }?)? == format!(r"\{}", plan.name),
        "Owned task path differs."
    );
    Ok(task)
}

fn guid(instance: &IRunningTask) -> Result<String> {
    let value = text(&unsafe { instance.InstanceGuid() }?)?;
    let id = uuid::Uuid::parse_str(&value).context("Task instance identity unavailable.")?;
    ensure!(!id.is_nil(), "Task instance identity unavailable.");
    Ok(value)
}

fn instances(native: &Native, task: &IRegisteredTask, until: Instant) -> Result<Vec<IRunningTask>> {
    ensure!(
        Instant::now() < until,
        "Running-task verification deadline expired."
    );
    let owned_path = text(&unsafe { task.Path() }?)?;
    // GetInstances reserves flags=0 and may omit the maintained hidden task.
    // Use the documented hidden-aware API on the SAME retained connection.
    let list = unsafe { native.scheduler.GetRunningTasks(TASK_ENUM_HIDDEN.0) }?;
    let count = unsafe { list.Count() }?;
    policy::running::select(count, &owned_path, until, |index| {
        let instance = unsafe { list.get_Item(&VARIANT::from(index)) }?;
        Ok((text(&unsafe { instance.Path() }?)?, instance))
    })
}

/// A filtered empty collection alone cannot establish absence or drain.
fn idle(task: &IRegisteredTask) -> Result<bool> {
    policy::running::idle(unsafe { task.State() }?.0)
}

/// Original-ID observation only. A filtered empty collection during activity,
/// queued/unknown state, duplicates or unreadable GUIDs supply no absence proof.
/// No later instance's PID, endpoint or capability is acquired here.
pub(super) fn original_guid_absent(native: &Native, plan: &Plan, wanted: &Shape,
    original: &policy::mode::ObservedInstance, until: Instant) -> Result<bool> {
    original.validate()?;
    let task = task(&native.folder, plan, wanted)?;
    let state = unsafe { task.State() }?.0;
    ensure!(matches!(state, 3 | 4), "Queued or unknown task activity requires review.");
    let current = instances(native, &task, until)?;
    ensure!((state == 3) == current.is_empty(), "Task activity cannot establish original GUID absence.");
    for instance in current {
        if uuid::Uuid::parse_str(&guid(&instance)?)? == uuid::Uuid::parse_str(&original.guid)? {
            return Ok(false);
        }
    }
    ensure!(unsafe { task.State() }?.0 == state && Instant::now() < until,
        "Task activity changed during original observation.");
    Ok(true)
}

fn session(pid: u32) -> Result<u32> {
    let mut session = 0;
    ensure!(
        unsafe {
            windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(pid, &mut session)
        } != 0
            && session > 0,
        "Task process session unavailable."
    );
    Ok(session)
}

pub(super) fn selected_image(path: &Path) -> Result<(File, crate::platform::FileIdentity)> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let (id, metadata) = crate::platform::FileIdentity::with_metadata(&file)?;
    ensure!(
        metadata.is_file()
            && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && id.inode != 0,
        "Selected desktop image unavailable."
    );
    Ok((file, id))
}

struct Service {
    process: Arc<Original>,
    image: image::Loaded,
    epoch: String,
    generation: u64,
}
impl Service {
    fn observe(
        location: &Location,
        plan: &Plan,
        process: Arc<Original>,
        until: Instant,
    ) -> Result<Option<Self>> {
        let Some(stream) = crate::desktop::connection(location, "status", None)? else {
            return Ok(None);
        };
        let pid = transport::server_pid(&stream)?;
        ensure!(
            process.pid == pid && process.own && process.current(),
            "Desktop process identity unavailable."
        );
        let current_session = crate::host::token::windows_session()?;
        ensure!(
            session(pid)? == current_session,
            "Desktop task belongs to another session."
        );
        let bytes = protocol::line(&mut BufReader::new(stream), protocol::FRAME_BYTES)?
            .context("Desktop readiness disconnected.")?;
        let reply: Value = serde_json::from_slice(&bytes)?;
        let value = &reply["value"];
        let lifecycle = &value["lifecycle"];
        ensure!(
            reply["abi"] == crate::desktop::ABI
                && lifecycle["abi"] == "desktop-lifecycle-v1"
                && lifecycle["configuration"] == location.configuration
                && lifecycle["ipc_ready"] == true
                && lifecycle["pid"].as_u64() == Some(pid as u64),
            "Desktop readiness does not match the original process."
        );
        let epoch = lifecycle["epoch"]
            .as_str()
            .context("Desktop epoch unavailable.")?;
        ensure!(
            uuid::Uuid::parse_str(epoch).is_ok_and(|id| id.to_string() == epoch),
            "Desktop epoch unavailable."
        );
        let generation = lifecycle["generation"].as_u64().filter(|n|*n <= 9_007_199_254_740_991)
            .context("Desktop generation unavailable.")?;
        let wanted_mode = if plan.argv.last().is_some_and(|v| v == "--background") {
            "background"
        } else {
            "passive"
        };
        ensure!(
            lifecycle["mode"] == wanted_mode,
            "Desktop running mode differs from the selected task."
        );
        let arguments = process.arguments()?;
        ensure!(
            arguments == plan.argv,
            "Desktop process command differs from the owned task."
        );
        // EnginePID alone never supplies a service PID. Require the SAME
        // process independently verified through the original desktop pipe.
        // A common scheduler/ancestor engine cannot bind a specific instance.
        ensure!(
            Instant::now() < until && process.current(),
            "Original task engine is no longer the verified desktop process."
        );
        // The original instance/pipe/kernel binding above selects this exact
        // runtime. An installer may have moved its image aside during upgrade;
        // retain that image independently of the replacement's selected file.
        let image = image::Loaded::capture(&process.handle(), Path::new(&plan.argv[0]), until)?;
        let value = Self {
            process,
            image,
            epoch: epoch.into(),
            generation,
        };
        value.check()?;
        value.exchange(location, plan, until)?;
        Ok(Some(value))
    }
    fn exchange(&self, location: &Location, plan: &Plan, until: Instant) -> Result<()> {
        self.status(location, plan, until).map(|_| ())
    }
    fn status(&self, location: &Location, plan: &Plan, until: Instant) -> Result<Value> {
        self.check()?;
        let mut stream =
            transport::connect(location)?.context("Original desktop pipe disconnected.")?;
        ensure!(
            transport::server_pid(&stream)? == self.process.pid && self.process.current(),
            "Original desktop pipe peer changed."
        );
        let remaining = until
            .checked_duration_since(Instant::now())
            .context("Desktop readiness deadline expired.")?;
        stream.set_read_timeout(Some(remaining.min(crate::desktop::TIMEOUT)))?;
        stream.set_write_timeout(Some(remaining.min(crate::desktop::TIMEOUT)))?;
        stream.write_all(&protocol::encode(&json!({"abi":crate::desktop::ABI,"configuration":location.configuration,"op":"status"}))?)?;
        let mut reader = BufReader::new(stream);
        let bytes = protocol::line(&mut reader, protocol::FRAME_BYTES)?
            .context("Original desktop pipe ended before its reply.")?;
        let remaining = until.checked_duration_since(Instant::now())
            .context("Original status EOF deadline expired.")?;
        reader.get_ref().set_read_timeout(Some(remaining.min(crate::desktop::TIMEOUT)))?;
        ensure!(reader.read(&mut [0])? == 0, "Original status pipe has extra output; drain unconfirmed.");
        let reply: Value = serde_json::from_slice(&bytes)?;
        let life = &reply["value"]["lifecycle"];
        let mode = if plan.argv.last().is_some_and(|v| v == "--background") {
            "background"
        } else {
            "passive"
        };
        ensure!(
            reply["abi"] == crate::desktop::ABI && reply["t"] == "status"
                && life["configuration"] == location.configuration
                && life["epoch"] == self.epoch
                && life["generation"].as_u64() == Some(self.generation)
                && life["pid"].as_u64() == Some(self.process.pid as u64)
                && life["mode"] == mode
                && life["ipc_ready"] == true
                && life["abi"] == "desktop-lifecycle-v1",
            "Original desktop pipe reply changed."
        );
        self.check()?;
        ensure!(Instant::now() < until, "Original status deadline expired.");
        Ok(reply["value"].clone())
    }
    fn check(&self) -> Result<()> {
        ensure!(self.process.current(), "Original desktop process changed.");
        self.image.check(&self.process.handle())?;
        ensure!(self.process.current(), "Original desktop process changed.");
        Ok(())
    }
}

/// The original interface, immutable instance GUID and actual process handles.
/// No Clone/serde or lookup-by-saved-PID constructor is provided.
pub(super) struct Running {
    instance: IRunningTask,
    guid: String,
    path: String,
    engine: u32,
    service: Service,
    stopped: bool,
}
impl Running {
    /// Read-only observations of these retained pins; no process is reopened.
    pub(super) fn refresh_images(&self) -> Result<(String, String)> {
        self.check()?;
        let (old, new) = self.service.image.ids();
        let encode = |(volume, file): zerus_client_core::desktop::windows::image::FileId|
            format!("{volume:016x}{file:032x}");
        Ok((encode(old), encode(new)))
    }
    pub(super) fn image_files_current(&self) -> Result<()> {
        self.service.image.check_files()
    }
    /// Diagnostic observation of the SAME retained kernel process, no PID lookup.
    pub(super) fn process_exited(&self) -> bool {
        self.service.process.exited()
    }
    pub(super) fn retirement_drained(&self, location: &Location, plan: &Plan, until: Instant) -> Result<bool> {
        self.check()?;
        let value = self.service.status(location, plan, until)?;
        self.check()?;
        policy::mode::refresh::drained(&value)
    }
    pub(super) fn scope(&self) -> (String, u64) {
        (self.service.epoch.clone(), self.service.generation)
    }
    pub(super) fn readiness(&self, location: &Location, plan: &Plan, until: Instant) -> Result<()> {
        self.check()?;
        self.service.exchange(location, plan, until)?;
        self.check()
    }
    pub(super) fn revoke(
        &self,
        location: &Location,
        request: Value,
        until: Instant,
    ) -> Result<Value> {
        ensure!(
            matches!(request["op"].as_str(), Some("revoke" | "revoke_status")),
            "Only the original rollback or its read-only result may use this channel."
        );
        let id = request["request_id"]
            .as_str()
            .context("Original rollback UUID unavailable.")?
            .to_owned();
        self.check()?;
        let mut stream =
            transport::connect(location)?.context("Original desktop service disconnected.")?;
        stream.set_read_timeout(Some(
            until
                .checked_duration_since(Instant::now())
                .context("Original rollback deadline expired.")?
                .min(Duration::from_secs(10)),
        ))?;
        stream.set_write_timeout(Some(crate::desktop::TIMEOUT))?;
        ensure!(
            transport::server_pid(&stream)? == self.service.process.pid,
            "Desktop service changed before rollback; no request was offered."
        );
        self.check()?;
        let hello = json!({"abi":crate::desktop::ABI,"configuration":location.configuration,"op":"request","request":request});
        stream.write_all(&protocol::encode(&hello)?)?;
        let bytes = protocol::line(&mut BufReader::new(stream), protocol::FRAME_BYTES)?
            .context("Original rollback result is unknown.")?;
        let reply: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            reply["abi"] == crate::desktop::ABI && reply["t"] != "error",
            "Original rollback result is unavailable."
        );
        let receipt = reply["value"].clone();
        ensure!(
            receipt["request_id"] == id,
            "Original rollback receipt identity differs."
        );
        if receipt["checkpointed"] == true {
            ensure!(
                receipt["configuration"] == location.configuration
                    && receipt
                        .get("writer_epoch")
                        .is_some_and(|epoch| epoch.is_null()
                            || epoch
                                .as_str()
                                .is_some_and(|epoch| uuid::Uuid::parse_str(epoch)
                                    .is_ok_and(|id| id.to_string() == epoch))),
                "Original rollback configuration or writer epoch is unavailable."
            );
        }
        ensure!(
            !matches!(
                receipt["code"].as_str(),
                Some("writer_epoch_changed" | "writer_authority_changed" | "request_conflict")
            ),
            "Original rollback authority changed; retain its UUID for explicit cancellation."
        );
        self.check()?;
        Ok(receipt)
    }
    pub(super) fn observation(
        &self,
    ) -> crate::desktop::windows_registration_policy::mode::ObservedInstance {
        crate::desktop::windows_registration_policy::mode::ObservedInstance {
            guid: self.guid.clone(),
            pid: self.service.process.pid,
            birth: self.service.process.birth,
            epoch: self.service.epoch.clone(),
        }
    }
    fn bind(
        instance: IRunningTask,
        location: &Location,
        plan: &Plan,
        candidate: Option<&Arc<Original>>,
        until: Instant,
    ) -> Result<Option<Self>> {
        let id = guid(&instance)?;
        let path = text(&unsafe { instance.Path() }?)?;
        ensure!(
            path == format!(r"\{}", plan.name),
            "Task instance path differs."
        );
        unsafe { instance.Refresh() }?;
        let engine = unsafe { instance.EnginePID() }?;
        if engine == 0 {
            ensure!(candidate.is_none(), "Original task engine disappeared.");
            return Ok(None);
        }
        // Pin the first engine incarnation even before its pipe exists. This
        // is only a candidate; the native pipe/config/instance join below is
        // still required before any ownership or stop decision.
        let process = match candidate {
            Some(original) => {
                ensure!(
                    original.pid == engine && original.current(),
                    "Original started task engine changed."
                );
                original.clone()
            }
            None => Arc::new(Original::open(engine)?),
        };
        let Some(service) = Service::observe(location, plan, process, until)? else {
            return Ok(None);
        };
        let value = Self {
            instance,
            guid: id,
            path,
            engine,
            service,
            stopped: false,
        };
        value.check()?;
        Ok(Some(value))
    }
    pub(super) fn check(&self) -> Result<()> {
        ensure!(!self.stopped, "Original task stop was already offered.");
        self.service.check()?;
        unsafe { self.instance.Refresh() }?;
        ensure!(
            guid(&self.instance)? == self.guid
                && unsafe { self.instance.State() }? == TASK_STATE_RUNNING
                && text(&unsafe { self.instance.Path() }?)? == self.path
                && unsafe { self.instance.EnginePID() }? == self.engine,
            "Original task instance changed."
        );
        self.service.check()
    }
    pub(super) fn stop(&mut self, fence: &mut impl FnMut() -> Result<()>) -> Result<()> {
        self.stop_measured(fence, None)
    }
    fn stop_measured(
        &mut self,
        fence: &mut impl FnMut() -> Result<()>,
        timing: Option<&policy::mode::timing::Timing>,
    ) -> Result<()> {
        use policy::mode::timing::Step;
        fence()?;
        if let Some(timing) = timing {
            timing.measure(Step::NativeRechecks, || self.check())?;
        } else {
            self.check()?;
        }
        fence()?;
        // Mark before calling COM: a failed/unknown result is never replayed.
        self.stopped = true;
        let stop = || unsafe { self.instance.Stop() };
        match timing {
            Some(timing) => timing.measure(Step::InstanceStop, stop),
            None => stop(),
        }.context("Original task stop result is unknown; inspect the pending transition.")?;
        fence()
    }
    pub(super) fn exited(
        &self,
        native: &Native,
        selected: &IRegisteredTask,
        until: Instant,
    ) -> Result<bool> {
        ensure!(self.stopped, "Original task was not stopped.");
        if !self.service.process.exited() {
            return Ok(false);
        }
        let visible = instances(native, selected, until)?;
        if visible.is_empty() {
            return idle(selected);
        }
        Ok(visible
            .iter()
            .all(|instance| guid(instance).is_ok_and(|id| id != self.guid)))
    }
}

pub(super) fn original(
    native: &Native,
    location: &Location,
    plan: &Plan,
    wanted: &Shape,
    until: Instant,
) -> Result<Option<Running>> {
    let selected = task(&native.folder, plan, wanted)?;
    let mut values = instances(native, &selected, until)?;
    match values.pop() {
        Some(instance) => Running::bind(instance, location, plan, None, until)?
            .map(Some)
            .context("Task is running but its original desktop process cannot be verified."),
        None => {
            ensure!(
                idle(&selected)?,
                "Owned task is active but its original instance is not visible."
            );
            ensure!(
                crate::desktop::connection(location, "status", None)?.is_none(),
                "A desktop process exists outside this task instance."
            );
            Ok(None)
        }
    }
}

/// Definition-only read; it obtains no later task instance/process capability.
pub(super) fn definition_current(native: &Native, plan: &Plan, wanted: &Shape, until: Instant) -> Result<IRegisteredTask> {
    ensure!(Instant::now() < until, "Owned definition deadline expired.");
    let selected = task(&native.folder, plan, wanted)?;
    ensure!(Instant::now() < until, "Owned definition deadline expired.");
    Ok(selected)
}

pub(super) fn stopped(
    native: &Native,
    plan: &Plan,
    wanted: &Shape,
    original: &Running,
    until: Instant,
) -> Result<bool> {
    original.exited(native, &task(&native.folder, plan, wanted)?, until)
}

/// Read the SAME retained original under the caller's registration lock.
/// Refusal is not absence and never acquires a later instance or Stop ability.
pub(super) fn check_original_selection(
    native: &Native,
    plan: &Plan,
    wanted: &Shape,
    original: &Running,
    until: Instant,
    fence: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    fence()?;
    let selected = task(&native.folder, plan, wanted)?;
    let values = instances(native, &selected, until)?;
    ensure!(
        values.len() == 1 && guid(&values[0])? == original.guid,
        "Original task selection changed before stop."
    );
    original.check()?;
    fence()
}

pub(super) fn stop_original(
    native: &Native,
    plan: &Plan,
    wanted: &Shape,
    original: &mut Running,
    until: Instant,
    fence: &mut impl FnMut() -> Result<()>,
    timing: &policy::mode::timing::Timing,
) -> Result<()> {
    super::mode::stop::checked(
        original,
        fence,
        |original, fence| timing.measure(policy::mode::timing::Step::NativeRechecks, || {
            check_original_selection(native, plan, wanted, original, until, fence)
        }),
        |original, fence| original.stop_measured(fence, Some(timing)),
    )
}

pub(super) fn replace(
    native: &Native,
    service: &ITaskService,
    location: &Location,
    old: (&Plan, &Shape),
    new: (&Plan, &Shape),
    until: Instant,
    fence: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    let (old_plan, old_shape) = old;
    let (new_plan, new_shape) = new;
    ensure!(
        old_plan.name == new_plan.name
            && old_plan.sid == new_plan.sid
            && old_plan.configuration == new_plan.configuration
            && old_plan.argv[..7] == new_plan.argv[..7],
        "A mode transition cannot change the installed task identity or paths."
    );
    let definition = unsafe { service.NewTask(0) }?;
    unsafe { definition.SetXmlText(&BSTR::from(&new_plan.definition)) }?;
    ensure!(
        shape(&definition)? == *new_shape,
        "Replacement task definition differs."
    );
    fence()?;
    let selected = task(&native.folder, old_plan, old_shape)?;
    ensure!(
        instances(native, &selected, until)?.is_empty() && idle(&selected)?,
        "Original task instances have not drained."
    );
    ensure!(
        crate::desktop::connection(location, "status", None)?.is_none(),
        "Desktop endpoint still has an owner."
    );
    let sddl = unsafe { selected.GetSecurityDescriptor(5) }?;
    fence()?;
    // Preserve the verified owner/DACL and do not execute registration triggers.
    // This is UPDATE only: a disappearing task must not become an implicit install.
    unsafe {
        native.folder.RegisterTask(
            &BSTR::from(&new_plan.name),
            &BSTR::from(&new_plan.definition),
            TASK_UPDATE.0 | TASK_IGNORE_REGISTRATION_TRIGGERS.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0,
            &VARIANT::from(new_plan.sid.as_str()),
            &VARIANT::default(),
            TASK_LOGON_INTERACTIVE_TOKEN,
            &VARIANT::from(sddl.to_string().as_str()),
        )
    }
    .context("Task update result is unknown; retain the pending transition.")?;
    fence()?;
    task(&native.folder, new_plan, new_shape)?;
    Ok(())
}

pub(super) struct Starting {
    instance: IRunningTask,
    guid: String,
    path: String,
    engine: Option<Arc<Original>>,
}
fn pin_engine(candidate: &mut Option<Arc<Original>>, pid: u32) -> Result<Option<Arc<Original>>> {
    if let Some(original) = candidate {
        ensure!(
            pid == original.pid && original.current(),
            "Original started task engine changed."
        );
        return Ok(Some(original.clone()));
    }
    if pid == 0 {
        return Ok(None);
    }
    let original = Arc::new(Original::open(pid)?);
    ensure!(
        original.current(),
        "Original started task engine is unavailable."
    );
    *candidate = Some(original.clone());
    Ok(Some(original))
}
impl Starting {
    pub(super) fn id(&self) -> &str {
        &self.guid
    }
    pub(super) fn poll(
        &mut self,
        location: &Location,
        plan: &Plan,
        until: Instant,
    ) -> Result<Option<Running>> {
        unsafe { self.instance.Refresh() }?;
        ensure!(
            guid(&self.instance)? == self.guid
                && text(&unsafe { self.instance.Path() }?)? == self.path,
            "Original started task instance changed."
        );
        let engine = unsafe { self.instance.EnginePID() }?;
        if pin_engine(&mut self.engine, engine)?.is_none() {
            return Ok(None);
        }
        // Clone retains this same returned COM interface; it never reselects
        // an instance by task name, saved GUID or process ID.
        Running::bind(
            self.instance.clone(),
            location,
            plan,
            self.engine.as_ref(),
            until,
        )
    }
}

pub(super) fn observe_started(
    native: &Native,
    location: &Location,
    plan: &Plan,
    wanted: &Shape,
    expected: &str,
    until: Instant,
) -> Result<Option<Running>> {
    let selected = task(&native.folder, plan, wanted)?;
    let Some(instance) = instances(native, &selected, until)?.pop() else {
        return Ok(None);
    };
    if guid(&instance)? != expected {
        return Ok(None);
    }
    Running::bind(instance, location, plan, None, until)
}

pub(super) fn start(
    native: &Native,
    location: &Location,
    plan: &Plan,
    wanted: &Shape,
    until: Instant,
    fence: &mut impl FnMut() -> Result<()>,
) -> Result<Starting> {
    let session = crate::host::token::windows_session()?;
    ensure!(
        session > 0 && session <= i32::MAX as u32,
        "Interactive task session unavailable."
    );
    fence()?;
    let selected = task(&native.folder, plan, wanted)?;
    ensure!(
        instances(native, &selected, until)?.is_empty() && idle(&selected)?,
        "A task instance already exists; no second start is allowed."
    );
    ensure!(
        crate::desktop::connection(location, "status", None)?.is_none(),
        "An existing desktop endpoint requires inspection."
    );
    ensure!(
        crate::host::token::windows_session()? == session,
        "Interactive task session changed."
    );
    fence()?;
    let instance = unsafe {
        selected.RunEx(
            &VARIANT::default(),
            // The user parameter is the already-verified SID, not an account
            // name. Keep both its type and the exact interactive session explicit.
            TASK_RUN_USE_SESSION_ID.0 | TASK_RUN_USER_SID.0,
            session as i32,
            &BSTR::from(plan.sid.as_str()),
        )
    }
    .context("Task start result is unknown; do not repeat this start.")?;
    let id = guid(&instance)?;
    let path = text(&unsafe { instance.Path() }?)?;
    ensure!(
        path == format!(r"\{}", plan.name),
        "Started task path differs."
    );
    fence()?;
    Ok(Starting {
        instance,
        guid: id,
        path,
        engine: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_engine_incarnation_is_not_reopened_when_its_numeric_pid_matches() {
        let mut candidate = None;
        let first = pin_engine(&mut candidate, std::process::id())
            .unwrap()
            .unwrap();
        assert!(first.current());
        drop(first);
        // A changed incarnation invalidates the retained proof. A fresh open
        // of this numeric PID would succeed, but must never replace that proof.
        Arc::get_mut(candidate.as_mut().unwrap()).unwrap().birth += 1;
        assert!(Original::open(std::process::id()).unwrap().current());
        assert!(pin_engine(&mut candidate, std::process::id()).is_err());
        assert!(pin_engine(&mut candidate, 0).is_err());
    }
    #[test]
    fn engine_exit_before_pipe_readiness_cannot_be_replaced_by_a_later_process() {
        use std::os::windows::process::CommandExt;
        use std::process::{Child, Command, Stdio};
        struct Held(Child);
        impl Drop for Held {
            fn drop(&mut self) {
                drop(self.0.stdin.take());
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Held(
            Command::new("cmd.exe")
                .args(["/D", "/Q", "/C", "set /p ZERUS_ENGINE_FIXTURE_WAIT="])
                .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut candidate = None;
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if pin_engine(&mut candidate, child.0.id()).is_ok() {
                break;
            }
            assert!(
                Instant::now() < until,
                "private engine did not become inspectable"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(child.0.stdin.take());
        let until = Instant::now() + Duration::from_secs(2);
        while child.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < until,
                "private original engine did not exit"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        child.0.wait().unwrap();
        assert!(candidate.as_ref().unwrap().exited());
        assert!(pin_engine(&mut candidate, child.0.id()).is_err());
        assert!(pin_engine(&mut candidate, std::process::id()).is_err());
    }
}
