//! Explicit owned-task mode changes. No generic registration request enters here.
use super::*;
use crate::{
    config::Config,
    desktop::{
        ownership, registration, revoke,
        store::{HandoffLock, Location},
        ui_authority,
    },
    platform,
};
use policy::mode::{Budget, Mode, Phase, Record, RECORD_BYTES};
use policy::mode::timing::{self, Step, Timing};
mod recovery;
mod refresh;
pub(super) mod stop;
mod boot;
mod review;
#[cfg(debug_assertions)]
mod held_pause;
use std::{
    path::Path,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

pub(in crate::desktop::windows_registration) const ENTRY: &str = "__desktop-registration-mode";
const MODE_ABI: &str = "desktop-registration-mode-v1";
const RECORD: &str = stop::RECORD;

struct Admission {
    scope: ui_authority::Scope,
    caller: ui_authority::registration::Caller,
}
impl Admission {
    fn check(&self) -> Result<()> {
        self.caller.check(&self.scope)
    }
}

fn environment(config: &Config, binary: &str) -> Result<Vec<(String, String)>> {
    let mut values = super::super::helper_environment("")?;
    for (key, value) in [
        ("HOME", config.home.clone()),
        ("USERPROFILE", config.home.clone()),
        (
            "HGS_CONFIG_DIR",
            config
                .dir
                .to_str()
                .context("Config path is not Unicode.")?
                .into(),
        ),
        (
            "HGS_STATE_DIR",
            config
                .state
                .to_str()
                .context("State path is not Unicode.")?
                .into(),
        ),
        ("HGS_EXECUTABLE", binary.into()),
    ] {
        values.push((key.into(), value));
    }
    if let Ok(path) = std::env::var("LOCALAPPDATA") {
        values.push(("LOCALAPPDATA".into(), path));
    }
    #[cfg(debug_assertions)]
    held_pause::forward(&mut values);
    Ok(values)
}

pub(in crate::desktop::windows_registration) fn command(
    config: &Config,
    location: &Location,
    request: Value,
) -> Result<Value> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    // The one mode-helper spawn: refuse a unit-test image before caller capture.
    let helper = crate::self_exec::current()?;
    let scope = ui_authority::Scope::new(config, location);
    // Capture the real caller BEFORE the helper environment is sanitized.
    let caller = ui_authority::registration::Caller::capture(&scope)?;
    let attached = caller.source() != "installed_frontend";
    let admission = Arc::new(Admission { scope, caller });
    admission.check()?;
    let plan = registration::windows_plan(config, location)?;
    let binary = plan["argv"][0]
        .as_str()
        .context("Selected desktop CLI unavailable.")?;
    let env = environment(config, binary)?;
    let frame = serde_json::to_vec(&request)?;
    ensure!(
        !frame.is_empty() && frame.len() <= CAP,
        "Mode request exceeds its bound."
    );
    let invocation = zerus_client_core::actions::Invocation {
        args: vec![ENTRY.into(), frame.len().to_string()],
        stdin: Some(frame),
        timeout: Budget::exchange_timeout(),
        request_id: None,
    };
    let proof = admission.clone();
    let child_env = env.clone();
    let references: Vec<_> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let output = zerus_client_core::run::run_owned(
        &helper, &invocation, &references, &AtomicBool::new(false),
        move |command| {
            proof.check().map_err(std::io::Error::other)?;
            command.env_clear().envs(child_env).creation_flags(if attached { 0 } else { CREATE_NO_WINDOW }).spawn()
        }, admission,
    ).map_err(|error| anyhow::anyhow!("Desktop mode result is unavailable ({error:?}); inspect status and retain the original pending UUID."))?;
    ensure!(output.len() <= CAP, "Mode response exceeds its bound.");
    let mut reply: Value = serde_json::from_slice(&output)?;
    ensure!(reply["abi"] == MODE_ABI, "Mode response ABI differs.");
    reply["operation"] = request["operation"].clone();
    reply["requested_mode"] = request["mode"].clone();
    if reply["error"].is_string() {
        if matches!(request["operation"].as_str(), Some("refresh" | "refresh_cancel")) {
            return Ok(refresh::after_error(config, location, &request, reply["error"].as_str().expect("error")));
        }
        reply["configuration"] = location.configuration.clone().into();
        reply["task_name"] = plan["task_name"].clone();
        reply["ok"] = false.into();
        let record = pending_status(config, location, &plan)?;
        reply["pending_transition"] = record["pending_transition"].clone();
        let pending = reply["pending_transition"].is_object();
        reply["changed"] = if pending && request["operation"] != "review" { Value::Null } else { false.into() };
        reply["manager_changed"] = reply["changed"].clone();
        reply["outcome"] = if pending || request["operation"] == "review" { "unknown" } else { "refused" }.into();
        return Ok(reply);
    }
    ensure!(
        reply["configuration"] == location.configuration,
        "Mode response differs from the selected registration."
    );
    let pending = reply
        .as_object_mut()
        .expect("object")
        .remove("pending")
        .unwrap_or(Value::Null);
    reply["ok"] = pending.is_null().into();
    if !reply["refresh_abi"].is_string() {
        reply["outcome"] = if request["operation"] == "review" {
            "unknown"
        } else if pending.is_null() {
            "settled"
        } else {
            "pending"
        }.into();
    } else if reply["outcome"].is_null() {
        reply["outcome"] = if request["operation"] == "refresh_cancel" {
            "refused"
        } else {
            "observed"
        }.into();
    }
    reply["pending_transition"] = pending;
    Ok(reply)
}

pub(in crate::desktop::windows_registration) fn refresh_result(config: &Config, location: &Location, id: &str) -> Result<Value> {
    refresh::result(config,location,id)
}

