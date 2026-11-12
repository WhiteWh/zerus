//! Optional initial-dialog projection. Classification cannot change discovery,
//! its complete execution manifest, custody or any later Stop/Force input.
use super::*;
use wire::presentation::{Identity, Presentation};

#[derive(Clone, Debug, PartialEq)]
struct Sample {
    native: crate::platform::ProcessIdentity,
    stamp: String,
}

fn sample(pid: u32, until: Instant) -> Option<Sample> {
    if Instant::now() >= until {
        return None;
    }
    let native = crate::platform::process_identity(pid)?;
    // The saved legacy stamp is only a record correlation, NEVER the native
    // birth. Bracket this second read to reject reuse/change during sampling.
    let stamp = crate::platform::process_start_time(pid)?;
    let after = crate::platform::process_identity(pid)?;
    (native == after && !stamp.is_empty() && Instant::now() < until)
        .then_some(Sample { native, stamp })
}

fn binding(record: &Value) -> Option<Value> {
    for key in [
        "name",
        "run_id",
        "conversation_id",
        "pane",
        "agent",
        "process_start",
    ] {
        if record[key].as_str()?.is_empty() {
            return None;
        }
    }
    if record["supervisor"]["start"].as_str()?.is_empty() {
        return None;
    }
    // Capture/owner/configuration are independently checked by discovery. Keep
    // the exact original launch/terminal record through this same census too.
    Some(json!({
        "name": record["name"], "run": record["run_id"],
        "conversation": record["conversation_id"], "pane": record["pane"],
        "agent": record["agent"], "pid": record["pid"],
        "start": record["process_start"], "supervisor": record["supervisor"],
        "owner": record["worktree_owner"], "launch": record["launch_id"],
        "terminal": record["terminal"], "session": record["session_id"],
    }))
}

