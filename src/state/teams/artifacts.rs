//! Explicit artifact executors use local profiles, never source runners.
use super::templates::{Outcome, Plan};
use anyhow::{ensure, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
const MAX_PROFILE: u64 = 65536;
const PREFIX: &str = "zerus-artifact:";
fn home() -> PathBuf {
    super::super::state_root().join("artifact-executors")
}
pub(crate) fn profile_of(command: &str) -> Option<&str> {
    command.strip_prefix(PREFIX)
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
fn job_id(job: &str) -> anyhow::Result<String> {
    ensure!(
        !job.is_empty() && job.len() <= 200 && !job.chars().any(char::is_control),
        "A stable artifact job ID is required"
    );
    Ok(format!("{:x}", Sha256::digest(job.as_bytes()))[..32].to_owned())
}
fn read_json(path: &Path) -> anyhow::Result<Value> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_PROFILE + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_PROFILE as usize,
        "Artifact metadata exceeds its bound"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
/// `hgs-executor.exe` installed beside this `hgs` (the fork's Windows zip).
fn bundled_helper() -> Option<PathBuf> {
    bundled_helper_in(std::env::current_exe().ok()?.parent()?)
}
fn bundled_helper_in(folder: &Path) -> Option<PathBuf> {
    let path = folder.join("hgs-executor.exe");
    path.is_file().then_some(path)
}
fn profile_at(base: &Path, profile: &str) -> anyhow::Result<Value> {
    ensure!(name(profile), "Invalid executor profile name");
    let mut value: Value = read_json(&base.join("profiles").join(format!("{profile}.json")))
        .context("Configure the local artifact executor profile first")?;
    // A fork release ships the helper beside hgs.exe; a profile may still name
    // another installed copy explicitly.
    if value["helper"].is_null() {
        if let Some(bundled) = bundled_helper() {
            value["helper"] = bundled.display().to_string().into();
        }
    }
    let helper = value["helper"]
        .as_str()
        .context("Executor profile needs its packaged helper")?;
    ensure!(
        Path::new(helper).is_absolute() && Path::new(helper).is_file(),
        "Executor helper must be an installed absolute path"
    );
    ensure!(
        matches!(value["source"].as_str(), Some("snapshot" | "shared")),
        "Executor source is snapshot or shared"
    );
    let relative = value["artifact_relative"]
        .as_str()
        .context("Name the output relative to its build folder")?;
    ensure!(
        !relative.is_empty()
            && !Path::new(relative).is_absolute()
            && Path::new(relative)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && !relative.contains(':'),
        "Artifact output must stay below its build folder"
    );
    ensure!(
        value["controller"].is_object(),
        "Executor profile needs controller configuration"
    );
    ensure!(
        value["controller"]["job_id"].is_null()
            && value["controller"]["artifact_root"].is_null()
            && value["controller"]["observation"].is_null(),
        "Job identity and source are supplied by Zerus"
    );
    Ok(value)
}
fn plan_at(base: &Path, profile: &str, job: &str) -> anyhow::Result<Plan> {
    let value = profile_at(base, profile)?;
    job_id(job)?;
    Ok(Plan {
        template: "artifact-test".into(),
        program: value["helper"].as_str().unwrap().into(),
        argv: vec!["zerus-profile".into(), profile.into(), job.into()],
        commit: None,
        more: Vec::new(),
        cwd: None,
    })
}
pub(crate) fn plan(profile: &str, job: &str) -> anyhow::Result<Plan> {
    plan_at(&home(), profile, job)
}
fn prepare_at(
    base: &Path,
    profile: &str,
    job: &str,
    source: &Path,
    team: &str,
    performance: Option<&Value>,
) -> anyhow::Result<Plan> {
    let value = profile_at(base, profile)?;
    let mut plan = plan_at(base, profile, job)?;
    let config_path = base.join("jobs").join(job_id(job)?).join("controller.json");
    plan.argv = vec!["controller-run".into(), config_path.display().to_string()];
    let mut controller = value["controller"].clone();
    if let Some(performance) = performance {
        controller["performance"] = performance.clone();
    }
    controller["job_id"] = job_id(job)?.into();
    let source = source.join(value["artifact_relative"].as_str().unwrap());
    ensure!(
        source.is_dir(),
        "The selected output does not exist; build it first"
    );
    controller["artifact_root"] = source.display().to_string().into();
    let path = Path::new(&plan.argv[1]);
    fs::create_dir_all(path.parent().unwrap())?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(&serde_json::to_vec(&controller)?)?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
            read_json(path)? == controller,
            "Artifact job configuration changed; use a new job ID"
        ),
        Err(error) => return Err(error.into()),
    }
    let index_dir = base.join("teams");
    fs::create_dir_all(&index_dir)?;
    let index_path = index_dir.join(format!("{:x}.json", Sha256::digest(team.as_bytes())));
    let _lock = super::broker::queue(&index_dir, Duration::from_secs(5))?;
    let mut jobs = if index_path.exists() {
        read_json(&index_path)?
            .as_array()
            .cloned()
            .context("Invalid artifact index")?
    } else {
        Vec::new()
    };
    ensure!(jobs.len() <= 20, "Artifact index exceeds its bound");
    let id = job_id(job)?;
    jobs.retain(|row| row["job"] != id);
    jobs.insert(0, json!({"job":id,"profile":profile}));
    jobs.truncate(20);
    super::super::storage::atomic_private(&index_path, &serde_json::to_string(&jobs)?)
        .map_err(anyhow::Error::msg)?;
    Ok(plan)
}
pub(crate) fn run(
    team: &super::model::Team,
    member: &super::model::Member,
    folder: &Path,
    args: &Value,
    timeout: Duration,
) -> anyhow::Result<Outcome> {
    super::templates::plan("artifact-test", args, folder)?;
    let profile = args["profile"]
        .as_str()
        .context("Name an executor profile")?;
    let job = args["job"].as_str().context("Name an immutable job")?;
    let shared = PathBuf::from(super::dispatcher::member_folder_path(team, member)?);
    run_in(
        &home(),
        profile,
        job,
        &shared,
        Some((team, member)),
        &team.id,
        folder,
        timeout,
        None,
    )
}
pub(crate) fn standalone(
    profile: &str,
    job: &str,
    folder: &Path,
    timeout: Duration,
) -> anyhow::Result<Outcome> {
    run_in(
        &home(),
        profile,
        job,
        folder,
        None,
        "",
        folder,
        timeout,
        None,
    )
}
fn run_in(
    base: &Path,
    profile: &str,
    job: &str,
    shared: &Path,
    owner: Option<(&super::model::Team, &super::model::Member)>,
    index_team: &str,
    folder: &Path,
    timeout: Duration,
    performance: Option<&Value>,
) -> anyhow::Result<Outcome> {
    ensure!(
        cfg!(windows),
        "This executor package currently supports Windows builders"
    );
    let began = Instant::now();
    let value = profile_at(base, profile)?;
    let source;
    let mut locks = Vec::new();
    locks.push(super::broker::queue(
        &shared.join(".zerus-team/runs/broker-shared"),
        timeout,
    )?);
    let team_id = if let Some((team, member)) = owner {
        let dir = super::broker::member_dir(shared, &member.id);
        locks.push(super::broker::queue(
            &dir,
            timeout.saturating_sub(began.elapsed()),
        )?);
        if value["source"] == "snapshot" {
            let config = super::broker::config(shared);
            ensure!(
                team.settings.build_broker && config.enabled && !config.shared,
                "The requested snapshot requires the enabled snapshot build broker"
            );
            source = dir.join("tree");
        } else {
            source = shared.to_path_buf();
        }
        team.id.as_str()
    } else {
        ensure!(
            value["source"] == "shared",
            "Queued providers need shared outputs; member snapshots use artifact-test"
        );
        source = shared.to_path_buf();
        index_team
    };
    let mut plan = prepare_at(base, profile, job, &source, team_id, performance)?;
    plan.argv[0] = "controller-deploy".into();
    let outcome =
        super::templates::execute(&plan, folder, timeout.saturating_sub(began.elapsed()), &[]);
    drop(locks); // Both input trees have been verified before deploy returns.
    if outcome.exit != Some(0) || outcome.timed_out {
        return Ok(outcome);
    }
    plan.argv[0] = "controller-run".into();
    Ok(super::templates::execute(
        &plan,
        folder,
        timeout.saturating_sub(began.elapsed()),
        &[],
    ))
}
pub(crate) fn queued(
    project: &Path,
    profile: &str,
    request: &super::test_queue::Request,
    limit: Duration,
) -> anyhow::Result<Outcome> {
    let performance = if request.performance {
        let settings = crate::test_discipline::load(project, None)?
            .config
            .performance;
        let declared = profile_at(&home(), profile)?["controller"]["performance"].clone();
        Some(if declared.is_object() {
            declared
        } else {
            json!({"cpu_percent":settings.cpu_percent,"disk_percent":settings.disk_percent,"disk_busy_percent":20,"wait_seconds":settings.wait_seconds.clamp(0.0,3600.0) as u64,"on_busy":"run_loaded"})
        })
    } else {
        None
    };
    run_in(
        &home(),
        profile,
        &format!("{}:{}", request.team, request.id),
        project,
        None,
        &request.team,
        project,
        limit,
        performance.as_ref(),
    )
}
pub(crate) fn result_status(job: &str) -> Option<String> {
    let value = read_json(
        &home()
            .join("jobs")
            .join(job_id(job).ok()?)
            .join("controller-state.json"),
    )
    .ok()?;
    let phase = value["phase"].as_str()?;
    Some(
        if matches!(
            phase,
            "passed" | "failed" | "timeout" | "infrastructure_error" | "not_run"
        ) && value["collected"] != true
        {
            "unknown".into()
        } else {
            phase.into()
        },
    )
}
pub(crate) fn result_load(job: &str) -> Option<Value> {
    let state = read_json(
        &home()
            .join("jobs")
            .join(job_id(job).ok()?)
            .join("controller-state.json"),
    )
    .ok()?;
    let load = state.get("performance")?;
    (state["collected"] == true && load["origin"] == "executor-host" && load.is_object())
        .then(|| load.clone())
}
pub(crate) fn view(team: &str) -> Value {
    let base = home();
    let index = base
        .join("teams")
        .join(format!("{:x}.json", Sha256::digest(team.as_bytes())));
    let rows = read_json(&index)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let rows:Vec<Value>=rows.into_iter().take(20).filter_map(|row|{
        let job=row["job"].as_str()?;if job.len()!=32||!job.bytes().all(|c|c.is_ascii_hexdigit()){return None}
        let state=read_json(&base.join("jobs").join(job).join("controller-state.json")).ok()?;
        Some(json!({"job":job,"profile":row["profile"],"executor":state["executor"],"device":state["device_id"],"phase":state["phase"],"instructions":state["instructions"],"updated":state["updated"]}))
    }).collect();
    json!(rows)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the isolated synthetic paired-executor fixture"]
    fn artifact_native_broker_pipeline() {
        let fixture = PathBuf::from(
            std::env::var_os("ZERUS_ARTIFACT_TEAM_FIXTURE").expect("synthetic fixture"),
        );
        let input = read_json(&fixture).unwrap();
        let base = PathBuf::from(input["base"].as_str().unwrap());
        let project = PathBuf::from(input["project"].as_str().unwrap());
        super::super::super::with_test_state_root(base.parent().unwrap(), || {
            if input["performance"] == true {
                let request = super::super::test_queue::Request {
                    id: "native-integration".into(),
                    team: "synthetic-team".into(),
                    suite: "synthetic".into(),
                    task: None,
                    targets: Vec::new(),
                    commit: "synthetic-commit".into(),
                    input_hash: String::new(),
                    asked_by: "synthetic".into(),
                    forced: true,
                    at: 100.0,
                    seconds: 0.0,
                    budget_minutes: 1.0,
                    resources: "small".into(),
                    command: "zerus-artifact:synthetic".into(),
                    platforms: Vec::new(),
                    performance: true,
                    compare: String::new(),
                };
                let database = super::super::test_queue::TestStore::open_at(
                    &base.join("synthetic-results.sqlite3"),
                    false,
                )
                .unwrap();
                let records = super::super::test_queue::run_due(
                    &database,
                    &project,
                    &[request],
                    100.0,
                    &Default::default(),
                    None,
                )
                .unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].status, "passed");
                assert!(
                    records[0].duration_ms > 0
                        && records[0].load.as_ref().is_some_and(|load| load["origin"]
                            == "executor-host"
                            && load["valid"] == true)
                );
                assert!(records[0].flags.iter().all(|flag| flag != "retried"));
                return;
            }
            let outcome = run_in(
                &base,
                "synthetic",
                "native-integration",
                &project,
                None,
                "synthetic-team",
                &project,
                Duration::from_secs(35),
                None,
            )
            .unwrap();
            assert_eq!(outcome.exit, Some(0), "{} {}", outcome.error, outcome.tail);
            let state = read_json(
                &base
                    .join("jobs")
                    .join(job_id("native-integration").unwrap())
                    .join("controller-state.json"),
            )
            .unwrap();
            assert_eq!(state["phase"], "passed");
            assert_eq!(state["collected"], true);
            let rows = view("synthetic-team");
            assert_eq!(rows.as_array().unwrap().len(), 1);
            assert_eq!(rows[0]["device"], "host");
        });
    }
    #[test]
    fn explicit_route_refuses_shell_syntax() {
        assert_eq!(profile_of("zerus-artifact:stand"), Some("stand"));
        assert!(!name("stand; echo secret"));
        assert!(!name("../stand"));
        assert_ne!(job_id("one").unwrap(), job_id("two").unwrap());
    }
    #[test]
    fn a_profile_without_helper_uses_only_the_executor_beside_hgs() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(bundled_helper_in(t.path()), None);
        fs::create_dir(t.path().join("hgs-executor.exe")).unwrap();
        assert_eq!(
            bundled_helper_in(t.path()),
            None,
            "a folder is not the executor"
        );
        let u = tempfile::tempdir().unwrap();
        fs::write(u.path().join("hgs-executor.exe"), b"synthetic").unwrap();
        assert_eq!(
            bundled_helper_in(u.path()),
            Some(u.path().join("hgs-executor.exe"))
        );
        // A test binary has no executor beside it: such a profile still asks for one.
        if bundled_helper().is_none() {
            fs::create_dir(t.path().join("profiles")).unwrap();
            let p = json!({"source":"shared","artifact_relative":"Build/Content","controller":{}});
            fs::write(
                t.path().join("profiles/bare.json"),
                serde_json::to_vec(&p).unwrap(),
            )
            .unwrap();
            let error = profile_at(t.path(), "bare").unwrap_err().to_string();
            assert!(error.contains("packaged helper"), "{error}");
        }
    }
    #[test]
    fn validation_creates_no_job() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("profiles")).unwrap();
        let p = json!({"helper":std::env::current_exe().unwrap(),"source":"shared","artifact_relative":"Build/Content","controller":{}});
        fs::write(
            t.path().join("profiles/synthetic.json"),
            serde_json::to_vec(&p).unwrap(),
        )
        .unwrap();
        let p = plan_at(t.path(), "synthetic", "request").unwrap();
        assert_eq!(p.argv[0], "zerus-profile");
        assert!(!t.path().join("jobs").exists());
    }
}