fn plans(config: &Config, location: &Location, sid: &str) -> Result<(Plan, Plan)> {
    let passive = registration::windows_plan(config, location)?;
    let base = Plan::parse(&json!({"schema":1,"operation":"check","plan":passive}), sid)?;
    let background = registration::windows_plan_mode(config, location, true)?;
    let mut other = base.clone();
    other.argv.push("--background".into());
    ensure!(
        background["argv"] == serde_json::to_value(&other.argv)?
            && background["configuration"] == base.configuration,
        "Maintained mode definitions differ."
    );
    other.definition = background["definition"]
        .as_str()
        .context("Background definition unavailable.")?
        .into();
    Ok((base, other))
}
fn wanted(plan: &Plan) -> Result<Shape> {
    Ok(plan.shape(crate::host::spawn::command_line(&plan.argv[1..])?))
}
const FIRST_ANCHOR_BYTES: usize = 1024;
/// The independent first-choice anchor (zerus-yfbd.1): the latest reserved
/// first-choice original, in the config folder, outside the desktop state
/// folder that holds this mode record and the Service Grant. A missing,
/// emptied or replaced journal or a wiped state folder is therefore "lost",
/// never "never written". Only wiping both trees looks like a fresh profile.
fn anchor_path(config: &Config, location: &Location) -> std::path::PathBuf {
    config
        .dir
        .join(format!("windows-first-choice-{}.json", location.configuration))
}
fn read_anchor(config: &Config, location: &Location, plan: &Plan) -> Result<Option<String>> {
    match crate::desktop::handoff::read_private(&anchor_path(config, location), FIRST_ANCHOR_BYTES) {
        Ok(bytes) => {
            let value: Value =
                serde_json::from_slice(&bytes).context("First-choice anchor unavailable.")?;
            let request = value["request"].as_str().unwrap_or("");
            ensure!(
                value["schema"] == 1
                    && value["configuration"] == location.configuration
                    && value["sid"] == plan.sid
                    && value.as_object().is_some_and(|o| o.len() == 4)
                    && uuid::Uuid::parse_str(request)
                        .is_ok_and(|u| !u.is_nil() && u.to_string() == request),
                "First-choice anchor unavailable; keep it and the mode record, and inspect status."
            );
            Ok(Some(request.into()))
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
/// The anchor proves a lost journal only from outside the state tree that
/// holds the journal and the Service Grant. Existing folders resolve natively
/// (junctions, symlinks, short names and drive aliases); the policy compares
/// the results by components with Windows case folding. An unresolvable or
/// non-Unicode layout counts as overlapping. Nothing repairs or moves it.
fn anchor_layout_separate(config: &Config, location: &Location) -> bool {
    let canonical = |path: &std::path::Path| {
        zerus_desktop_migration::canonical(path)
            .ok()
            .and_then(|path| path.to_str().map(str::to_owned))
    };
    match (
        canonical(&config.dir),
        canonical(&config.state),
        canonical(&location.root),
    ) {
        (Some(anchor), Some(state), Some(root)) => {
            policy::mode::first::anchor_separate(&anchor, &state, &root)
        }
        _ => false,
    }
}
const ANCHOR_LAYOUT: &str = "The first background choice needs the config folder outside the desktop state folder; this layout keeps both in one loss domain. Nothing was reserved or changed.";
pub(in crate::desktop::windows_registration) fn ensure_first_layout(
    config: &Config,
    location: &Location,
) -> Result<()> {
    ensure!(anchor_layout_separate(config, location), ANCHOR_LAYOUT);
    Ok(())
}
/// The journal and its anchor as one generation: every writer holds the
/// registration lock from anchor publication through its record save, so a
/// reader under the same lock never pairs one commit's anchor with another's
/// record. A busy writer is bounded unavailability (the lock wait), never loss.
fn journal(
    sid: &str,
    config: &Config,
    location: &Location,
    plan: &Plan,
) -> Result<(Record, Option<String>)> {
    let _lock = Lock::acquire(sid)?;
    let record = load(location, plan)?;
    let lost = record.first_lost(read_anchor(config, location, plan)?.as_deref())
        .or(refresh::lost(config, location, plan, &record)?);
    Ok((record, lost))
}
/// Durable before the record save that reserves this first choice.
fn write_anchor(
    config: &Config,
    location: &Location,
    plan: &Plan,
    request: &str,
    fence: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    ensure!(anchor_layout_separate(config, location), ANCHOR_LAYOUT);
    fence()?;
    let bytes = serde_json::to_vec(&json!({"schema":1,"configuration":location.configuration,
        "sid":plan.sid,"request":request}))?;
    let mut file = tempfile::NamedTempFile::new_in(&config.dir)?;
    platform::make_private(file.as_file())?;
    ensure!(
        platform::verify_private_file(file.as_file())?,
        "First-choice anchor is not private."
    );
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    fence()?;
    platform::persist(file, anchor_path(config, location)).map_err(|error| error.error)?;
    platform::sync_dir(&config.dir)?;
    fence()
}
fn load(location: &Location, plan: &Plan) -> Result<Record> {
    match crate::desktop::handoff::read_private(&location.root.join(RECORD), RECORD_BYTES) {
        Ok(bytes) => Record::decode(&bytes, plan),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Record::empty(plan)
        }
        Err(error) => Err(error),
    }
}
/// Original-ID metadata only. No COM connection, caller admission, Grant,
/// current-instance lookup or readiness inference belongs to this path.
pub(in crate::desktop::windows_registration) fn result(
    config: &Config, location: &Location, request: &str,
) -> Result<Value> {
    let sid = crate::host::token::user_sid()?;
    let preview = registration::windows_plan(config, location)?;
    let plan = Plan::parse(&json!({"schema":1,"operation":"status","plan":preview}), &sid)?;
    let (record, lost) = journal(&sid, config, location, &plan)?;
    // A journal that lost its anchored first choice answers nothing settled.
    if let Some(lost) = lost {
        return Ok(json!({"abi":MODE_ABI,"schema":1,"configuration":location.configuration,
            "request_id":request,"read_only":true,"receipt":null,
            "outcome":policy::mode::review::ChoicePhase::Unknown,"code":"first_choice_lost",
            "first_choice_lost":lost,"manager_changed":false,"authority_changed":false,"changed":false}));
    }
    // A reviewed choice or a first choice: each answers only its own original ID.
    let receipt = match record.choices.iter().find(|r|r.request == request) {
        Some(r) => Some((serde_json::to_value(r)?, r.phase)),
        None => match record.first_choices.iter().find(|r|r.request == request) {
            Some(r) => Some((serde_json::to_value(r)?, r.phase)),
            None => None,
        },
    };
    Ok(json!({"abi":MODE_ABI,"schema":1,"configuration":location.configuration,
        "request_id":request,"read_only":true,"receipt":receipt.as_ref().map(|r|&r.0),
        "outcome":receipt.as_ref().map(|r|r.1).unwrap_or(policy::mode::review::ChoicePhase::Unknown),
        "code":if receipt.is_some() {"original_choice_result"} else {"original_choice_unavailable"},
        "manager_changed":false,"authority_changed":false,"changed":false}))
}
pub(in crate::desktop::windows_registration) fn pending_status(
    config: &Config,
    location: &Location,
    preview: &Value,
) -> Result<Value> {
    let sid = crate::host::token::user_sid()?;
    let plan = Plan::parse(
        &json!({"schema":1,"operation":"status","plan":preview}),
        &sid,
    )?;
    let (record, lost) = journal(&sid, config, location, &plan)?;
    // An anchor inside the state loss domain could never prove a loss: no
    // first choice is offered or admitted on that layout.
    let unavailable = (!anchor_layout_separate(config, location)).then_some("anchor_layout");
    Ok(
        json!({"pending_transition":record.pending,"cancelled_request_id":record.cancelled,"completed_request_id":record.completed,
            "reviewed_transition":record.reviewed.last(),
            "mode_choice":record.choices.last(),
            "first_choice":record.first_choices.last(),
            "first_choice_lost":lost,
            "first_choice_unavailable":unavailable,
            "activation_automatic_blocked":lost.is_some() || !record.reviewed.is_empty() || record.pending.as_ref().is_some_and(|p|p.review.is_some())}),
    )
}
fn save(
    location: &Location,
    plan: &Plan,
    record: &Record,
    fence: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    stop::publish(&location.root, fence, || {
        location.prepare()?;
        record.encode(plan)
    })
}

fn selected(native: &mut Native, passive: &Plan, background: &Plan) -> Result<Mode> {
    let current = native
        .read(&passive.name)?
        .context("Install the maintained passive task before changing mode.")?;
    if current.validate(&wanted(passive)?, &passive.sid).is_ok() {
        return Ok(Mode::Passive);
    }
    current.validate(&wanted(background)?, &background.sid)?;
    Ok(Mode::Background)
}

enum Exclusion {
    Rollback(Box<hold_child::Hold>),
    Handoff(HandoffLock),
}
struct Fence<'a> {
    admission: &'a Admission,
    location: &'a Location,
    background: bool,
    grant: Option<ownership::RegistrationGrant>,
    exclusion: Option<Exclusion>,
    until: Instant,
    stop_timing: Option<&'a Timing>,
    source: std::fs::File,
    identity: platform::FileIdentity,
    image: &'a Path,
}
impl Fence<'_> {
    fn phase(&mut self, phase: Budget) {
        self.until = phase.deadline(Instant::now());
    }
    fn check(&mut self) -> Result<()> {
        match self.stop_timing {
            Some(timing) => timing.measure(Step::NativeRechecks, || self.check_inner()),
            None => self.check_inner(),
        }
    }
    fn check_inner(&mut self) -> Result<()> {
        ensure!(
            Instant::now() < self.until,
            "Mode transition deadline reached; retain its original UUID."
        );
        self.admission.check()?;
        let held = platform::FileIdentity::of(&self.source)?;
        let (_, current) = instances::selected_image(self.image)?;
        ensure!(
            held.device == self.identity.device
                && held.inode == self.identity.inode
                && current.device == self.identity.device
                && current.inode == self.identity.inode,
            "Selected installed CLI changed during the transition."
        );
        if let Some(exclusion) = &mut self.exclusion {
            match exclusion {
                Exclusion::Rollback(hold) => hold.check()?,
                Exclusion::Handoff(lock) => {
                    lock.verify(self.location)?;
                    if self.background {
                        let (db, _verification) = revoke::hold::database(self.location)?;
                        ensure!(
                            crate::desktop::store::operation_undrained(&db)? == 0,
                            "Desktop operation resources have not drained."
                        );
                    }
                }
            }
        }
        // The retained guard checks the SAME database/Origin/incarnation and
        // current Service state. Do that stronger read at final admission;
        // reading it earlier AND then rereading a Boolean duplicated database
        // I/O at every publication/Stop fence without adding a condition.
        if let Some(grant) = &self.grant {
            grant.check(self.location)?;
        } else if self.background {
            ensure!(
                ownership::registration_granted(self.location)?,
                "Grant or Rearm from Zerus Settings is required before background mode."
            );
        }
        self.admission.check()
    }
}