pub(super) struct Pair {
    record: Value,
    agent: (u32, Sample),
    supervisor: (u32, Sample),
}
impl Pair {
    pub(super) fn before(record: &Value, until: Instant) -> Option<Self> {
        Self::read(record, until, sample)
    }
    fn read(
        record: &Value,
        until: Instant,
        mut observe: impl FnMut(u32, Instant) -> Option<Sample>,
    ) -> Option<Self> {
        let binding = binding(record)?;
        let pid = |v: &Value| {
            v.as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0)
        };
        let agent = pid(&record["pid"])?;
        let supervisor = pid(&record["supervisor"]["pid"])?;
        if agent == supervisor {
            return None;
        }
        let agent_sample = observe(agent, until)?;
        let supervisor_sample = observe(supervisor, until)?;
        if agent_sample.stamp != record["process_start"]
            || supervisor_sample.stamp != record["supervisor"]["start"]
            || agent_sample.native.parent != supervisor
            || agent_sample.native.birth.is_empty()
            || supervisor_sample.native.birth.is_empty()
            || Instant::now() >= until
        {
            return None;
        }
        Some(Self {
            record: binding,
            agent: (agent, agent_sample),
            supervisor: (supervisor, supervisor_sample),
        })
    }
    pub(super) fn finish(
        self,
        record: &Value,
        manifest: &wire::Manifest,
        until: Instant,
    ) -> Option<Presentation> {
        self.finish_with(record, manifest, until, sample)
    }
    fn finish_with(
        self,
        record: &Value,
        manifest: &wire::Manifest,
        until: Instant,
        observe: impl FnMut(u32, Instant) -> Option<Sample>,
    ) -> Option<Presentation> {
        let after = Self::read(record, until, observe)?;
        if self.record != after.record
            || self.agent != after.agent
            || self.supervisor != after.supervisor
        {
            return None;
        }
        Some(Presentation::new(
            manifest,
            Identity {
                pid: self.agent.0,
                birth: self.agent.1.native.birth,
            },
            Identity {
                pid: self.supervisor.0,
                birth: self.supervisor.1.native.birth,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> Value {
        json!({"name":"codex/example/original","run_id":"run-a","conversation_id":"conversation-a",
            "agent":"codex","pane":"%12","pid":10,"process_start":"saved-agent",
            "supervisor":{"pid":20,"start":"saved-supervisor"},
            "worktree_owner":{"id":"owner-a"},"launch_id":"launch-a"})
    }
    fn observed(pid: u32, _: Instant) -> Option<Sample> {
        Some(Sample {
            native: crate::platform::ProcessIdentity {
                parent: if pid == 10 { 20 } else { 1 },
                group: Some(20),
                birth: format!("native-{pid}"),
                started: 100.,
            },
            stamp: if pid == 10 {
                "saved-agent"
            } else {
                "saved-supervisor"
            }
            .into(),
        })
    }
    fn manifest() -> wire::Manifest {
        serde_json::from_value(json!({"abi":wire::ABI,"id":"11111111-1111-4111-8111-111111111111",
            "capture":{"target":{"computer":"","name":"codex/example/original","run_id":"run-a","conversation_id":"conversation-a"},
                "machine":"11111111-1111-4111-8111-111111111111","root":"/srv/example/work","common_dir":"/srv/example/main/.git","owner_id":"11111111-1111-4111-8111-111111111111"},
            "configuration":"a".repeat(64),"boot":"11111111-1111-4111-8111-111111111111",
            "checkout":{"device":1,"inode":2,"mount":"native-mount"},"issued":1000,"expires":1300,
            "programs":[{"pid":10,"birth":"native-10","label":"agent"},
                {"pid":20,"birth":"native-20","label":"hgs"},{"pid":30,"birth":"native-30","label":"hgs"},
                {"pid":40,"birth":"native-40","label":"agent child"}]})).unwrap()
    }
    #[test]
    fn exact_record_pair_brackets_census_but_stranger_and_descendant_stay_visible() {
        let until = Instant::now() + Duration::from_secs(5);
        let record = record();
        let manifest = manifest();
        let binding = manifest.binding();
        let before = Pair::read(&record, until, observed).unwrap();
        let display = before
            .finish_with(&record, &manifest, until, observed)
            .unwrap();
        assert_eq!(
            Presentation::visible(Some(&display), &manifest)
                .iter()
                .map(|p| p.pid)
                .collect::<Vec<_>>(),
            [30, 40]
        );
        assert_eq!(manifest.programs.len(), 4);
        assert_eq!(manifest.binding(), binding);
    }
    #[test]
    fn stale_missing_changed_record_or_native_pair_never_hides_rows() {
        let until = Instant::now() + Duration::from_secs(5);
        for key in [
            "run_id",
            "conversation_id",
            "pane",
            "worktree_owner",
            "launch_id",
            "pid",
            "supervisor",
        ] {
            let record = record();
            let before = Pair::read(&record, until, observed).unwrap();
            let mut after = record.clone();
            after[key] = Value::Null;
            assert!(
                before
                    .finish_with(&after, &manifest(), until, observed)
                    .is_none(),
                "{key}"
            );
        }
        for change in 0..5 {
            let record = record();
            let before = Pair::read(&record, until, observed).unwrap();
            let after = |pid, deadline| {
                let mut value = observed(pid, deadline)?;
                match change {
                    0 => value.native.birth = "reused".into(),
                    1 => value.native.parent = 999,
                    2 => value.stamp = "stale".into(),
                    3 => return None,
                    _ => value.native.started += 1.,
                }
                Some(value)
            };
            assert!(before
                .finish_with(&record, &manifest(), until, after)
                .is_none());
        }
        // A stable stranger/descendant with matching saved stamps still is not
        // the record-bound direct pair. Pin initial admission, not only drift.
        for change in 0..3 {
            let unrelated = |pid, deadline| {
                let mut value = observed(pid, deadline)?;
                if pid == 10 {
                    match change {
                        0 => value.native.parent = 40,
                        1 => value.stamp = "stale".into(),
                        _ => value.native.birth.clear(),
                    }
                }
                Some(value)
            };
            assert!(Pair::read(&record(), until, unrelated).is_none());
        }
        let mut missing = record();
        missing["process_start"] = Value::Null;
        assert!(Pair::read(&missing, until, observed).is_none());
        missing = record();
        missing["pid"] = json!(20);
        assert!(Pair::read(&missing, until, observed).is_none());
        assert!(Pair::read(&record(), Instant::now(), observed).is_none());
    }
    #[test]
    fn current_native_child_and_parent_correlate_without_name_matching() {
        use std::process::{Command, Stdio};
        // Owned private child only. Its test parent is the synthetic supervisor.
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let supervisor = std::process::id();
        let until = Instant::now() + Duration::from_secs(5);
        let mut record = record();
        record["pid"] = json!(pid);
        record["process_start"] = json!(crate::platform::process_start_time(pid).unwrap());
        record["supervisor"] = json!({"pid":supervisor,"start":crate::platform::process_start_time(supervisor).unwrap()});
        let before = Pair::before(&record, until);
        let mut manifest = manifest();
        manifest.programs = vec![wire::Program {
            pid,
            birth: crate::platform::process_identity(pid).unwrap().birth,
            label: "unrelated label".into(),
        }];
        let result = before.and_then(|pair| pair.finish(&record, &manifest, until));
        drop(child.stdin.take());
        let status = child.wait().unwrap();
        assert!(status.success());
        assert!(result.is_some());
        assert!(Presentation::visible(result.as_ref(), &manifest).is_empty());
    }

    #[test]
    fn pid_one_native_pairs_are_valid_but_pid_zero_is_refused() {
        let until = Instant::now() + Duration::from_secs(5);
        let mut supervisor_init = record();
        supervisor_init["supervisor"] = json!({"pid":1,"start":"saved-supervisor"});
        let observed_supervisor = |pid, deadline| {
            let mut sample = observed(pid, deadline)?;
            if pid == 10 {
                sample.native.parent = 1;
            }
            Some(sample)
        };
        assert!(Pair::read(&supervisor_init, until, observed_supervisor).is_some());

        let mut agent_init = record();
        agent_init["pid"] = json!(1);
        let observed_agent = |pid, deadline| {
            let mut sample = observed(pid, deadline)?;
            if pid == 1 {
                sample.native.parent = 20;
                sample.stamp = "saved-agent".into();
            }
            Some(sample)
        };
        assert!(Pair::read(&agent_init, until, observed_agent).is_some());

        let mut invalid = record();
        invalid["supervisor"]["pid"] = json!(0);
        assert!(Pair::read(&invalid, until, observed_supervisor).is_none());
    }
}
