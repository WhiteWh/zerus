//! Observable Task Scheduler ownership rules, shared by native calls and tests.
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};

pub(super) const CAP: usize = 64 * 1024;

/// Read only the original length-delimited request; never wait for pipe EOF.
/// The caller's original process runner owns the deadline and physical wait.
pub(super) fn read_frame(reader: &mut impl std::io::Read, length: usize) -> Result<Value> {
    ensure!(
        length > 0 && length <= CAP,
        "Invalid native registration length."
    );
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .context("Truncated native registration frame.")?;
    let value: Value =
        serde_json::from_slice(&bytes).context("Invalid native registration JSON.")?;
    ensure!(
        value.is_object(),
        "Registration input must be a JSON object."
    );
    Ok(value)
}

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub operation: String,
    pub configuration: String,
    pub sid: String,
    pub name: String,
    pub argv: Vec<String>,
    pub definition: String,
}
impl Plan {
    pub fn parse(request: &Value, actual_sid: &str) -> Result<Self> {
        ensure!(
            actual_sid.starts_with("S-1-")
                && actual_sid.len() <= 184
                && actual_sid
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'S' | b'-'))
                && !matches!(actual_sid, "S-1-5-18" | "S-1-5-19" | "S-1-5-20")
                && !actual_sid.starts_with("S-1-5-80-"),
            "A system/service account cannot own a desktop logon registration."
        );
        let plan = &request["plan"];
        let operation = request["operation"].as_str().unwrap_or("");
        ensure!(
            request["schema"] == 1
                && matches!(operation, "check" | "status" | "install" | "remove"),
            "Unsupported registration request."
        );
        ensure!(
            plan["schema"] == 1 && plan["operation"] == "preview" && plan["label"] == "hgs-desktop",
            "Unsupported registration plan."
        );
        let name = format!("hgs-desktop-{actual_sid}");
        ensure!(
            plan["user_sid"] == actual_sid && plan["task_name"] == name,
            "Registration does not select the actual current user."
        );
        let configuration = plan["configuration"].as_str().unwrap_or("");
        ensure!(
            configuration.len() == 64
                && configuration
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Invalid registration configuration."
        );
        let definition = plan["definition"]
            .as_str()
            .context("Missing registration definition.")?;
        ensure!(
            !definition.is_empty() && definition.len() <= CAP,
            "Invalid registration definition."
        );
        let argv = plan["argv"]
            .as_array()
            .context("Missing registration command.")?;
        ensure!(
            argv.len() == 7,
            "Only the passive desktop command may be registered."
        );
        let argv: Vec<String> = argv
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .context("Invalid registration argument.")
            })
            .collect::<Result<_>>()?;
        ensure!(
            argv[1] == "desktop"
                && argv[2] == "serve"
                && argv[3] == "--config-dir"
                && argv[5] == "--state-dir",
            "Only the passive desktop command may be registered."
        );
        for path in [&argv[0], &argv[4], &argv[6]] {
            let bytes = path.as_bytes();
            let absolute = (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\'))
                || path.strip_prefix(r"\\").is_some_and(|p| {
                    let mut parts = p.split('\\');
                    parts.next().is_some_and(|p| !p.is_empty())
                        && parts.next().is_some_and(|p| !p.is_empty())
                });
            ensure!(
                absolute
                    && path.encode_utf16().count() < 32767
                    && !path.chars().any(char::is_control)
                    && !path.contains('%'),
                "Invalid registration path."
            );
        }
        ensure!(
            plan["session"]["kind"] == "interactive_user"
                && plan["session"]["allow_session_zero"] == false,
            "Registration requires an interactive user session."
        );
        Ok(Self {
            operation: operation.into(),
            configuration: configuration.into(),
            sid: actual_sid.into(),
            name,
            argv,
            definition: definition.into(),
        })
    }
    pub fn shape(&self, arguments: String) -> Shape {
        Shape {
            description: format!(
                "Zerus desktop registration v1; configuration={}",
                self.configuration
            ),
            uri: format!(r"\{}", self.name),
            user: self.sid.clone(),
            group: String::new(),
            principal: "DesktopUser".into(),
            context: "DesktopUser".into(),
            logon: 3,
            run_level: 0,
            action: Action {
                kind: 0,
                path: self.argv[0].clone(),
                arguments,
                directory: String::new(),
            },
            trigger: Trigger {
                kind: 9,
                user: self.sid.clone(),
                enabled: true,
                start: String::new(),
                end: String::new(),
                delay: String::new(),
                interval: String::new(),
                duration: String::new(),
            },
            settings: Settings {
                enabled: true,
                hidden: true,
                instances: 2,
                execution: "PT0S".into(),
                restart: "PT1M".into(),
                retries: 3,
                disallow_battery: false,
                stop_battery: false,
                available: true,
                idle: false,
                network: false,
                wake: false,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Action {
    pub kind: i32,
    pub path: String,
    pub arguments: String,
    pub directory: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Trigger {
    pub kind: i32,
    pub user: String,
    pub enabled: bool,
    pub start: String,
    pub end: String,
    pub delay: String,
    pub interval: String,
    pub duration: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Settings {
    pub enabled: bool,
    pub hidden: bool,
    pub instances: i32,
    pub execution: String,
    pub restart: String,
    pub retries: i32,
    pub disallow_battery: bool,
    pub stop_battery: bool,
    pub available: bool,
    pub idle: bool,
    pub network: bool,
    pub wake: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shape {
    pub description: String,
    pub uri: String,
    pub user: String,
    pub group: String,
    pub principal: String,
    pub context: String,
    pub logon: i32,
    pub run_level: i32,
    pub action: Action,
    pub trigger: Trigger,
    pub settings: Settings,
}
#[derive(Clone, Debug)]
pub(super) struct Ace {
    pub allowed: bool,
    pub sid: String,
}
#[derive(Clone, Debug)]
pub(super) struct Task {
    pub shape: Shape,
    pub owner: String,
    pub acl: Option<Vec<Ace>>,
    pub enabled: bool,
    pub state: i32,
}
impl Task {
    fn validate(&self, wanted: &Shape, sid: &str) -> Result<()> {
        ensure!(
            self.shape == *wanted && self.enabled && (0..=4).contains(&self.state),
            "Existing task differs from this managed configuration; it was kept."
        );
        ensure!(
            self.owner == sid,
            "Existing task ownership is unknown; it was kept."
        );
        let acl = self
            .acl
            .as_ref()
            .context("Existing task DACL is unknown; it was kept.")?;
        ensure!(
            acl.len() <= 16
                && acl
                    .iter()
                    .all(|a| a.allowed
                        && [sid, "S-1-5-18", "S-1-5-32-544"].contains(&a.sid.as_str())),
            "Existing task grants access to another principal; it was kept."
        );
        Ok(())
    }
}
pub(super) trait Scheduler {
    fn read(&mut self, name: &str) -> Result<Option<Task>>;
    /// TASK_CREATE | TASK_IGNORE_REGISTRATION_TRIGGERS: no replacement or Run.
    fn create_only(&mut self, plan: &Plan) -> Result<()>;
    /// Called once only after current ownership validation; never Run/Stop/End.
    fn delete(&mut self, name: &str) -> Result<()>;
}
pub(super) fn perform(
    plan: &Plan,
    wanted: &Shape,
    scheduler: &mut impl Scheduler,
) -> Result<Value> {
    let mut task = scheduler.read(&plan.name)?;
    if let Some(task) = &task {
        task.validate(wanted, &plan.sid)?;
    }
    if task.is_none() && matches!(plan.operation.as_str(), "check" | "install") {
        ensure!(
            scheduler.read("hgs-desktop")?.is_none(),
            "A legacy global desktop task exists; inspect it before registering a per-user task."
        );
    }
    let mut changed = false;
    match plan.operation.as_str() {
        "install" if task.is_none() => {
            scheduler
                .create_only(plan)
                .context("Task creation was not confirmed; inspect status before retrying.")?;
            task = scheduler.read(&plan.name)?;
            task.as_ref()
                .context("Created task could not be read back; inspect status before retrying.")?
                .validate(wanted, &plan.sid)?;
            changed = true;
        }
        "remove" if task.is_some() => {
            scheduler
                .delete(&plan.name)
                .context("Task removal was not confirmed; inspect status before retrying.")?;
            ensure!(
                scheduler.read(&plan.name)?.is_none(),
                "Removed task is still present; inspect status before retrying."
            );
            task = None;
            changed = true;
        }
        "check" | "status" | "install" | "remove" => {}
        _ => bail!("Unsupported registration operation."),
    }
    Ok(
        json!({"schema":1,"operation":plan.operation,"task_name":plan.name,"configuration":plan.configuration,"registered":task.is_some(),"task_state":task.map(|t| t.state),"changed":changed}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan(op: &str) -> Plan {
        Plan::parse(&json!({"schema":1,"operation":op,"plan":{"schema":1,"operation":"preview","label":"hgs-desktop","configuration":"a".repeat(64),"user_sid":"S-1-5-21-1001","task_name":"hgs-desktop-S-1-5-21-1001","definition":"synthetic definition","argv":["C:/Example/hgs.exe","desktop","serve","--config-dir","C:/Example/config","--state-dir","C:/Example/state"],"session":{"kind":"interactive_user","allow_session_zero":false}}}), "S-1-5-21-1001").unwrap()
    }
    fn task(shape: Shape) -> Task {
        Task {
            owner: shape.user.clone(),
            acl: Some(vec![Ace {
                allowed: true,
                sid: shape.user.clone(),
            }]),
            shape,
            enabled: true,
            state: 3,
        }
    }
    struct Fake {
        task: Option<Task>,
        wanted: Shape,
        writes: Vec<&'static str>,
        fail_after_create: bool,
    }
    impl Scheduler for Fake {
        fn read(&mut self, name: &str) -> Result<Option<Task>> {
            Ok((name != "hgs-desktop").then(|| self.task.clone()).flatten())
        }
        fn create_only(&mut self, _: &Plan) -> Result<()> {
            self.writes.push("create");
            ensure!(self.task.is_none(), "task appeared");
            self.task = Some(task(self.wanted.clone()));
            ensure!(!self.fail_after_create, "unknown delivery");
            Ok(())
        }
        fn delete(&mut self, _: &str) -> Result<()> {
            self.writes.push("delete");
            self.task = None;
            Ok(())
        }
    }
    #[test]
    fn frames_are_bounded_strict_json_and_do_not_consume_a_second_request() {
        let mut bytes = std::io::Cursor::new(b"{\"schema\":1}SECOND");
        assert_eq!(read_frame(&mut bytes, 12).unwrap()["schema"], 1);
        assert_eq!(bytes.position(), 12);
        for (mut input, length) in [
            (b"{}".as_slice(), 0),
            (b"{}", CAP + 1),
            (b"{}", 3),
            (b"[1]", 3),
            (b"{x}", 3),
            (b"{\"x\":\"\xff\"}", 9),
        ] {
            assert!(read_frame(&mut input, length).is_err());
        }
        assert_eq!(read_frame(&mut b"{}".as_slice(), 2).unwrap(), json!({}));
        assert_eq!(plan("status").definition, "synthetic definition");
    }
    #[test]
    fn malformed_plans_refuse_before_the_scheduler_is_opened() {
        let p = plan("status");
        let request = json!({"schema":1,"operation":"status","plan":{"schema":1,"operation":"preview","label":"hgs-desktop","configuration":p.configuration,"user_sid":p.sid,"task_name":p.name,"definition":p.definition,"argv":p.argv,"session":{"kind":"interactive_user","allow_session_zero":false}}});
        for pointer in [
            "/schema",
            "/operation",
            "/plan/schema",
            "/plan/operation",
            "/plan/configuration",
            "/plan/user_sid",
            "/plan/task_name",
            "/plan/argv/1",
            "/plan/session/kind",
            "/plan/session/allow_session_zero",
        ] {
            let mut changed = request.clone();
            *changed.pointer_mut(pointer).unwrap() = Value::Null;
            assert!(Plan::parse(&changed, &p.sid).is_err(), "{pointer}");
        }
        let mut changed = request.clone();
        changed["plan"]["argv"]
            .as_array_mut()
            .unwrap()
            .push("--background".into());
        assert!(Plan::parse(&changed, &p.sid).is_err());
        for sid in [
            "S-1-5-18",
            "S-1-5-19",
            "S-1-5-20",
            "S-1-5-80-123",
            "bad/../SID",
        ] {
            assert!(Plan::parse(&request, sid).is_err());
        }
    }
    #[test]
    fn owned_round_trip_is_idempotent_and_never_replays_unknown_creation() {
        let p = plan("install");
        let wanted = p.shape("args".into());
        let mut s = Fake {
            task: None,
            wanted: wanted.clone(),
            writes: vec![],
            fail_after_create: false,
        };
        assert_eq!(perform(&p, &wanted, &mut s).unwrap()["changed"], true);
        assert_eq!(perform(&p, &wanted, &mut s).unwrap()["changed"], false);
        assert_eq!(
            perform(&plan("remove"), &wanted, &mut s).unwrap()["registered"],
            false
        );
        assert_eq!(s.writes, ["create", "delete"]);
        s.fail_after_create = true;
        assert!(perform(&p, &wanted, &mut s).is_err());
        assert_eq!(s.writes, ["create", "delete", "create"]);
        assert!(s.task.is_some()); // No rollback, second write or false success.
    }
    #[test]
    fn legacy_conflicts_and_unconfirmed_reads_or_removal_never_replay() {
        struct Cut {
            original: Option<Task>,
            legacy: bool,
            unreadable: bool,
            writes: Vec<&'static str>,
        }
        impl Scheduler for Cut {
            fn read(&mut self, name: &str) -> Result<Option<Task>> {
                ensure!(!self.unreadable, "access denied or disconnected");
                Ok(if name == "hgs-desktop" {
                    self.legacy
                        .then(|| task(plan("status").shape("args".into())))
                } else {
                    self.original.clone()
                })
            }
            fn create_only(&mut self, _: &Plan) -> Result<()> {
                self.writes.push("create");
                bail!("create-only race refused")
            }
            fn delete(&mut self, _: &str) -> Result<()> {
                self.writes.push("delete");
                self.original = None;
                bail!("lost result after deletion")
            }
        }
        let wanted = plan("status").shape("args".into());
        for op in ["status", "install", "remove"] {
            let mut s = Cut {
                original: None,
                legacy: false,
                unreadable: true,
                writes: vec![],
            };
            assert!(perform(&plan(op), &wanted, &mut s).is_err());
            assert!(s.writes.is_empty());
        }
        let mut s = Cut {
            original: None,
            legacy: true,
            unreadable: false,
            writes: vec![],
        };
        assert!(perform(&plan("install"), &wanted, &mut s).is_err());
        assert!(s.writes.is_empty());
        s.legacy = false;
        assert!(perform(&plan("install"), &wanted, &mut s).is_err());
        assert_eq!(s.writes, ["create"]);
        s.original = Some(task(wanted.clone()));
        assert!(perform(&plan("remove"), &wanted, &mut s).is_err());
        assert_eq!(s.writes, ["create", "delete"]);
        assert!(s.original.is_none()); // No retry, rollback or false settled result.
    }
    #[test]
    fn changed_definition_or_unknown_acl_refuses_before_any_effect() {
        let p = plan("remove");
        let wanted = p.shape("args".into());
        let mutations: &[fn(&mut Task)] = &[
            |t| t.shape.action.path.push('x'),
            |t| t.shape.action.arguments.push('x'),
            |t| t.shape.context.clear(),
            |t| t.shape.user.push('x'),
            |t| t.shape.run_level = 1,
            |t| t.shape.trigger.enabled = false,
            |t| t.shape.trigger.delay = "PT1M".into(),
            |t| t.shape.settings.network = true,
            |t| t.shape.settings.retries = 4,
            |t| t.enabled = false,
            |t| t.owner.clear(),
            |t| t.acl = None,
            |t| {
                t.acl.as_mut().unwrap().push(Ace {
                    allowed: true,
                    sid: "S-1-1-0".into(),
                })
            },
            |t| t.acl.as_mut().unwrap()[0].allowed = false,
        ];
        for mutate in mutations {
            let mut t = task(wanted.clone());
            mutate(&mut t);
            let mut s = Fake {
                task: Some(t),
                wanted: wanted.clone(),
                writes: vec![],
                fail_after_create: false,
            };
            assert!(perform(&p, &wanted, &mut s).is_err());
            assert!(s.writes.is_empty());
            assert!(s.task.is_some());
        }
    }
}