fn marker(config: &Config, binary: &str) -> Result<()> {
    let invocation = zerus_client_core::actions::Invocation {
        args: vec!["--help".into()],
        stdin: None,
        timeout: Duration::from_secs(2),
        request_id: None,
    };
    let env = environment(config, binary)?;
    let refs: Vec<_> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let result =
        zerus_client_core::run::run_hidden_limited(Path::new(binary), &invocation, &refs, CAP)
            .map_err(|error| {
                anyhow::anyhow!("Selected helper ABI probe is unavailable: {error:?}")
            })?;
    let zerus_client_core::actions::Exit::Code {
        code: 0, stdout, ..
    } = result
    else {
        bail!("Selected helper ABI probe failed.");
    };
    ensure!(
        String::from_utf8(stdout)?
            .lines()
            .any(|line| line.trim() == "Desktop revoke hold ABI: desktop-revoke-hold-v1"),
        "Update the selected CLI before changing desktop mode."
    );
    Ok(())
}

fn revoke_original(
    location: &Location,
    request_id: &str,
    original: Option<&instances::Running>,
    until: Instant,
) -> Result<Value> {
    let command = revoke::cli(&["--json", "--request-id", request_id, "--reason", "rollback"])?;
    match original {
        Some(original) => original.revoke(location, revoke::payload(&command)?, until),
        None => revoke::offline(location, &command),
    }
}

fn rollback_result(
    location: &Location,
    id: &str,
    original: Option<&instances::Running>,
) -> Result<Value> {
    if let Some(original) = original {
        original.check()?;
    }
    let value = revoke::rollback_receipt(location, id)?;
    if let Some(original) = original {
        original.check()?;
    }
    Ok(value)
}

struct Runtime<'a> {
    config: &'a Config,
    location: &'a Location,
    native: &'a mut Native,
    scheduler: &'a ITaskService,
    admission: &'a Admission,
    passive: &'a Plan,
    background: &'a Plan,
}
fn effect(
    runtime: Runtime<'_>,
    desired: Mode,
    choice: Option<&policy::mode::review::Choice>,
    first: Option<&policy::mode::first::First>,
) -> Result<Value> {
    let until = Budget::Preflight.deadline(Instant::now());
    let Runtime {
        config,
        location,
        native,
        scheduler,
        admission,
        passive,
        background,
    } = runtime;
    let from = selected(native, passive, background)?;
    let old = if from == Mode::Passive {
        passive
    } else {
        background
    };
    let new = if desired == Mode::Passive {
        passive
    } else {
        background
    };
    let mut record = load(location, passive)?;
    // A first choice never takes the reviewed route: any pending, interrupted
    // or reviewed original refuses it here, under the owned lock.
    let requested = match first {
        Some(first) => {
            ensure!(choice.is_none() && desired == Mode::Background && from == Mode::Passive,
                "A first mode choice selects background from a passive registration only.");
            Some(record.first_choice(first)?)
        }
        None => record.choice(choice)?,
    };
    let resuming = record.pending.clone();
    if let Some(pending) = &resuming {
        ensure!(pending.review.is_none(), "Review interrupted mode change by its original UUID; a reboot barrier cannot resume or replay its effects.");
        if requested.is_some() || pending.desired != desired || !pending.phase.pre_manager() {
            return recovery::observe(
                recovery::Inputs {
                    config,
                    location,
                    native,
                    admission,
                    passive,
                    background,
                },
                &mut record,
                desired,
                from,
            );
        }
    }
    let (source, identity) = instances::selected_image(Path::new(&new.argv[0]))?;
    let mut fence = Fence {
        admission,
        location,
        background: desired == Mode::Background,
        grant: if desired == Mode::Background && (choice.is_some() || first.is_some()) {
            Some(ownership::RegistrationGrant::capture(location)?.context("Current Service Grant/Rearm required.")?)
        } else { None },
        exclusion: None,
        until,
        stop_timing: None,
        source,
        identity,
        image: Path::new(&new.argv[0]),
    };
    fence.check()?;
    let mut original = instances::original(native, location, old, &wanted(old)?, until)?;
    ensure!(first.is_none() || original.is_some(),
        "A first background choice needs the selected passive service running.");
    let observation = original.as_ref().map(instances::Running::observation);
    if let Some(pending) = &resuming {
        ensure!(
            pending.original == observation,
            "Original task or service changed; retain the pending UUID and inspect it."
        );
    }
    let consumer = if resuming.is_none() {
        admission.check()?;
        let boot = boot::current()?;
        ensure!(choice.is_none() || record.reviewed.last().is_some_and(|a|
            a.original.review.as_ref().is_some_and(|r|r.barrier.machine == boot.machine)),
            "The reviewed original belongs to another machine.");
        let (pid, birth) = admission.caller.consumer();
        admission.check()?;
        Some(policy::mode::review::Consumer { pid, birth, boot })
    } else { None };
    if resuming.is_none() && from == desired && original.is_some() {
        if let Some(choice) = choice {
            record.reserve_choice(choice, desired)?;
            finish_choice(&mut record, &choice.request, desired,
                original.as_ref().context("Original instance absent.")?, &fence)?;
            save(location, passive, &record, &mut || {
                fence.check()?;
                original.as_ref().context("Original instance absent.")?.check()?;
                ensure!(selected(native, passive, background)? == from, "Owned mode changed.");
                Ok(())
            })?;
        }
        return Ok(
            json!({"abi":MODE_ABI,"schema":1,"configuration":location.configuration,"task_name":passive.name,"selected_mode":desired.name(),"running_mode":desired.name(),"pending":null,"changed":false,"request_id":requested}),
        );
    }
    marker(config, &new.argv[0])?;
    let database = location.root.join("state.sqlite3").try_exists()?;
    if desired == Mode::Background || !database {
        // Do not reserve metadata for a transient exclusion/drain refusal.
        // Rollback alone must save its UUID before its authority reduction.
        fence.phase(Budget::Hold);
        location.prepare()?;
        let lock = location
            .try_handoff_lock()?
            .context("Desktop native or operation resources have not drained.")?;
        if desired == Mode::Passive {
            ensure!(
                !location.root.join("state.sqlite3").try_exists()?
                    && !location
                        .root
                        .join(zerus_desktop_migration::HANDOFF_FILE)
                        .try_exists()?,
                "Passive bootstrap state changed."
            );
        }
        fence.exclusion = Some(Exclusion::Handoff(lock));
        fence.check()?;
        if let Some(original) = &original {
            original.check()?;
        }
    }
    fence.check()?;
    crate::desktop::startup_diagnostic::reserve(location);
    fence.check()?;
    let id = if let Some(pending) = &resuming {
        pending.request_id.clone()
    } else {
        if let Some(choice) = choice { record.reserve_choice(choice, desired)?; }
        if let Some(first) = first {
            record.reserve_first(first)?;
            write_anchor(config, location, passive, &first.request, &mut || fence.check())?;
        }
        let id = match requested {
            Some(id) => record.begin_id(desired, observation, id)?,
            None => record.begin(desired, observation)?,
        }.request_id.clone();
        record.pending.as_mut().expect("original").consumer = consumer;
        save(location, passive, &record, &mut || fence.check())?;
        id
    };
    if desired == Mode::Passive && database {
        fence.phase(Budget::Rollback);
        let mut receipt = if record.pending.as_ref().expect("pending").phase == Phase::Prepared {
            record.advance(Phase::Prepared, Phase::RevokeOffered)?;
            save(location, passive, &record, &mut || fence.check())?;
            let value = revoke_original(location, &id, original.as_ref(), fence.until)?;
            if value["checkpointed"] == true {
                value
            } else {
                rollback_result(location, &id, original.as_ref())?
            }
        } else {
            rollback_result(location, &id, original.as_ref())?
        };
        loop {
            fence.check()?;
            if receipt["accepted"] == true
                && receipt["durable"] == true
                && receipt["revoked"] == true
                && receipt["checkpointed"] == true
            {
                break;
            }
            ensure!(receipt["durable"]==true,"Original rollback was not applied; inspect its result or cancel the exact saved UUID after a current Service grant is verified.");
            std::thread::sleep(Duration::from_millis(50));
            // This is a KNOWN committed, prepared authority reduction, with
            // the same UUID and original process. The maintained revoke API
            // reconciles that original drain; an absent/unknown reservation is
            // never resent and a new writer still fails its native epoch check.
            receipt = revoke_original(location, &id, original.as_ref(), fence.until)?;
            if receipt["checkpointed"] != true {
                receipt = rollback_result(location, &id, original.as_ref())?;
            }
        }
        fence.phase(Budget::Hold);
        fence.exclusion = Some(Exclusion::Rollback(Box::new(hold_child::Hold::start(
            Path::new(&new.argv[0]),
            &environment(config, &new.argv[0])?,
            &receipt,
            &fence.identity,
            admission.caller.source() != "installed_frontend",
            fence.until,
        )?)));
        if record.pending.as_ref().expect("pending").phase == Phase::RevokeOffered {
            record.advance(Phase::RevokeOffered, Phase::Held)?;
        }
    } else {
        // The handoff and aggregate drain checks already preceded begin().
        fence.check()?;
        if record.pending.as_ref().expect("pending").phase == Phase::Prepared {
            record.advance(Phase::Prepared, Phase::Held)?;
        }
    }
    save(location, passive, &record, &mut || fence.check())?;
    fence.phase(Budget::Stop);
    let timing = Timing::new();
    fence.stop_timing = Some(&timing);
    let stopped = (|| -> Result<()> {
        // Debug builds only: the hosted fixture's bounded hold of an exact first
        // choice at Held. It consumes the Stop deadline just started, so the
        // unchanged StopOffered save below refuses an expired wait at Held.
        #[cfg(debug_assertions)]
        if first.is_some() {
            held_pause::wait(config, &id);
        }
        if let Some(original) = &mut original {
            let until = fence.until;
            let shape = wanted(old)?;
            stop::offer(
                &mut record,
                original,
                &mut || fence.check(),
                |original, check| {
                    timing.measure(Step::NativeRechecks, || {
                        instances::check_original_selection(native, old, &shape, original, until, check)
                    })
                },
                |record, check| timing.measure(Step::MetadataSync, || save(location, passive, record, check)),
            )?;
            instances::stop_original(
                native,
                old,
                &wanted(old)?,
                original,
                fence.until,
                &mut || fence.check(),
                &timing,
            )?;
            timing.measure(Step::PhysicalExit, || -> Result<()> {
                while !instances::stopped(native, old, &wanted(old)?, original, fence.until)? {
                    fence.check()?;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(())
            })?;
            record.advance(Phase::StopOffered, Phase::Stopped)?;
        } else {
            record.advance(Phase::Held, Phase::Stopped)?;
        }
        timing.measure(Step::MetadataSync, || save(location, passive, &record, &mut || fence.check()))
    })();
    fence.stop_timing = None;
    stopped.map_err(|error| timing.failure(error))?;
    fence.phase(Budget::Update);
    record.advance(Phase::Stopped, Phase::UpdateOffered)?;
    save(location, passive, &record, &mut || fence.check())?;
    instances::replace(
        native,
        scheduler,
        location,
        (old, &wanted(old)?),
        (new, &wanted(new)?),
        fence.until,
        &mut || fence.check(),
    )?;
    record.advance(Phase::UpdateOffered, Phase::Updated)?;
    save(location, passive, &record, &mut || fence.check())?;
    fence.phase(Budget::Start);
    record.advance(Phase::Updated, Phase::StartOffered)?;
    save(location, passive, &record, &mut || fence.check())?;
    let mut started = instances::start(
        native,
        location,
        new,
        &wanted(new)?,
        fence.until,
        &mut || fence.check(),
    )?;
    record.pending.as_mut().expect("pending").started = Some(started.id().into());
    save(location, passive, &record, &mut || fence.check())?;
    fence.phase(Budget::Readiness);
    let ready = loop {
        fence.check()?;
        if let Some(ready) = started.poll(location, new, fence.until)? {
            break ready;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    ready.check()?;
    fence.check()?;
    record.advance(Phase::StartOffered, Phase::Ready)?;
    save(location, passive, &record, &mut || fence.check())?;
    fence.phase(Budget::Release);
    // Keep the ORIGINAL hold through actual passive SDK/process/IPC readiness.
    if let Some(Exclusion::Rollback(hold)) = &mut fence.exclusion {
        hold.finish(fence.until)?;
    } else {
        drop(fence.exclusion.take());
    }
    admission.check()?;
    ready.check()?;
    finish_choice(&mut record, &id, desired, &ready, &fence)?;
    save(location, passive, &record, &mut || {
        admission.check()?; ready.check()?;
        if let Some(grant) = &fence.grant { grant.check(location)?; }
        Ok(())
    })?;
    Ok(
        json!({"abi":MODE_ABI,"schema":1,"configuration":location.configuration,"task_name":passive.name,"selected_mode":desired.name(),"running_mode":desired.name(),"pending":null,"request_id":id,"changed":true,"instance":ready.observation()}),
    )
}

fn finish_choice(record: &mut Record, id: &str, desired: Mode, ready: &instances::Running, fence: &Fence<'_>) -> Result<()> {
    if desired == Mode::Background && (record.choices.iter().any(|r|r.request == id)
        || record.first_choices.iter().any(|r|r.request == id)) {
        let grant = fence.grant.as_ref().context("Original background Grant unavailable.")?;
        grant.check(fence.location)?;
        let (journal, owner_epoch) = grant.incarnation();
        let (service_epoch, generation) = ready.scope();
        record.complete_in_scope(id, policy::mode::review::ChoiceScope { journal, owner_epoch, service_epoch, generation })
    } else { record.complete(id) }
}

fn operation(args: &[String]) -> Result<Value> {
    // Includes frame/config/caller/lock/COM work before the refresh engine.
    // The parent's existing owned-child timeout remains the outer hard bound.
    let refresh_overall = Instant::now() + Budget::exchange_timeout();
    ensure!(args.len() == 1, "Invalid mode helper arguments.");
    let length: usize = args[0].parse()?;
    ensure!(length.to_string() == args[0], "Invalid mode helper length.");
    let request = policy::read_frame(&mut std::io::stdin().lock(), length)?;
    let op = request["operation"].as_str().unwrap_or("");
    ensure!(
        request["schema"] == 1 && matches!(op, "switch" | "cancel_rollback" | "review" | "refresh" | "refresh_info" | "refresh_cancel"),
        "Unsupported mode request."
    );
    let keys = request.as_object().context("Invalid mode request.")?;
    // The first-choice form: exactly a background switch with the caller's
    // NEW request UUID, and no reviewed linkage.
    let first_form = op == "switch" && keys.len() == 4 && request["mode"] == "background"
        && request["request_id"].is_string() && !keys.contains_key("after_review");
    ensure!(
        ((op == "refresh" && keys.len() == 4 && keys.contains_key("target") && keys.contains_key("request_id"))
            || (keys.len() == 3 && op != "refresh") || first_form || (op == "switch" && keys.len() == 5
            && request["after_review"].is_string() && request["request_id"].is_string()))
            && if op == "switch" {
                keys.contains_key("mode")
            } else if op == "refresh_info" {
                keys.contains_key("configuration")
            } else {
                keys.contains_key("request_id")
            },
        "Unexpected mode request fields."
    );
    let config = Config::load().map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let location = Location::new(&config)?;
    // Before admission, the lock, any anchor or record reservation and any
    // manager effect: an overlapping layout never takes a first choice.
    ensure!(!first_form || anchor_layout_separate(&config, &location), ANCHOR_LAYOUT);
    let scope = ui_authority::Scope::new(&config, &location);
    let caller = ui_authority::registration::Caller::capture(&scope)?;
    let admission = Admission { scope, caller };
    admission.check()?;
    crate::host::token::private_creation::prepare()
        .context("select exact-user mode-worker creation owner")?;
    admission.check()?;
    let sid = crate::host::token::user_sid()?;
    let (passive, background) = plans(&config, &location, &sid)?;
    let _lock = Lock::acquire(&sid)?;
    // No mode operation runs while the journal and its independent
    // first-choice anchor disagree: lost is never "never written".
    let checked_record = load(&location, &passive)?;
    if let Some(lost) = checked_record.first_lost(read_anchor(&config, &location, &passive)?.as_deref())
        .or(refresh::lost(&config, &location, &passive, &checked_record)?)
    {
        anyhow::bail!("The desktop mode journal no longer shows first choice {lost}; keep both records and inspect status. No mode operation is admitted.");
    }
    if op == "refresh_cancel" {
        let id = request["request_id"].as_str().context("Original refresh UUID required.")?;
        let mut record = load(&location, &passive)?;
        ensure!(record.pending.as_ref().is_some_and(|p|p.request_id == id && p.refresh.is_some()),
            "Original pre-manager refresh is absent; lookup never adopts another operation.");
        let needs_grant = record.cancellation_requires_grant(id)?;
        let mut check = || {
            admission.check()?;
            if needs_grant { ensure!(ownership::registration_granted(&location)?,
                "Current Service Grant required for this passive metadata cancellation."); }
            Ok(())
        };
        check()?;
        record.cancel(id)?;
        save(&location,&passive,&record,&mut check)?;
        return Ok(json!({"abi":MODE_ABI,"refresh_abi":policy::mode::refresh::ABI,"schema":1,
            "configuration":location.configuration,"request_id":id,"pending":null,
            "receipt":record.refreshes.iter().find(|r|r.request==id),
            "changed":true,"manager_changed":false,"authority_changed":false}));
    }
    // A repeated refresh is ONLY its original result, before any COM/current
    // instance acquisition. Even Prepared has no continuation on this frame.
    if op == "refresh" {
        let id = request["request_id"].as_str().context("Original refresh UUID required.")?;
        let target = policy::mode::refresh::Target::decode(&serde_json::to_vec(&request["target"])?)?;
        ensure!(target.configuration == location.configuration, "Refresh configuration changed.");
        if checked_record.refreshes.iter().any(|r|r.request == id) {
            return Ok(refresh::view(&location,&checked_record,id));
        }
    }
    let mut offered_cancel = None;
    if op == "cancel_rollback" {
        admission.check()?;
        let mut record = load(&location, &passive)?;
        let id = request["request_id"]
            .as_str()
            .context("Original transition UUID required.")?;
        if !record
            .pending
            .as_ref()
            .is_some_and(|p| p.phase.pre_manager())
        {
            record.ready_cancellation(id)?;
            offered_cancel = Some((record, id));
        } else {
            let needs_grant = record.cancellation_requires_grant(id)?;
            let mut check = || {
                admission.check()?;
                if needs_grant {
                    ensure!(ownership::registration_granted(&location)?,"Passive rollback cancellation requires a fresh Service Grant/Rearm; it changes metadata only.");
                }
                Ok(())
            };
            check()?;
            record.cancel(id)?;
            save(&location, &passive, &record, &mut check)?;
            return Ok(
                json!({"abi":MODE_ABI,"schema":1,"configuration":location.configuration,"task_name":passive.name,
            "selected_mode":null,"pending":null,"cancelled_request_id":id,"changed":true,"manager_changed":false}),
            );
        }
    }
    let _apartment = Apartment::new()?;
    let scheduler: ITaskService =
        unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }?;
    unsafe {
        scheduler.Connect(
            &VARIANT::default(),
            &VARIANT::default(),
            &VARIANT::default(),
            &VARIANT::default(),
        )
    }?;
    let mut native = Native {
        folder: unsafe { scheduler.GetFolder(&BSTR::from("\\")) }?,
        scheduler: scheduler.clone(),
    };
    if op == "refresh_info" {
        ensure!(request["configuration"] == location.configuration, "Refresh configuration changed.");
        return refresh::info(Runtime { config:&config,location:&location,native:&mut native,
            scheduler:&scheduler,admission:&admission,passive:&passive,background:&background });
    }
    if op == "refresh" {
        let id = request["request_id"].as_str().context("Original refresh UUID required.")?;
        ensure!(uuid::Uuid::parse_str(id).is_ok_and(|u| !u.is_nil() && u.to_string() == id),
            "Invalid original refresh UUID.");
        let target = policy::mode::refresh::Target::decode(&serde_json::to_vec(&request["target"])?)?;
        return refresh::effect(Runtime { config:&config,location:&location,native:&mut native,
            scheduler:&scheduler,admission:&admission,passive:&passive,background:&background },id,target,refresh_overall);
    }
    if op == "review" {
        let id = request["request_id"].as_str().context("Original transition UUID required.")?;
        return review::command(recovery::Inputs { config: &config, location: &location,
            native: &mut native, admission: &admission, passive: &passive, background: &background }, id);
    }
    if let Some((mut record, id)) = offered_cancel {
        // This read-only lane returns before the effect path. It cannot stop,
        // update, start, revoke or acquire a later runtime's rollback hold.
        return recovery::cancel_ready(
            recovery::Inputs {
                config: &config,
                location: &location,
                native: &mut native,
                admission: &admission,
                passive: &passive,
                background: &background,
            },
            &mut record,
            id,
        );
    }
    let desired = Mode::parse(request["mode"].as_str().unwrap_or(""))?;
    let choice = request["after_review"].as_str().map(|reviewed| policy::mode::review::Choice {
        reviewed: reviewed.into(), request: request["request_id"].as_str().unwrap_or("").into(),
    });
    let first = first_form.then(|| policy::mode::first::First {
        request: request["request_id"].as_str().unwrap_or("").into(),
    });
    effect(
        Runtime {
            config: &config,
            location: &location,
            native: &mut native,
            scheduler: &scheduler,
            admission: &admission,
            passive: &passive,
            background: &background,
        },
        desired,
        choice.as_ref(),
        first.as_ref(),
    )
}

pub(in crate::desktop::windows_registration) fn serve(args: &[String]) -> i32 {
    let value = match operation(args) {
        Ok(value) => value,
        Err(error) => {
            let mut value = timing::error_detail(&error);
            value["abi"] = MODE_ABI.into();
            value["schema"] = 1.into();
            value["code"] = "mode_transition_unconfirmed".into();
            value
        }
    };
    // An error receipt is data; callers inspect the retained transition. A
    // successful transport never turns an unknown manager result into success.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let bytes=match serde_json::to_vec(&value) {
        Ok(bytes) if bytes.len()<=CAP=>bytes,
        _=>serde_json::to_vec(&json!({"abi":MODE_ABI,"schema":1,"code":"mode_response_unavailable","error":"Inspect the retained original transition."})).expect("fixed response"),
    };
    if out.write_all(&bytes).is_err() || out.write_all(b"\n").is_err() || out.flush().is_err() {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_image_refuses_the_mode_helper_before_caller_capture() {
        let root = tempfile::tempdir().unwrap();
        let config = Config {
            home: root.path().to_str().unwrap().into(),
            dir: root.path().join("config"),
            state: root.path().join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        let location = Location {
            root: root.path().join("state").join("desktop"),
            configuration: "a".repeat(64),
        };
        for request in [
            json!({"schema":1,"operation":"switch","mode":"passive"}),
            json!({"schema":1,"operation":"cancel_rollback","request_id":"11111111-1111-4111-8111-111111111111"}),
            json!({"schema":1,"operation":"review","request_id":"11111111-1111-4111-8111-111111111111"}),
            json!({"schema":1,"operation":"switch","mode":"background","after_review":"11111111-1111-4111-8111-111111111111","request_id":"22222222-2222-4222-8222-222222222222"}),
            json!({"schema":1,"operation":"switch","mode":"background","request_id":"33333333-3333-4333-8333-333333333333"}),
        ] {
            let error = command(&config, &location, request).unwrap_err();
            assert_eq!(error.to_string(), crate::self_exec::TEST_IMAGE);
        }
        // Refused before any record, plan or helper environment was touched.
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    fn layout(root: &std::path::Path, dir: std::path::PathBuf) -> (Config, Location) {
        let state = root.join("state");
        std::fs::create_dir_all(state.join("desktop")).unwrap();
        let config = Config {
            home: root.to_str().unwrap().into(),
            dir,
            state: state.clone(),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        let location = Location::new(&config).unwrap();
        (config, location)
    }
    fn plan() -> Plan {
        Plan::parse(&json!({"schema":1,"operation":"status","plan":{"schema":1,"operation":"preview","label":"hgs-desktop","configuration":"a".repeat(64),"user_sid":"S-1-5-21-1001","task_name":"hgs-desktop-S-1-5-21-1001","definition":"synthetic definition","argv":["C:/Example/hgs.exe","desktop","serve","--config-dir","C:/Example/config","--state-dir","C:/Example/state"],"session":{"kind":"interactive_user","allow_session_zero":false}}}), "S-1-5-21-1001").unwrap()
    }

    #[test]
    fn only_a_config_folder_outside_the_state_tree_takes_a_first_choice_anchor() {
        let root = tempfile::tempdir().unwrap();
        let root = zerus_desktop_migration::canonical(root.path()).unwrap();
        let (disjoint, location) = layout(&root, root.join("config"));
        std::fs::create_dir_all(&disjoint.dir).unwrap();
        assert!(anchor_layout_separate(&disjoint, &location));
        // A junction outside the state tree that resolves into it.
        let junction = root.join("alias");
        let made = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(root.join("state"))
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let upper = root.join("state").to_str().unwrap().to_uppercase();
        for (case, dir) in [
            ("inside the mode folder", root.join(r"state\desktop\config")),
            ("the state tree itself", root.join("state")),
            ("other case", std::path::PathBuf::from(format!(r"{upper}\cfg"))),
            ("through a junction", junction.join("cfg")),
        ] {
            let (config, location) = layout(&root, dir);
            assert!(!anchor_layout_separate(&config, &location), "{case}");
            std::fs::create_dir_all(&config.dir).unwrap();
            let request = uuid::Uuid::new_v4().to_string();
            let error = write_anchor(&config, &location, &plan(), &request, &mut || Ok(()))
                .unwrap_err();
            assert_eq!(error.to_string(), ANCHOR_LAYOUT, "{case}");
            assert!(!anchor_path(&config, &location).exists(), "{case}");
        }
    }

    #[test]
    fn a_journal_read_waits_out_a_writer_and_never_pairs_a_new_anchor_with_an_old_record() {
        let root = tempfile::tempdir().unwrap();
        let root = zerus_desktop_migration::canonical(root.path()).unwrap();
        let (config, location) = layout(&root, root.join("config"));
        std::fs::create_dir_all(&config.dir).unwrap();
        let plan = plan();
        // A writer's commit window: its new anchor is published under the
        // lock and its record save has not happened yet.
        let request = uuid::Uuid::new_v4().to_string();
        write_anchor(&config, &location, &plan, &request, &mut || Ok(())).unwrap();
        let lock = format!("test-{}", uuid::Uuid::new_v4());
        let (held, wait) = std::sync::mpsc::channel();
        let (release, done) = std::sync::mpsc::channel::<()>();
        let writer = {
            let lock = lock.clone();
            std::thread::spawn(move || {
                let _held = Lock::acquire(&lock).unwrap();
                held.send(()).unwrap();
                done.recv().unwrap();
            })
        };
        wait.recv().unwrap();
        let started = Instant::now();
        let error = journal(&lock, &config, &location, &plan).unwrap_err();
        assert!(
            error.to_string().contains("Registration lock not confirmed"),
            "{error:#}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        release.send(()).unwrap();
        writer.join().unwrap();
        // Outside any writer the same pair is a durable loss.
        let (_, lost) = journal(&lock, &config, &location, &plan).unwrap();
        assert_eq!(lost, Some(request));
    }
}
